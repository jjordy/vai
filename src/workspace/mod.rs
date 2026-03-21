//! Workspace management — isolated environments for agent changes.
//!
//! A workspace is an isolated environment where an agent makes changes against
//! a snapshot of the codebase. Changes are tracked as events and can be
//! submitted for merging or discarded.
//!
//! ## On-Disk Layout
//!
//! ```text
//! .vai/workspaces/
//!     active              # contains the active workspace ID (optional)
//!     <id>/
//!         meta.toml       # workspace metadata
//!         overlay/        # changed files (mirrors project root structure)
//! ```

use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::event_log::{EventKind, EventLog};

/// Errors from workspace operations.
#[derive(Debug, Error)]
pub enum WorkspaceError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("TOML serialization error: {0}")]
    TomlSer(#[from] toml::ser::Error),

    #[error("TOML deserialization error: {0}")]
    TomlDe(#[from] toml::de::Error),

    #[error("Event log error: {0}")]
    EventLog(#[from] crate::event_log::EventLogError),

    #[error("Workspace not found: {0}")]
    NotFound(String),

    #[error("No active workspace set")]
    NoActiveWorkspace,
}

// ── Workspace status ──────────────────────────────────────────────────────────

/// Lifecycle states for a workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkspaceStatus {
    /// Workspace created but no files modified yet.
    Created,
    /// Agent has started making changes.
    Active,
    /// Submitted for merging; awaiting resolution.
    Submitted,
    /// Successfully merged into main version.
    Merged,
    /// Discarded without merging.
    Discarded,
}

impl WorkspaceStatus {
    /// Human-readable display string.
    pub fn as_str(&self) -> &'static str {
        match self {
            WorkspaceStatus::Created => "Created",
            WorkspaceStatus::Active => "Active",
            WorkspaceStatus::Submitted => "Submitted",
            WorkspaceStatus::Merged => "Merged",
            WorkspaceStatus::Discarded => "Discarded",
        }
    }
}

// ── Workspace metadata ────────────────────────────────────────────────────────

/// Metadata for a single workspace, stored in `meta.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceMeta {
    /// Unique workspace identifier (UUID v4).
    pub id: Uuid,
    /// Agent's stated intent for this workspace.
    pub intent: String,
    /// The version ID that was HEAD when this workspace was created.
    pub base_version: String,
    /// Current lifecycle status.
    pub status: WorkspaceStatus,
    /// When this workspace was created.
    pub created_at: DateTime<Utc>,
    /// When this workspace was last updated.
    pub updated_at: DateTime<Utc>,
    /// Optional issue ID this workspace was created to address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_id: Option<Uuid>,
}

// ── Result types ──────────────────────────────────────────────────────────────

/// Result returned by `create`.
#[derive(Debug, Serialize)]
pub struct CreateResult {
    /// Metadata of the newly created workspace.
    pub workspace: WorkspaceMeta,
    /// Path to the workspace directory.
    pub path: PathBuf,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Creates a new workspace with the given intent.
///
/// Generates a unique ID, creates `.vai/workspaces/<id>/` with `meta.toml`
/// and an empty `overlay/` directory, records a `WorkspaceCreated` event,
/// and sets this workspace as active.
pub fn create(
    vai_dir: &Path,
    intent: &str,
    base_version: &str,
) -> Result<CreateResult, WorkspaceError> {
    create_with_id(vai_dir, intent, base_version, Uuid::new_v4())
}

/// Creates a new workspace using an explicitly supplied UUID.
///
/// Used by the remote workflow so the local workspace ID matches the ID
/// assigned by the server when the workspace was registered there.
/// Behaviour is otherwise identical to [`create`].
pub fn create_with_id(
    vai_dir: &Path,
    intent: &str,
    base_version: &str,
    id: Uuid,
) -> Result<CreateResult, WorkspaceError> {
    let now = Utc::now();

    let ws_dir = vai_dir.join("workspaces").join(id.to_string());
    fs::create_dir_all(ws_dir.join("overlay"))?;

    let meta = WorkspaceMeta {
        id,
        intent: intent.to_string(),
        base_version: base_version.to_string(),
        status: WorkspaceStatus::Created,
        created_at: now,
        updated_at: now,
        issue_id: None,
    };

    write_meta(&ws_dir, &meta)?;

    // Record event
    let log_dir = vai_dir.join("event_log");
    let mut log = EventLog::open(&log_dir)?;
    log.append(EventKind::WorkspaceCreated {
        workspace_id: id,
        intent: intent.to_string(),
        base_version: base_version.to_string(),
    })?;

    // Set as active workspace
    set_active(vai_dir, &id.to_string())?;

    Ok(CreateResult {
        workspace: meta,
        path: ws_dir,
    })
}

/// Lists all non-discarded workspaces under `.vai/workspaces/`.
///
/// Workspaces with `Discarded` or `Merged` status are excluded from the
/// default listing; use `list_all` to include them.
pub fn list(vai_dir: &Path) -> Result<Vec<WorkspaceMeta>, WorkspaceError> {
    list_filtered(vai_dir, false)
}

/// Lists all workspaces including discarded and merged ones.
pub fn list_all(vai_dir: &Path) -> Result<Vec<WorkspaceMeta>, WorkspaceError> {
    list_filtered(vai_dir, true)
}

fn list_filtered(vai_dir: &Path, include_inactive: bool) -> Result<Vec<WorkspaceMeta>, WorkspaceError> {
    let workspaces_dir = vai_dir.join("workspaces");
    let mut results = Vec::new();

    let entries = match fs::read_dir(&workspaces_dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(results),
        Err(e) => return Err(WorkspaceError::Io(e)),
    };

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        // Skip the "active" file (it's not a dir, but guard anyway)
        let meta_path = path.join("meta.toml");
        if !meta_path.exists() {
            continue;
        }
        let meta = read_meta(&path)?;
        if !include_inactive
            && (meta.status == WorkspaceStatus::Discarded
                || meta.status == WorkspaceStatus::Merged)
        {
            continue;
        }
        results.push(meta);
    }

    // Sort by creation time, newest first
    results.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(results)
}

/// Returns the metadata for a specific workspace by ID.
pub fn get(vai_dir: &Path, id: &str) -> Result<WorkspaceMeta, WorkspaceError> {
    let ws_dir = vai_dir.join("workspaces").join(id);
    if !ws_dir.exists() {
        return Err(WorkspaceError::NotFound(id.to_string()));
    }
    read_meta(&ws_dir)
}

/// Switches the active workspace context to the given ID.
///
/// Verifies the workspace exists and is not discarded/merged before switching.
pub fn switch(vai_dir: &Path, id: &str) -> Result<WorkspaceMeta, WorkspaceError> {
    let meta = get(vai_dir, id)?;
    if meta.status == WorkspaceStatus::Discarded || meta.status == WorkspaceStatus::Merged {
        return Err(WorkspaceError::NotFound(format!(
            "{id} (workspace is {}, cannot switch to it)",
            meta.status.as_str()
        )));
    }
    set_active(vai_dir, id)?;
    Ok(meta)
}

/// Discards a workspace: removes its directory, records a `WorkspaceDiscarded` event.
///
/// The workspace directory is deleted. The event log retains the full history.
pub fn discard(
    vai_dir: &Path,
    id: &str,
    reason: Option<&str>,
) -> Result<WorkspaceMeta, WorkspaceError> {
    let ws_dir = vai_dir.join("workspaces").join(id);
    if !ws_dir.exists() {
        return Err(WorkspaceError::NotFound(id.to_string()));
    }

    let mut meta = read_meta(&ws_dir)?;
    let uuid = meta.id;

    // Record event before removing files
    let log_dir = vai_dir.join("event_log");
    let mut log = EventLog::open(&log_dir)?;
    log.append(EventKind::WorkspaceDiscarded {
        workspace_id: uuid,
        reason: reason.unwrap_or("discarded by user").to_string(),
    })?;

    // Update status on disk, then remove directory
    meta.status = WorkspaceStatus::Discarded;
    meta.updated_at = Utc::now();
    write_meta(&ws_dir, &meta)?;
    fs::remove_dir_all(&ws_dir)?;

    // Clear active pointer if it was pointing to this workspace
    let active = read_active(vai_dir).ok();
    if active.as_deref() == Some(id) {
        let active_file = vai_dir.join("workspaces").join("active");
        let _ = fs::remove_file(active_file);
    }

    Ok(meta)
}

/// Returns the ID of the currently active workspace, if any.
pub fn active_id(vai_dir: &Path) -> Option<String> {
    read_active(vai_dir).ok()
}

/// Returns the metadata of the currently active workspace, if any.
pub fn active(vai_dir: &Path) -> Result<WorkspaceMeta, WorkspaceError> {
    let id = read_active(vai_dir)?;
    get(vai_dir, &id)
}

/// Updates a workspace's metadata on disk.
///
/// Used by other modules (diff, submit) to update status.
pub fn update_meta(vai_dir: &Path, meta: &WorkspaceMeta) -> Result<(), WorkspaceError> {
    let ws_dir = vai_dir.join("workspaces").join(meta.id.to_string());
    write_meta(&ws_dir, meta)
}

/// Returns the overlay directory for a workspace.
///
/// Files modified within a workspace are stored here, mirroring the project layout.
pub fn overlay_dir(vai_dir: &Path, id: &str) -> PathBuf {
    vai_dir.join("workspaces").join(id).join("overlay")
}

// ── Private helpers ───────────────────────────────────────────────────────────

fn write_meta(ws_dir: &Path, meta: &WorkspaceMeta) -> Result<(), WorkspaceError> {
    let path = ws_dir.join("meta.toml");
    let content = toml::to_string_pretty(meta)?;
    fs::write(path, content)?;
    Ok(())
}

fn read_meta(ws_dir: &Path) -> Result<WorkspaceMeta, WorkspaceError> {
    let path = ws_dir.join("meta.toml");
    let content = fs::read_to_string(&path)?;
    Ok(toml::from_str(&content)?)
}

fn set_active(vai_dir: &Path, id: &str) -> Result<(), WorkspaceError> {
    let active_file = vai_dir.join("workspaces").join("active");
    fs::write(active_file, id)?;
    Ok(())
}

fn read_active(vai_dir: &Path) -> Result<String, WorkspaceError> {
    let active_file = vai_dir.join("workspaces").join("active");
    if !active_file.exists() {
        return Err(WorkspaceError::NoActiveWorkspace);
    }
    let id = fs::read_to_string(active_file)?.trim().to_string();
    if id.is_empty() {
        return Err(WorkspaceError::NoActiveWorkspace);
    }
    Ok(id)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup_vai_dir() -> TempDir {
        let dir = TempDir::new().unwrap();
        let vai_dir = dir.path().join(".vai");
        fs::create_dir_all(vai_dir.join("workspaces")).unwrap();
        fs::create_dir_all(vai_dir.join("event_log")).unwrap();
        dir
    }

    #[test]
    fn test_create_workspace() {
        let dir = setup_vai_dir();
        let vai_dir = dir.path().join(".vai");

        let result = create(&vai_dir, "fix auth bug", "v1").unwrap();

        assert_eq!(result.workspace.intent, "fix auth bug");
        assert_eq!(result.workspace.base_version, "v1");
        assert_eq!(result.workspace.status, WorkspaceStatus::Created);
        assert!(result.path.exists());
        assert!(result.path.join("overlay").exists());
        assert!(result.path.join("meta.toml").exists());
    }

    #[test]
    fn test_active_workspace_set_on_create() {
        let dir = setup_vai_dir();
        let vai_dir = dir.path().join(".vai");

        let result = create(&vai_dir, "test intent", "v1").unwrap();
        let active = active_id(&vai_dir).unwrap();

        assert_eq!(active, result.workspace.id.to_string());
    }

    #[test]
    fn test_list_workspaces() {
        let dir = setup_vai_dir();
        let vai_dir = dir.path().join(".vai");

        create(&vai_dir, "workspace A", "v1").unwrap();
        create(&vai_dir, "workspace B", "v1").unwrap();

        let workspaces = list(&vai_dir).unwrap();
        assert_eq!(workspaces.len(), 2);
    }

    #[test]
    fn test_list_excludes_discarded() {
        let dir = setup_vai_dir();
        let vai_dir = dir.path().join(".vai");

        let r = create(&vai_dir, "workspace to discard", "v1").unwrap();
        create(&vai_dir, "workspace to keep", "v1").unwrap();

        discard(&vai_dir, &r.workspace.id.to_string(), None).unwrap();

        let active_workspaces = list(&vai_dir).unwrap();
        assert_eq!(active_workspaces.len(), 1);
        assert_eq!(active_workspaces[0].intent, "workspace to keep");
    }

    #[test]
    fn test_discard_workspace() {
        let dir = setup_vai_dir();
        let vai_dir = dir.path().join(".vai");

        let r = create(&vai_dir, "workspace to discard", "v1").unwrap();
        let id = r.workspace.id.to_string();

        discard(&vai_dir, &id, Some("no longer needed")).unwrap();

        // Directory is removed
        assert!(!vai_dir.join("workspaces").join(&id).exists());

        // Active pointer is cleared
        assert!(active_id(&vai_dir).is_none());
    }

    #[test]
    fn test_discard_nonexistent_returns_error() {
        let dir = setup_vai_dir();
        let vai_dir = dir.path().join(".vai");

        let result = discard(&vai_dir, "nonexistent-id", None);
        assert!(matches!(result, Err(WorkspaceError::NotFound(_))));
    }

    #[test]
    fn test_switch_workspace() {
        let dir = setup_vai_dir();
        let vai_dir = dir.path().join(".vai");

        let r1 = create(&vai_dir, "first workspace", "v1").unwrap();
        let r2 = create(&vai_dir, "second workspace", "v1").unwrap();

        // After two creates, active is r2
        assert_eq!(active_id(&vai_dir).unwrap(), r2.workspace.id.to_string());

        // Switch back to r1
        switch(&vai_dir, &r1.workspace.id.to_string()).unwrap();
        assert_eq!(active_id(&vai_dir).unwrap(), r1.workspace.id.to_string());
    }

    #[test]
    fn test_workspace_status_transitions() {
        let dir = setup_vai_dir();
        let vai_dir = dir.path().join(".vai");

        let r = create(&vai_dir, "test workspace", "v1").unwrap();
        assert_eq!(r.workspace.status, WorkspaceStatus::Created);

        // Manually update status
        let mut meta = r.workspace.clone();
        meta.status = WorkspaceStatus::Active;
        meta.updated_at = Utc::now();
        update_meta(&vai_dir, &meta).unwrap();

        let fetched = get(&vai_dir, &r.workspace.id.to_string()).unwrap();
        assert_eq!(fetched.status, WorkspaceStatus::Active);
    }
}
