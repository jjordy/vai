//! Issue storage — SQLite-backed issue CRUD with event log integration.
//!
//! Issues are first-class objects representing units of work. They are stored in
//! `.vai/issues.db` and all state transitions are recorded in the event log.
//!
//! ## Issue Lifecycle
//! ```text
//! Open → InProgress → Resolved → Closed
//!      → Closed (wontfix / duplicate)
//! ```

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::event_log::{EventKind, EventLog};

/// Errors from issue operations.
#[derive(Debug, Error)]
pub enum IssueError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("Event log error: {0}")]
    EventLog(#[from] crate::event_log::EventLogError),

    #[error("Issue not found: {0}")]
    NotFound(Uuid),

    #[error("Invalid state transition from {from} to {to}")]
    InvalidTransition { from: String, to: String },

    #[error("Issue store not initialized at {0}")]
    NotInitialized(PathBuf),
}

// ── Issue priority ────────────────────────────────────────────────────────────

/// Priority level for an issue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IssuePriority {
    Critical,
    High,
    Medium,
    Low,
}

impl IssuePriority {
    /// Return the string representation stored in SQLite.
    pub fn as_str(&self) -> &'static str {
        match self {
            IssuePriority::Critical => "critical",
            IssuePriority::High => "high",
            IssuePriority::Medium => "medium",
            IssuePriority::Low => "low",
        }
    }

    /// Parse from a string.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "critical" => Some(IssuePriority::Critical),
            "high" => Some(IssuePriority::High),
            "medium" => Some(IssuePriority::Medium),
            "low" => Some(IssuePriority::Low),
            _ => None,
        }
    }
}

// ── Issue status ──────────────────────────────────────────────────────────────

/// Lifecycle status of an issue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueStatus {
    Open,
    InProgress,
    Resolved,
    Closed,
}

impl IssueStatus {
    /// Return the string representation stored in SQLite.
    pub fn as_str(&self) -> &'static str {
        match self {
            IssueStatus::Open => "open",
            IssueStatus::InProgress => "in_progress",
            IssueStatus::Resolved => "resolved",
            IssueStatus::Closed => "closed",
        }
    }

    /// Parse from a string.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "open" => Some(IssueStatus::Open),
            "in_progress" => Some(IssueStatus::InProgress),
            "resolved" => Some(IssueStatus::Resolved),
            "closed" => Some(IssueStatus::Closed),
            _ => None,
        }
    }
}

// ── Issue resolution ──────────────────────────────────────────────────────────

/// Resolution type when closing an issue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueResolution {
    Resolved,
    WontFix,
    Duplicate,
}

impl IssueResolution {
    /// Return the string representation stored in SQLite.
    pub fn as_str(&self) -> &'static str {
        match self {
            IssueResolution::Resolved => "resolved",
            IssueResolution::WontFix => "wontfix",
            IssueResolution::Duplicate => "duplicate",
        }
    }

    /// Parse from a string.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "resolved" => Some(IssueResolution::Resolved),
            "wontfix" => Some(IssueResolution::WontFix),
            "duplicate" => Some(IssueResolution::Duplicate),
            _ => None,
        }
    }
}

// ── Issue data model ──────────────────────────────────────────────────────────

/// A single issue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    /// Unique issue identifier.
    pub id: Uuid,
    /// Short summary of the issue.
    pub title: String,
    /// Full description (Markdown supported).
    pub description: String,
    /// Current lifecycle status.
    pub status: IssueStatus,
    /// Priority level.
    pub priority: IssuePriority,
    /// Comma-separated labels stored as a single string, exposed as a `Vec`.
    pub labels: Vec<String>,
    /// Human username or agent ID that created this issue.
    pub creator: String,
    /// Resolution string (set when status becomes Resolved or Closed).
    pub resolution: Option<String>,
    /// When the issue was created.
    pub created_at: DateTime<Utc>,
    /// When the issue was last updated.
    pub updated_at: DateTime<Utc>,
}

// ── Filter for list queries ───────────────────────────────────────────────────

/// Filters for [`IssueStore::list`].
#[derive(Debug, Default, Clone)]
pub struct IssueFilter {
    pub status: Option<IssueStatus>,
    pub priority: Option<IssuePriority>,
    /// Filter by label substring (case-insensitive).
    pub label: Option<String>,
    /// Filter by creator (human or agent ID).
    pub creator: Option<String>,
}

// ── IssueStore ────────────────────────────────────────────────────────────────

/// SQLite-backed storage for issues.
///
/// One store per repository; the database file lives at `.vai/issues.db`.
pub struct IssueStore {
    conn: Connection,
}

impl IssueStore {
    /// Open (or create) the issue store at `<vai_dir>/issues.db`.
    pub fn open(vai_dir: &Path) -> Result<Self, IssueError> {
        let db_path = vai_dir.join("issues.db");
        let conn = Connection::open(&db_path)?;
        let store = Self { conn };
        store.init_schema()?;
        Ok(store)
    }

    /// Initialize the database schema (idempotent).
    fn init_schema(&self) -> Result<(), IssueError> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS issues (
                id          TEXT PRIMARY KEY,
                title       TEXT NOT NULL,
                description TEXT NOT NULL DEFAULT '',
                status      TEXT NOT NULL DEFAULT 'open',
                priority    TEXT NOT NULL DEFAULT 'medium',
                labels      TEXT NOT NULL DEFAULT '',
                creator     TEXT NOT NULL,
                resolution  TEXT,
                created_at  TEXT NOT NULL,
                updated_at  TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_issues_status   ON issues(status);
            CREATE INDEX IF NOT EXISTS idx_issues_priority ON issues(priority);
            CREATE INDEX IF NOT EXISTS idx_issues_creator  ON issues(creator);

            CREATE TABLE IF NOT EXISTS issue_workspace_links (
                issue_id     TEXT NOT NULL,
                workspace_id TEXT NOT NULL,
                linked_at    TEXT NOT NULL,
                PRIMARY KEY (issue_id, workspace_id)
            );",
        )?;
        Ok(())
    }

    // ── Create ────────────────────────────────────────────────────────────────

    /// Create a new issue and record an `IssueCreated` event.
    pub fn create(
        &self,
        title: impl Into<String>,
        description: impl Into<String>,
        priority: IssuePriority,
        labels: Vec<String>,
        creator: impl Into<String>,
        event_log: &mut EventLog,
    ) -> Result<Issue, IssueError> {
        let id = Uuid::new_v4();
        let now = Utc::now();
        let title = title.into();
        let description = description.into();
        let creator = creator.into();
        let labels_str = labels.join(",");

        self.conn.execute(
            "INSERT INTO issues (id, title, description, status, priority, labels, creator, created_at, updated_at)
             VALUES (?1, ?2, ?3, 'open', ?4, ?5, ?6, ?7, ?8)",
            params![
                id.to_string(),
                &title,
                &description,
                priority.as_str(),
                &labels_str,
                &creator,
                now.to_rfc3339(),
                now.to_rfc3339(),
            ],
        )?;

        event_log.append(EventKind::IssueCreated {
            issue_id: id,
            title: title.clone(),
            creator: creator.clone(),
            priority: priority.as_str().to_string(),
        })?;

        Ok(Issue {
            id,
            title,
            description,
            status: IssueStatus::Open,
            priority,
            labels,
            creator,
            resolution: None,
            created_at: now,
            updated_at: now,
        })
    }

    // ── Read ──────────────────────────────────────────────────────────────────

    /// Fetch a single issue by ID.
    pub fn get(&self, id: Uuid) -> Result<Issue, IssueError> {
        let result = self.conn.query_row(
            "SELECT id, title, description, status, priority, labels, creator, resolution, created_at, updated_at
             FROM issues WHERE id = ?1",
            params![id.to_string()],
            row_to_issue,
        );
        match result {
            Ok(issue) => Ok(issue),
            Err(rusqlite::Error::QueryReturnedNoRows) => Err(IssueError::NotFound(id)),
            Err(e) => Err(IssueError::Sqlite(e)),
        }
    }

    /// List issues with optional filters.
    pub fn list(&self, filter: &IssueFilter) -> Result<Vec<Issue>, IssueError> {
        let mut conditions: Vec<String> = Vec::new();
        let mut values: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        if let Some(status) = &filter.status {
            conditions.push(format!("status = ?{}", conditions.len() + 1));
            values.push(Box::new(status.as_str().to_string()));
        }
        if let Some(priority) = &filter.priority {
            conditions.push(format!("priority = ?{}", conditions.len() + 1));
            values.push(Box::new(priority.as_str().to_string()));
        }
        if let Some(label) = &filter.label {
            conditions.push(format!(
                "(labels = ?{n} OR labels LIKE ?{n2} OR labels LIKE ?{n3} OR labels LIKE ?{n4})",
                n = conditions.len() + 1,
                n2 = conditions.len() + 2,
                n3 = conditions.len() + 3,
                n4 = conditions.len() + 4,
            ));
            values.push(Box::new(label.clone()));
            values.push(Box::new(format!("{},%", label)));
            values.push(Box::new(format!("%,{}", label)));
            values.push(Box::new(format!("%,{},%", label)));
        }
        if let Some(creator) = &filter.creator {
            conditions.push(format!("creator = ?{}", conditions.len() + 1));
            values.push(Box::new(creator.clone()));
        }

        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conditions.join(" AND "))
        };

        let sql = format!(
            "SELECT id, title, description, status, priority, labels, creator, resolution, created_at, updated_at
             FROM issues {} ORDER BY created_at DESC",
            where_clause
        );

        let mut stmt = self.conn.prepare(&sql)?;
        let refs: Vec<&dyn rusqlite::ToSql> = values.iter().map(|b| b.as_ref()).collect();
        let rows = stmt.query_map(refs.as_slice(), row_to_issue)?;

        let mut issues = Vec::new();
        for row in rows {
            issues.push(row?);
        }
        Ok(issues)
    }

    // ── Update ────────────────────────────────────────────────────────────────

    /// Update mutable fields of an issue and record an `IssueUpdated` event.
    ///
    /// Pass `None` for any field to leave it unchanged.
    pub fn update(
        &self,
        id: Uuid,
        title: Option<String>,
        description: Option<String>,
        priority: Option<IssuePriority>,
        labels: Option<Vec<String>>,
        event_log: &mut EventLog,
    ) -> Result<Issue, IssueError> {
        // Verify issue exists first.
        let existing = self.get(id)?;

        let mut changed: Vec<String> = Vec::new();
        let now = Utc::now();

        let new_title = title.unwrap_or_else(|| existing.title.clone());
        let new_description = description.unwrap_or_else(|| existing.description.clone());
        let new_priority = priority.unwrap_or_else(|| existing.priority.clone());
        let new_labels = labels.unwrap_or_else(|| existing.labels.clone());

        if new_title != existing.title { changed.push("title".into()); }
        if new_description != existing.description { changed.push("description".into()); }
        if new_priority != existing.priority { changed.push("priority".into()); }
        if new_labels != existing.labels { changed.push("labels".into()); }

        self.conn.execute(
            "UPDATE issues SET title=?2, description=?3, priority=?4, labels=?5, updated_at=?6 WHERE id=?1",
            params![
                id.to_string(),
                &new_title,
                &new_description,
                new_priority.as_str(),
                new_labels.join(","),
                now.to_rfc3339(),
            ],
        )?;

        if !changed.is_empty() {
            event_log.append(EventKind::IssueUpdated {
                issue_id: id,
                fields_changed: changed,
            })?;
        }

        self.get(id)
    }

    // ── State transitions ─────────────────────────────────────────────────────

    /// Transition an issue to `InProgress` (called when linked to a workspace).
    pub fn set_in_progress(
        &self,
        id: Uuid,
        workspace_id: Uuid,
        event_log: &mut EventLog,
    ) -> Result<Issue, IssueError> {
        let existing = self.get(id)?;
        if existing.status != IssueStatus::Open && existing.status != IssueStatus::InProgress {
            return Err(IssueError::InvalidTransition {
                from: existing.status.as_str().to_string(),
                to: "in_progress".into(),
            });
        }
        let now = Utc::now();
        self.conn.execute(
            "UPDATE issues SET status='in_progress', updated_at=?2 WHERE id=?1",
            params![id.to_string(), now.to_rfc3339()],
        )?;
        // Record workspace link.
        self.conn.execute(
            "INSERT OR IGNORE INTO issue_workspace_links (issue_id, workspace_id, linked_at) VALUES (?1, ?2, ?3)",
            params![id.to_string(), workspace_id.to_string(), now.to_rfc3339()],
        )?;
        event_log.append(EventKind::IssueLinkedToWorkspace {
            issue_id: id,
            workspace_id,
        })?;
        self.get(id)
    }

    /// Transition an issue back to `Open` (called when linked workspace is discarded).
    pub fn reopen(
        &self,
        id: Uuid,
        event_log: &mut EventLog,
    ) -> Result<Issue, IssueError> {
        let existing = self.get(id)?;
        if existing.status != IssueStatus::InProgress {
            return Err(IssueError::InvalidTransition {
                from: existing.status.as_str().to_string(),
                to: "open".into(),
            });
        }
        let now = Utc::now();
        self.conn.execute(
            "UPDATE issues SET status='open', updated_at=?2 WHERE id=?1",
            params![id.to_string(), now.to_rfc3339()],
        )?;
        event_log.append(EventKind::IssueUpdated {
            issue_id: id,
            fields_changed: vec!["status".into()],
        })?;
        self.get(id)
    }

    /// Transition an issue to `Resolved` (called when linked workspace merges).
    pub fn resolve(
        &self,
        id: Uuid,
        version_id: Option<String>,
        event_log: &mut EventLog,
    ) -> Result<Issue, IssueError> {
        let existing = self.get(id)?;
        if existing.status != IssueStatus::InProgress && existing.status != IssueStatus::Open {
            return Err(IssueError::InvalidTransition {
                from: existing.status.as_str().to_string(),
                to: "resolved".into(),
            });
        }
        let now = Utc::now();
        self.conn.execute(
            "UPDATE issues SET status='resolved', resolution='resolved', updated_at=?2 WHERE id=?1",
            params![id.to_string(), now.to_rfc3339()],
        )?;
        event_log.append(EventKind::IssueResolved {
            issue_id: id,
            resolution: "resolved".into(),
            version_id,
        })?;
        self.get(id)
    }

    /// Close an issue with a resolution (can come from any state).
    pub fn close(
        &self,
        id: Uuid,
        resolution: IssueResolution,
        event_log: &mut EventLog,
    ) -> Result<Issue, IssueError> {
        // Verify it exists (returns NotFound if not).
        self.get(id)?;
        let now = Utc::now();
        let res_str = resolution.as_str();
        self.conn.execute(
            "UPDATE issues SET status='closed', resolution=?2, updated_at=?3 WHERE id=?1",
            params![id.to_string(), res_str, now.to_rfc3339()],
        )?;
        event_log.append(EventKind::IssueClosed {
            issue_id: id,
            resolution: res_str.to_string(),
        })?;
        self.get(id)
    }

    /// List workspaces linked to an issue.
    pub fn linked_workspaces(&self, id: Uuid) -> Result<Vec<Uuid>, IssueError> {
        let mut stmt = self.conn.prepare(
            "SELECT workspace_id FROM issue_workspace_links WHERE issue_id = ?1 ORDER BY linked_at",
        )?;
        let rows = stmt.query_map(params![id.to_string()], |row| {
            let s: String = row.get(0)?;
            Ok(s)
        })?;
        let mut ids = Vec::new();
        for row in rows {
            let s = row?;
            if let Ok(wid) = Uuid::parse_str(&s) {
                ids.push(wid);
            }
        }
        Ok(ids)
    }
}

// ── Row mapping helper ────────────────────────────────────────────────────────

fn row_to_issue(row: &rusqlite::Row<'_>) -> rusqlite::Result<Issue> {
    let id_str: String = row.get(0)?;
    let title: String = row.get(1)?;
    let description: String = row.get(2)?;
    let status_str: String = row.get(3)?;
    let priority_str: String = row.get(4)?;
    let labels_str: String = row.get(5)?;
    let creator: String = row.get(6)?;
    let resolution: Option<String> = row.get(7)?;
    let created_str: String = row.get(8)?;
    let updated_str: String = row.get(9)?;

    let id = Uuid::parse_str(&id_str).unwrap_or_else(|_| Uuid::nil());
    let status = IssueStatus::from_str(&status_str).unwrap_or(IssueStatus::Open);
    let priority = IssuePriority::from_str(&priority_str).unwrap_or(IssuePriority::Medium);
    let labels: Vec<String> = if labels_str.is_empty() {
        Vec::new()
    } else {
        labels_str.split(',').map(|s| s.to_string()).collect()
    };
    let created_at = DateTime::parse_from_rfc3339(&created_str)
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now());
    let updated_at = DateTime::parse_from_rfc3339(&updated_str)
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now());

    Ok(Issue {
        id,
        title,
        description,
        status,
        priority,
        labels,
        creator,
        resolution,
        created_at,
        updated_at,
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn setup() -> (TempDir, IssueStore, EventLog) {
        let tmp = TempDir::new().unwrap();
        let vai_dir = tmp.path().join(".vai");
        std::fs::create_dir_all(vai_dir.join("event_log")).unwrap();
        let store = IssueStore::open(&vai_dir).unwrap();
        let event_log = EventLog::open(&vai_dir).unwrap();
        (tmp, store, event_log)
    }

    #[test]
    fn test_create_and_get() {
        let (_tmp, store, mut log) = setup();
        let issue = store
            .create("Fix login bug", "Details here", IssuePriority::High, vec!["bug".into()], "alice", &mut log)
            .unwrap();
        assert_eq!(issue.title, "Fix login bug");
        assert_eq!(issue.status, IssueStatus::Open);
        assert_eq!(issue.priority, IssuePriority::High);
        assert_eq!(issue.labels, vec!["bug".to_string()]);

        let fetched = store.get(issue.id).unwrap();
        assert_eq!(fetched.id, issue.id);
        assert_eq!(fetched.title, "Fix login bug");
    }

    #[test]
    fn test_list_and_filter() {
        let (_tmp, store, mut log) = setup();
        store.create("Issue A", "", IssuePriority::High, vec![], "alice", &mut log).unwrap();
        store.create("Issue B", "", IssuePriority::Low, vec!["bug".into()], "bob", &mut log).unwrap();

        let all = store.list(&IssueFilter::default()).unwrap();
        assert_eq!(all.len(), 2);

        let high = store.list(&IssueFilter { priority: Some(IssuePriority::High), ..Default::default() }).unwrap();
        assert_eq!(high.len(), 1);
        assert_eq!(high[0].title, "Issue A");

        let by_creator = store.list(&IssueFilter { creator: Some("bob".into()), ..Default::default() }).unwrap();
        assert_eq!(by_creator.len(), 1);
        assert_eq!(by_creator[0].title, "Issue B");

        let by_label = store.list(&IssueFilter { label: Some("bug".into()), ..Default::default() }).unwrap();
        assert_eq!(by_label.len(), 1);
    }

    #[test]
    fn test_update() {
        let (_tmp, store, mut log) = setup();
        let issue = store
            .create("Old title", "", IssuePriority::Low, vec![], "alice", &mut log)
            .unwrap();

        let updated = store
            .update(issue.id, Some("New title".into()), None, Some(IssuePriority::High), None, &mut log)
            .unwrap();
        assert_eq!(updated.title, "New title");
        assert_eq!(updated.priority, IssuePriority::High);
    }

    #[test]
    fn test_state_transitions() {
        let (_tmp, store, mut log) = setup();
        let issue = store
            .create("A task", "", IssuePriority::Medium, vec![], "alice", &mut log)
            .unwrap();
        let ws_id = Uuid::new_v4();

        let in_progress = store.set_in_progress(issue.id, ws_id, &mut log).unwrap();
        assert_eq!(in_progress.status, IssueStatus::InProgress);

        let resolved = store.resolve(issue.id, Some("v5".into()), &mut log).unwrap();
        assert_eq!(resolved.status, IssueStatus::Resolved);
    }

    #[test]
    fn test_close() {
        let (_tmp, store, mut log) = setup();
        let issue = store
            .create("Won't do", "", IssuePriority::Low, vec![], "alice", &mut log)
            .unwrap();

        let closed = store.close(issue.id, IssueResolution::WontFix, &mut log).unwrap();
        assert_eq!(closed.status, IssueStatus::Closed);
        assert_eq!(closed.resolution.as_deref(), Some("wontfix"));
    }

    #[test]
    fn test_not_found() {
        let (_tmp, store, _log) = setup();
        let result = store.get(Uuid::new_v4());
        assert!(matches!(result, Err(IssueError::NotFound(_))));
    }

    #[test]
    fn test_reopen_on_workspace_discard() {
        let (_tmp, store, mut log) = setup();
        let issue = store
            .create("Retry task", "", IssuePriority::Medium, vec![], "alice", &mut log)
            .unwrap();
        let ws_id = Uuid::new_v4();

        store.set_in_progress(issue.id, ws_id, &mut log).unwrap();
        let reopened = store.reopen(issue.id, &mut log).unwrap();
        assert_eq!(reopened.status, IssueStatus::Open);
    }
}
