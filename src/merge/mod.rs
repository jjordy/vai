//! Semantic merge engine — workspace submission and fast-forward merge.
//!
//! When an agent submits a workspace, the merge engine determines how to
//! integrate the changes into the main version history. This module currently
//! implements the simplest merge path: fast-forward merge, used when no other
//! changes have occurred since the workspace was created.
//!
//! ## Fast-Forward Merge
//!
//! A fast-forward merge applies when `HEAD == workspace.base_version`. In that
//! case there is nothing to reconcile — we simply copy the overlay files into
//! the project root, update the semantic graph, and create a new version.
//!
//! ## Three-Level Semantic Merge (future)
//!
//! When HEAD has advanced, the engine will perform three-level analysis:
//! 1. **Textual** — detect overlapping line changes, auto-merge non-overlapping.
//! 2. **Structural (AST)** — detect changes to different entities in the same
//!    file; auto-merge if they don't overlap at the AST level.
//! 3. **Referential** — check semantic graph for dependency conflicts (rename
//!    vs usage, signature change vs caller).

use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::Serialize;
use thiserror::Error;

use crate::diff::{self, DiffError};
use crate::event_log::{EventKind, EventLog, EventLogError};
use crate::graph::{GraphError, GraphSnapshot};
use crate::repo::{self, RepoError};
use crate::version::{self, VersionError, VersionMeta};
use crate::workspace::{self, WorkspaceError, WorkspaceStatus};

// ── Errors ────────────────────────────────────────────────────────────────────

/// Errors from merge operations.
#[derive(Debug, Error)]
pub enum MergeError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Workspace error: {0}")]
    Workspace(#[from] WorkspaceError),

    #[error("Diff error: {0}")]
    Diff(#[from] DiffError),

    #[error("Event log error: {0}")]
    EventLog(#[from] EventLogError),

    #[error("Graph error: {0}")]
    Graph(#[from] GraphError),

    #[error("Version error: {0}")]
    Version(#[from] VersionError),

    #[error("Repo error: {0}")]
    Repo(#[from] RepoError),

    #[error(
        "HEAD has advanced since workspace creation — fast-forward not possible \
         (workspace base: {base}, current HEAD: {current})"
    )]
    HeadAdvanced { base: String, current: String },
}

// ── Result types ──────────────────────────────────────────────────────────────

/// Result of a successful workspace submission and merge.
#[derive(Debug, Serialize)]
pub struct SubmitResult {
    /// The new version created by this merge.
    pub version: VersionMeta,
    /// Number of files applied to the project root.
    pub files_applied: usize,
    /// Number of entity-level changes (added + modified + removed).
    pub entities_changed: usize,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Submits the active workspace for merging and performs a fast-forward merge.
///
/// ## Steps
///
/// 1. Compute the workspace diff.
/// 2. Record a `WorkspaceSubmitted` event.
/// 3. Check that HEAD has not advanced (fast-forward guard).
/// 4. Apply overlay files to the project root.
/// 5. Update the semantic graph for all changed `.rs` files.
/// 6. Determine the next version ID.
/// 7. Record `MergeCompleted` and `VersionCreated` events.
/// 8. Write the version metadata to `.vai/versions/<id>.toml`.
/// 9. Advance HEAD and mark the workspace as `Merged`.
///
/// Returns `MergeError::HeadAdvanced` if another workspace has already
/// been merged since this workspace was created.
pub fn submit(vai_dir: &Path, repo_root: &Path) -> Result<SubmitResult, MergeError> {
    let ws_meta = workspace::active(vai_dir)?;

    // 1. Compute diff and record file/entity events (idempotent — skipped if
    //    events were already recorded by a prior `vai workspace diff` call).
    let workspace_diff = diff::compute(vai_dir, repo_root)?;
    diff::record_events(vai_dir, &workspace_diff)?;

    // 2. Record WorkspaceSubmitted.
    let log_dir = vai_dir.join("event_log");
    let mut log = EventLog::open(&log_dir)?;
    let changes_summary = format!(
        "{} file(s), {} entity change(s)",
        workspace_diff.file_diffs.len(),
        workspace_diff.entity_changes.len()
    );
    log.append(EventKind::WorkspaceSubmitted {
        workspace_id: ws_meta.id,
        changes_summary,
    })?;

    // 3. Fast-forward guard: HEAD must not have advanced.
    let current_head = repo::read_head(vai_dir)?;
    if current_head != ws_meta.base_version {
        return Err(MergeError::HeadAdvanced {
            base: ws_meta.base_version,
            current: current_head,
        });
    }

    // 4. Save pre-change snapshot for rollback support.
    //    Must happen BEFORE applying overlay so we capture the pre-change state.
    let new_version_id_preview = version::next_version_id(vai_dir)?;
    save_pre_change_snapshot(vai_dir, &new_version_id_preview, &workspace_diff, repo_root)?;

    // 5. Apply overlay files to the project root.
    let overlay = workspace::overlay_dir(vai_dir, &ws_meta.id.to_string());
    let files_applied = apply_overlay(&overlay, repo_root)?;

    // 5b. Update semantic graph for changed Rust files.
    let snapshot_path = vai_dir.join("graph").join("snapshot.db");
    let snapshot = GraphSnapshot::open(&snapshot_path)?;
    for fd in &workspace_diff.file_diffs {
        if fd.path.ends_with(".rs") {
            let abs_path = repo_root.join(&fd.path);
            if let Ok(content) = fs::read(&abs_path) {
                let _ = snapshot.update_file(&fd.path, &content);
            }
        }
    }

    // 6. Use the pre-computed version ID.
    let new_version_id = new_version_id_preview;

    // 7a. Record MergeCompleted.
    let merge_event = log.append(EventKind::MergeCompleted {
        workspace_id: ws_meta.id,
        new_version_id: new_version_id.clone(),
        auto_resolved_conflicts: 0,
    })?;

    // 7b. Record VersionCreated.
    log.append(EventKind::VersionCreated {
        version_id: new_version_id.clone(),
        parent_version_id: Some(ws_meta.base_version.clone()),
        intent: ws_meta.intent.clone(),
    })?;

    // 8. Write version metadata.
    let version_meta = version::create_version(
        vai_dir,
        &new_version_id,
        Some(&ws_meta.base_version),
        &ws_meta.intent,
        "agent",
        Some(merge_event.id),
    )?;

    // 9a. Advance HEAD.
    fs::write(vai_dir.join("head"), format!("{new_version_id}\n"))?;

    // 9b. Mark workspace as Merged.
    let mut updated_meta = ws_meta.clone();
    updated_meta.status = WorkspaceStatus::Merged;
    updated_meta.updated_at = Utc::now();
    workspace::update_meta(vai_dir, &updated_meta)?;

    // 9c. Clear the active workspace pointer.
    let active_file = vai_dir.join("workspaces").join("active");
    let _ = fs::remove_file(active_file);

    let entities_changed = workspace_diff.entity_changes.len();

    Ok(SubmitResult {
        version: version_meta,
        files_applied,
        entities_changed,
    })
}

// ── Private helpers ───────────────────────────────────────────────────────────

/// Saves the pre-change content of files that are about to be modified into
/// `.vai/versions/<new_version_id>/snapshot/`.
///
/// This snapshot enables rollback to restore files to their state before this
/// version was applied.
fn save_pre_change_snapshot(
    vai_dir: &Path,
    new_version_id: &str,
    workspace_diff: &diff::WorkspaceDiff,
    repo_root: &Path,
) -> Result<(), MergeError> {
    let snapshot_dir = vai_dir
        .join("versions")
        .join(new_version_id)
        .join("snapshot");

    for fd in &workspace_diff.file_diffs {
        let src = repo_root.join(&fd.path);
        if src.exists() {
            // Only snapshot files that already exist (modified or to-be-removed).
            // Added files have no pre-change content to snapshot.
            let dest = snapshot_dir.join(&fd.path);
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&src, &dest)?;
        }
    }

    Ok(())
}

/// Copies all files from the overlay directory into the project root.
///
/// Creates parent directories as needed. Returns the number of files copied.
fn apply_overlay(overlay: &Path, repo_root: &Path) -> Result<usize, MergeError> {
    if !overlay.exists() {
        return Ok(0);
    }
    let files = collect_files(overlay)?;
    let count = files.len();
    for abs_path in files {
        let rel = abs_path
            .strip_prefix(overlay)
            .expect("path inside overlay")
            .to_path_buf();
        let dest = repo_root.join(&rel);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(&abs_path, &dest)?;
    }
    Ok(count)
}

fn collect_files(dir: &Path) -> Result<Vec<PathBuf>, MergeError> {
    let mut files = Vec::new();
    collect_recursive(dir, &mut files)?;
    Ok(files)
}

fn collect_recursive(dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), MergeError> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_recursive(&path, files)?;
        } else {
            files.push(path);
        }
    }
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Sets up a minimal vai repository with a single Rust source file.
    fn setup_repo(source_files: &[(&str, &str)]) -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_path_buf();

        for (rel, content) in source_files {
            let abs = root.join(rel);
            if let Some(p) = abs.parent() {
                fs::create_dir_all(p).unwrap();
            }
            fs::write(&abs, content).unwrap();
        }

        crate::repo::init(&root).unwrap();
        (dir, root)
    }

    /// Writes files into the active workspace's overlay.
    fn write_overlay(vai_dir: &Path, ws_id: &uuid::Uuid, files: &[(&str, &str)]) {
        let overlay = vai_dir
            .join("workspaces")
            .join(ws_id.to_string())
            .join("overlay");
        for (rel, content) in files {
            let abs = overlay.join(rel);
            if let Some(p) = abs.parent() {
                fs::create_dir_all(p).unwrap();
            }
            fs::write(&abs, content).unwrap();
        }
    }

    const BASE_RS: &str = "fn hello() -> &'static str { \"hello\" }\n";
    const MODIFIED_RS: &str =
        "fn hello() -> &'static str { \"hello, world!\" }\nfn greet() {}\n";

    #[test]
    fn test_fast_forward_merge_creates_new_version() {
        let (_dir, root) = setup_repo(&[("src/lib.rs", BASE_RS)]);
        let vai_dir = root.join(".vai");

        let result = workspace::create(&vai_dir, "add greeting", "v1").unwrap();
        write_overlay(&vai_dir, &result.workspace.id, &[("src/lib.rs", MODIFIED_RS)]);

        let submit_result = submit(&vai_dir, &root).unwrap();

        assert_eq!(submit_result.version.version_id, "v2");
        assert_eq!(submit_result.version.intent, "add greeting");
        assert_eq!(submit_result.files_applied, 1);
    }

    #[test]
    fn test_fast_forward_advances_head() {
        let (_dir, root) = setup_repo(&[("src/lib.rs", BASE_RS)]);
        let vai_dir = root.join(".vai");

        let result = workspace::create(&vai_dir, "test", "v1").unwrap();
        write_overlay(&vai_dir, &result.workspace.id, &[("src/lib.rs", MODIFIED_RS)]);

        submit(&vai_dir, &root).unwrap();

        let head = repo::read_head(&vai_dir).unwrap();
        assert_eq!(head, "v2");
    }

    #[test]
    fn test_fast_forward_applies_overlay_to_root() {
        let (_dir, root) = setup_repo(&[("src/lib.rs", BASE_RS)]);
        let vai_dir = root.join(".vai");

        let result = workspace::create(&vai_dir, "test", "v1").unwrap();
        write_overlay(
            &vai_dir,
            &result.workspace.id,
            &[("src/lib.rs", MODIFIED_RS)],
        );

        submit(&vai_dir, &root).unwrap();

        let content = fs::read_to_string(root.join("src/lib.rs")).unwrap();
        assert_eq!(content, MODIFIED_RS);
    }

    #[test]
    fn test_fast_forward_marks_workspace_merged() {
        let (_dir, root) = setup_repo(&[("src/lib.rs", BASE_RS)]);
        let vai_dir = root.join(".vai");

        let result = workspace::create(&vai_dir, "test", "v1").unwrap();
        write_overlay(&vai_dir, &result.workspace.id, &[("src/lib.rs", MODIFIED_RS)]);

        let ws_id = result.workspace.id.to_string();
        submit(&vai_dir, &root).unwrap();

        let ws = workspace::get(&vai_dir, &ws_id).unwrap();
        assert_eq!(ws.status, WorkspaceStatus::Merged);
    }

    #[test]
    fn test_head_advanced_returns_error() {
        let (_dir, root) = setup_repo(&[("src/lib.rs", BASE_RS)]);
        let vai_dir = root.join(".vai");

        // Create workspace based on v1.
        let result = workspace::create(&vai_dir, "test", "v1").unwrap();
        write_overlay(&vai_dir, &result.workspace.id, &[("src/lib.rs", MODIFIED_RS)]);

        // Manually advance HEAD to simulate another merge landing first.
        version::create_version(&vai_dir, "v2", Some("v1"), "other work", "agent", None).unwrap();
        fs::write(vai_dir.join("head"), "v2\n").unwrap();

        let err = submit(&vai_dir, &root).unwrap_err();
        assert!(matches!(err, MergeError::HeadAdvanced { .. }));
    }

    #[test]
    fn test_empty_overlay_fast_forward() {
        let (_dir, root) = setup_repo(&[("src/lib.rs", BASE_RS)]);
        let vai_dir = root.join(".vai");

        workspace::create(&vai_dir, "no-op", "v1").unwrap();
        // No overlay files written — diff will be empty.

        let result = submit(&vai_dir, &root).unwrap();
        assert_eq!(result.files_applied, 0);
        assert_eq!(result.version.version_id, "v2");
    }
}
