//! Version history management.
//!
//! Versions are labeled states of the codebase after successful merges.
//! The version history is a linear sequence (v1, v2, v3, …) of intent-labeled
//! states. Each version is stored as a TOML file in `.vai/versions/`.
//!
//! ## On-Disk Layout
//!
//! ```text
//! .vai/versions/
//!     v1.toml     # initial repository
//!     v2.toml     # first merge
//!     v3.toml     # second merge
//! ```

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
#[cfg(feature = "server")]
use utoipa::ToSchema;

use crate::event_log::{EventKind, EventLog, EventLogError};

// ── Errors ────────────────────────────────────────────────────────────────────

/// Errors from version operations.
#[derive(Debug, Error)]
pub enum VersionError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("TOML serialization error: {0}")]
    TomlSer(#[from] toml::ser::Error),

    #[error("TOML deserialization error: {0}")]
    TomlDe(#[from] toml::de::Error),

    #[error("Version not found: {0}")]
    NotFound(String),

    #[error("Event log error: {0}")]
    EventLog(#[from] EventLogError),
}

// ── Types ─────────────────────────────────────────────────────────────────────

/// Metadata for a single version, stored in `.vai/versions/v<N>.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
pub struct VersionMeta {
    /// Version identifier, e.g. `"v1"`.
    pub version_id: String,
    /// Parent version ID, if any.
    pub parent_version_id: Option<String>,
    /// Intent description for this version.
    pub intent: String,
    /// Agent or user who created this version.
    pub created_by: String,
    /// When this version was created.
    pub created_at: DateTime<Utc>,
    /// ID of the merge event in the event log that produced this version, if any.
    pub merge_event_id: Option<u64>,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Creates and persists a new version to `.vai/versions/<version_id>.toml`.
pub fn create_version(
    vai_dir: &Path,
    version_id: &str,
    parent_version_id: Option<&str>,
    intent: &str,
    created_by: &str,
    merge_event_id: Option<u64>,
) -> Result<VersionMeta, VersionError> {
    let meta = VersionMeta {
        version_id: version_id.to_string(),
        parent_version_id: parent_version_id.map(|s| s.to_string()),
        intent: intent.to_string(),
        created_by: created_by.to_string(),
        created_at: Utc::now(),
        merge_event_id,
    };
    let path = vai_dir
        .join("versions")
        .join(format!("{version_id}.toml"));
    fs::write(path, toml::to_string_pretty(&meta)?)?;
    Ok(meta)
}

/// Returns all versions sorted chronologically (v1 first).
pub fn list_versions(vai_dir: &Path) -> Result<Vec<VersionMeta>, VersionError> {
    let versions_dir = vai_dir.join("versions");
    let mut versions = Vec::new();

    let entries = match fs::read_dir(&versions_dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(versions),
        Err(e) => return Err(VersionError::Io(e)),
    };

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let content = fs::read_to_string(&path)?;
        let meta: VersionMeta = toml::from_str(&content)?;
        versions.push(meta);
    }

    // Sort numerically: v1 < v2 < v10 (not lexicographic).
    versions.sort_by_key(|v| parse_version_number(&v.version_id));
    Ok(versions)
}

/// Returns metadata for a specific version by ID (e.g., `"v2"`).
pub fn get_version(vai_dir: &Path, version_id: &str) -> Result<VersionMeta, VersionError> {
    let path = vai_dir
        .join("versions")
        .join(format!("{version_id}.toml"));
    if !path.exists() {
        return Err(VersionError::NotFound(version_id.to_string()));
    }
    let content = fs::read_to_string(&path)?;
    Ok(toml::from_str(&content)?)
}

/// Computes the next version identifier based on existing versions.
///
/// Scans `.vai/versions/`, finds the highest-numbered version,
/// and returns `v(N+1)`.
pub fn next_version_id(vai_dir: &Path) -> Result<String, VersionError> {
    let versions = list_versions(vai_dir)?;
    let max = versions
        .iter()
        .map(|v| parse_version_number(&v.version_id))
        .max()
        .unwrap_or(0);
    Ok(format!("v{}", max + 1))
}

// ── Change summary types ───────────────────────────────────────────────────────

/// How an entity was changed within a version.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum VersionChangeType {
    Added,
    Modified,
    Removed,
}

impl VersionChangeType {
    /// Returns the sigil used in human-readable output (`+`, `~`, `-`).
    pub fn sigil(&self) -> &'static str {
        match self {
            VersionChangeType::Added => "+",
            VersionChangeType::Modified => "~",
            VersionChangeType::Removed => "-",
        }
    }

    /// Returns the label used in human-readable output.
    pub fn label(&self) -> &'static str {
        match self {
            VersionChangeType::Added => "added",
            VersionChangeType::Modified => "modified",
            VersionChangeType::Removed => "removed",
        }
    }
}

/// Summary of an entity change in a version, derived from event log records.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
pub struct VersionEntityChange {
    /// The stable entity ID (SHA-256).
    pub entity_id: String,
    /// How the entity changed.
    pub change_type: VersionChangeType,
    /// Entity kind string (e.g., `"function"`), if known.
    pub kind: Option<String>,
    /// Fully-qualified name (e.g., `AuthService::validate_token`), if known.
    pub qualified_name: Option<String>,
    /// File path relative to repo root, if known.
    pub file_path: Option<String>,
    /// Human-readable description from `EntityModified` events, if present.
    pub change_description: Option<String>,
}

/// How a file was changed within a version.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum VersionFileChangeType {
    Added,
    Modified,
    Removed,
}

/// Summary of a file change in a version.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
pub struct VersionFileChange {
    /// File path relative to the repository root.
    pub path: String,
    /// How the file changed.
    pub change_type: VersionFileChangeType,
    /// Content hash of the new version, if available.
    pub hash: Option<String>,
}

/// All changes introduced in a single version.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
pub struct VersionChanges {
    /// Version metadata.
    pub version: VersionMeta,
    /// Entity-level changes.
    pub entity_changes: Vec<VersionEntityChange>,
    /// File-level changes.
    pub file_changes: Vec<VersionFileChange>,
}

// ── Rollback types ─────────────────────────────────────────────────────────────

/// Risk level for a downstream version that depends on rolled-back changes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum RiskLevel {
    /// Version references rolled-back entities but doesn't directly depend on changed logic.
    Low,
    /// Version modifies the same entities that are being rolled back.
    Medium,
    /// Version has strong semantic dependencies on rolled-back changes.
    High,
}

impl RiskLevel {
    /// Returns the display symbol for this risk level.
    pub fn symbol(&self) -> &'static str {
        match self {
            RiskLevel::Low => "ℹ",
            RiskLevel::Medium => "⚠",
            RiskLevel::High => "✖",
        }
    }
}

/// A downstream version that may be affected by rolling back a target version.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
pub struct ImpactItem {
    /// Version ID of the downstream version (e.g., `"v3"`).
    pub version_id: String,
    /// Intent of the downstream version.
    pub intent: String,
    /// Entity qualified names that overlap with the rolled-back changes.
    pub overlapping_entities: Vec<String>,
    /// File paths that overlap with the rolled-back changes.
    pub overlapping_files: Vec<String>,
    /// Assessed risk level.
    pub risk: RiskLevel,
}

/// Full impact analysis for a prospective rollback.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
pub struct ImpactAnalysis {
    /// The version being rolled back.
    pub target_version: VersionMeta,
    /// Changes introduced by the target version.
    pub target_changes: VersionChanges,
    /// Downstream versions that depend on the target version's changes.
    pub downstream_impacts: Vec<ImpactItem>,
}

/// Result of a successful rollback operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
pub struct RollbackResult {
    /// The new version created by this rollback.
    pub new_version: VersionMeta,
    /// Number of files restored to their pre-target-version state.
    pub files_restored: usize,
    /// Number of files deleted (those that were added in the target version).
    pub files_deleted: usize,
}

// ── Extended public API ────────────────────────────────────────────────────────

/// Loads all changes introduced by a version by replaying the event log.
///
/// For the initial version (no `merge_event_id`), returns empty change lists.
pub fn get_version_changes(
    vai_dir: &Path,
    version_id: &str,
) -> Result<VersionChanges, VersionError> {
    let version = get_version(vai_dir, version_id)?;

    let Some(merge_event_id) = version.merge_event_id else {
        return Ok(VersionChanges {
            version,
            entity_changes: vec![],
            file_changes: vec![],
        });
    };

    let log = EventLog::open(&vai_dir.join("event_log"))?;

    // Look up the MergeCompleted event to find the workspace that produced this version.
    let merge_event = log.get_by_id(merge_event_id)?;
    let workspace_id = match merge_event.and_then(|e| e.kind.workspace_id()) {
        Some(id) => id,
        None => {
            return Ok(VersionChanges {
                version,
                entity_changes: vec![],
                file_changes: vec![],
            });
        }
    };

    let events = log.query_by_workspace(workspace_id)?;

    let mut entity_changes = Vec::new();
    let mut file_changes = Vec::new();

    for event in events {
        match event.kind {
            EventKind::EntityAdded { entity, .. } => {
                entity_changes.push(VersionEntityChange {
                    entity_id: entity.id,
                    change_type: VersionChangeType::Added,
                    kind: Some(entity.kind),
                    qualified_name: Some(entity.qualified_name),
                    file_path: Some(entity.file_path),
                    change_description: None,
                });
            }
            EventKind::EntityModified {
                entity_id,
                change_description,
                ..
            } => {
                entity_changes.push(VersionEntityChange {
                    entity_id,
                    change_type: VersionChangeType::Modified,
                    kind: None,
                    qualified_name: None,
                    file_path: None,
                    change_description: Some(change_description),
                });
            }
            EventKind::EntityRemoved { entity_id, .. } => {
                entity_changes.push(VersionEntityChange {
                    entity_id,
                    change_type: VersionChangeType::Removed,
                    kind: None,
                    qualified_name: None,
                    file_path: None,
                    change_description: None,
                });
            }
            EventKind::FileAdded { path, hash, .. } => {
                file_changes.push(VersionFileChange {
                    path,
                    change_type: VersionFileChangeType::Added,
                    hash: Some(hash),
                });
            }
            EventKind::FileModified { path, new_hash, .. } => {
                file_changes.push(VersionFileChange {
                    path,
                    change_type: VersionFileChangeType::Modified,
                    hash: Some(new_hash),
                });
            }
            EventKind::FileRemoved { path, .. } => {
                file_changes.push(VersionFileChange {
                    path,
                    change_type: VersionFileChangeType::Removed,
                    hash: None,
                });
            }
            _ => {}
        }
    }

    Ok(VersionChanges {
        version,
        entity_changes,
        file_changes,
    })
}

/// Returns entity and file changes for all versions strictly after `version_a`
/// up to and including `version_b`.
///
/// Versions are compared numerically (v1 < v2 < v10). If `version_a` ≥
/// `version_b` an empty vec is returned.
pub fn get_versions_diff(
    vai_dir: &Path,
    version_a: &str,
    version_b: &str,
) -> Result<Vec<VersionChanges>, VersionError> {
    let n_a = parse_version_number(version_a);
    let n_b = parse_version_number(version_b);

    if n_a >= n_b {
        return Ok(vec![]);
    }

    let all_versions = list_versions(vai_dir)?;
    let mut result = Vec::new();

    for version in all_versions {
        let n = parse_version_number(&version.version_id);
        if n > n_a && n <= n_b {
            result.push(get_version_changes(vai_dir, &version.version_id)?);
        }
    }

    Ok(result)
}

/// Analyzes the impact of rolling back a given version.
///
/// Scans all versions that came after `version_id` and identifies those that
/// modified or referenced the same entities or files. Returns an `ImpactAnalysis`
/// with risk-rated downstream items.
pub fn analyze_rollback_impact(
    vai_dir: &Path,
    version_id: &str,
) -> Result<ImpactAnalysis, VersionError> {
    let target_changes = get_version_changes(vai_dir, version_id)?;
    let target_num = parse_version_number(version_id);

    let target_entity_ids: HashSet<String> = target_changes
        .entity_changes
        .iter()
        .map(|ec| ec.entity_id.clone())
        .collect();

    let target_file_paths: HashSet<String> = target_changes
        .file_changes
        .iter()
        .map(|fc| fc.path.clone())
        .collect();

    let all_versions = list_versions(vai_dir)?;
    let mut downstream_impacts = Vec::new();

    for version in &all_versions {
        let n = parse_version_number(&version.version_id);
        if n <= target_num {
            continue;
        }

        let changes = get_version_changes(vai_dir, &version.version_id)?;

        let overlapping_entities: Vec<String> = changes
            .entity_changes
            .iter()
            .filter(|ec| target_entity_ids.contains(&ec.entity_id))
            .map(|ec| {
                ec.qualified_name
                    .clone()
                    .unwrap_or_else(|| ec.entity_id.clone())
            })
            .collect();

        let overlapping_files: Vec<String> = changes
            .file_changes
            .iter()
            .filter(|fc| target_file_paths.contains(&fc.path))
            .map(|fc| fc.path.clone())
            .collect();

        if overlapping_entities.is_empty() && overlapping_files.is_empty() {
            continue;
        }

        let risk = if !overlapping_entities.is_empty() {
            RiskLevel::High
        } else {
            RiskLevel::Low
        };

        downstream_impacts.push(ImpactItem {
            version_id: version.version_id.clone(),
            intent: version.intent.clone(),
            overlapping_entities,
            overlapping_files,
            risk,
        });
    }

    Ok(ImpactAnalysis {
        target_version: target_changes.version.clone(),
        target_changes,
        downstream_impacts,
    })
}

/// Rolls back the changes introduced by `version_id` by creating a new version
/// that restores the prior state.
///
/// The rollback is append-only: a new version is created rather than rewriting
/// history. Files are restored from the pre-change snapshot stored at
/// `.vai/versions/<version_id>/snapshot/`.
///
/// If `entity_filter` is provided, only files associated with that entity name
/// (matched by `qualified_name`) are restored.
///
/// Returns `VersionError::NotFound` if the target version does not exist.
pub fn rollback(
    vai_dir: &Path,
    repo_root: &Path,
    version_id: &str,
    entity_filter: Option<&str>,
) -> Result<RollbackResult, VersionError> {
    let target_changes = get_version_changes(vai_dir, version_id)?;

    // Determine which files to restore.  When an entity filter is given, only
    // restore files that contain the matching entity changes.
    let filtered_files: Option<HashSet<String>> = entity_filter.map(|name| {
        target_changes
            .entity_changes
            .iter()
            .filter(|ec| {
                ec.qualified_name.as_deref() == Some(name)
                    || ec.entity_id.contains(name)
            })
            .filter_map(|ec| ec.file_path.clone())
            .collect()
    });

    let snapshot_dir = vai_dir
        .join("versions")
        .join(version_id)
        .join("snapshot");

    let mut files_restored: usize = 0;
    let mut files_deleted: usize = 0;

    for file_change in &target_changes.file_changes {
        // Apply entity filter if set.
        if let Some(ref allowed) = filtered_files {
            if !allowed.contains(&file_change.path) {
                continue;
            }
        }

        match file_change.change_type {
            VersionFileChangeType::Added => {
                // File was added in the target version → delete it to roll back.
                let dest = repo_root.join(&file_change.path);
                if dest.exists() {
                    std::fs::remove_file(&dest)?;
                    files_deleted += 1;
                }
            }
            VersionFileChangeType::Modified | VersionFileChangeType::Removed => {
                // File was modified or removed → restore from pre-change snapshot.
                let src = snapshot_dir.join(&file_change.path);
                if src.exists() {
                    let dest = repo_root.join(&file_change.path);
                    if let Some(parent) = dest.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::copy(&src, &dest)?;
                    files_restored += 1;
                }
            }
        }
    }

    // Record rollback events and create the new version.
    let log_dir = vai_dir.join("event_log");
    let mut log = EventLog::open(&log_dir)?;

    let head = std::fs::read_to_string(vai_dir.join("head"))
        .map(|s| s.trim().to_string())?;

    let new_version_id = next_version_id(vai_dir)?;

    log.append(EventKind::RollbackCreated {
        target_version_id: version_id.to_string(),
        new_version_id: new_version_id.clone(),
        entity_filter: entity_filter.map(|s| s.to_string()),
    })?;

    log.append(EventKind::VersionCreated {
        version_id: new_version_id.clone(),
        parent_version_id: Some(head.clone()),
        intent: format!("rollback {version_id}"),
    })?;

    let version_meta = create_version(
        vai_dir,
        &new_version_id,
        Some(&head),
        &format!("rollback {version_id}"),
        "agent",
        None,
    )?;

    // Advance HEAD.
    std::fs::write(vai_dir.join("head"), format!("{new_version_id}\n"))?;

    Ok(RollbackResult {
        new_version: version_meta,
        files_restored,
        files_deleted,
    })
}

// ── Private helpers ───────────────────────────────────────────────────────────

/// Parses the numeric part of a version ID like `"v3"` → `3`.
fn parse_version_number(version_id: &str) -> u64 {
    version_id
        .strip_prefix('v')
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup_versions_dir() -> TempDir {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join(".vai").join("versions")).unwrap();
        dir
    }

    #[test]
    fn test_create_and_get_version() {
        let dir = setup_versions_dir();
        let vai_dir = dir.path().join(".vai");

        create_version(&vai_dir, "v1", None, "initial repository", "system", None).unwrap();

        let meta = get_version(&vai_dir, "v1").unwrap();
        assert_eq!(meta.version_id, "v1");
        assert_eq!(meta.intent, "initial repository");
        assert!(meta.parent_version_id.is_none());
    }

    #[test]
    fn test_list_versions_sorted() {
        let dir = setup_versions_dir();
        let vai_dir = dir.path().join(".vai");

        create_version(&vai_dir, "v1", None, "initial", "system", None).unwrap();
        create_version(&vai_dir, "v3", Some("v2"), "third", "agent", None).unwrap();
        create_version(&vai_dir, "v2", Some("v1"), "second", "agent", None).unwrap();

        let versions = list_versions(&vai_dir).unwrap();
        assert_eq!(versions.len(), 3);
        assert_eq!(versions[0].version_id, "v1");
        assert_eq!(versions[1].version_id, "v2");
        assert_eq!(versions[2].version_id, "v3");
    }

    #[test]
    fn test_next_version_id() {
        let dir = setup_versions_dir();
        let vai_dir = dir.path().join(".vai");

        // Empty directory → next is v1.
        assert_eq!(next_version_id(&vai_dir).unwrap(), "v1");

        create_version(&vai_dir, "v1", None, "initial", "system", None).unwrap();
        assert_eq!(next_version_id(&vai_dir).unwrap(), "v2");

        create_version(&vai_dir, "v2", Some("v1"), "second", "agent", None).unwrap();
        assert_eq!(next_version_id(&vai_dir).unwrap(), "v3");
    }

    #[test]
    fn test_get_version_not_found() {
        let dir = setup_versions_dir();
        let vai_dir = dir.path().join(".vai");
        let result = get_version(&vai_dir, "v99");
        assert!(matches!(result, Err(VersionError::NotFound(_))));
    }

    #[test]
    fn test_parse_version_number() {
        assert_eq!(parse_version_number("v1"), 1);
        assert_eq!(parse_version_number("v10"), 10);
        assert_eq!(parse_version_number("v100"), 100);
        assert_eq!(parse_version_number("unknown"), 0);
    }

    #[test]
    fn test_get_version_changes_initial_version_empty() {
        let dir = setup_versions_dir();
        let vai_dir = dir.path().join(".vai");
        // Create initial version (no merge_event_id → no changes)
        create_version(&vai_dir, "v1", None, "initial repository", "system", None).unwrap();
        let changes = get_version_changes(&vai_dir, "v1").unwrap();
        assert!(changes.entity_changes.is_empty());
        assert!(changes.file_changes.is_empty());
    }

    #[test]
    fn test_get_versions_diff_empty_when_same() {
        let dir = setup_versions_dir();
        let vai_dir = dir.path().join(".vai");
        create_version(&vai_dir, "v1", None, "initial", "system", None).unwrap();
        create_version(&vai_dir, "v2", Some("v1"), "second", "agent", None).unwrap();
        let diff = get_versions_diff(&vai_dir, "v2", "v2").unwrap();
        assert!(diff.is_empty());
    }

    #[test]
    fn test_get_versions_diff_returns_versions_in_range() {
        let dir = setup_versions_dir();
        let vai_dir = dir.path().join(".vai");
        create_version(&vai_dir, "v1", None, "initial", "system", None).unwrap();
        create_version(&vai_dir, "v2", Some("v1"), "second", "agent", None).unwrap();
        create_version(&vai_dir, "v3", Some("v2"), "third", "agent", None).unwrap();
        // v1 → v3 should include changes from v2 and v3
        let diff = get_versions_diff(&vai_dir, "v1", "v3").unwrap();
        assert_eq!(diff.len(), 2);
        assert_eq!(diff[0].version.version_id, "v2");
        assert_eq!(diff[1].version.version_id, "v3");
    }

    // ── Rollback tests ──────────────────────────────────────────────────────────

    /// Sets up a full vai repository with two submitted versions and returns
    /// `(TempDir, repo_root)`.  Version v2 adds `src/lib.rs`; version v3
    /// adds `src/other.rs`.  Both use the merge machinery so the event log and
    /// snapshots are properly populated.
    fn setup_two_version_repo() -> (tempfile::TempDir, std::path::PathBuf) {
        use crate::merge;
        use crate::workspace;

        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().to_path_buf();

        crate::repo::init(&root).unwrap();
        let vai_dir = root.join(".vai");

        // Create v2: add src/lib.rs
        let ws = workspace::create(&vai_dir, "add lib", "v1").unwrap();
        let overlay = vai_dir
            .join("workspaces")
            .join(ws.workspace.id.to_string())
            .join("overlay");
        fs::create_dir_all(overlay.join("src")).unwrap();
        fs::write(overlay.join("src/lib.rs"), b"pub fn hello() {}\n").unwrap();
        merge::submit(&vai_dir, &root).unwrap();

        // Create v3: add src/other.rs
        let ws2 = workspace::create(&vai_dir, "add other", "v2").unwrap();
        let overlay2 = vai_dir
            .join("workspaces")
            .join(ws2.workspace.id.to_string())
            .join("overlay");
        fs::create_dir_all(overlay2.join("src")).unwrap();
        fs::write(overlay2.join("src/other.rs"), b"pub fn other() {}\n").unwrap();
        merge::submit(&vai_dir, &root).unwrap();

        (dir, root)
    }

    #[test]
    fn test_analyze_rollback_impact_no_downstream() {
        // Rolling back v3 (the latest version) should have no downstream impacts.
        let (_dir, root) = setup_two_version_repo();
        let vai_dir = root.join(".vai");

        let impact = analyze_rollback_impact(&vai_dir, "v3").unwrap();
        assert_eq!(impact.target_version.version_id, "v3");
        assert!(
            impact.downstream_impacts.is_empty(),
            "v3 is HEAD — no downstream versions"
        );
    }

    #[test]
    fn test_analyze_rollback_impact_detects_overlapping_file() {
        // Rolling back v2 when v3 modifies the same file should flag v3.
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().to_path_buf();

        use crate::merge;
        use crate::workspace;

        crate::repo::init(&root).unwrap();
        let vai_dir = root.join(".vai");

        // v2: add src/lib.rs
        let ws = workspace::create(&vai_dir, "add lib", "v1").unwrap();
        let overlay = vai_dir
            .join("workspaces")
            .join(ws.workspace.id.to_string())
            .join("overlay");
        fs::create_dir_all(overlay.join("src")).unwrap();
        fs::write(overlay.join("src/lib.rs"), b"pub fn hello() {}\n").unwrap();
        merge::submit(&vai_dir, &root).unwrap();

        // v3: modify the same src/lib.rs
        let ws2 = workspace::create(&vai_dir, "modify lib", "v2").unwrap();
        let overlay2 = vai_dir
            .join("workspaces")
            .join(ws2.workspace.id.to_string())
            .join("overlay");
        fs::create_dir_all(overlay2.join("src")).unwrap();
        fs::write(
            overlay2.join("src/lib.rs"),
            b"pub fn hello() {}\npub fn world() {}\n",
        )
        .unwrap();
        merge::submit(&vai_dir, &root).unwrap();

        // Impact analysis of v2: v3 touches the same file → should be flagged.
        let impact = analyze_rollback_impact(&vai_dir, "v2").unwrap();
        assert_eq!(impact.target_version.version_id, "v2");
        assert!(
            !impact.downstream_impacts.is_empty(),
            "v3 should be flagged as downstream of v2"
        );
        assert_eq!(impact.downstream_impacts[0].version_id, "v3");
        assert!(
            impact.downstream_impacts[0]
                .overlapping_files
                .contains(&"src/lib.rs".to_string()),
            "src/lib.rs should be listed as an overlapping file"
        );
    }

    #[test]
    fn test_rollback_deletes_added_file() {
        // Rolling back v2 (which added src/lib.rs) should delete src/lib.rs.
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().to_path_buf();

        use crate::merge;
        use crate::workspace;

        crate::repo::init(&root).unwrap();
        let vai_dir = root.join(".vai");

        // v2: add src/lib.rs
        let ws = workspace::create(&vai_dir, "add lib", "v1").unwrap();
        let overlay = vai_dir
            .join("workspaces")
            .join(ws.workspace.id.to_string())
            .join("overlay");
        fs::create_dir_all(overlay.join("src")).unwrap();
        fs::write(overlay.join("src/lib.rs"), b"pub fn hello() {}\n").unwrap();
        merge::submit(&vai_dir, &root).unwrap();

        assert!(root.join("src/lib.rs").exists(), "file should exist before rollback");

        // Rollback v2 → creates v3 that deletes src/lib.rs.
        let result = rollback(&vai_dir, &root, "v2", None).unwrap();

        assert_eq!(result.new_version.version_id, "v3");
        assert_eq!(result.files_deleted, 1);
        assert_eq!(result.files_restored, 0);
        assert!(
            !root.join("src/lib.rs").exists(),
            "file should be deleted after rollback"
        );

        // HEAD should now point to v3.
        let head = fs::read_to_string(vai_dir.join("head")).unwrap();
        assert_eq!(head.trim(), "v3");
    }

    #[test]
    fn test_rollback_restores_modified_file() {
        // Rolling back v2 (which modified src/lib.rs) should restore the original content.
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().to_path_buf();

        use crate::merge;
        use crate::workspace;

        // Pre-populate src/lib.rs before init so v1 has the original content.
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), b"pub fn original() {}\n").unwrap();

        crate::repo::init(&root).unwrap();
        let vai_dir = root.join(".vai");

        // v2: modify src/lib.rs
        let ws = workspace::create(&vai_dir, "modify lib", "v1").unwrap();
        let overlay = vai_dir
            .join("workspaces")
            .join(ws.workspace.id.to_string())
            .join("overlay");
        fs::create_dir_all(overlay.join("src")).unwrap();
        fs::write(overlay.join("src/lib.rs"), b"pub fn modified() {}\n").unwrap();
        merge::submit(&vai_dir, &root).unwrap();

        let content_after_v2 = fs::read_to_string(root.join("src/lib.rs")).unwrap();
        assert_eq!(content_after_v2, "pub fn modified() {}\n");

        // Rollback v2 → restores src/lib.rs to pre-v2 (original) content.
        let result = rollback(&vai_dir, &root, "v2", None).unwrap();

        assert_eq!(result.files_restored, 1);
        let content_after_rollback = fs::read_to_string(root.join("src/lib.rs")).unwrap();
        assert_eq!(
            content_after_rollback, "pub fn original() {}\n",
            "file should be restored to its pre-v2 content"
        );
    }

    #[test]
    fn test_rollback_creates_append_only_version() {
        // Rollback must create a new version (append-only), never rewrite history.
        let (_dir, root) = setup_two_version_repo();
        let vai_dir = root.join(".vai");

        let versions_before = list_versions(&vai_dir).unwrap();
        assert_eq!(versions_before.len(), 3, "v1, v2, v3 before rollback");

        rollback(&vai_dir, &root, "v2", None).unwrap();

        let versions_after = list_versions(&vai_dir).unwrap();
        assert_eq!(versions_after.len(), 4, "v1, v2, v3, v4 after rollback");
        assert_eq!(versions_after[3].version_id, "v4");
        assert_eq!(versions_after[3].intent, "rollback v2");
        // Original v2 must still exist.
        assert!(get_version(&vai_dir, "v2").is_ok(), "v2 must not be rewritten");
    }

    #[test]
    fn test_analyze_rollback_impact_version_not_found() {
        let dir = setup_versions_dir();
        let vai_dir = dir.path().join(".vai");
        // No event log present — version doesn't exist.
        let result = analyze_rollback_impact(&vai_dir, "v99");
        assert!(result.is_err());
    }
}
