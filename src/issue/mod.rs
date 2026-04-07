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

use std::collections::HashSet;
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

    #[error("Rate limit exceeded: agent {agent_id} has created {count} issues this hour (max {max})")]
    RateLimitExceeded { agent_id: String, count: u32, max: u32 },
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
    pub fn from_db_str(s: &str) -> Option<Self> {
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
    pub fn from_db_str(s: &str) -> Option<Self> {
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
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "resolved" => Some(IssueResolution::Resolved),
            "wontfix" => Some(IssueResolution::WontFix),
            "duplicate" => Some(IssueResolution::Duplicate),
            _ => None,
        }
    }
}

// ── Issue comment ─────────────────────────────────────────────────────────────

/// A comment on an issue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueComment {
    /// Unique comment identifier.
    pub id: Uuid,
    /// The issue this comment belongs to.
    pub issue_id: Uuid,
    /// Author username or agent ID.
    pub author: String,
    /// Comment body (Markdown supported). `None` when soft-deleted.
    pub body: Option<String>,
    /// Whether the author is a `"human"` or `"agent"`.
    pub author_type: String,
    /// Optional structured author identifier (e.g. agent instance ID).
    pub author_id: Option<String>,
    /// When the comment was created.
    pub created_at: DateTime<Utc>,
    /// Parent comment UUID for threaded replies.
    pub parent_id: Option<Uuid>,
    /// When the comment was last edited, if ever.
    pub edited_at: Option<DateTime<Utc>>,
    /// When the comment was soft-deleted, if ever.
    pub deleted_at: Option<DateTime<Utc>>,
}

// ── Issue attachment ──────────────────────────────────────────────────────────

/// A file attached to an issue.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
pub struct IssueAttachment {
    /// Unique attachment identifier.
    pub id: Uuid,
    /// The issue this attachment belongs to.
    pub issue_id: Uuid,
    /// Original filename as uploaded.
    pub filename: String,
    /// MIME content type (e.g. `"image/png"`, `"text/plain"`).
    pub content_type: String,
    /// File size in bytes.
    pub size_bytes: i64,
    /// Storage key used to retrieve content from S3.
    pub s3_key: String,
    /// Username or agent ID that uploaded the file.
    pub uploaded_by: String,
    /// When the attachment was uploaded.
    pub created_at: DateTime<Utc>,
}

// ── Agent source metadata ─────────────────────────────────────────────────────

/// Source metadata attached to issues created by an agent.
///
/// Describes how and why the agent discovered the issue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSource {
    /// Discovery type, e.g. `"test_failure"`, `"security_vulnerability"`,
    /// `"code_quality"`, `"performance_regression"`, `"dependency_update"`.
    pub source_type: String,
    /// Arbitrary key/value details about the discovery (test suite name,
    /// severity, affected entity, etc.).
    #[serde(default)]
    pub details: serde_json::Value,
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
    /// Source metadata if this issue was created by an agent; `None` for
    /// human-created issues.
    pub agent_source: Option<AgentSource>,
    /// Testable conditions that define when the issue is considered complete.
    ///
    /// The work queue prefers issues with non-empty acceptance criteria because
    /// they give agents a concrete definition of done.
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
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
                id           TEXT PRIMARY KEY,
                title        TEXT NOT NULL,
                description  TEXT NOT NULL DEFAULT '',
                status       TEXT NOT NULL DEFAULT 'open',
                priority     TEXT NOT NULL DEFAULT 'medium',
                labels       TEXT NOT NULL DEFAULT '',
                creator      TEXT NOT NULL,
                resolution   TEXT,
                agent_source TEXT,
                created_at   TEXT NOT NULL,
                updated_at   TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_issues_status   ON issues(status);
            CREATE INDEX IF NOT EXISTS idx_issues_priority ON issues(priority);
            CREATE INDEX IF NOT EXISTS idx_issues_creator  ON issues(creator);

            CREATE TABLE IF NOT EXISTS issue_workspace_links (
                issue_id     TEXT NOT NULL,
                workspace_id TEXT NOT NULL,
                linked_at    TEXT NOT NULL,
                PRIMARY KEY (issue_id, workspace_id)
            );

            CREATE TABLE IF NOT EXISTS agent_rate_limits (
                agent_id    TEXT NOT NULL,
                hour_bucket TEXT NOT NULL,
                count       INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (agent_id, hour_bucket)
            );

            CREATE TABLE IF NOT EXISTS issue_dependencies (
                issue_id      TEXT NOT NULL,
                depends_on_id TEXT NOT NULL,
                PRIMARY KEY (issue_id, depends_on_id)
            );

            CREATE TABLE IF NOT EXISTS issue_comments (
                id         TEXT NOT NULL PRIMARY KEY,
                issue_id   TEXT NOT NULL,
                author     TEXT NOT NULL,
                body       TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_issue_comments_issue_id ON issue_comments (issue_id);",
        )?;
        // Migrate existing databases that lack the agent_source column.
        let _ = self.conn.execute(
            "ALTER TABLE issues ADD COLUMN agent_source TEXT",
            [],
        );
        // Migrate existing databases that lack the acceptance_criteria column.
        let _ = self.conn.execute(
            "ALTER TABLE issues ADD COLUMN acceptance_criteria TEXT NOT NULL DEFAULT '[]'",
            [],
        );
        // Migrate existing databases that lack author_type / author_id on comments.
        let _ = self.conn.execute(
            "ALTER TABLE issue_comments ADD COLUMN author_type TEXT NOT NULL DEFAULT 'human'",
            [],
        );
        let _ = self.conn.execute(
            "ALTER TABLE issue_comments ADD COLUMN author_id TEXT",
            [],
        );
        // Migrate existing databases that lack threading / soft-delete columns.
        let _ = self.conn.execute(
            "ALTER TABLE issue_comments ADD COLUMN parent_id TEXT",
            [],
        );
        let _ = self.conn.execute(
            "ALTER TABLE issue_comments ADD COLUMN edited_at TEXT",
            [],
        );
        let _ = self.conn.execute(
            "ALTER TABLE issue_comments ADD COLUMN deleted_at TEXT",
            [],
        );
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
            "INSERT INTO issues (id, title, description, status, priority, labels, creator, agent_source, created_at, updated_at)
             VALUES (?1, ?2, ?3, 'open', ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                id.to_string(),
                &title,
                &description,
                priority.as_str(),
                &labels_str,
                &creator,
                Option::<String>::None,
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
            agent_source: None,
            acceptance_criteria: Vec::new(),
            created_at: now,
            updated_at: now,
        })
    }

    // ── Agent-initiated issue creation ────────────────────────────────────────

    /// Create an issue on behalf of an agent, with rate-limiting and duplicate detection.
    ///
    /// Returns `(issue, possible_duplicate_id)`.  If a similar open issue
    /// already exists the second element is `Some(id)` — the issue is still
    /// created, but callers should surface the warning.
    ///
    /// Returns [`IssueError::RateLimitExceeded`] when `agent_id` has already
    /// created `max_per_hour` issues within the current clock hour.
    #[allow(clippy::too_many_arguments)]
    pub fn create_by_agent(
        &self,
        title: impl Into<String>,
        description: impl Into<String>,
        priority: IssuePriority,
        labels: Vec<String>,
        agent_id: impl Into<String>,
        source: AgentSource,
        max_per_hour: u32,
        event_log: &mut EventLog,
    ) -> Result<(Issue, Option<Uuid>), IssueError> {
        let agent_id = agent_id.into();
        let title = title.into();

        // ── Rate limit check ──────────────────────────────────────────────────
        let count = self.increment_agent_rate_limit(&agent_id)?;
        if count > max_per_hour {
            // Decrement back so the over-limit request doesn't count.
            let bucket = current_hour_bucket();
            let _ = self.conn.execute(
                "UPDATE agent_rate_limits SET count = count - 1 WHERE agent_id = ?1 AND hour_bucket = ?2",
                params![&agent_id, &bucket],
            );
            return Err(IssueError::RateLimitExceeded {
                agent_id,
                count,
                max: max_per_hour,
            });
        }

        // ── Duplicate detection ───────────────────────────────────────────────
        let possible_duplicate = self.find_similar_open_issue(&title)?;

        // ── Create the issue ──────────────────────────────────────────────────
        let description = description.into();
        let id = Uuid::new_v4();
        let now = Utc::now();
        let labels_str = labels.join(",");
        let source_json = serde_json::to_string(&source).unwrap_or_default();

        self.conn.execute(
            "INSERT INTO issues (id, title, description, status, priority, labels, creator, agent_source, created_at, updated_at)
             VALUES (?1, ?2, ?3, 'open', ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                id.to_string(),
                &title,
                &description,
                priority.as_str(),
                &labels_str,
                &agent_id,
                &source_json,
                now.to_rfc3339(),
                now.to_rfc3339(),
            ],
        )?;

        event_log.append(EventKind::IssueCreated {
            issue_id: id,
            title: title.clone(),
            creator: agent_id.clone(),
            priority: priority.as_str().to_string(),
        })?;

        let issue = Issue {
            id,
            title,
            description,
            status: IssueStatus::Open,
            priority,
            labels,
            creator: agent_id,
            resolution: None,
            agent_source: Some(source),
            acceptance_criteria: Vec::new(),
            created_at: now,
            updated_at: now,
        };
        Ok((issue, possible_duplicate))
    }

    /// Increment the per-agent, per-hour issue creation counter.
    ///
    /// Returns the new count after incrementing.
    fn increment_agent_rate_limit(&self, agent_id: &str) -> Result<u32, IssueError> {
        let bucket = current_hour_bucket();
        self.conn.execute(
            "INSERT INTO agent_rate_limits (agent_id, hour_bucket, count)
             VALUES (?1, ?2, 1)
             ON CONFLICT(agent_id, hour_bucket) DO UPDATE SET count = count + 1",
            params![agent_id, &bucket],
        )?;
        let count: u32 = self.conn.query_row(
            "SELECT count FROM agent_rate_limits WHERE agent_id = ?1 AND hour_bucket = ?2",
            params![agent_id, &bucket],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    /// Find an open issue whose title is similar to `title` (Jaccard word overlap ≥ 0.5).
    ///
    /// Returns the ID of the first match found, or `None`.
    fn find_similar_open_issue(&self, title: &str) -> Result<Option<Uuid>, IssueError> {
        let candidates: Vec<(Uuid, String)> = {
            let mut stmt = self.conn.prepare(
                "SELECT id, title FROM issues WHERE status IN ('open', 'in_progress')",
            )?;
            let rows = stmt.query_map([], |row| {
                let id_str: String = row.get(0)?;
                let t: String = row.get(1)?;
                Ok((id_str, t))
            })?;
            let mut out = Vec::new();
            for row in rows {
                let (id_str, t) = row?;
                if let Ok(id) = Uuid::parse_str(&id_str) {
                    out.push((id, t));
                }
            }
            out
        };

        let query_tokens = tokenize_title(title);
        if query_tokens.is_empty() {
            return Ok(None);
        }

        for (id, candidate_title) in candidates {
            let candidate_tokens = tokenize_title(&candidate_title);
            if jaccard_similarity(&query_tokens, &candidate_tokens) >= 0.5 {
                return Ok(Some(id));
            }
        }
        Ok(None)
    }

    /// Return the number of issues an agent has created in the current hour.
    pub fn agent_issue_count_this_hour(&self, agent_id: &str) -> Result<u32, IssueError> {
        let bucket = current_hour_bucket();
        let count: u32 = self.conn.query_row(
            "SELECT COALESCE(count, 0) FROM agent_rate_limits WHERE agent_id = ?1 AND hour_bucket = ?2",
            params![agent_id, &bucket],
            |row| row.get(0),
        ).unwrap_or(0);
        Ok(count)
    }

    /// Count the total number of open issues (status = `open`).
    /// Update the `agent_source` JSON column for an existing issue.
    ///
    /// Used by storage trait implementations that create issues via [`Self::create`]
    /// and then need to attach agent discovery metadata in a separate step.
    pub fn set_agent_source(
        &self,
        id: Uuid,
        source_json: &str,
    ) -> Result<(), IssueError> {
        self.conn.execute(
            "UPDATE issues SET agent_source = ?1 WHERE id = ?2",
            params![source_json, id.to_string()],
        )?;
        Ok(())
    }

    /// Set the acceptance criteria for an existing issue.
    ///
    /// Stores the criteria as a JSON array in the `acceptance_criteria` column.
    /// Used by storage trait implementations to attach criteria after `create()`.
    pub fn set_acceptance_criteria(
        &self,
        id: Uuid,
        criteria: &[String],
    ) -> Result<(), IssueError> {
        let json = serde_json::to_string(criteria).unwrap_or_else(|_| "[]".to_string());
        self.conn.execute(
            "UPDATE issues SET acceptance_criteria = ?1 WHERE id = ?2",
            params![json, id.to_string()],
        )?;
        Ok(())
    }

    pub fn count_open(&self) -> Result<usize, IssueError> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM issues WHERE status = 'open'",
            [],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    // ── Read ──────────────────────────────────────────────────────────────────

    /// Fetch a single issue by ID.
    pub fn get(&self, id: Uuid) -> Result<Issue, IssueError> {
        let result = self.conn.query_row(
            "SELECT id, title, description, status, priority, labels, creator, resolution, agent_source, created_at, updated_at, acceptance_criteria
             FROM issues WHERE id = ?1",
            params![id.to_string()],
            row_to_issue,
        );
        match result {
            Ok(issue) => {
                Ok(issue)
            }
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
            "SELECT id, title, description, status, priority, labels, creator, resolution, agent_source, created_at, updated_at, acceptance_criteria
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

    /// Close an issue with a resolution string (can come from any state).
    ///
    /// `resolution` can be any string; common values are `"resolved"`, `"wontfix"`, and
    /// `"duplicate"`, but free-form text (e.g. `"resolved in v5"`) is also accepted.
    pub fn close(
        &self,
        id: Uuid,
        resolution: &str,
        event_log: &mut EventLog,
    ) -> Result<Issue, IssueError> {
        // Verify it exists (returns NotFound if not).
        self.get(id)?;
        let now = Utc::now();
        self.conn.execute(
            "UPDATE issues SET status='closed', resolution=?2, updated_at=?3 WHERE id=?1",
            params![id.to_string(), resolution, now.to_rfc3339()],
        )?;
        event_log.append(EventKind::IssueClosed {
            issue_id: id,
            resolution: resolution.to_string(),
        })?;
        self.get(id)
    }

    /// Create a comment on an issue.
    pub fn create_comment(
        &self,
        issue_id: Uuid,
        author: &str,
        body: &str,
        author_type: &str,
        author_id: Option<&str>,
        parent_id: Option<Uuid>,
    ) -> Result<IssueComment, IssueError> {
        let id = Uuid::new_v4();
        let now = Utc::now();
        self.conn.execute(
            "INSERT INTO issue_comments (id, issue_id, author, body, created_at, author_type, author_id, parent_id) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                id.to_string(),
                issue_id.to_string(),
                author,
                body,
                now.to_rfc3339(),
                author_type,
                author_id,
                parent_id.map(|p| p.to_string()),
            ],
        )?;
        Ok(IssueComment {
            id,
            issue_id,
            author: author.to_string(),
            body: Some(body.to_string()),
            author_type: author_type.to_string(),
            author_id: author_id.map(|s| s.to_string()),
            created_at: now,
            parent_id,
            edited_at: None,
            deleted_at: None,
        })
    }

    /// List all comments for an issue, ordered by `created_at` ascending.
    pub fn list_comments(&self, issue_id: Uuid) -> Result<Vec<IssueComment>, IssueError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, issue_id, author, body, created_at, author_type, author_id, parent_id, edited_at, deleted_at \
             FROM issue_comments WHERE issue_id = ?1 ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map(params![issue_id.to_string()], |row| {
            let id_str: String = row.get(0)?;
            let iid_str: String = row.get(1)?;
            let author: String = row.get(2)?;
            let body: Option<String> = row.get(3)?;
            let ts: String = row.get(4)?;
            let author_type: String = row.get(5)?;
            let author_id: Option<String> = row.get(6)?;
            let parent_id_str: Option<String> = row.get(7)?;
            let edited_at_str: Option<String> = row.get(8)?;
            let deleted_at_str: Option<String> = row.get(9)?;
            Ok((id_str, iid_str, author, body, ts, author_type, author_id, parent_id_str, edited_at_str, deleted_at_str))
        })?;

        let mut comments = Vec::new();
        for row in rows {
            let (id_str, iid_str, author, body, ts, author_type, author_id, parent_id_str, edited_at_str, deleted_at_str) = row?;
            let id = Uuid::parse_str(&id_str)
                .map_err(|_| IssueError::Sqlite(rusqlite::Error::InvalidColumnType(0, "id".into(), rusqlite::types::Type::Text)))?;
            let iid = Uuid::parse_str(&iid_str)
                .map_err(|_| IssueError::Sqlite(rusqlite::Error::InvalidColumnType(1, "issue_id".into(), rusqlite::types::Type::Text)))?;
            let created_at = chrono::DateTime::parse_from_rfc3339(&ts)
                .map(|dt| dt.with_timezone(&Utc))
                .map_err(|_| IssueError::Sqlite(rusqlite::Error::InvalidColumnType(4, "created_at".into(), rusqlite::types::Type::Text)))?;
            let parent_id = parent_id_str.as_deref().and_then(|s| Uuid::parse_str(s).ok());
            let edited_at = edited_at_str.as_deref()
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.with_timezone(&Utc));
            let deleted_at = deleted_at_str.as_deref()
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.with_timezone(&Utc));
            comments.push(IssueComment { id, issue_id: iid, author, body, author_type, author_id, created_at, parent_id, edited_at, deleted_at });
        }
        Ok(comments)
    }

    /// Update a comment body and set `edited_at` to now.
    pub fn update_comment(&self, comment_id: Uuid, new_body: &str) -> Result<IssueComment, IssueError> {
        let now = Utc::now();
        let rows_changed = self.conn.execute(
            "UPDATE issue_comments SET body = ?1, edited_at = ?2 WHERE id = ?3",
            params![new_body, now.to_rfc3339(), comment_id.to_string()],
        )?;
        if rows_changed == 0 {
            return Err(IssueError::NotFound(comment_id));
        }
        let mut stmt = self.conn.prepare(
            "SELECT id, issue_id, author, body, created_at, author_type, author_id, parent_id, edited_at, deleted_at \
             FROM issue_comments WHERE id = ?1",
        )?;
        let row = stmt.query_row(params![comment_id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, Option<String>>(9)?,
            ))
        })?;
        Self::row_to_comment(row)
    }

    /// Soft-delete a comment by setting `deleted_at` to now.
    pub fn soft_delete_comment(&self, comment_id: Uuid) -> Result<IssueComment, IssueError> {
        let now = Utc::now();
        let rows_changed = self.conn.execute(
            "UPDATE issue_comments SET deleted_at = ?1 WHERE id = ?2",
            params![now.to_rfc3339(), comment_id.to_string()],
        )?;
        if rows_changed == 0 {
            return Err(IssueError::NotFound(comment_id));
        }
        let mut stmt = self.conn.prepare(
            "SELECT id, issue_id, author, body, created_at, author_type, author_id, parent_id, edited_at, deleted_at \
             FROM issue_comments WHERE id = ?1",
        )?;
        let row = stmt.query_row(params![comment_id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, Option<String>>(9)?,
            ))
        })?;
        Self::row_to_comment(row)
    }

    /// Parse a comment from a row tuple.
    #[allow(clippy::type_complexity)]
    fn row_to_comment(
        row: (String, String, String, Option<String>, String, String, Option<String>, Option<String>, Option<String>, Option<String>),
    ) -> Result<IssueComment, IssueError> {
        let (id_str, iid_str, author, body, ts, author_type, author_id, parent_id_str, edited_at_str, deleted_at_str) = row;
        let id = Uuid::parse_str(&id_str)
            .map_err(|_| IssueError::Sqlite(rusqlite::Error::InvalidColumnType(0, "id".into(), rusqlite::types::Type::Text)))?;
        let issue_id = Uuid::parse_str(&iid_str)
            .map_err(|_| IssueError::Sqlite(rusqlite::Error::InvalidColumnType(1, "issue_id".into(), rusqlite::types::Type::Text)))?;
        let created_at = chrono::DateTime::parse_from_rfc3339(&ts)
            .map(|dt| dt.with_timezone(&Utc))
            .map_err(|_| IssueError::Sqlite(rusqlite::Error::InvalidColumnType(4, "created_at".into(), rusqlite::types::Type::Text)))?;
        let parent_id = parent_id_str.as_deref().and_then(|s| Uuid::parse_str(s).ok());
        let edited_at = edited_at_str.as_deref()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc));
        let deleted_at = deleted_at_str.as_deref()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc));
        Ok(IssueComment { id, issue_id, author, body, author_type, author_id, created_at, parent_id, edited_at, deleted_at })
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

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Return the current UTC hour as a bucket string, e.g. `"2026-03-21T10"`.
fn current_hour_bucket() -> String {
    let now = Utc::now();
    now.format("%Y-%m-%dT%H").to_string()
}

/// Stop words excluded from title tokenization.
const STOP_WORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "but", "by", "for", "from",
    "has", "have", "in", "is", "it", "its", "of", "on", "or", "that", "the",
    "this", "to", "was", "will", "with",
];

/// Tokenize a title into a set of lowercase alphabetic words, excluding stop words.
/// Find an open issue in `issues` whose title is similar to `title`
/// (Jaccard word overlap ≥ 0.5).
///
/// Returns the ID of the first match found, or `None`.  Used by server
/// handlers that have fetched all open issues via the storage trait rather
/// than through the SQLite-specific `IssueStore` method.
pub fn find_similar_open_issue(issues: &[Issue], title: &str) -> Option<Uuid> {
    let query_tokens = tokenize_title(title);
    if query_tokens.is_empty() {
        return None;
    }
    for issue in issues {
        if issue.status != IssueStatus::Open && issue.status != IssueStatus::InProgress {
            continue;
        }
        let candidate_tokens = tokenize_title(&issue.title);
        if jaccard_similarity(&query_tokens, &candidate_tokens) >= 0.5 {
            return Some(issue.id);
        }
    }
    None
}

fn tokenize_title(title: &str) -> HashSet<String> {
    title
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .filter(|w| !STOP_WORDS.contains(&w.as_str()))
        .collect()
}

/// Compute Jaccard similarity between two token sets.
fn jaccard_similarity(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let intersection = a.intersection(b).count() as f64;
    let union = a.union(b).count() as f64;
    if union == 0.0 { 0.0 } else { intersection / union }
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
    let agent_source_json: Option<String> = row.get(8)?;
    let created_str: String = row.get(9)?;
    let updated_str: String = row.get(10)?;
    let acceptance_criteria_json: Option<String> = row.get(11).unwrap_or(None);

    let id = Uuid::parse_str(&id_str).unwrap_or_else(|_| Uuid::nil());
    let status = IssueStatus::from_db_str(&status_str).unwrap_or(IssueStatus::Open);
    let priority = IssuePriority::from_db_str(&priority_str).unwrap_or(IssuePriority::Medium);
    let labels: Vec<String> = if labels_str.is_empty() {
        Vec::new()
    } else {
        labels_str.split(',').map(|s| s.to_string()).collect()
    };
    let agent_source = agent_source_json
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok());
    let acceptance_criteria: Vec<String> = acceptance_criteria_json
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
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
        agent_source,
        acceptance_criteria,
        created_at,
        updated_at,
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
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

        let closed = store.close(issue.id, "wontfix", &mut log).unwrap();
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

    #[test]
    fn test_create_by_agent_basic() {
        let (_tmp, store, mut log) = setup();
        let source = AgentSource {
            source_type: "test_failure".into(),
            details: serde_json::json!({ "suite": "unit", "test": "auth::login" }),
        };
        let (issue, dup) = store
            .create_by_agent(
                "Login test failing",
                "Auth unit test fails on nil session",
                IssuePriority::High,
                vec!["bug".into()],
                "ci-agent",
                source,
                20,
                &mut log,
            )
            .unwrap();

        assert_eq!(issue.creator, "ci-agent");
        assert!(issue.agent_source.is_some());
        assert_eq!(issue.agent_source.as_ref().unwrap().source_type, "test_failure");
        assert!(dup.is_none(), "no duplicate expected for first issue");

        // Fetched from DB should round-trip agent_source.
        let fetched = store.get(issue.id).unwrap();
        assert!(fetched.agent_source.is_some());
    }

    #[test]
    fn test_rate_limit_exceeded() {
        let (_tmp, store, mut log) = setup();
        let make_source = || AgentSource {
            source_type: "test_failure".into(),
            details: serde_json::Value::Null,
        };

        // Fill up to the limit (max_per_hour = 2).
        for i in 0..2u32 {
            store
                .create_by_agent(
                    format!("Issue {i}"),
                    "",
                    IssuePriority::Low,
                    vec![],
                    "flood-agent",
                    make_source(),
                    2,
                    &mut log,
                )
                .unwrap();
        }

        // Next one should be rejected.
        let result = store.create_by_agent(
            "Issue overflow",
            "",
            IssuePriority::Low,
            vec![],
            "flood-agent",
            make_source(),
            2,
            &mut log,
        );
        assert!(
            matches!(result, Err(IssueError::RateLimitExceeded { .. })),
            "expected RateLimitExceeded"
        );

        // Verify count didn't increase beyond limit.
        let count = store.agent_issue_count_this_hour("flood-agent").unwrap();
        assert_eq!(count, 2, "rate-limited request must not increment counter");
    }

    #[test]
    fn test_duplicate_detection_warning() {
        let (_tmp, store, mut log) = setup();
        let source = || AgentSource {
            source_type: "test_failure".into(),
            details: serde_json::Value::Null,
        };

        // Create first issue.
        let (first, dup1) = store
            .create_by_agent(
                "Auth service login fails",
                "",
                IssuePriority::High,
                vec![],
                "agent-a",
                source(),
                20,
                &mut log,
            )
            .unwrap();
        assert!(dup1.is_none());

        // Create similar issue — should warn about the first.
        let (_second, dup2) = store
            .create_by_agent(
                "Auth service login failing",
                "",
                IssuePriority::Medium,
                vec![],
                "agent-b",
                source(),
                20,
                &mut log,
            )
            .unwrap();
        assert_eq!(dup2, Some(first.id), "should detect similar open issue as potential duplicate");
    }

    #[test]
    fn test_tokenize_and_similarity() {
        let a = tokenize_title("Fix login bug in auth service");
        let b = tokenize_title("Bug fix login auth service module");
        let sim = jaccard_similarity(&a, &b);
        assert!(sim >= 0.5, "similar titles should have similarity >= 0.5, got {sim}");

        let c = tokenize_title("Add database migration");
        let sim2 = jaccard_similarity(&a, &c);
        assert!(sim2 < 0.5, "dissimilar titles should have similarity < 0.5, got {sim2}");
    }
}
