//! Escalation system — human oversight for unresolvable conflicts.
//!
//! Escalations are created automatically when the merge engine cannot resolve
//! a Level 3 semantic conflict, when critical workspace overlaps occur, or
//! when an agent explicitly requests human review.
//!
//! ## Escalation Lifecycle
//! ```text
//! Pending → Resolved
//! ```
//!
//! Escalations are stored in `.vai/escalations.db` with all state transitions
//! recorded in the event log.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::event_log::{EventKind, EventLog};

/// Errors from escalation operations.
#[derive(Debug, Error)]
pub enum EscalationError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("Event log error: {0}")]
    EventLog(#[from] crate::event_log::EventLogError),

    #[error("Escalation not found: {0}")]
    NotFound(Uuid),

    #[error("Escalation already resolved: {0}")]
    AlreadyResolved(Uuid),

    #[error("Escalation store not initialized at {0}")]
    NotInitialized(PathBuf),
}

// ── Escalation type ───────────────────────────────────────────────────────────

/// The type of situation that triggered an escalation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EscalationType {
    /// A merge conflict that could not be auto-resolved.
    MergeConflict,
    /// Two intents conflict at the semantic level.
    IntentConflict,
    /// An agent explicitly requested human review.
    ReviewRequest,
    /// Post-merge validation failed (e.g., parse error).
    ValidationFailure,
}

impl EscalationType {
    /// Return the string representation stored in SQLite.
    pub fn as_str(&self) -> &'static str {
        match self {
            EscalationType::MergeConflict => "merge_conflict",
            EscalationType::IntentConflict => "intent_conflict",
            EscalationType::ReviewRequest => "review_request",
            EscalationType::ValidationFailure => "validation_failure",
        }
    }

    /// Parse from a string.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "merge_conflict" => Some(EscalationType::MergeConflict),
            "intent_conflict" => Some(EscalationType::IntentConflict),
            "review_request" => Some(EscalationType::ReviewRequest),
            "validation_failure" => Some(EscalationType::ValidationFailure),
            _ => None,
        }
    }
}

// ── Escalation severity ───────────────────────────────────────────────────────

/// Severity level of an escalation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EscalationSeverity {
    /// Significant conflict requiring prompt attention.
    High,
    /// Blocking conflict requiring immediate attention.
    Critical,
}

impl EscalationSeverity {
    /// Return the string representation stored in SQLite.
    pub fn as_str(&self) -> &'static str {
        match self {
            EscalationSeverity::High => "high",
            EscalationSeverity::Critical => "critical",
        }
    }

    /// Parse from a string.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "high" => Some(EscalationSeverity::High),
            "critical" => Some(EscalationSeverity::Critical),
            _ => None,
        }
    }
}

// ── Resolution option ─────────────────────────────────────────────────────────

/// A resolution option presented to the human operator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionOption {
    /// Accept changes from the first agent (workspace A).
    KeepAgentA,
    /// Accept changes from the second agent (workspace B).
    KeepAgentB,
    /// Send the conflict back to agent A with context for re-attempt.
    SendBackToAgentA,
    /// Send the conflict back to agent B with context for re-attempt.
    SendBackToAgentB,
    /// Pause both workspaces pending further review.
    PauseBoth,
}

impl ResolutionOption {
    /// Return the string representation stored in SQLite.
    pub fn as_str(&self) -> &'static str {
        match self {
            ResolutionOption::KeepAgentA => "keep_agent_a",
            ResolutionOption::KeepAgentB => "keep_agent_b",
            ResolutionOption::SendBackToAgentA => "send_back_to_agent_a",
            ResolutionOption::SendBackToAgentB => "send_back_to_agent_b",
            ResolutionOption::PauseBoth => "pause_both",
        }
    }

    /// Parse from a string.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "keep_agent_a" => Some(ResolutionOption::KeepAgentA),
            "keep_agent_b" => Some(ResolutionOption::KeepAgentB),
            "send_back_to_agent_a" => Some(ResolutionOption::SendBackToAgentA),
            "send_back_to_agent_b" => Some(ResolutionOption::SendBackToAgentB),
            "pause_both" => Some(ResolutionOption::PauseBoth),
            _ => None,
        }
    }

    /// Human-readable label for this option.
    pub fn label(&self) -> &'static str {
        match self {
            ResolutionOption::KeepAgentA => "Keep agent A's changes",
            ResolutionOption::KeepAgentB => "Keep agent B's changes",
            ResolutionOption::SendBackToAgentA => "Send back to agent A with context",
            ResolutionOption::SendBackToAgentB => "Send back to agent B with context",
            ResolutionOption::PauseBoth => "Pause both workspaces",
        }
    }
}

// ── Escalation status ─────────────────────────────────────────────────────────

/// Lifecycle status of an escalation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EscalationStatus {
    /// Awaiting human resolution.
    Pending,
    /// Resolved by a human operator.
    Resolved,
}

impl EscalationStatus {
    /// Return the string representation stored in SQLite.
    pub fn as_str(&self) -> &'static str {
        match self {
            EscalationStatus::Pending => "pending",
            EscalationStatus::Resolved => "resolved",
        }
    }

    /// Parse from a string.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(EscalationStatus::Pending),
            "resolved" => Some(EscalationStatus::Resolved),
            _ => None,
        }
    }
}

// ── Escalation ────────────────────────────────────────────────────────────────

/// A single escalation requiring human attention.
///
/// Escalations are created automatically when the system cannot resolve a
/// conflict. They present the situation at the intent level to minimise
/// human cognitive burden.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Escalation {
    /// Unique identifier for this escalation.
    pub id: Uuid,
    /// The type of event that triggered this escalation.
    pub escalation_type: EscalationType,
    /// Severity level.
    pub severity: EscalationSeverity,
    /// Current status.
    pub status: EscalationStatus,
    /// Human-readable summary of the conflict.
    pub summary: String,
    /// Intents (workspace descriptions) of the agents involved.
    pub intents: Vec<String>,
    /// IDs of the agents involved.
    pub agents: Vec<String>,
    /// IDs of the workspaces involved.
    pub workspace_ids: Vec<Uuid>,
    /// IDs of the semantic entities affected.
    pub affected_entities: Vec<String>,
    /// Available resolution options for the human operator.
    pub resolution_options: Vec<ResolutionOption>,
    /// The resolution chosen by the human, if resolved.
    pub resolution: Option<ResolutionOption>,
    /// Who resolved this escalation (human username or agent ID).
    pub resolved_by: Option<String>,
    /// When this escalation was created.
    pub created_at: DateTime<Utc>,
    /// When this escalation was resolved, if at all.
    pub resolved_at: Option<DateTime<Utc>>,
}

impl Escalation {
    /// Returns `true` if this escalation has not yet been resolved.
    pub fn is_pending(&self) -> bool {
        self.status == EscalationStatus::Pending
    }
}

// ── EscalationStore ───────────────────────────────────────────────────────────

/// SQLite-backed storage for escalations.
///
/// Data is persisted in `.vai/escalations.db`. All state changes are also
/// recorded in the event log for auditability.
pub struct EscalationStore {
    db: Connection,
}

impl EscalationStore {
    /// Opens (or creates) the escalation store at `vai_dir`.
    pub fn open(vai_dir: &Path) -> Result<Self, EscalationError> {
        let path = vai_dir.join("escalations.db");
        let db = Connection::open(&path)?;
        let store = EscalationStore { db };
        store.init_schema()?;
        Ok(store)
    }

    fn init_schema(&self) -> Result<(), EscalationError> {
        self.db.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS escalations (
                 id                 TEXT PRIMARY KEY,
                 escalation_type    TEXT NOT NULL,
                 severity           TEXT NOT NULL,
                 status             TEXT NOT NULL DEFAULT 'pending',
                 summary            TEXT NOT NULL,
                 intents            TEXT NOT NULL DEFAULT '[]',
                 agents             TEXT NOT NULL DEFAULT '[]',
                 workspace_ids      TEXT NOT NULL DEFAULT '[]',
                 affected_entities  TEXT NOT NULL DEFAULT '[]',
                 resolution_options TEXT NOT NULL DEFAULT '[]',
                 resolution         TEXT,
                 resolved_by        TEXT,
                 created_at         TEXT NOT NULL,
                 resolved_at        TEXT
             );
             CREATE INDEX IF NOT EXISTS idx_escalations_status
                 ON escalations (status);
             CREATE INDEX IF NOT EXISTS idx_escalations_created
                 ON escalations (created_at);",
        )?;
        Ok(())
    }

    /// Create a new escalation and record an `EscalationCreated` event.
    ///
    /// Resolution options are auto-generated based on the escalation type and
    /// the number of workspaces involved.
    pub fn create(
        &self,
        escalation_type: EscalationType,
        severity: EscalationSeverity,
        summary: String,
        intents: Vec<String>,
        agents: Vec<String>,
        workspace_ids: Vec<Uuid>,
        affected_entities: Vec<String>,
        event_log: &mut EventLog,
    ) -> Result<Escalation, EscalationError> {
        let id = Uuid::new_v4();
        let now = Utc::now();
        let resolution_options = default_resolution_options(&escalation_type, &workspace_ids);

        let escalation = Escalation {
            id,
            escalation_type: escalation_type.clone(),
            severity: severity.clone(),
            status: EscalationStatus::Pending,
            summary: summary.clone(),
            intents: intents.clone(),
            agents: agents.clone(),
            workspace_ids: workspace_ids.clone(),
            affected_entities: affected_entities.clone(),
            resolution_options: resolution_options.clone(),
            resolution: None,
            resolved_by: None,
            created_at: now,
            resolved_at: None,
        };

        self.db.execute(
            "INSERT INTO escalations
             (id, escalation_type, severity, status, summary,
              intents, agents, workspace_ids, affected_entities,
              resolution_options, created_at)
             VALUES (?1, ?2, ?3, 'pending', ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                id.to_string(),
                escalation_type.as_str(),
                severity.as_str(),
                &summary,
                serde_json::to_string(&intents).unwrap_or_default(),
                serde_json::to_string(&agents).unwrap_or_default(),
                serde_json::to_string(
                    &workspace_ids.iter().map(|u| u.to_string()).collect::<Vec<_>>()
                )
                .unwrap_or_default(),
                serde_json::to_string(&affected_entities).unwrap_or_default(),
                serde_json::to_string(&resolution_options).unwrap_or_default(),
                now.to_rfc3339(),
            ],
        )?;

        let _ = event_log.append(EventKind::EscalationCreated {
            escalation_id: id,
            escalation_type: escalation_type.as_str().to_string(),
            severity: severity.as_str().to_string(),
            workspace_ids: workspace_ids.iter().map(|u| u.to_string()).collect(),
            summary,
        });

        Ok(escalation)
    }

    /// Retrieve a single escalation by ID.
    ///
    /// Returns `EscalationError::NotFound` if no escalation with this ID exists.
    pub fn get(&self, id: Uuid) -> Result<Escalation, EscalationError> {
        let mut stmt = self.db.prepare(
            "SELECT id, escalation_type, severity, status, summary,
                    intents, agents, workspace_ids, affected_entities,
                    resolution_options, resolution, resolved_by,
                    created_at, resolved_at
             FROM escalations WHERE id = ?1",
        )?;

        match stmt.query_row(params![id.to_string()], row_to_escalation) {
            Ok(e) => Ok(e),
            Err(rusqlite::Error::QueryReturnedNoRows) => Err(EscalationError::NotFound(id)),
            Err(e) => Err(EscalationError::Sqlite(e)),
        }
    }

    /// List escalations, optionally filtered by status.
    ///
    /// Results are returned newest-first.
    pub fn list(
        &self,
        status: Option<&EscalationStatus>,
    ) -> Result<Vec<Escalation>, EscalationError> {
        if let Some(s) = status {
            let mut stmt = self.db.prepare(
                "SELECT id, escalation_type, severity, status, summary,
                        intents, agents, workspace_ids, affected_entities,
                        resolution_options, resolution, resolved_by,
                        created_at, resolved_at
                 FROM escalations WHERE status = ?1 ORDER BY created_at DESC",
            )?;
            let rows = stmt
                .query_map(params![s.as_str()], row_to_escalation)?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        } else {
            let mut stmt = self.db.prepare(
                "SELECT id, escalation_type, severity, status, summary,
                        intents, agents, workspace_ids, affected_entities,
                        resolution_options, resolution, resolved_by,
                        created_at, resolved_at
                 FROM escalations ORDER BY created_at DESC",
            )?;
            let rows = stmt
                .query_map([], row_to_escalation)?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        }
    }

    /// Resolve an escalation with the chosen option and record an event.
    ///
    /// Returns `EscalationError::AlreadyResolved` if the escalation is already
    /// in the `Resolved` state.
    pub fn resolve(
        &self,
        id: Uuid,
        option: ResolutionOption,
        resolved_by: String,
        event_log: &mut EventLog,
    ) -> Result<Escalation, EscalationError> {
        let escalation = self.get(id)?;
        if escalation.status == EscalationStatus::Resolved {
            return Err(EscalationError::AlreadyResolved(id));
        }

        let now = Utc::now();
        self.db.execute(
            "UPDATE escalations
             SET status = 'resolved', resolution = ?1,
                 resolved_by = ?2, resolved_at = ?3
             WHERE id = ?4",
            params![
                option.as_str(),
                &resolved_by,
                now.to_rfc3339(),
                id.to_string(),
            ],
        )?;

        let _ = event_log.append(EventKind::EscalationResolved {
            escalation_id: id,
            resolution: option.as_str().to_string(),
            resolved_by: resolved_by.clone(),
        });

        Ok(Escalation {
            status: EscalationStatus::Resolved,
            resolution: Some(option),
            resolved_by: Some(resolved_by),
            resolved_at: Some(now),
            ..escalation
        })
    }

    /// Count the number of pending escalations (used in `vai status`).
    pub fn count_pending(&self) -> Result<usize, EscalationError> {
        let count: i64 = self.db.query_row(
            "SELECT COUNT(*) FROM escalations WHERE status = 'pending'",
            [],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Generate the default resolution options for an escalation.
///
/// Options depend on the escalation type and the number of workspaces involved.
fn default_resolution_options(
    escalation_type: &EscalationType,
    workspace_ids: &[Uuid],
) -> Vec<ResolutionOption> {
    match escalation_type {
        EscalationType::MergeConflict | EscalationType::IntentConflict => {
            if workspace_ids.len() >= 2 {
                vec![
                    ResolutionOption::KeepAgentA,
                    ResolutionOption::KeepAgentB,
                    ResolutionOption::SendBackToAgentA,
                    ResolutionOption::SendBackToAgentB,
                    ResolutionOption::PauseBoth,
                ]
            } else {
                vec![
                    ResolutionOption::SendBackToAgentA,
                    ResolutionOption::PauseBoth,
                ]
            }
        }
        EscalationType::ReviewRequest => vec![
            ResolutionOption::KeepAgentA,
            ResolutionOption::SendBackToAgentA,
            ResolutionOption::PauseBoth,
        ],
        EscalationType::ValidationFailure => vec![
            ResolutionOption::SendBackToAgentA,
            ResolutionOption::PauseBoth,
        ],
    }
}

/// Convert a SQLite row to an [`Escalation`].
fn row_to_escalation(row: &rusqlite::Row<'_>) -> rusqlite::Result<Escalation> {
    let id_str: String = row.get(0)?;
    let type_str: String = row.get(1)?;
    let severity_str: String = row.get(2)?;
    let status_str: String = row.get(3)?;
    let summary: String = row.get(4)?;
    let intents_json: String = row.get(5)?;
    let agents_json: String = row.get(6)?;
    let ws_ids_json: String = row.get(7)?;
    let entities_json: String = row.get(8)?;
    let options_json: String = row.get(9)?;
    let resolution_str: Option<String> = row.get(10)?;
    let resolved_by: Option<String> = row.get(11)?;
    let created_at_str: String = row.get(12)?;
    let resolved_at_str: Option<String> = row.get(13)?;

    let id = Uuid::parse_str(&id_str).unwrap_or_default();
    let escalation_type =
        EscalationType::from_str(&type_str).unwrap_or(EscalationType::MergeConflict);
    let severity =
        EscalationSeverity::from_str(&severity_str).unwrap_or(EscalationSeverity::High);
    let status = EscalationStatus::from_str(&status_str).unwrap_or(EscalationStatus::Pending);

    let intents: Vec<String> = serde_json::from_str(&intents_json).unwrap_or_default();
    let agents: Vec<String> = serde_json::from_str(&agents_json).unwrap_or_default();
    let ws_id_strings: Vec<String> = serde_json::from_str(&ws_ids_json).unwrap_or_default();
    let workspace_ids: Vec<Uuid> = ws_id_strings
        .iter()
        .filter_map(|s| Uuid::parse_str(s).ok())
        .collect();
    let affected_entities: Vec<String> =
        serde_json::from_str(&entities_json).unwrap_or_default();
    let resolution_options: Vec<ResolutionOption> =
        serde_json::from_str(&options_json).unwrap_or_default();
    let resolution = resolution_str
        .as_deref()
        .and_then(ResolutionOption::from_str);

    let created_at = created_at_str
        .parse::<DateTime<Utc>>()
        .unwrap_or_default();
    let resolved_at = resolved_at_str.and_then(|s| s.parse::<DateTime<Utc>>().ok());

    Ok(Escalation {
        id,
        escalation_type,
        severity,
        status,
        summary,
        intents,
        agents,
        workspace_ids,
        affected_entities,
        resolution_options,
        resolution,
        resolved_by,
        created_at,
        resolved_at,
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn make_store(tmp: &TempDir) -> (EscalationStore, EventLog) {
        let vai_dir = tmp.path().join(".vai");
        fs::create_dir_all(&vai_dir).unwrap();
        let log_dir = vai_dir.join("event_log");
        fs::create_dir_all(&log_dir).unwrap();
        let store = EscalationStore::open(&vai_dir).unwrap();
        let log = EventLog::open(&log_dir).unwrap();
        (store, log)
    }

    #[test]
    fn create_and_get_escalation() {
        let tmp = TempDir::new().unwrap();
        let (store, mut log) = make_store(&tmp);

        let ws_a = Uuid::new_v4();
        let ws_b = Uuid::new_v4();
        let e = store
            .create(
                EscalationType::MergeConflict,
                EscalationSeverity::High,
                "Two agents modified the same function".to_string(),
                vec!["add auth".to_string(), "add logging".to_string()],
                vec!["agent-a".to_string(), "agent-b".to_string()],
                vec![ws_a, ws_b],
                vec!["fn::authenticate".to_string()],
                &mut log,
            )
            .unwrap();

        assert_eq!(e.status, EscalationStatus::Pending);
        assert_eq!(e.escalation_type, EscalationType::MergeConflict);
        assert!(e.resolution_options.len() >= 2);

        let fetched = store.get(e.id).unwrap();
        assert_eq!(fetched.id, e.id);
        assert_eq!(fetched.summary, "Two agents modified the same function");
        assert_eq!(fetched.workspace_ids.len(), 2);
    }

    #[test]
    fn list_and_filter_escalations() {
        let tmp = TempDir::new().unwrap();
        let (store, mut log) = make_store(&tmp);

        store
            .create(
                EscalationType::MergeConflict,
                EscalationSeverity::High,
                "Conflict A".to_string(),
                vec![],
                vec![],
                vec![],
                vec![],
                &mut log,
            )
            .unwrap();

        store
            .create(
                EscalationType::ReviewRequest,
                EscalationSeverity::Critical,
                "Conflict B".to_string(),
                vec![],
                vec![],
                vec![],
                vec![],
                &mut log,
            )
            .unwrap();

        let all = store.list(None).unwrap();
        assert_eq!(all.len(), 2);

        let pending = store.list(Some(&EscalationStatus::Pending)).unwrap();
        assert_eq!(pending.len(), 2);

        let resolved = store.list(Some(&EscalationStatus::Resolved)).unwrap();
        assert_eq!(resolved.len(), 0);

        let count = store.count_pending().unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn resolve_escalation() {
        let tmp = TempDir::new().unwrap();
        let (store, mut log) = make_store(&tmp);

        let e = store
            .create(
                EscalationType::MergeConflict,
                EscalationSeverity::Critical,
                "Critical conflict".to_string(),
                vec!["fix bug".to_string()],
                vec!["agent-a".to_string()],
                vec![Uuid::new_v4()],
                vec![],
                &mut log,
            )
            .unwrap();

        let resolved = store
            .resolve(
                e.id,
                ResolutionOption::KeepAgentA,
                "human-1".to_string(),
                &mut log,
            )
            .unwrap();

        assert_eq!(resolved.status, EscalationStatus::Resolved);
        assert_eq!(resolved.resolution, Some(ResolutionOption::KeepAgentA));
        assert_eq!(resolved.resolved_by.as_deref(), Some("human-1"));
        assert!(resolved.resolved_at.is_some());

        let count = store.count_pending().unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn cannot_resolve_twice() {
        let tmp = TempDir::new().unwrap();
        let (store, mut log) = make_store(&tmp);

        let e = store
            .create(
                EscalationType::MergeConflict,
                EscalationSeverity::High,
                "Conflict".to_string(),
                vec![],
                vec![],
                vec![],
                vec![],
                &mut log,
            )
            .unwrap();

        store
            .resolve(
                e.id,
                ResolutionOption::PauseBoth,
                "human".to_string(),
                &mut log,
            )
            .unwrap();
        let result = store.resolve(
            e.id,
            ResolutionOption::PauseBoth,
            "human".to_string(),
            &mut log,
        );
        assert!(matches!(result, Err(EscalationError::AlreadyResolved(_))));
    }

    #[test]
    fn not_found_returns_error() {
        let tmp = TempDir::new().unwrap();
        let (store, _log) = make_store(&tmp);
        let bogus = Uuid::new_v4();
        assert!(matches!(store.get(bogus), Err(EscalationError::NotFound(_))));
    }

    #[test]
    fn default_options_two_workspaces() {
        let ids = vec![Uuid::new_v4(), Uuid::new_v4()];
        let opts = default_resolution_options(&EscalationType::MergeConflict, &ids);
        assert!(opts.contains(&ResolutionOption::KeepAgentA));
        assert!(opts.contains(&ResolutionOption::KeepAgentB));
    }

    #[test]
    fn default_options_one_workspace() {
        let ids = vec![Uuid::new_v4()];
        let opts = default_resolution_options(&EscalationType::ValidationFailure, &ids);
        assert!(!opts.contains(&ResolutionOption::KeepAgentB));
        assert!(opts.contains(&ResolutionOption::PauseBoth));
    }
}
