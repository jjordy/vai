//! Workspace diff computation.
//!
//! Computes file-level and entity-level changes between a workspace's overlay
//! directory and the base version of the repository. Used by `vai workspace diff`
//! to show what an agent has changed so far.
//!
//! ## Design
//!
//! The overlay directory in `.vai/workspaces/<id>/overlay/` mirrors the project
//! root. Any file present there has been added or modified by the agent. We walk
//! the overlay, compare content against the project root, and re-parse Rust files
//! with tree-sitter to get entity-level changes.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::event_log::{EntitySummary, EventKind, EventLog, EventLogError};
use crate::graph::{Entity, EntityKind, GraphError, parse_rust_source};
use crate::workspace::{self, WorkspaceError, WorkspaceStatus};

// ── Errors ────────────────────────────────────────────────────────────────────

/// Errors from diff operations.
#[derive(Debug, Error)]
pub enum DiffError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Workspace error: {0}")]
    Workspace(#[from] WorkspaceError),

    #[error("Graph error: {0}")]
    Graph(#[from] GraphError),

    #[error("Event log error: {0}")]
    EventLog(#[from] EventLogError),
}

// ── Result types ──────────────────────────────────────────────────────────────

/// How a file was changed within a workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileChangeType {
    /// File exists in the overlay but not in the project root.
    Added,
    /// File exists in both places but content differs.
    Modified,
    /// File existed in the project root but was deleted by the agent.
    Deleted,
}

/// File-level change recorded for a workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDiff {
    /// File path relative to the repository root.
    pub path: String,
    /// How the file was changed.
    pub change_type: FileChangeType,
    /// Total line count in the modified version.
    pub lines: usize,
    /// SHA-256 of the overlay file content.
    pub content_hash: String,
}

/// How a semantic entity was changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntityChangeType {
    /// Entity is new — not present in the base version.
    Added,
    /// Entity exists in both versions but its content changed.
    Modified,
    /// Entity was present in the base version but is absent in the overlay.
    Removed,
}

/// Entity-level change within a workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityChange {
    /// Stable entity ID (SHA-256 of `file_path::qualified_name`).
    pub entity_id: String,
    /// Human-readable qualified name (e.g., `AuthService::validate_token`).
    pub qualified_name: String,
    /// Kind of this entity.
    pub kind: EntityKind,
    /// File path relative to the repository root.
    pub file_path: String,
    /// How this entity was changed.
    pub change_type: EntityChangeType,
    /// Line range in the new version (absent for removed entities).
    pub line_range: Option<(usize, usize)>,
}

/// Complete diff result for the active workspace.
#[derive(Debug, Serialize, Deserialize)]
pub struct WorkspaceDiff {
    /// ID of the workspace this diff belongs to.
    pub workspace_id: Uuid,
    /// Version that was HEAD when this workspace was created.
    pub base_version: String,
    /// File-level changes.
    pub file_diffs: Vec<FileDiff>,
    /// Entity-level changes.
    pub entity_changes: Vec<EntityChange>,
}

impl WorkspaceDiff {
    /// Returns `true` if there are no changes in this diff.
    pub fn is_empty(&self) -> bool {
        self.file_diffs.is_empty() && self.entity_changes.is_empty()
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Computes the diff for the currently active workspace.
///
/// Walks the overlay directory, compares files against the project root,
/// and re-parses Rust source files to detect entity-level changes.
/// Does **not** write events or update workspace state.
pub fn compute(vai_dir: &Path, repo_root: &Path) -> Result<WorkspaceDiff, DiffError> {
    let meta = workspace::active(vai_dir)?;
    let overlay = workspace::overlay_dir(vai_dir, &meta.id.to_string());

    let mut file_diffs = Vec::new();
    let mut entity_changes = Vec::new();

    if overlay.exists() {
        let overlay_files = collect_files(&overlay)?;
        for abs_path in overlay_files {
            let rel_path = abs_path
                .strip_prefix(&overlay)
                .expect("path inside overlay")
                .to_string_lossy()
                .to_string();

            let overlay_content = fs::read(&abs_path)?;
            let content_hash = sha256_hex(&overlay_content);
            let lines = count_lines(&overlay_content);

            let base_file = repo_root.join(&rel_path);
            let change_type = if base_file.exists() {
                FileChangeType::Modified
            } else {
                FileChangeType::Added
            };

            file_diffs.push(FileDiff {
                path: rel_path.clone(),
                change_type,
                lines,
                content_hash,
            });

            // Entity-level diff for Rust files.
            if rel_path.ends_with(".rs") {
                let base_content = if base_file.exists() {
                    Some(fs::read(&base_file)?)
                } else {
                    None
                };
                compute_entity_diff(
                    &rel_path,
                    &overlay_content,
                    base_content.as_deref(),
                    &mut entity_changes,
                )?;
            }
        }
    }

    // Check the deletion manifest for files that were explicitly deleted.
    // The manifest lives at `.vai/workspaces/<id>/.vai-deleted` (sibling of overlay/).
    let ws_dir = vai_dir
        .join("workspaces")
        .join(meta.id.to_string());
    let manifest_path = ws_dir.join(".vai-deleted");
    if manifest_path.exists() {
        let bytes = fs::read(&manifest_path)?;
        if let Ok(deleted_paths) = serde_json::from_slice::<Vec<String>>(&bytes) {
            for path in deleted_paths {
                // Only record as Deleted if the file actually exists in the base.
                let base_file = repo_root.join(&path);
                if base_file.exists() {
                    file_diffs.push(FileDiff {
                        path: path.clone(),
                        change_type: FileChangeType::Deleted,
                        lines: 0,
                        content_hash: String::new(),
                    });

                    // Entity-level: mark all entities in the deleted file as removed.
                    if path.ends_with(".rs") {
                        if let Ok(base_content) = fs::read(&base_file) {
                            if let Ok((entities, _)) =
                                crate::graph::parse_rust_source(&path, &base_content)
                            {
                                for entity in entities {
                                    entity_changes.push(EntityChange {
                                        entity_id: entity.id.clone(),
                                        qualified_name: entity.qualified_name.clone(),
                                        kind: entity.kind.clone(),
                                        file_path: path.clone(),
                                        change_type: EntityChangeType::Removed,
                                        line_range: None,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(WorkspaceDiff {
        workspace_id: meta.id,
        base_version: meta.base_version,
        file_diffs,
        entity_changes,
    })
}

/// Records file and entity change events to the event log for a computed diff.
///
/// Also transitions the workspace status from `Created` → `Active` if needed.
/// This should be called once when the diff is first computed; subsequent
/// `vai workspace diff` runs will not re-record events.
pub fn record_events(
    vai_dir: &Path,
    diff: &WorkspaceDiff,
) -> Result<(), DiffError> {
    let mut meta = workspace::active(vai_dir)?;

    // Only record events once — on the first diff after workspace creation.
    if meta.status != WorkspaceStatus::Created {
        return Ok(());
    }

    let log_dir = vai_dir.join("event_log");
    let mut log = EventLog::open(&log_dir)?;

    // Record file events.
    for fd in &diff.file_diffs {
        match fd.change_type {
            FileChangeType::Added => {
                log.append(EventKind::FileAdded {
                    workspace_id: diff.workspace_id,
                    path: fd.path.clone(),
                    hash: fd.content_hash.clone(),
                })?;
            }
            FileChangeType::Modified => {
                log.append(EventKind::FileModified {
                    workspace_id: diff.workspace_id,
                    path: fd.path.clone(),
                    old_hash: String::new(), // base hash not stored; acceptable placeholder
                    new_hash: fd.content_hash.clone(),
                })?;
            }
            FileChangeType::Deleted => {
                log.append(EventKind::FileRemoved {
                    workspace_id: diff.workspace_id,
                    path: fd.path.clone(),
                })?;
            }
        }
    }

    // Record entity events.
    for ec in &diff.entity_changes {
        match ec.change_type {
            EntityChangeType::Added => {
                log.append(EventKind::EntityAdded {
                    workspace_id: diff.workspace_id,
                    entity: EntitySummary {
                        id: ec.entity_id.clone(),
                        kind: ec.kind.as_str().to_string(),
                        name: ec.qualified_name.split("::").last().unwrap_or("").to_string(),
                        qualified_name: ec.qualified_name.clone(),
                        file_path: ec.file_path.clone(),
                    },
                })?;
            }
            EntityChangeType::Modified => {
                log.append(EventKind::EntityModified {
                    workspace_id: diff.workspace_id,
                    entity_id: ec.entity_id.clone(),
                    change_description: format!(
                        "{} `{}` modified in {}",
                        ec.kind.as_str(),
                        ec.qualified_name,
                        ec.file_path
                    ),
                })?;
            }
            EntityChangeType::Removed => {
                log.append(EventKind::EntityRemoved {
                    workspace_id: diff.workspace_id,
                    entity_id: ec.entity_id.clone(),
                })?;
            }
        }
    }

    // Transition workspace to Active.
    meta.status = WorkspaceStatus::Active;
    meta.updated_at = chrono::Utc::now();
    workspace::update_meta(vai_dir, &meta)?;

    Ok(())
}

// ── Private helpers ───────────────────────────────────────────────────────────

/// Computes entity-level changes for a single file.
///
/// Parses both the overlay and base versions of the file, then compares
/// entity IDs. Content hashes of each entity's byte span are used to detect
/// modifications.
fn compute_entity_diff(
    rel_path: &str,
    overlay_content: &[u8],
    base_content: Option<&[u8]>,
    changes: &mut Vec<EntityChange>,
) -> Result<(), DiffError> {
    let (overlay_entities, _) = parse_rust_source(rel_path, overlay_content)?;
    let overlay_map: HashMap<String, Entity> = overlay_entities
        .into_iter()
        .map(|e| (e.id.clone(), e))
        .collect();

    if let Some(base) = base_content {
        let (base_entities, _) = parse_rust_source(rel_path, base)?;
        let base_map: HashMap<String, Entity> = base_entities
            .into_iter()
            .map(|e| (e.id.clone(), e))
            .collect();

        // Added or modified entities.
        for (id, entity) in &overlay_map {
            if let Some(base_entity) = base_map.get(id) {
                // Same identity — check if content bytes changed.
                let b_start = base_entity.byte_range.0;
                let b_end = base_entity.byte_range.1.min(base.len());
                let o_start = entity.byte_range.0;
                let o_end = entity.byte_range.1.min(overlay_content.len());

                let base_bytes = base.get(b_start..b_end).unwrap_or(&[]);
                let overlay_bytes = overlay_content.get(o_start..o_end).unwrap_or(&[]);

                if base_bytes != overlay_bytes {
                    changes.push(EntityChange {
                        entity_id: id.clone(),
                        qualified_name: entity.qualified_name.clone(),
                        kind: entity.kind.clone(),
                        file_path: rel_path.to_string(),
                        change_type: EntityChangeType::Modified,
                        line_range: Some(entity.line_range),
                    });
                }
            } else {
                changes.push(EntityChange {
                    entity_id: id.clone(),
                    qualified_name: entity.qualified_name.clone(),
                    kind: entity.kind.clone(),
                    file_path: rel_path.to_string(),
                    change_type: EntityChangeType::Added,
                    line_range: Some(entity.line_range),
                });
            }
        }

        // Removed entities.
        for (id, entity) in &base_map {
            if !overlay_map.contains_key(id) {
                changes.push(EntityChange {
                    entity_id: id.clone(),
                    qualified_name: entity.qualified_name.clone(),
                    kind: entity.kind.clone(),
                    file_path: rel_path.to_string(),
                    change_type: EntityChangeType::Removed,
                    line_range: None,
                });
            }
        }
    } else {
        // New file — all entities are additions.
        for entity in overlay_map.into_values() {
            changes.push(EntityChange {
                entity_id: entity.id.clone(),
                qualified_name: entity.qualified_name.clone(),
                kind: entity.kind.clone(),
                file_path: rel_path.to_string(),
                change_type: EntityChangeType::Added,
                line_range: Some(entity.line_range),
            });
        }
    }

    Ok(())
}

/// Recursively collects all file paths under `dir`.
fn collect_files(dir: &Path) -> Result<Vec<PathBuf>, DiffError> {
    let mut files = Vec::new();
    collect_files_recursive(dir, &mut files)?;
    Ok(files)
}

fn collect_files_recursive(dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), DiffError> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_files_recursive(&path, files)?;
        } else {
            files.push(path);
        }
    }
    Ok(())
}

/// Counts the number of lines in `content`.
fn count_lines(content: &[u8]) -> usize {
    if content.is_empty() {
        return 0;
    }
    content.iter().filter(|&&b| b == b'\n').count() + 1
}

/// Returns the hex-encoded SHA-256 digest of `data`.
fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup_repo(source_files: &[(&str, &str)]) -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_path_buf();

        // Write source files.
        for (rel_path, content) in source_files {
            let abs = root.join(rel_path);
            if let Some(parent) = abs.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&abs, content).unwrap();
        }

        // Create minimal .vai structure.
        let vai_dir = root.join(".vai");
        fs::create_dir_all(vai_dir.join("event_log")).unwrap();
        fs::create_dir_all(vai_dir.join("workspaces")).unwrap();

        (dir, root)
    }

    fn make_overlay(vai_dir: &Path, ws_id: &Uuid, files: &[(&str, &str)]) {
        let overlay = vai_dir
            .join("workspaces")
            .join(ws_id.to_string())
            .join("overlay");
        for (rel_path, content) in files {
            let abs = overlay.join(rel_path);
            if let Some(parent) = abs.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&abs, content).unwrap();
        }
    }

    const BASE_RS: &str = r#"fn hello() -> &'static str {
    "hello"
}

fn world() -> &'static str {
    "world"
}
"#;

    const MODIFIED_RS: &str = r#"fn hello() -> &'static str {
    "hello, world!"
}

fn world() -> &'static str {
    "world"
}

fn new_fn() -> u32 {
    42
}
"#;

    #[test]
    fn test_added_file_detected() {
        let (_dir, root) = setup_repo(&[]);
        let vai_dir = root.join(".vai");

        let result = crate::workspace::create(&vai_dir, "test intent", "v1").unwrap();
        let ws_id = result.workspace.id;

        make_overlay(&vai_dir, &ws_id, &[("src/new.rs", "fn foo() {}")]);

        let diff = compute(&vai_dir, &root).unwrap();

        assert_eq!(diff.file_diffs.len(), 1);
        let fd = &diff.file_diffs[0];
        assert_eq!(fd.path, "src/new.rs");
        assert_eq!(fd.change_type, FileChangeType::Added);
        assert!(!fd.content_hash.is_empty(), "content_hash should be populated for added files");

        // Entity-level: should detect `foo` as added.
        assert!(
            !diff.entity_changes.is_empty(),
            "entity changes should be detected for the new file"
        );
        assert!(
            diff.entity_changes.iter().any(|c| c.qualified_name == "foo"
                && c.change_type == EntityChangeType::Added),
            "foo should be detected as an added entity, got: {:?}",
            diff.entity_changes.iter().map(|c| (&c.qualified_name, &c.change_type)).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_modified_file_detected() {
        let (_dir, root) = setup_repo(&[("src/lib.rs", BASE_RS)]);
        let vai_dir = root.join(".vai");

        let result = crate::workspace::create(&vai_dir, "test intent", "v1").unwrap();
        let ws_id = result.workspace.id;

        make_overlay(&vai_dir, &ws_id, &[("src/lib.rs", MODIFIED_RS)]);

        let diff = compute(&vai_dir, &root).unwrap();

        assert_eq!(diff.file_diffs.len(), 1);
        assert_eq!(diff.file_diffs[0].change_type, FileChangeType::Modified);
    }

    #[test]
    fn test_entity_added_detected() {
        let (_dir, root) = setup_repo(&[("src/lib.rs", BASE_RS)]);
        let vai_dir = root.join(".vai");

        let result = crate::workspace::create(&vai_dir, "test intent", "v1").unwrap();
        let ws_id = result.workspace.id;

        make_overlay(&vai_dir, &ws_id, &[("src/lib.rs", MODIFIED_RS)]);

        let diff = compute(&vai_dir, &root).unwrap();

        let added: Vec<_> = diff
            .entity_changes
            .iter()
            .filter(|c| c.change_type == EntityChangeType::Added)
            .collect();
        assert_eq!(added.len(), 1, "exactly one entity should be added (new_fn)");
        assert_eq!(
            added[0].qualified_name, "new_fn",
            "the added entity should be new_fn"
        );
        assert_eq!(added[0].file_path, "src/lib.rs");
    }

    #[test]
    fn test_entity_modified_detected() {
        let (_dir, root) = setup_repo(&[("src/lib.rs", BASE_RS)]);
        let vai_dir = root.join(".vai");

        let result = crate::workspace::create(&vai_dir, "test intent", "v1").unwrap();
        let ws_id = result.workspace.id;

        make_overlay(&vai_dir, &ws_id, &[("src/lib.rs", MODIFIED_RS)]);

        let diff = compute(&vai_dir, &root).unwrap();

        let modified: Vec<_> = diff
            .entity_changes
            .iter()
            .filter(|c| c.change_type == EntityChangeType::Modified)
            .collect();
        assert_eq!(modified.len(), 1, "exactly one entity should be modified (hello)");
        assert_eq!(
            modified[0].qualified_name, "hello",
            "the modified entity should be hello"
        );
        assert_eq!(modified[0].file_path, "src/lib.rs");
    }

    #[test]
    fn test_entity_removed_detected() {
        let base = r#"fn keep() {}
fn remove_me() {}
"#;
        let overlay = r#"fn keep() {}
"#;
        let (_dir, root) = setup_repo(&[("src/lib.rs", base)]);
        let vai_dir = root.join(".vai");

        let result = crate::workspace::create(&vai_dir, "test intent", "v1").unwrap();
        let ws_id = result.workspace.id;

        make_overlay(&vai_dir, &ws_id, &[("src/lib.rs", overlay)]);

        let diff = compute(&vai_dir, &root).unwrap();

        let removed: Vec<_> = diff
            .entity_changes
            .iter()
            .filter(|c| c.change_type == EntityChangeType::Removed)
            .collect();
        assert_eq!(removed.len(), 1, "exactly one entity should be removed (remove_me)");
        assert_eq!(
            removed[0].qualified_name, "remove_me",
            "the removed entity should be remove_me"
        );
        assert_eq!(removed[0].file_path, "src/lib.rs");
    }

    #[test]
    fn test_empty_overlay_returns_empty_diff() {
        let (_dir, root) = setup_repo(&[("src/lib.rs", BASE_RS)]);
        let vai_dir = root.join(".vai");

        crate::workspace::create(&vai_dir, "test intent", "v1").unwrap();

        let diff = compute(&vai_dir, &root).unwrap();
        assert!(diff.is_empty());
    }

}
