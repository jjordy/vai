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

use std::fs;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

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
#[serde(rename_all = "lowercase")]
pub enum VersionFileChangeType {
    Added,
    Modified,
    Removed,
}

/// Summary of a file change in a version.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
pub struct VersionChanges {
    /// Version metadata.
    pub version: VersionMeta,
    /// Entity-level changes.
    pub entity_changes: Vec<VersionEntityChange>,
    /// File-level changes.
    pub file_changes: Vec<VersionFileChange>,
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
}
