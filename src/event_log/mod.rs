//! Event log — append-only storage and retrieval of vai events.
//!
//! The event log is vai's source of truth. Every action in the system is recorded
//! as an immutable, append-only event. The current state of the repository is
//! derived by replaying the log.
//!
//! ## On-Disk Format
//!
//! Events are stored as newline-delimited JSON (NDJSON) in segment files under
//! `.vai/event_log/`. A new segment is created when the current one exceeds 64MB.
//! A SQLite index (`index.db`) enables fast queries without scanning all segments.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// Errors from event log operations.
#[derive(Debug, Error)]
pub enum EventLogError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("Serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("Event log directory not found: {0}")]
    NotFound(PathBuf),
}

/// Maximum segment size before rotating to a new file (64 MiB).
const MAX_SEGMENT_BYTES: u64 = 64 * 1024 * 1024;

// ── Event payload types ───────────────────────────────────────────────────────

/// Summary of an entity change, embedded in payloads.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntitySummary {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub qualified_name: String,
    pub file_path: String,
}

/// A single merge conflict, embedded in payloads.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictInfo {
    pub conflict_id: Uuid,
    pub entity_a: String,
    pub entity_b: String,
    pub description: String,
    pub severity: ConflictSeverity,
}

/// Conflict severity levels.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConflictSeverity {
    Low,
    Medium,
    High,
}

// ── Core event kind ───────────────────────────────────────────────────────────

/// All event types emitted by vai.
///
/// Each variant contains its own payload. `event_type` in the index is the
/// discriminant name (e.g., `"RepoInitialized"`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event_type", content = "payload")]
pub enum EventKind {
    /// A new vai repository was initialized.
    RepoInitialized {
        repo_id: Uuid,
        name: String,
    },
    /// A new version was created from a successful merge.
    VersionCreated {
        version_id: String,
        parent_version_id: Option<String>,
        intent: String,
    },
    /// A workspace was created.
    WorkspaceCreated {
        workspace_id: Uuid,
        intent: String,
        base_version: String,
    },
    /// A workspace was submitted for merging.
    WorkspaceSubmitted {
        workspace_id: Uuid,
        changes_summary: String,
    },
    /// A workspace was discarded.
    WorkspaceDiscarded {
        workspace_id: Uuid,
        reason: String,
    },
    /// An entity was added within a workspace.
    EntityAdded {
        workspace_id: Uuid,
        entity: EntitySummary,
    },
    /// An entity was modified within a workspace.
    EntityModified {
        workspace_id: Uuid,
        entity_id: String,
        change_description: String,
    },
    /// An entity was removed within a workspace.
    EntityRemoved {
        workspace_id: Uuid,
        entity_id: String,
    },
    /// A file was added within a workspace.
    FileAdded {
        workspace_id: Uuid,
        path: String,
        hash: String,
    },
    /// A file was modified within a workspace.
    FileModified {
        workspace_id: Uuid,
        path: String,
        old_hash: String,
        new_hash: String,
    },
    /// A file was removed within a workspace.
    FileRemoved {
        workspace_id: Uuid,
        path: String,
    },
    /// A merge completed successfully.
    MergeCompleted {
        workspace_id: Uuid,
        new_version_id: String,
        auto_resolved_conflicts: u32,
    },
    /// A merge conflict was detected.
    MergeConflictDetected {
        workspace_id: Uuid,
        conflict: ConflictInfo,
    },
    /// A merge conflict was resolved.
    MergeConflictResolved {
        conflict_id: Uuid,
        resolution: String,
        resolved_by: String,
    },
    /// A rollback was performed, creating a new version that undoes changes.
    RollbackCreated {
        target_version_id: String,
        new_version_id: String,
        entity_filter: Option<String>,
    },
}

impl EventKind {
    /// Returns the string discriminant used in the SQLite index.
    pub fn event_type(&self) -> &'static str {
        match self {
            EventKind::RepoInitialized { .. } => "RepoInitialized",
            EventKind::VersionCreated { .. } => "VersionCreated",
            EventKind::WorkspaceCreated { .. } => "WorkspaceCreated",
            EventKind::WorkspaceSubmitted { .. } => "WorkspaceSubmitted",
            EventKind::WorkspaceDiscarded { .. } => "WorkspaceDiscarded",
            EventKind::EntityAdded { .. } => "EntityAdded",
            EventKind::EntityModified { .. } => "EntityModified",
            EventKind::EntityRemoved { .. } => "EntityRemoved",
            EventKind::FileAdded { .. } => "FileAdded",
            EventKind::FileModified { .. } => "FileModified",
            EventKind::FileRemoved { .. } => "FileRemoved",
            EventKind::MergeCompleted { .. } => "MergeCompleted",
            EventKind::MergeConflictDetected { .. } => "MergeConflictDetected",
            EventKind::MergeConflictResolved { .. } => "MergeConflictResolved",
            EventKind::RollbackCreated { .. } => "RollbackCreated",
        }
    }

    /// Extracts the workspace ID from event kinds that carry one.
    pub fn workspace_id(&self) -> Option<Uuid> {
        match self {
            EventKind::WorkspaceCreated { workspace_id, .. }
            | EventKind::WorkspaceSubmitted { workspace_id, .. }
            | EventKind::WorkspaceDiscarded { workspace_id, .. }
            | EventKind::EntityAdded { workspace_id, .. }
            | EventKind::EntityModified { workspace_id, .. }
            | EventKind::EntityRemoved { workspace_id, .. }
            | EventKind::FileAdded { workspace_id, .. }
            | EventKind::FileModified { workspace_id, .. }
            | EventKind::FileRemoved { workspace_id, .. }
            | EventKind::MergeCompleted { workspace_id, .. }
            | EventKind::MergeConflictDetected { workspace_id, .. } => Some(*workspace_id),
            _ => None,
        }
    }

    /// Extracts the entity ID from event kinds that carry one.
    pub fn entity_id(&self) -> Option<&str> {
        match self {
            EventKind::EntityAdded { entity, .. } => Some(&entity.id),
            EventKind::EntityModified { entity_id, .. }
            | EventKind::EntityRemoved { entity_id, .. } => Some(entity_id),
            _ => None,
        }
    }
}

// ── Envelope ──────────────────────────────────────────────────────────────────

/// A single event as stored on disk and in memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    /// Monotonically increasing ID within this repository.
    pub id: u64,
    /// Wall-clock time when the event was recorded.
    pub timestamp: DateTime<Utc>,
    /// The actual event data.
    #[serde(flatten)]
    pub kind: EventKind,
}

// ── EventLog ──────────────────────────────────────────────────────────────────

/// Manages the append-only event log for a vai repository.
///
/// Events are written to NDJSON segment files and indexed in SQLite for fast
/// queries. The index is fully rebuildable from the segment files.
pub struct EventLog {
    dir: PathBuf,
    db: Connection,
}

impl EventLog {
    /// Opens (or creates) the event log at `dir`.
    ///
    /// `dir` should be the `.vai/event_log/` directory.
    pub fn open(dir: &Path) -> Result<Self, EventLogError> {
        fs::create_dir_all(dir)?;
        let db_path = dir.join("index.db");
        let db = Connection::open(&db_path)?;
        let log = EventLog {
            dir: dir.to_owned(),
            db,
        };
        log.init_schema()?;
        Ok(log)
    }

    fn init_schema(&self) -> Result<(), EventLogError> {
        self.db.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS events (
                 id           INTEGER PRIMARY KEY,
                 timestamp    TEXT    NOT NULL,
                 event_type   TEXT    NOT NULL,
                 workspace_id TEXT,
                 entity_id    TEXT,
                 segment_file TEXT    NOT NULL,
                 byte_offset  INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_event_type   ON events (event_type);
             CREATE INDEX IF NOT EXISTS idx_workspace_id ON events (workspace_id);
             CREATE INDEX IF NOT EXISTS idx_entity_id    ON events (entity_id);
             CREATE INDEX IF NOT EXISTS idx_timestamp    ON events (timestamp);",
        )?;
        Ok(())
    }

    /// Appends an event to the log and updates the index.
    ///
    /// The write is atomic at the OS level — the event line is written in a
    /// single `write(2)` call so a crash cannot produce a partial line.
    pub fn append(&mut self, kind: EventKind) -> Result<Event, EventLogError> {
        let id = self.next_id()?;
        let event = Event {
            id,
            timestamp: Utc::now(),
            kind,
        };

        let (segment_name, byte_offset) = self.write_to_segment(&event)?;
        self.index_event(&event, &segment_name, byte_offset)?;

        Ok(event)
    }

    /// Returns the next monotonic event ID.
    fn next_id(&self) -> Result<u64, EventLogError> {
        let max: Option<i64> = self
            .db
            .query_row("SELECT MAX(id) FROM events", [], |r| r.get(0))
            .unwrap_or(None);
        Ok(max.unwrap_or(0) as u64 + 1)
    }

    /// Writes the event to the current segment file, rotating if needed.
    ///
    /// Returns `(segment_name, byte_offset)`.
    fn write_to_segment(&self, event: &Event) -> Result<(String, u64), EventLogError> {
        let segment_name = self.current_segment_name()?;
        let segment_path = self.dir.join(&segment_name);

        let byte_offset = if segment_path.exists() {
            segment_path.metadata()?.len()
        } else {
            0
        };

        let mut line = serde_json::to_string(event)?;
        line.push('\n');

        // Single write call for atomicity.
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&segment_path)?;
        file.write_all(line.as_bytes())?;
        file.flush()?;

        Ok((segment_name, byte_offset))
    }

    /// Returns the name of the current segment, creating a new one if the
    /// existing segment is at or over `MAX_SEGMENT_BYTES`.
    fn current_segment_name(&self) -> Result<String, EventLogError> {
        let mut segments = self.list_segments()?;
        segments.sort();

        if let Some(last) = segments.last() {
            let path = self.dir.join(last);
            let size = path.metadata().map(|m| m.len()).unwrap_or(0);
            if size < MAX_SEGMENT_BYTES {
                return Ok(last.clone());
            }
            // Rotate: new segment number = last + 1.
            let n: u32 = last
                .trim_end_matches(".events")
                .parse()
                .unwrap_or(0);
            return Ok(format!("{:06}.events", n + 1));
        }

        Ok("000001.events".to_string())
    }

    /// Lists all `*.events` segment file names in the log directory.
    fn list_segments(&self) -> Result<Vec<String>, EventLogError> {
        let mut names = Vec::new();
        for entry in fs::read_dir(&self.dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".events") {
                names.push(name);
            }
        }
        Ok(names)
    }

    /// Inserts an event into the SQLite index.
    fn index_event(
        &self,
        event: &Event,
        segment_file: &str,
        byte_offset: u64,
    ) -> Result<(), EventLogError> {
        self.db.execute(
            "INSERT INTO events (id, timestamp, event_type, workspace_id, entity_id, segment_file, byte_offset)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                event.id as i64,
                event.timestamp.to_rfc3339(),
                event.kind.event_type(),
                event.kind.workspace_id().map(|u| u.to_string()),
                event.kind.entity_id().map(String::from),
                segment_file,
                byte_offset as i64,
            ],
        )?;
        Ok(())
    }

    // ── Query API ─────────────────────────────────────────────────────────────

    /// Returns all events of a given type (e.g., `"WorkspaceCreated"`).
    pub fn query_by_type(&self, event_type: &str) -> Result<Vec<Event>, EventLogError> {
        let rows = self.query_index(
            "SELECT segment_file, byte_offset FROM events WHERE event_type = ?1 ORDER BY id",
            params![event_type],
        )?;
        self.load_events(rows)
    }

    /// Returns all events associated with a workspace.
    pub fn query_by_workspace(&self, workspace_id: Uuid) -> Result<Vec<Event>, EventLogError> {
        let rows = self.query_index(
            "SELECT segment_file, byte_offset FROM events WHERE workspace_id = ?1 ORDER BY id",
            params![workspace_id.to_string()],
        )?;
        self.load_events(rows)
    }

    /// Returns all events associated with an entity.
    pub fn query_by_entity(&self, entity_id: &str) -> Result<Vec<Event>, EventLogError> {
        let rows = self.query_index(
            "SELECT segment_file, byte_offset FROM events WHERE entity_id = ?1 ORDER BY id",
            params![entity_id],
        )?;
        self.load_events(rows)
    }

    /// Returns all events in the given time range (inclusive).
    pub fn query_by_time_range(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<Event>, EventLogError> {
        let rows = self.query_index(
            "SELECT segment_file, byte_offset FROM events WHERE timestamp >= ?1 AND timestamp <= ?2 ORDER BY id",
            params![start.to_rfc3339(), end.to_rfc3339()],
        )?;
        self.load_events(rows)
    }

    /// Returns all events, ordered by ID.
    pub fn all(&self) -> Result<Vec<Event>, EventLogError> {
        let rows = self.query_index(
            "SELECT segment_file, byte_offset FROM events ORDER BY id",
            params![],
        )?;
        self.load_events(rows)
    }

    /// Counts all events.
    pub fn count(&self) -> Result<u64, EventLogError> {
        let n: i64 = self
            .db
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))?;
        Ok(n as u64)
    }

    /// Returns the event with the given ID, or `None` if not found.
    pub fn get_by_id(&self, id: u64) -> Result<Option<Event>, EventLogError> {
        let rows = self.query_index(
            "SELECT segment_file, byte_offset FROM events WHERE id = ?1",
            params![id as i64],
        )?;
        match rows.into_iter().next() {
            Some((segment_file, byte_offset)) => {
                Ok(Some(self.read_event_at(&segment_file, byte_offset)?))
            }
            None => Ok(None),
        }
    }

    fn query_index(
        &self,
        sql: &str,
        params: impl rusqlite::Params,
    ) -> Result<Vec<(String, u64)>, EventLogError> {
        let mut stmt = self.db.prepare(sql)?;
        let rows = stmt.query_map(params, |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64))
        })?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }

    fn load_events(&self, rows: Vec<(String, u64)>) -> Result<Vec<Event>, EventLogError> {
        let mut events = Vec::new();
        for (segment_file, byte_offset) in rows {
            let event = self.read_event_at(&segment_file, byte_offset)?;
            events.push(event);
        }
        Ok(events)
    }

    /// Reads a single event from a segment file at the given byte offset.
    fn read_event_at(&self, segment_file: &str, byte_offset: u64) -> Result<Event, EventLogError> {
        use std::io::{BufRead as _, Seek, SeekFrom};
        let mut file = File::open(self.dir.join(segment_file))?;
        file.seek(SeekFrom::Start(byte_offset))?;
        let mut line = String::new();
        let mut buf = BufReader::new(file);
        buf.read_line(&mut line)?;
        Ok(serde_json::from_str(line.trim_end())?)
    }

    // ── Index rebuild ─────────────────────────────────────────────────────────

    /// Rebuilds the SQLite index by scanning all segment files.
    ///
    /// Use this for crash recovery when the index may be incomplete.
    pub fn rebuild_index(&mut self) -> Result<(), EventLogError> {
        self.db.execute("DELETE FROM events", [])?;

        let mut segments = self.list_segments()?;
        segments.sort();

        for segment_name in segments {
            let segment_path = self.dir.join(&segment_name);
            let file = File::open(&segment_path)?;
            let reader = BufReader::new(file);

            let mut byte_offset: u64 = 0;
            for line_result in reader.lines() {
                let line = line_result?;
                if line.trim().is_empty() {
                    byte_offset += line.len() as u64 + 1;
                    continue;
                }
                let event: Event = serde_json::from_str(&line)?;
                self.index_event(&event, &segment_name, byte_offset)?;
                byte_offset += line.len() as u64 + 1; // +1 for '\n'
            }
        }

        Ok(())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn open_log(tmp: &TempDir) -> EventLog {
        EventLog::open(&tmp.path().join("event_log")).expect("open EventLog")
    }

    #[test]
    fn append_and_read_back() {
        let tmp = TempDir::new().unwrap();
        let mut log = open_log(&tmp);

        let repo_id = Uuid::new_v4();
        let ev = log
            .append(EventKind::RepoInitialized {
                repo_id,
                name: "test".into(),
            })
            .unwrap();

        assert_eq!(ev.id, 1);
        assert_eq!(ev.kind.event_type(), "RepoInitialized");

        let all = log.all().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, 1);
    }

    #[test]
    fn query_by_type() {
        let tmp = TempDir::new().unwrap();
        let mut log = open_log(&tmp);

        log.append(EventKind::RepoInitialized {
            repo_id: Uuid::new_v4(),
            name: "test".into(),
        })
        .unwrap();

        let ws_id = Uuid::new_v4();
        log.append(EventKind::WorkspaceCreated {
            workspace_id: ws_id,
            intent: "fix bug".into(),
            base_version: "v1".into(),
        })
        .unwrap();

        log.append(EventKind::WorkspaceCreated {
            workspace_id: Uuid::new_v4(),
            intent: "add feature".into(),
            base_version: "v1".into(),
        })
        .unwrap();

        let workspaces = log.query_by_type("WorkspaceCreated").unwrap();
        assert_eq!(workspaces.len(), 2);

        let inits = log.query_by_type("RepoInitialized").unwrap();
        assert_eq!(inits.len(), 1);
    }

    #[test]
    fn query_by_workspace() {
        let tmp = TempDir::new().unwrap();
        let mut log = open_log(&tmp);

        let ws_a = Uuid::new_v4();
        let ws_b = Uuid::new_v4();

        log.append(EventKind::WorkspaceCreated {
            workspace_id: ws_a,
            intent: "workspace A".into(),
            base_version: "v1".into(),
        })
        .unwrap();

        log.append(EventKind::FileAdded {
            workspace_id: ws_a,
            path: "src/foo.rs".into(),
            hash: "abc123".into(),
        })
        .unwrap();

        log.append(EventKind::WorkspaceCreated {
            workspace_id: ws_b,
            intent: "workspace B".into(),
            base_version: "v1".into(),
        })
        .unwrap();

        let events_a = log.query_by_workspace(ws_a).unwrap();
        assert_eq!(events_a.len(), 2);

        let events_b = log.query_by_workspace(ws_b).unwrap();
        assert_eq!(events_b.len(), 1);
    }

    #[test]
    fn query_by_time_range() {
        let tmp = TempDir::new().unwrap();
        let mut log = open_log(&tmp);

        let before = Utc::now();
        log.append(EventKind::RepoInitialized {
            repo_id: Uuid::new_v4(),
            name: "t".into(),
        })
        .unwrap();
        let after = Utc::now();

        let results = log.query_by_time_range(before, after).unwrap();
        assert_eq!(results.len(), 1);

        // Range before first event → empty.
        let empty = log
            .query_by_time_range(
                before - chrono::Duration::seconds(10),
                before - chrono::Duration::seconds(1),
            )
            .unwrap();
        assert!(empty.is_empty());
    }

    #[test]
    fn rebuild_index() {
        let tmp = TempDir::new().unwrap();
        let mut log = open_log(&tmp);

        let ws_id = Uuid::new_v4();
        log.append(EventKind::WorkspaceCreated {
            workspace_id: ws_id,
            intent: "test".into(),
            base_version: "v1".into(),
        })
        .unwrap();
        log.append(EventKind::WorkspaceSubmitted {
            workspace_id: ws_id,
            changes_summary: "added 1 file".into(),
        })
        .unwrap();

        // Corrupt index by clearing it.
        log.db.execute("DELETE FROM events", []).unwrap();
        assert_eq!(log.count().unwrap(), 0);

        // Rebuild should restore all events.
        log.rebuild_index().unwrap();
        assert_eq!(log.count().unwrap(), 2);

        let by_ws = log.query_by_workspace(ws_id).unwrap();
        assert_eq!(by_ws.len(), 2);
    }
}
