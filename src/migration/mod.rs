//! Local-to-remote migration — payload types and local data gathering.
//!
//! Used by `vai remote migrate` to read all local SQLite/filesystem data and
//! serialize it for upload to the remote server's bulk migration endpoint
//! (`POST /api/migrate`).
//!
//! # Flow
//!
//! 1. CLI calls [`gather_local_data`] to collect all events, issues, versions,
//!    and escalations from the local `.vai/` directory.
//! 2. The resulting [`MigrationPayload`] is POSTed to the server.
//! 3. The server inserts everything in a single Postgres transaction and returns
//!    a [`MigrationSummary`].
//! 4. On success, the CLI writes `.vai/migrated_at` as a marker and switches
//!    all subsequent commands to proxy to the remote.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
#[cfg(feature = "server")]
use utoipa::ToSchema;

use crate::escalation::{Escalation, EscalationStore};
use crate::event_log::{Event, EventLog};
use crate::issue::{Issue, IssueFilter, IssueStore};
use crate::version::{list_versions, VersionMeta};

// ── Error type ────────────────────────────────────────────────────────────────

/// Errors that can occur during the local data gathering phase.
#[derive(Debug, Error)]
pub enum MigrationError {
    /// Event log could not be read.
    #[error("event log error: {0}")]
    EventLog(#[from] crate::event_log::EventLogError),

    /// Issue store could not be opened or queried.
    #[error("issue store error: {0}")]
    Issue(#[from] crate::issue::IssueError),

    /// Escalation store could not be opened or queried.
    #[error("escalation store error: {0}")]
    Escalation(#[from] crate::escalation::EscalationError),

    /// Version files could not be read.
    #[error("version error: {0}")]
    Version(#[from] crate::version::VersionError),

    /// Generic I/O failure.
    #[error("I/O error: {0}")]
    Io(String),
}

// ── Payload ───────────────────────────────────────────────────────────────────

/// Full local dataset to migrate to the remote server.
///
/// Serialized as JSON and POSTed to `POST /api/migrate` (single-repo mode) or
/// `POST /api/repos/:repo/migrate` (multi-repo mode).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationPayload {
    /// All events in chronological order.
    pub events: Vec<Event>,
    /// All issues regardless of status.
    pub issues: Vec<Issue>,
    /// All versions in chronological order.
    pub versions: Vec<VersionMeta>,
    /// All escalations.
    pub escalations: Vec<Escalation>,
    /// Current HEAD version ID, if any.
    pub head_version: Option<String>,
}

// ── Summary ───────────────────────────────────────────────────────────────────

/// Server response from a successful migration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
pub struct MigrationSummary {
    /// Number of events inserted into Postgres.
    pub events_migrated: usize,
    /// Number of issues inserted into Postgres.
    pub issues_migrated: usize,
    /// Number of versions inserted into Postgres.
    pub versions_migrated: usize,
    /// Number of escalations inserted into Postgres.
    pub escalations_migrated: usize,
    /// HEAD version after migration.
    pub head_version: Option<String>,
    /// Server-side timestamp when migration completed.
    pub migrated_at: DateTime<Utc>,
}

// ── Local data gathering ──────────────────────────────────────────────────────

/// Reads all local data from `.vai/` and returns it as a [`MigrationPayload`].
///
/// Handles missing directories/databases gracefully — if a component has never
/// been initialised, the corresponding slice in the payload will be empty.
pub fn gather_local_data(vai_dir: &Path) -> Result<MigrationPayload, MigrationError> {
    // Events — stored in NDJSON segment files, indexed in SQLite.
    let events = {
        let event_log_dir = vai_dir.join("event_log");
        if event_log_dir.exists() {
            let log = EventLog::open(&event_log_dir)?;
            log.all()?
        } else {
            Vec::new()
        }
    };

    // Issues.
    let issues = {
        let db_path = vai_dir.join("issues.db");
        if db_path.exists() {
            let store = IssueStore::open(vai_dir)?;
            store.list(&IssueFilter::default())?
        } else {
            Vec::new()
        }
    };

    // Versions — TOML files under `.vai/versions/`.
    let versions = list_versions(vai_dir)?;

    // Escalations.
    let escalations = {
        let db_path = vai_dir.join("escalations.db");
        if db_path.exists() {
            let store = EscalationStore::open(vai_dir)?;
            store.list(None)?
        } else {
            Vec::new()
        }
    };

    // HEAD pointer.
    let head_version = crate::repo::read_head(vai_dir).ok();

    Ok(MigrationPayload {
        events,
        issues,
        versions,
        escalations,
        head_version,
    })
}

// ── Migration marker ──────────────────────────────────────────────────────────

/// Parsed contents of the `.vai/migrated_at` marker file.
///
/// Written by `vai remote migrate` after a successful migration so that
/// `vai remote status` can show verification info without re-querying all
/// local data from scratch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationMarker {
    /// UTC timestamp when migration completed (RFC 3339).
    pub migrated_at: DateTime<Utc>,
    /// URL of the remote server that received the migration.
    pub remote_url: String,
    /// Number of events migrated.
    pub events_migrated: usize,
    /// Number of issues migrated.
    pub issues_migrated: usize,
    /// Number of versions migrated.
    pub versions_migrated: usize,
    /// Number of escalations migrated.
    pub escalations_migrated: usize,
    /// HEAD version at the time of migration, if any.
    pub head_version: Option<String>,
}

impl MigrationMarker {
    /// Writes this marker to `<vai_dir>/migrated_at` in TOML format.
    pub fn write(&self, vai_dir: &Path) -> Result<(), MigrationError> {
        let path = vai_dir.join("migrated_at");
        let content = toml::to_string(self)
            .map_err(|e| MigrationError::Io(format!("failed to serialise migration marker: {e}")))?;
        std::fs::write(&path, content)
            .map_err(|e| MigrationError::Io(format!("failed to write migrated_at: {e}")))?;
        Ok(())
    }

    /// Reads and parses `.vai/migrated_at`, returning `None` if the file does
    /// not exist or cannot be parsed.
    pub fn read(vai_dir: &Path) -> Option<Self> {
        let path = vai_dir.join("migrated_at");
        let content = std::fs::read_to_string(&path).ok()?;
        toml::from_str(&content).ok()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn migration_marker_roundtrips() {
        let dir = TempDir::new().unwrap();
        let vai_dir = dir.path().join(".vai");
        std::fs::create_dir_all(&vai_dir).unwrap();

        let marker = MigrationMarker {
            migrated_at: Utc::now(),
            remote_url: "https://vai.example.com".to_string(),
            events_migrated: 42,
            issues_migrated: 5,
            versions_migrated: 3,
            escalations_migrated: 1,
            head_version: Some("v7".to_string()),
        };
        marker.write(&vai_dir).unwrap();

        let back = MigrationMarker::read(&vai_dir).expect("marker should be readable");
        assert_eq!(back.remote_url, "https://vai.example.com");
        assert_eq!(back.events_migrated, 42);
        assert_eq!(back.issues_migrated, 5);
        assert_eq!(back.versions_migrated, 3);
        assert_eq!(back.escalations_migrated, 1);
        assert_eq!(back.head_version, Some("v7".to_string()));
    }

    #[test]
    fn migration_marker_read_missing_returns_none() {
        let dir = TempDir::new().unwrap();
        let vai_dir = dir.path().join(".vai");
        std::fs::create_dir_all(&vai_dir).unwrap();
        assert!(MigrationMarker::read(&vai_dir).is_none());
    }

    #[test]
    fn gather_empty_repo() {
        let dir = TempDir::new().unwrap();
        let vai_dir = dir.path().join(".vai");
        std::fs::create_dir_all(&vai_dir).unwrap();

        let payload = gather_local_data(&vai_dir).unwrap();
        assert!(payload.events.is_empty());
        assert!(payload.issues.is_empty());
        assert!(payload.versions.is_empty());
        assert!(payload.escalations.is_empty());
        assert!(payload.head_version.is_none());
    }

    #[test]
    fn migration_payload_roundtrips_json() {
        let payload = MigrationPayload {
            events: Vec::new(),
            issues: Vec::new(),
            versions: Vec::new(),
            escalations: Vec::new(),
            head_version: Some("v3".to_string()),
        };
        let json = serde_json::to_string(&payload).unwrap();
        let back: MigrationPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(back.head_version, Some("v3".to_string()));
    }

    #[test]
    fn migration_summary_roundtrips_json() {
        let summary = MigrationSummary {
            events_migrated: 10,
            issues_migrated: 3,
            versions_migrated: 2,
            escalations_migrated: 0,
            head_version: Some("v2".to_string()),
            migrated_at: Utc::now(),
        };
        let json = serde_json::to_string(&summary).unwrap();
        let back: MigrationSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(back.events_migrated, 10);
        assert_eq!(back.versions_migrated, 2);
    }
}
