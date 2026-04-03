//! Postgres implementation of all storage traits.
//!
//! [`PostgresStorage`] is the server-mode backend. It stores all data in a
//! shared Postgres database with `repo_id` scoping on every table, allowing
//! multiple repositories to coexist in a single schema.
//!
//! # Connections
//!
//! Uses a [`sqlx::PgPool`] connection pool.  Create one with
//! [`PostgresStorage::connect`] and share it across handlers via `Arc`.
//!
//! # Migrations
//!
//! Call [`PostgresStorage::migrate`] once at server startup to apply any
//! pending SQL migrations from the `migrations/` directory.
//!
//! # Compile-time query checking
//!
//! This module uses `sqlx::query()` (runtime queries) rather than
//! `sqlx::query!` macros so that a live database is not required during CI
//! compilation.  Query correctness is verified by the integration tests in
//! `tests/`.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use uuid::Uuid;

use crate::auth::ApiKey;
use crate::escalation::{
    Escalation, EscalationSeverity, EscalationStatus, EscalationType, ResolutionOption,
};
use crate::event_log::{Event, EventKind};
use crate::graph::{Entity, EntityKind, Relationship, RelationshipKind};
use crate::issue::{AgentSource, Issue, IssueAttachment, IssueFilter, IssuePriority, IssueStatus};
use crate::version::VersionMeta;
use crate::watcher::{
    DiscoveryEventKind, DiscoveryPreparation, DiscoveryRecord, IssueCreationPolicy, Watcher,
    WatchType, WatcherStatus,
};
use crate::workspace::{WorkspaceMeta, WorkspaceStatus};

use super::{
    AttachmentStore, AuthStore, CommentStore, EscalationStore, EventFilter, EventStore,
    FileMetadata, FileStore, GraphStore, IssueComment, IssueLink, IssueLinkRelationship,
    IssueLinkStore, IssueStore, IssueUpdate, NewEscalation, NewIssue, NewIssueAttachment,
    NewIssueComment, NewIssueLink, NewOrg, NewUser, NewVersion, NewWorkspace, OrgMember, OrgRole,
    OrgStore, Organization, RepoCollaborator, RepoRole, StorageError, User, VersionStore,
    WatcherRegistryStore, WorkspaceStore, WorkspaceUpdate,
};
use super::pagination::{ListQuery, ListResult};

// ── PostgresStorage ───────────────────────────────────────────────────────────

/// Connection pool utilization snapshot.
#[derive(Debug, Clone, Copy)]
pub struct PoolStats {
    /// Connections currently checked out (in use by a query).
    pub active: u32,
    /// Connections currently idle in the pool.
    pub idle: u32,
    /// Maximum number of connections allowed by the pool configuration.
    pub max: u32,
}

/// Postgres-backed storage for multi-tenant hosted vai.
///
/// All trait methods accept a `repo_id` parameter and scope every SQL query
/// to that repository.  The underlying connection pool is cheaply cloneable.
#[derive(Clone, Debug)]
pub struct PostgresStorage {
    pool: PgPool,
    /// Configured upper limit for the connection pool (stored so it can be
    /// reported in the server stats endpoint without calling into pool internals).
    max_connections: u32,
    /// In-memory cache of the last time we wrote `last_used_at` for each key
    /// ID. Used to debounce writes to once per minute so high-frequency API
    /// callers don't generate excessive UPDATE traffic.
    last_used_cache: Arc<Mutex<HashMap<String, Instant>>>,
}

impl PostgresStorage {
    /// Connects to Postgres at `database_url` and returns a new storage handle.
    ///
    /// `max_connections` caps the pool size. 25 is a reasonable default for
    /// single-server deployments under moderate load; increase for high-throughput
    /// scenarios.  The pool is configured with a 5-second acquire timeout (so
    /// callers get a clear error instead of hanging indefinitely) and releases
    /// connections idle for more than 10 minutes.
    pub async fn connect(database_url: &str, max_connections: u32) -> Result<Self, StorageError> {
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            // Fail fast with a clear error rather than waiting indefinitely when
            // the pool is exhausted.
            .acquire_timeout(Duration::from_secs(5))
            // Release idle connections after 10 minutes to avoid accumulating
            // stale connections during quiet periods.
            .idle_timeout(Duration::from_secs(600))
            .connect(database_url)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;
        Ok(Self { pool, max_connections, last_used_cache: Arc::new(Mutex::new(HashMap::new())) })
    }

    /// Applies all pending SQL migrations from `migrations/` at `migrations_path`.
    ///
    /// Call once at server startup before serving requests.  The migrations
    /// directory is loaded from disk at runtime so that the binary does not
    /// need to be recompiled when SQL files change.
    pub async fn migrate(&self, migrations_path: &str) -> Result<(), StorageError> {
        let migrator = sqlx::migrate::Migrator::new(std::path::Path::new(migrations_path))
            .await
            .map_err(|e| StorageError::Database(format!("failed to load migrations: {e}")))?;
        migrator
            .run(&self.pool)
            .await
            .map_err(|e| StorageError::Database(format!("migration failed: {e}")))?;
        Ok(())
    }

    /// Returns a reference to the underlying connection pool.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Returns a snapshot of connection pool utilization.
    pub fn pool_stats(&self) -> PoolStats {
        let size = self.pool.size();
        let idle = self.pool.num_idle() as u32;
        PoolStats {
            active: size.saturating_sub(idle),
            idle,
            max: self.max_connections,
        }
    }

    /// Verifies database connectivity by executing a lightweight `SELECT 1`.
    pub async fn ping(&self) -> Result<(), sqlx::Error> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }

    /// Creates a [`sqlx::postgres::PgListener`] connected via this pool.
    ///
    /// Used by the WebSocket handler to receive `LISTEN/NOTIFY` signals from
    /// Postgres without blocking a pool connection indefinitely.
    pub async fn create_listener(&self) -> Result<sqlx::postgres::PgListener, StorageError> {
        sqlx::postgres::PgListener::connect_with(&self.pool)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))
    }
}

// ── internal helpers ──────────────────────────────────────────────────────────

/// Hashes a plaintext API token and returns the SHA-256 hex digest.
fn hash_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Generates a cryptographically suitable random hex token of `hex_chars` length.
fn random_token(hex_chars: usize) -> String {
    let mut out = String::with_capacity(hex_chars);
    while out.len() < hex_chars {
        out.push_str(&Uuid::new_v4().simple().to_string());
    }
    out.truncate(hex_chars);
    out
}

// ── EventStore ────────────────────────────────────────────────────────────────

#[async_trait]
impl EventStore for PostgresStorage {
    async fn append(&self, repo_id: &Uuid, event: EventKind) -> Result<Event, StorageError> {
        let event_type = event.event_type();
        let workspace_id: Option<Uuid> = event.workspace_id();
        let payload =
            serde_json::to_value(&event).map_err(|e| StorageError::Serialization(e.to_string()))?;

        // Use a transaction so the NOTIFY fires only after the INSERT commits.
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;

        let row = sqlx::query(
            r#"
            INSERT INTO events (repo_id, event_type, workspace_id, payload)
            VALUES ($1, $2, $3, $4)
            RETURNING id, created_at
            "#,
        )
        .bind(repo_id)
        .bind(event_type)
        .bind(workspace_id)
        .bind(&payload)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        let id: i64 = row.get("id");
        let created_at: DateTime<Utc> = row.get("created_at");

        // Notify WebSocket listeners that a new event is available.
        // Payload format: "<repo_id>:<event_id>" — lightweight pointer only.
        // The listener queries the full event from the database.
        let notify_payload = format!("{repo_id}:{id}");
        sqlx::query("SELECT pg_notify('vai_events', $1)")
            .bind(&notify_payload)
            .execute(&mut *tx)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;

        tx.commit()
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(Event {
            id: id as u64,
            kind: event,
            timestamp: created_at,
        })
    }

    async fn query_by_type(
        &self,
        repo_id: &Uuid,
        event_type: &str,
    ) -> Result<Vec<Event>, StorageError> {
        let rows = sqlx::query(
            "SELECT id, payload, created_at FROM events \
             WHERE repo_id = $1 AND event_type = $2 ORDER BY id",
        )
        .bind(repo_id)
        .bind(event_type)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        rows_to_events(rows)
    }

    async fn query_by_workspace(
        &self,
        repo_id: &Uuid,
        workspace_id: &Uuid,
    ) -> Result<Vec<Event>, StorageError> {
        let rows = sqlx::query(
            "SELECT id, payload, created_at FROM events \
             WHERE repo_id = $1 AND workspace_id = $2 ORDER BY id",
        )
        .bind(repo_id)
        .bind(workspace_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        rows_to_events(rows)
    }

    async fn query_by_time_range(
        &self,
        repo_id: &Uuid,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<Event>, StorageError> {
        let rows = sqlx::query(
            "SELECT id, payload, created_at FROM events \
             WHERE repo_id = $1 AND created_at >= $2 AND created_at <= $3 ORDER BY id",
        )
        .bind(repo_id)
        .bind(from)
        .bind(to)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        rows_to_events(rows)
    }

    async fn query_since_id(
        &self,
        repo_id: &Uuid,
        last_id: i64,
    ) -> Result<Vec<Event>, StorageError> {
        let rows = sqlx::query(
            "SELECT id, payload, created_at FROM events \
             WHERE repo_id = $1 AND id > $2 ORDER BY id",
        )
        .bind(repo_id)
        .bind(last_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        rows_to_events(rows)
    }

    /// Server-side filtered query — pushes all active filter dimensions to
    /// Postgres so only matching rows are transferred over the wire.
    ///
    /// Dimensions applied in SQL:
    /// - `event_types` → `event_type = ANY($n)`
    /// - `workspace_ids` → `workspace_id = ANY($n)`
    /// - `entity_ids` / `paths` → `payload::text LIKE '%…%'` OR-chain
    async fn query_since_id_filtered(
        &self,
        repo_id: &Uuid,
        last_id: i64,
        filter: &EventFilter,
    ) -> Result<Vec<Event>, StorageError> {
        if filter.is_empty() {
            return self.query_since_id(repo_id, last_id).await;
        }

        let mut qb: sqlx::QueryBuilder<sqlx::Postgres> = sqlx::QueryBuilder::new(
            "SELECT id, payload, created_at FROM events WHERE repo_id = ",
        );
        qb.push_bind(repo_id);
        qb.push(" AND id > ");
        qb.push_bind(last_id);

        if !filter.event_types.is_empty() {
            qb.push(" AND event_type = ANY(");
            qb.push_bind(filter.event_types.clone());
            qb.push(")");
        }

        if !filter.workspace_ids.is_empty() {
            qb.push(" AND workspace_id = ANY(");
            qb.push_bind(filter.workspace_ids.clone());
            qb.push(")");
        }

        // Entity IDs: at least one must appear (substring) in the payload.
        if !filter.entity_ids.is_empty() {
            qb.push(" AND (");
            for (i, eid) in filter.entity_ids.iter().enumerate() {
                if i > 0 {
                    qb.push(" OR ");
                }
                qb.push("payload::text LIKE ");
                qb.push_bind(format!("%{eid}%"));
            }
            qb.push(")");
        }

        // Paths: at least one must appear (substring) in the payload.
        if !filter.paths.is_empty() {
            qb.push(" AND (");
            for (i, path) in filter.paths.iter().enumerate() {
                if i > 0 {
                    qb.push(" OR ");
                }
                qb.push("payload::text LIKE ");
                qb.push_bind(format!("%{path}%"));
            }
            qb.push(")");
        }

        qb.push(" ORDER BY id");

        let rows = qb
            .build()
            .fetch_all(&self.pool)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;

        rows_to_events(rows)
    }

    async fn count(&self, repo_id: &Uuid) -> Result<u64, StorageError> {
        let row = sqlx::query("SELECT COUNT(*) AS n FROM events WHERE repo_id = $1")
            .bind(repo_id)
            .fetch_one(&self.pool)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;
        let n: i64 = row.get("n");
        Ok(n as u64)
    }
}

/// Deserialises a batch of event rows into [`Event`] values.
fn rows_to_events(rows: Vec<sqlx::postgres::PgRow>) -> Result<Vec<Event>, StorageError> {
    let mut events = Vec::with_capacity(rows.len());
    for row in rows {
        let id: i64 = row.get("id");
        let payload: serde_json::Value = row.get("payload");
        let created_at: DateTime<Utc> = row.get("created_at");
        let kind: EventKind = serde_json::from_value(payload)
            .map_err(|e| StorageError::Serialization(e.to_string()))?;
        events.push(Event {
            id: id as u64,
            kind,
            timestamp: created_at,
        });
    }
    Ok(events)
}

// ── IssueStore ────────────────────────────────────────────────────────────────

#[async_trait]
impl IssueStore for PostgresStorage {
    async fn create_issue(&self, repo_id: &Uuid, issue: NewIssue) -> Result<Issue, StorageError> {
        let id = Uuid::new_v4();
        let priority = issue.priority.as_str().to_string();
        let agent_source = issue
            .agent_source
            .map(|v| serde_json::to_value(v).unwrap_or(serde_json::Value::Null));

        sqlx::query(
            r#"
            INSERT INTO issues (id, repo_id, title, body, priority, labels, creator, agent_source, acceptance_criteria)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            "#,
        )
        .bind(id)
        .bind(repo_id)
        .bind(&issue.title)
        .bind(&issue.description)
        .bind(&priority)
        .bind(&issue.labels)
        .bind(&issue.creator)
        .bind(&agent_source)
        .bind(&issue.acceptance_criteria)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        self.get_issue(repo_id, &id).await
    }

    async fn get_issue(&self, repo_id: &Uuid, id: &Uuid) -> Result<Issue, StorageError> {
        let row = sqlx::query(
            "SELECT id, title, body, status, priority, labels, creator, agent_source, \
                    resolution, created_at, updated_at, acceptance_criteria \
             FROM issues WHERE repo_id = $1 AND id = $2",
        )
        .bind(repo_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?
        .ok_or_else(|| StorageError::NotFound(format!("issue {id}")))?;

        let issue = row_to_issue(row)?;

        Ok(issue)
    }

    async fn list_issues(
        &self,
        repo_id: &Uuid,
        filter: &IssueFilter,
        query: &ListQuery,
    ) -> Result<ListResult<Issue>, StorageError> {
        // Build dynamic WHERE clause from filter fields.
        let mut conditions = vec!["repo_id = $1".to_string()];
        let mut param_idx = 2usize;

        if filter.status.is_some() {
            conditions.push(format!("status = ${param_idx}"));
            param_idx += 1;
        }
        if filter.priority.is_some() {
            conditions.push(format!("priority = ${param_idx}"));
            param_idx += 1;
        }
        if filter.label.is_some() {
            // Case-insensitive array element match.
            conditions.push(format!(
                "EXISTS (SELECT 1 FROM unnest(labels) l WHERE lower(l) = lower(${param_idx}))"
            ));
            param_idx += 1;
        }
        if filter.creator.is_some() {
            conditions.push(format!("creator = ${param_idx}"));
            param_idx += 1;
        }
        let _ = param_idx; // suppress unused warning after last use

        let where_clause = conditions.join(" AND ");

        // Build ORDER BY from query sort fields.
        let col_map: HashMap<&str, &str> = [
            ("created_at", "created_at"),
            ("updated_at", "updated_at"),
            ("priority", "priority"),
            ("status", "status"),
            ("title", "title"),
        ]
        .into_iter()
        .collect();
        let order_by = query.sql_order_by(&col_map);
        let order_by = if order_by.is_empty() {
            "ORDER BY created_at DESC".to_string()
        } else {
            order_by
        };

        let (limit, offset) = query.sql_limit_offset();
        let limit_clause = if limit == i64::MAX {
            String::new()
        } else {
            format!(" LIMIT {limit} OFFSET {offset}")
        };

        let select_sql = format!(
            "SELECT id, title, body, status, priority, labels, creator, agent_source, \
             resolution, created_at, updated_at, acceptance_criteria \
             FROM issues WHERE {where_clause} {order_by}{limit_clause}"
        );
        let count_sql = format!(
            "SELECT COUNT(*) FROM issues WHERE {where_clause}"
        );

        // Bind parameters in the same order as the WHERE clause.
        macro_rules! bind_filter {
            ($q:expr) => {{
                let mut q = $q;
                q = q.bind(*repo_id);
                if let Some(ref s) = filter.status {
                    q = q.bind(s.as_str().to_string());
                }
                if let Some(ref p) = filter.priority {
                    q = q.bind(p.as_str().to_string());
                }
                if let Some(ref l) = filter.label {
                    q = q.bind(l.clone());
                }
                if let Some(ref c) = filter.creator {
                    q = q.bind(c.clone());
                }
                q
            }};
        }

        let count_row = bind_filter!(sqlx::query(&count_sql))
            .fetch_one(&self.pool)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;
        let total: i64 = count_row.get(0);

        let rows = bind_filter!(sqlx::query(&select_sql))
            .fetch_all(&self.pool)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;

        let items: Result<Vec<Issue>, StorageError> = rows.into_iter().map(row_to_issue).collect();
        Ok(ListResult { items: items?, total: total as u64 })
    }

    async fn update_issue(
        &self,
        repo_id: &Uuid,
        id: &Uuid,
        update: IssueUpdate,
    ) -> Result<Issue, StorageError> {
        let current = self.get_issue(repo_id, id).await?;

        let title = update.title.unwrap_or(current.title);
        let body = update.description.unwrap_or(current.description);
        let priority = update
            .priority
            .map(|p| p.as_str().to_string())
            .unwrap_or_else(|| current.priority.as_str().to_string());
        let labels = update.labels.unwrap_or(current.labels);
        let status = update
            .status
            .map(|s| s.as_str().to_string())
            .unwrap_or_else(|| current.status.as_str().to_string());
        let resolution = update.resolution.or(current.resolution);
        let acceptance_criteria = update.acceptance_criteria.unwrap_or(current.acceptance_criteria);

        sqlx::query(
            r#"
            UPDATE issues
            SET title = $1, body = $2, priority = $3, labels = $4,
                status = $5, resolution = $6, acceptance_criteria = $7, updated_at = now()
            WHERE repo_id = $8 AND id = $9
            "#,
        )
        .bind(&title)
        .bind(&body)
        .bind(&priority)
        .bind(&labels)
        .bind(&status)
        .bind(&resolution)
        .bind(&acceptance_criteria)
        .bind(repo_id)
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        self.get_issue(repo_id, id).await
    }

    async fn close_issue(
        &self,
        repo_id: &Uuid,
        id: &Uuid,
        resolution: &str,
    ) -> Result<Issue, StorageError> {
        sqlx::query(
            "UPDATE issues SET status = $1, resolution = $2, updated_at = now() \
             WHERE repo_id = $3 AND id = $4",
        )
        .bind(IssueStatus::Closed.as_str())
        .bind(resolution)
        .bind(repo_id)
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        self.get_issue(repo_id, id).await
    }
}

fn row_to_issue(row: sqlx::postgres::PgRow) -> Result<Issue, StorageError> {
    let id: Uuid = row.get("id");
    let title: String = row.get("title");
    let description: String = row.get("body");
    let status_str: String = row.get("status");
    let priority_str: String = row.get("priority");
    let labels: Vec<String> = row.get("labels");
    let creator: String = row.get("creator");
    let agent_source_val: Option<serde_json::Value> = row.get("agent_source");
    let resolution: Option<String> = row.get("resolution");
    let created_at: DateTime<Utc> = row.get("created_at");
    let updated_at: DateTime<Utc> = row.get("updated_at");
    let acceptance_criteria: Vec<String> = row.try_get("acceptance_criteria").unwrap_or_default();

    let status = IssueStatus::from_db_str(&status_str).unwrap_or(IssueStatus::Open);
    let priority = IssuePriority::from_db_str(&priority_str).unwrap_or(IssuePriority::Medium);
    let agent_source: Option<AgentSource> = agent_source_val
        .and_then(|v| serde_json::from_value(v).ok());

    Ok(Issue {
        id,
        title,
        description,
        status,
        priority,
        labels,
        creator,
        agent_source,
        resolution,
        acceptance_criteria,
        created_at,
        updated_at,
    })
}

// ── CommentStore ──────────────────────────────────────────────────────────────

#[async_trait]
impl CommentStore for PostgresStorage {
    async fn create_comment(
        &self,
        repo_id: &Uuid,
        issue_id: &Uuid,
        comment: NewIssueComment,
    ) -> Result<IssueComment, StorageError> {
        let row = sqlx::query(
            r#"
            INSERT INTO issue_comments (repo_id, issue_id, author, body, author_type, author_id)
            VALUES ($1, $2, $3, $4, $5, $6)
            RETURNING id, issue_id, author, body, created_at, author_type, author_id
            "#,
        )
        .bind(repo_id)
        .bind(issue_id)
        .bind(&comment.author)
        .bind(&comment.body)
        .bind(&comment.author_type)
        .bind(&comment.author_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(IssueComment {
            id: row.get("id"),
            issue_id: row.get("issue_id"),
            author: row.get("author"),
            body: row.get("body"),
            author_type: row.get("author_type"),
            author_id: row.get("author_id"),
            created_at: row.get("created_at"),
        })
    }

    async fn list_comments(
        &self,
        repo_id: &Uuid,
        issue_id: &Uuid,
    ) -> Result<Vec<IssueComment>, StorageError> {
        let rows = sqlx::query(
            "SELECT id, issue_id, author, body, created_at, author_type, author_id \
             FROM issue_comments \
             WHERE repo_id = $1 AND issue_id = $2 \
             ORDER BY created_at ASC",
        )
        .bind(repo_id)
        .bind(issue_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(rows
            .into_iter()
            .map(|row| IssueComment {
                id: row.get("id"),
                issue_id: row.get("issue_id"),
                author: row.get("author"),
                body: row.get("body"),
                author_type: row.get("author_type"),
                author_id: row.get("author_id"),
                created_at: row.get("created_at"),
            })
            .collect())
    }
}

// ── IssueLinkStore ────────────────────────────────────────────────────────────

#[async_trait]
impl IssueLinkStore for PostgresStorage {
    async fn create_link(
        &self,
        repo_id: &Uuid,
        source_id: &Uuid,
        link: NewIssueLink,
    ) -> Result<IssueLink, StorageError> {
        sqlx::query(
            r#"
            INSERT INTO issue_links (repo_id, source_id, target_id, relationship)
            VALUES ($1, $2, $3, $4)
            ON CONFLICT (source_id, target_id) DO UPDATE SET relationship = $4
            "#,
        )
        .bind(repo_id)
        .bind(source_id)
        .bind(link.target_id)
        .bind(link.relationship.as_str())
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(IssueLink {
            source_id: *source_id,
            target_id: link.target_id,
            relationship: link.relationship,
        })
    }

    async fn list_links(
        &self,
        repo_id: &Uuid,
        issue_id: &Uuid,
    ) -> Result<Vec<IssueLink>, StorageError> {
        let rows = sqlx::query(
            r#"
            SELECT source_id, target_id, relationship
            FROM issue_links
            WHERE repo_id = $1 AND (source_id = $2 OR target_id = $2)
            "#,
        )
        .bind(repo_id)
        .bind(issue_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(rows
            .into_iter()
            .map(|row| {
                let source_id: Uuid = row.get("source_id");
                let target_id: Uuid = row.get("target_id");
                let rel_str: String = row.get("relationship");
                let relationship =
                    IssueLinkRelationship::from_db_str(&rel_str).unwrap_or(IssueLinkRelationship::RelatesTo);
                // Return raw direction so API handlers can apply correct inverse strings.
                IssueLink {
                    source_id,
                    target_id,
                    relationship,
                }
            })
            .collect())
    }

    async fn delete_link(
        &self,
        repo_id: &Uuid,
        source_id: &Uuid,
        target_id: &Uuid,
    ) -> Result<(), StorageError> {
        sqlx::query(
            "DELETE FROM issue_links WHERE repo_id = $1 AND source_id = $2 AND target_id = $3",
        )
        .bind(repo_id)
        .bind(source_id)
        .bind(target_id)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(())
    }
}

// ── EscalationStore ───────────────────────────────────────────────────────────

#[async_trait]
impl EscalationStore for PostgresStorage {
    async fn create_escalation(
        &self,
        repo_id: &Uuid,
        esc: NewEscalation,
    ) -> Result<Escalation, StorageError> {
        let id = Uuid::new_v4();
        let esc_type = esc.escalation_type.as_str();
        let severity = esc.severity.as_str();
        let workspace_ids: Vec<Uuid> = esc.workspace_ids;
        let resolution_options =
            serde_json::to_value(&esc.resolution_options)
                .map_err(|e| StorageError::Serialization(e.to_string()))?;

        let conflicts =
            serde_json::to_value(&esc.conflicts)
                .map_err(|e| StorageError::Serialization(e.to_string()))?;

        sqlx::query(
            r#"
            INSERT INTO escalations
                (id, repo_id, escalation_type, severity, summary,
                 intents, agents, workspace_ids, affected_entities,
                 conflicts, resolution_options)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
            "#,
        )
        .bind(id)
        .bind(repo_id)
        .bind(esc_type)
        .bind(severity)
        .bind(&esc.summary)
        .bind(&esc.intents)
        .bind(&esc.agents)
        .bind(&workspace_ids)
        .bind(&esc.affected_entities)
        .bind(&conflicts)
        .bind(&resolution_options)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        self.get_escalation(repo_id, &id).await
    }

    async fn get_escalation(
        &self,
        repo_id: &Uuid,
        id: &Uuid,
    ) -> Result<Escalation, StorageError> {
        let row = sqlx::query(
            "SELECT id, escalation_type, severity, summary, intents, agents, workspace_ids, \
                    affected_entities, conflicts, resolution_options, resolved, resolution, \
                    resolved_by, resolved_at, created_at \
             FROM escalations WHERE repo_id = $1 AND id = $2",
        )
        .bind(repo_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?
        .ok_or_else(|| StorageError::NotFound(format!("escalation {id}")))?;

        row_to_escalation(row)
    }

    async fn list_escalations(
        &self,
        repo_id: &Uuid,
        pending_only: bool,
        query: &ListQuery,
    ) -> Result<ListResult<Escalation>, StorageError> {
        let where_clause = if pending_only {
            "repo_id = $1 AND resolved = false"
        } else {
            "repo_id = $1"
        };

        let col_map: HashMap<&str, &str> = [
            ("created_at", "created_at"),
            ("status", "resolved"),
        ]
        .into_iter()
        .collect();
        let order_by = query.sql_order_by(&col_map);
        let order_by = if order_by.is_empty() {
            "ORDER BY created_at DESC".to_string()
        } else {
            order_by
        };

        let (limit, offset) = query.sql_limit_offset();
        let limit_clause = if limit == i64::MAX {
            String::new()
        } else {
            format!(" LIMIT {limit} OFFSET {offset}")
        };

        let count_sql = format!(
            "SELECT COUNT(*) FROM escalations WHERE {where_clause}"
        );
        let select_sql = format!(
            "SELECT id, escalation_type, severity, summary, intents, agents, workspace_ids, \
             affected_entities, conflicts, resolution_options, resolved, resolution, \
             resolved_by, resolved_at, created_at \
             FROM escalations WHERE {where_clause} {order_by}{limit_clause}"
        );

        let count_row = sqlx::query(&count_sql)
            .bind(repo_id)
            .fetch_one(&self.pool)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;
        let total: i64 = count_row.get(0);

        let rows = sqlx::query(&select_sql)
            .bind(repo_id)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;

        let items: Result<Vec<Escalation>, StorageError> = rows.into_iter().map(row_to_escalation).collect();
        Ok(ListResult { items: items?, total: total as u64 })
    }

    async fn resolve_escalation(
        &self,
        repo_id: &Uuid,
        id: &Uuid,
        resolution: ResolutionOption,
        resolved_by: &str,
    ) -> Result<Escalation, StorageError> {
        sqlx::query(
            "UPDATE escalations \
             SET resolved = true, resolution = $1, resolved_by = $2, resolved_at = now() \
             WHERE repo_id = $3 AND id = $4",
        )
        .bind(resolution.as_str())
        .bind(resolved_by)
        .bind(repo_id)
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        self.get_escalation(repo_id, id).await
    }
}

fn row_to_escalation(row: sqlx::postgres::PgRow) -> Result<Escalation, StorageError> {
    use crate::escalation::EscalationConflict;

    let id: Uuid = row.get("id");
    let esc_type_str: String = row.get("escalation_type");
    let severity_str: String = row.get("severity");
    let summary: String = row.get("summary");
    let intents: Vec<String> = row.get("intents");
    let agents: Vec<String> = row.get("agents");
    let workspace_ids: Vec<Uuid> = row.get("workspace_ids");
    let affected_entities: Vec<String> = row.get("affected_entities");
    let conflicts_val: serde_json::Value = row.get("conflicts");
    let resolution_options_val: serde_json::Value = row.get("resolution_options");
    let resolved: bool = row.get("resolved");
    let resolution_str: Option<String> = row.get("resolution");
    let resolved_by: Option<String> = row.get("resolved_by");
    let resolved_at: Option<DateTime<Utc>> = row.get("resolved_at");
    let created_at: DateTime<Utc> = row.get("created_at");

    let escalation_type = EscalationType::from_db_str(&esc_type_str)
        .unwrap_or(EscalationType::MergeConflict);
    let severity = EscalationSeverity::from_db_str(&severity_str)
        .unwrap_or(EscalationSeverity::High);
    let conflicts: Vec<EscalationConflict> =
        serde_json::from_value(conflicts_val).unwrap_or_default();
    let resolution_options: Vec<ResolutionOption> =
        serde_json::from_value(resolution_options_val)
            .map_err(|e| StorageError::Serialization(e.to_string()))?;
    let resolution: Option<ResolutionOption> = resolution_str
        .and_then(|s| ResolutionOption::from_db_str(&s));
    let status = if resolved {
        EscalationStatus::Resolved
    } else {
        EscalationStatus::Pending
    };

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
        conflicts,
        resolution_options,
        resolution,
        resolved_by,
        resolved_at,
        created_at,
    })
}

// ── VersionStore ──────────────────────────────────────────────────────────────

#[async_trait]
impl VersionStore for PostgresStorage {
    async fn create_version(
        &self,
        repo_id: &Uuid,
        version: NewVersion,
    ) -> Result<VersionMeta, StorageError> {
        let id = Uuid::new_v4();
        let merge_event_id = version.merge_event_id.map(|x| x as i64);

        sqlx::query(
            r#"
            INSERT INTO versions
                (id, repo_id, version_id, parent_version_id, intent, created_by, merge_event_id)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            "#,
        )
        .bind(id)
        .bind(repo_id)
        .bind(&version.version_id)
        .bind(&version.parent_version_id)
        .bind(&version.intent)
        .bind(&version.created_by)
        .bind(merge_event_id)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        self.get_version(repo_id, &version.version_id).await
    }

    async fn get_version(
        &self,
        repo_id: &Uuid,
        version_id: &str,
    ) -> Result<VersionMeta, StorageError> {
        let row = sqlx::query(
            "SELECT version_id, parent_version_id, intent, created_by, merge_event_id, created_at \
             FROM versions WHERE repo_id = $1 AND version_id = $2",
        )
        .bind(repo_id)
        .bind(version_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?
        .ok_or_else(|| StorageError::NotFound(format!("version {version_id}")))?;

        row_to_version(row)
    }

    async fn list_versions(
        &self,
        repo_id: &Uuid,
        query: &ListQuery,
    ) -> Result<ListResult<VersionMeta>, StorageError> {
        let col_map: HashMap<&str, &str> = [
            ("created_at", "created_at"),
            ("version_id", "version_id"),
        ]
        .into_iter()
        .collect();
        let order_by = query.sql_order_by(&col_map);
        let order_by = if order_by.is_empty() {
            "ORDER BY created_at DESC".to_string()
        } else {
            order_by
        };

        let (limit, offset) = query.sql_limit_offset();
        let limit_clause = if limit == i64::MAX {
            String::new()
        } else {
            format!(" LIMIT {limit} OFFSET {offset}")
        };

        let count_row = sqlx::query("SELECT COUNT(*) FROM versions WHERE repo_id = $1")
            .bind(repo_id)
            .fetch_one(&self.pool)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;
        let total: i64 = count_row.get(0);

        let select_sql = format!(
            "SELECT version_id, parent_version_id, intent, created_by, merge_event_id, created_at \
             FROM versions WHERE repo_id = $1 {order_by}{limit_clause}"
        );
        let rows = sqlx::query(&select_sql)
            .bind(repo_id)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;

        let items: Result<Vec<VersionMeta>, StorageError> = rows.into_iter().map(row_to_version).collect();
        Ok(ListResult { items: items?, total: total as u64 })
    }

    async fn list_versions_since(
        &self,
        repo_id: &Uuid,
        since_num: u64,
        head_num: u64,
    ) -> Result<Vec<VersionMeta>, StorageError> {
        // Cast the numeric suffix of version_id (e.g. "v7" → 7) for range filtering.
        // This avoids loading all versions into memory for large repos.
        let rows = sqlx::query(
            "SELECT version_id, parent_version_id, intent, created_by, merge_event_id, created_at \
             FROM versions \
             WHERE repo_id = $1 \
               AND CAST(SUBSTRING(version_id FROM 2) AS BIGINT) > $2 \
               AND CAST(SUBSTRING(version_id FROM 2) AS BIGINT) <= $3 \
             ORDER BY CAST(SUBSTRING(version_id FROM 2) AS BIGINT)",
        )
        .bind(repo_id)
        .bind(since_num as i64)
        .bind(head_num as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        rows.into_iter().map(row_to_version).collect()
    }

    async fn read_head(&self, repo_id: &Uuid) -> Result<Option<String>, StorageError> {
        let row = sqlx::query(
            "SELECT version_id FROM version_head WHERE repo_id = $1",
        )
        .bind(repo_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(row.map(|r| r.get("version_id")))
    }

    async fn advance_head(&self, repo_id: &Uuid, version_id: &str) -> Result<(), StorageError> {
        sqlx::query(
            r#"
            INSERT INTO version_head (repo_id, version_id) VALUES ($1, $2)
            ON CONFLICT (repo_id) DO UPDATE SET version_id = EXCLUDED.version_id
            "#,
        )
        .bind(repo_id)
        .bind(version_id)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(())
    }
}

fn row_to_version(row: sqlx::postgres::PgRow) -> Result<VersionMeta, StorageError> {
    let version_id: String = row.get("version_id");
    let parent_version_id: Option<String> = row.get("parent_version_id");
    let intent: String = row.get("intent");
    let created_by: String = row.get("created_by");
    let merge_event_id: Option<i64> = row.get("merge_event_id");
    let created_at: DateTime<Utc> = row.get("created_at");

    Ok(VersionMeta {
        version_id,
        parent_version_id,
        intent,
        created_by,
        merge_event_id: merge_event_id.map(|x| x as u64),
        created_at,
    })
}

// ── WorkspaceStore ────────────────────────────────────────────────────────────

#[async_trait]
impl WorkspaceStore for PostgresStorage {
    async fn create_workspace(
        &self,
        repo_id: &Uuid,
        ws: NewWorkspace,
    ) -> Result<WorkspaceMeta, StorageError> {
        let id = ws.id.unwrap_or_else(Uuid::new_v4);

        sqlx::query(
            r#"
            INSERT INTO workspaces (id, repo_id, intent, base_version, issue_id)
            VALUES ($1, $2, $3, $4, $5)
            "#,
        )
        .bind(id)
        .bind(repo_id)
        .bind(&ws.intent)
        .bind(&ws.base_version)
        .bind(ws.issue_id)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        self.get_workspace(repo_id, &id).await
    }

    async fn get_workspace(
        &self,
        repo_id: &Uuid,
        id: &Uuid,
    ) -> Result<WorkspaceMeta, StorageError> {
        let row = sqlx::query(
            "SELECT id, intent, base_version, status, issue_id, deleted_paths, \
             created_at, updated_at \
             FROM workspaces WHERE repo_id = $1 AND id = $2",
        )
        .bind(repo_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?
        .ok_or_else(|| StorageError::NotFound(format!("workspace {id}")))?;

        row_to_workspace(row)
    }

    async fn list_workspaces(
        &self,
        repo_id: &Uuid,
        include_inactive: bool,
        query: &ListQuery,
    ) -> Result<ListResult<WorkspaceMeta>, StorageError> {
        let where_clause = if include_inactive {
            "repo_id = $1"
        } else {
            "repo_id = $1 AND status NOT IN ('Discarded', 'Merged')"
        };

        let col_map: HashMap<&str, &str> = [
            ("created_at", "created_at"),
            ("updated_at", "updated_at"),
            ("status", "status"),
            ("intent", "intent"),
        ]
        .into_iter()
        .collect();
        let order_by = query.sql_order_by(&col_map);
        let order_by = if order_by.is_empty() {
            "ORDER BY created_at DESC".to_string()
        } else {
            order_by
        };

        let (limit, offset) = query.sql_limit_offset();
        let limit_clause = if limit == i64::MAX {
            String::new()
        } else {
            format!(" LIMIT {limit} OFFSET {offset}")
        };

        let count_sql = format!("SELECT COUNT(*) FROM workspaces WHERE {where_clause}");
        let select_sql = format!(
            "SELECT id, intent, base_version, status, issue_id, deleted_paths, \
             created_at, updated_at \
             FROM workspaces WHERE {where_clause} {order_by}{limit_clause}"
        );

        let count_row = sqlx::query(&count_sql)
            .bind(repo_id)
            .fetch_one(&self.pool)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;
        let total: i64 = count_row.get(0);

        let rows = sqlx::query(&select_sql)
            .bind(repo_id)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;

        let items: Result<Vec<WorkspaceMeta>, StorageError> = rows.into_iter().map(row_to_workspace).collect();
        Ok(ListResult { items: items?, total: total as u64 })
    }

    async fn update_workspace(
        &self,
        repo_id: &Uuid,
        id: &Uuid,
        update: WorkspaceUpdate,
    ) -> Result<WorkspaceMeta, StorageError> {
        let current = self.get_workspace(repo_id, id).await?;
        let status = update
            .status
            .map(|s| s.as_str().to_string())
            .unwrap_or_else(|| current.status.as_str().to_string());
        let issue_id = update.issue_id.or(current.issue_id);
        let deleted_paths = update.deleted_paths.unwrap_or(current.deleted_paths);

        sqlx::query(
            "UPDATE workspaces SET status = $1, issue_id = $2, deleted_paths = $3, \
             updated_at = now() WHERE repo_id = $4 AND id = $5",
        )
        .bind(&status)
        .bind(issue_id)
        .bind(&deleted_paths)
        .bind(repo_id)
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        self.get_workspace(repo_id, id).await
    }

    async fn discard_workspace(&self, repo_id: &Uuid, id: &Uuid) -> Result<(), StorageError> {
        sqlx::query(
            "UPDATE workspaces SET status = 'Discarded', updated_at = now() \
             WHERE repo_id = $1 AND id = $2",
        )
        .bind(repo_id)
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(())
    }
}

fn row_to_workspace(row: sqlx::postgres::PgRow) -> Result<WorkspaceMeta, StorageError> {
    let id: Uuid = row.get("id");
    let intent: String = row.get("intent");
    let base_version: String = row.get("base_version");
    let status_str: String = row.get("status");
    let issue_id: Option<Uuid> = row.get("issue_id");
    let deleted_paths: Vec<String> = row.try_get("deleted_paths").unwrap_or_default();
    let created_at: DateTime<Utc> = row.get("created_at");
    let updated_at: DateTime<Utc> = row.get("updated_at");

    let status = match status_str.as_str() {
        "Active" => WorkspaceStatus::Active,
        "Submitted" => WorkspaceStatus::Submitted,
        "Merged" => WorkspaceStatus::Merged,
        "Discarded" => WorkspaceStatus::Discarded,
        _ => WorkspaceStatus::Created,
    };

    Ok(WorkspaceMeta {
        id,
        intent,
        base_version,
        status,
        issue_id,
        deleted_paths,
        created_at,
        updated_at,
    })
}

// ── GraphStore ────────────────────────────────────────────────────────────────

#[async_trait]
impl GraphStore for PostgresStorage {
    async fn upsert_entities(
        &self,
        repo_id: &Uuid,
        entities: Vec<Entity>,
    ) -> Result<(), StorageError> {
        for entity in entities {
            sqlx::query(
                r#"
                INSERT INTO entities
                    (id, repo_id, kind, name, qualified_name, file_path,
                     line_start, line_end, parent_entity_id)
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                ON CONFLICT (repo_id, id) DO UPDATE SET
                    kind = EXCLUDED.kind,
                    name = EXCLUDED.name,
                    qualified_name = EXCLUDED.qualified_name,
                    file_path = EXCLUDED.file_path,
                    line_start = EXCLUDED.line_start,
                    line_end = EXCLUDED.line_end,
                    parent_entity_id = EXCLUDED.parent_entity_id
                "#,
            )
            .bind(&entity.id)
            .bind(repo_id)
            .bind(entity.kind.as_str())
            .bind(&entity.name)
            .bind(&entity.qualified_name)
            .bind(&entity.file_path)
            .bind(entity.line_range.0 as i32)
            .bind(entity.line_range.1 as i32)
            .bind(&entity.parent_entity)
            .execute(&self.pool)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;
        }
        Ok(())
    }

    async fn upsert_relationships(
        &self,
        repo_id: &Uuid,
        rels: Vec<Relationship>,
    ) -> Result<(), StorageError> {
        for rel in rels {
            sqlx::query(
                r#"
                INSERT INTO relationships (id, repo_id, kind, from_entity_id, to_entity_id)
                VALUES ($1, $2, $3, $4, $5)
                ON CONFLICT (repo_id, id) DO UPDATE SET
                    kind = EXCLUDED.kind,
                    from_entity_id = EXCLUDED.from_entity_id,
                    to_entity_id = EXCLUDED.to_entity_id
                "#,
            )
            .bind(&rel.id)
            .bind(repo_id)
            .bind(rel.kind.as_str())
            .bind(&rel.from_entity)
            .bind(&rel.to_entity)
            .execute(&self.pool)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;
        }
        Ok(())
    }

    async fn get_entity(&self, repo_id: &Uuid, id: &str) -> Result<Entity, StorageError> {
        let row = sqlx::query(
            "SELECT id, kind, name, qualified_name, file_path, \
                    line_start, line_end, parent_entity_id \
             FROM entities WHERE repo_id = $1 AND id = $2",
        )
        .bind(repo_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?
        .ok_or_else(|| StorageError::NotFound(format!("entity {id}")))?;

        row_to_entity(row)
    }

    async fn list_entities(
        &self,
        repo_id: &Uuid,
        file_path: Option<&str>,
    ) -> Result<Vec<Entity>, StorageError> {
        let rows = match file_path {
            Some(fp) => sqlx::query(
                "SELECT id, kind, name, qualified_name, file_path, \
                        line_start, line_end, parent_entity_id \
                 FROM entities WHERE repo_id = $1 AND file_path = $2 ORDER BY line_start",
            )
            .bind(repo_id)
            .bind(fp)
            .fetch_all(&self.pool)
            .await,
            None => sqlx::query(
                "SELECT id, kind, name, qualified_name, file_path, \
                        line_start, line_end, parent_entity_id \
                 FROM entities WHERE repo_id = $1 ORDER BY file_path, line_start",
            )
            .bind(repo_id)
            .fetch_all(&self.pool)
            .await,
        }
        .map_err(|e| StorageError::Database(e.to_string()))?;

        rows.into_iter().map(row_to_entity).collect()
    }

    async fn get_relationships(
        &self,
        repo_id: &Uuid,
        from_entity_id: &str,
    ) -> Result<Vec<Relationship>, StorageError> {
        let rows = sqlx::query(
            "SELECT id, kind, from_entity_id, to_entity_id \
             FROM relationships WHERE repo_id = $1 AND from_entity_id = $2",
        )
        .bind(repo_id)
        .bind(from_entity_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        rows.into_iter().map(row_to_relationship).collect()
    }

    async fn clear_file(&self, repo_id: &Uuid, file_path: &str) -> Result<(), StorageError> {
        // Remove relationships whose source entity lives in this file.
        sqlx::query(
            "DELETE FROM relationships WHERE repo_id = $1 AND from_entity_id IN \
             (SELECT id FROM entities WHERE repo_id = $1 AND file_path = $2)",
        )
        .bind(repo_id)
        .bind(file_path)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        sqlx::query("DELETE FROM entities WHERE repo_id = $1 AND file_path = $2")
            .bind(repo_id)
            .bind(file_path)
            .execute(&self.pool)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(())
    }
}

fn row_to_entity(row: sqlx::postgres::PgRow) -> Result<Entity, StorageError> {
    let id: String = row.get("id");
    let kind_str: String = row.get("kind");
    let name: String = row.get("name");
    let qualified_name: String = row.get("qualified_name");
    let file_path: String = row.get("file_path");
    let line_start: i32 = row.try_get("line_start").unwrap_or(0);
    let line_end: i32 = row.try_get("line_end").unwrap_or(0);
    let parent_entity: Option<String> = row.get("parent_entity_id");

    let kind = match kind_str.as_str() {
        "function" => EntityKind::Function,
        "method" => EntityKind::Method,
        "struct" => EntityKind::Struct,
        "enum" => EntityKind::Enum,
        "trait" => EntityKind::Trait,
        "impl" => EntityKind::Impl,
        "module" => EntityKind::Module,
        "use_statement" => EntityKind::UseStatement,
        "class" => EntityKind::Class,
        "interface" => EntityKind::Interface,
        "type_alias" => EntityKind::TypeAlias,
        "component" => EntityKind::Component,
        "hook" => EntityKind::Hook,
        "export_statement" => EntityKind::ExportStatement,
        _ => EntityKind::Function,
    };

    Ok(Entity {
        id,
        kind,
        name,
        qualified_name,
        file_path,
        // byte_range is not stored in Postgres; use 0..0 as a sentinel.
        byte_range: (0, 0),
        line_range: (line_start as usize, line_end as usize),
        parent_entity,
    })
}

fn row_to_relationship(row: sqlx::postgres::PgRow) -> Result<Relationship, StorageError> {
    let id: String = row.get("id");
    let kind_str: String = row.get("kind");
    let from_entity: String = row.get("from_entity_id");
    let to_entity: String = row.get("to_entity_id");

    let kind = match kind_str.as_str() {
        "contains" => RelationshipKind::Contains,
        "imports" => RelationshipKind::Imports,
        "calls" => RelationshipKind::Calls,
        "implements" => RelationshipKind::Implements,
        "extends" => RelationshipKind::Extends,
        _ => RelationshipKind::Calls,
    };

    Ok(Relationship {
        id,
        kind,
        from_entity,
        to_entity,
    })
}

// ── AuthStore ─────────────────────────────────────────────────────────────────

#[async_trait]
impl AuthStore for PostgresStorage {
    async fn create_key(
        &self,
        repo_id: Option<&Uuid>,
        name: &str,
        user_id: Option<&Uuid>,
        role_override: Option<&str>,
        agent_type: Option<&str>,
        expires_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<(ApiKey, String), StorageError> {
        let id = Uuid::new_v4().to_string();
        let token = random_token(64);
        let key_hash = hash_token(&token);
        let key_prefix = token[..8].to_string();

        sqlx::query(
            r#"
            INSERT INTO api_keys
                (id, repo_id, name, key_hash, key_prefix, user_id, role_override,
                 agent_type, expires_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            "#,
        )
        .bind(&id)
        .bind(repo_id)
        .bind(name)
        .bind(&key_hash)
        .bind(&key_prefix)
        .bind(user_id)
        .bind(role_override)
        .bind(agent_type)
        .bind(expires_at)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        let key = ApiKey {
            id,
            name: name.to_string(),
            key_prefix,
            last_used_at: None,
            created_at: Utc::now(),
            revoked: false,
            user_id: user_id.copied(),
            role_override: role_override.map(|s| s.to_string()),
            agent_type: agent_type.map(|s| s.to_string()),
            expires_at,
        };

        Ok((key, token))
    }

    async fn validate_key(&self, token: &str) -> Result<ApiKey, StorageError> {
        let key_hash = hash_token(token);

        let row = sqlx::query(
            "SELECT id, name, key_prefix, last_used_at, created_at, revoked, \
                    user_id, role_override, agent_type, expires_at \
             FROM api_keys \
             WHERE key_hash = $1 \
               AND revoked = false \
               AND (expires_at IS NULL OR expires_at > now())",
        )
        .bind(&key_hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?
        .ok_or_else(|| StorageError::NotFound("invalid or revoked API key".to_string()))?;

        // Update last_used_at asynchronously (best-effort), debounced to at
        // most once per minute per key to avoid write amplification under
        // high-frequency API traffic.
        let id: String = row.get("id");
        let should_update = {
            let mut cache = self.last_used_cache.lock().unwrap_or_else(|p| p.into_inner());
            let now = Instant::now();
            match cache.get(&id) {
                Some(&last) if now.duration_since(last) < Duration::from_secs(60) => false,
                _ => {
                    cache.insert(id.clone(), now);
                    true
                }
            }
        };
        if should_update {
            let pool = self.pool.clone();
            let id_clone = id.clone();
            tokio::spawn(async move {
                let _ = sqlx::query("UPDATE api_keys SET last_used_at = now() WHERE id = $1")
                    .bind(&id_clone)
                    .execute(&pool)
                    .await;
            });
        }

        row_to_api_key(row)
    }

    async fn list_keys(&self, repo_id: Option<&Uuid>) -> Result<Vec<ApiKey>, StorageError> {
        let rows = match repo_id {
            Some(rid) => sqlx::query(
                "SELECT id, name, key_prefix, last_used_at, created_at, revoked, \
                        user_id, role_override, agent_type, expires_at \
                 FROM api_keys WHERE repo_id = $1 AND revoked = false ORDER BY created_at",
            )
            .bind(rid)
            .fetch_all(&self.pool)
            .await,
            None => sqlx::query(
                "SELECT id, name, key_prefix, last_used_at, created_at, revoked, \
                        user_id, role_override, agent_type, expires_at \
                 FROM api_keys WHERE repo_id IS NULL AND revoked = false ORDER BY created_at",
            )
            .fetch_all(&self.pool)
            .await,
        }
        .map_err(|e| StorageError::Database(e.to_string()))?;

        rows.into_iter().map(row_to_api_key).collect()
    }

    async fn list_keys_by_user(&self, user_id: &Uuid) -> Result<Vec<ApiKey>, StorageError> {
        let rows = sqlx::query(
            "SELECT id, name, key_prefix, last_used_at, created_at, revoked, \
                    user_id, role_override, agent_type, expires_at \
             FROM api_keys WHERE user_id = $1 AND revoked = false ORDER BY created_at",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        rows.into_iter().map(row_to_api_key).collect()
    }

    async fn revoke_key(&self, id: &str) -> Result<(), StorageError> {
        sqlx::query("UPDATE api_keys SET revoked = true WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(())
    }

    async fn revoke_keys_by_repo(&self, repo_id: &Uuid) -> Result<u64, StorageError> {
        let result =
            sqlx::query("UPDATE api_keys SET revoked = true WHERE repo_id = $1 AND revoked = false")
                .bind(repo_id)
                .execute(&self.pool)
                .await
                .map_err(|e| StorageError::Database(e.to_string()))?;
        Ok(result.rows_affected())
    }

    async fn revoke_keys_by_user(&self, user_id: &Uuid) -> Result<u64, StorageError> {
        let result = sqlx::query(
            "UPDATE api_keys SET revoked = true WHERE user_id = $1 AND revoked = false",
        )
        .bind(user_id)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;
        Ok(result.rows_affected())
    }

    async fn validate_session(&self, session_token: &str) -> Result<String, StorageError> {
        // Query the Better Auth `session` table. Better Auth uses camelCase
        // column names: "userId", "expiresAt", "token".
        let row = sqlx::query(
            r#"SELECT "userId" FROM session WHERE token = $1 AND "expiresAt" > now()"#,
        )
        .bind(session_token)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?
        .ok_or_else(|| StorageError::NotFound("invalid or expired session".to_string()))?;

        Ok(row.get("userId"))
    }

    async fn get_better_auth_user(
        &self,
        ba_user_id: &str,
    ) -> Result<(String, String), StorageError> {
        // Query the Better Auth `user` table (camelCase columns).
        let row = sqlx::query(r#"SELECT email, name FROM "user" WHERE id = $1"#)
            .bind(ba_user_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?
            .ok_or_else(|| {
                StorageError::NotFound(format!("Better Auth user '{ba_user_id}'"))
            })?;

        let email: String = row.get("email");
        let name: String = row.get("name");
        Ok((email, name))
    }

    async fn create_refresh_token(
        &self,
        user_id: &Uuid,
        expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<String, StorageError> {
        let token = random_token(64);
        let token_hash = hash_token(&token);
        let plaintext = format!("rt_{token}");

        sqlx::query(
            "INSERT INTO refresh_tokens (user_id, token_hash, expires_at) \
             VALUES ($1, $2, $3)",
        )
        .bind(user_id)
        .bind(&token_hash)
        .bind(expires_at)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(plaintext)
    }

    async fn validate_refresh_token(&self, token: &str) -> Result<Uuid, StorageError> {
        let token_hash = hash_token(token);

        let row = sqlx::query(
            "SELECT user_id FROM refresh_tokens \
             WHERE token_hash = $1 \
               AND expires_at > now() \
               AND revoked_at IS NULL",
        )
        .bind(&token_hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?
        .ok_or_else(|| {
            StorageError::NotFound("invalid, expired, or revoked refresh token".to_string())
        })?;

        let user_id: Uuid = row.get("user_id");
        Ok(user_id)
    }

    async fn revoke_refresh_token(&self, token: &str) -> Result<(), StorageError> {
        let token_hash = hash_token(token);

        let result = sqlx::query(
            "UPDATE refresh_tokens \
             SET revoked_at = now() \
             WHERE token_hash = $1 AND revoked_at IS NULL",
        )
        .bind(&token_hash)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        if result.rows_affected() == 0 {
            return Err(StorageError::NotFound(
                "refresh token not found or already revoked".to_string(),
            ));
        }
        Ok(())
    }
}

fn row_to_api_key(row: sqlx::postgres::PgRow) -> Result<ApiKey, StorageError> {
    Ok(ApiKey {
        id: row.get("id"),
        name: row.get("name"),
        key_prefix: row.get("key_prefix"),
        last_used_at: row.get("last_used_at"),
        created_at: row.get("created_at"),
        revoked: row.get("revoked"),
        user_id: row.get("user_id"),
        role_override: row.get("role_override"),
        agent_type: row.get("agent_type"),
        expires_at: row.get("expires_at"),
    })
}

// ── FileStore ─────────────────────────────────────────────────────────────────
//
// The Postgres backend does not implement FileStore — file content is stored
// in S3.  The `S3FileStore` (issue #76) will provide the real implementation.
// This stub returns an error at runtime so that accidental usage is surfaced
// immediately rather than silently failing.

/// Placeholder FileStore that always returns an error.
///
/// In server mode, wire an `S3FileStore` for file content storage.
#[async_trait]
impl FileStore for PostgresStorage {
    async fn put(
        &self,
        _repo_id: &Uuid,
        _path: &str,
        _content: &[u8],
    ) -> Result<String, StorageError> {
        Err(StorageError::Io(
            "PostgresStorage does not implement FileStore; use S3FileStore".to_string(),
        ))
    }

    async fn get(&self, _repo_id: &Uuid, _path: &str) -> Result<Vec<u8>, StorageError> {
        Err(StorageError::Io(
            "PostgresStorage does not implement FileStore; use S3FileStore".to_string(),
        ))
    }

    async fn list(
        &self,
        _repo_id: &Uuid,
        _prefix: &str,
    ) -> Result<Vec<FileMetadata>, StorageError> {
        Err(StorageError::Io(
            "PostgresStorage does not implement FileStore; use S3FileStore".to_string(),
        ))
    }

    async fn delete(&self, _repo_id: &Uuid, _path: &str) -> Result<(), StorageError> {
        Err(StorageError::Io(
            "PostgresStorage does not implement FileStore; use S3FileStore".to_string(),
        ))
    }

    async fn exists(&self, _repo_id: &Uuid, _path: &str) -> Result<bool, StorageError> {
        Err(StorageError::Io(
            "PostgresStorage does not implement FileStore; use S3FileStore".to_string(),
        ))
    }
}

// ── OrgStore ──────────────────────────────────────────────────────────────────

#[async_trait]
impl OrgStore for PostgresStorage {
    // ── Organizations ──────────────────────────────────────────────────────────

    async fn create_org(&self, org: NewOrg) -> Result<Organization, StorageError> {
        let row = sqlx::query(
            "INSERT INTO organizations (name, slug)
             VALUES ($1, $2)
             RETURNING id, name, slug, created_at",
        )
        .bind(&org.name)
        .bind(&org.slug)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            if let sqlx::Error::Database(ref db_err) = e {
                if db_err.constraint().is_some() {
                    return StorageError::Conflict(format!("slug '{}' already exists", org.slug));
                }
            }
            StorageError::Database(e.to_string())
        })?;

        Ok(Organization {
            id: row.get("id"),
            name: row.get("name"),
            slug: row.get("slug"),
            created_at: row.get("created_at"),
        })
    }

    async fn get_org(&self, org_id: &Uuid) -> Result<Organization, StorageError> {
        let row = sqlx::query(
            "SELECT id, name, slug, created_at FROM organizations WHERE id = $1",
        )
        .bind(org_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?
        .ok_or_else(|| StorageError::NotFound(format!("org {org_id}")))?;

        Ok(Organization {
            id: row.get("id"),
            name: row.get("name"),
            slug: row.get("slug"),
            created_at: row.get("created_at"),
        })
    }

    async fn get_org_by_slug(&self, slug: &str) -> Result<Organization, StorageError> {
        let row = sqlx::query(
            "SELECT id, name, slug, created_at FROM organizations WHERE slug = $1",
        )
        .bind(slug)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?
        .ok_or_else(|| StorageError::NotFound(format!("org with slug '{slug}'")))?;

        Ok(Organization {
            id: row.get("id"),
            name: row.get("name"),
            slug: row.get("slug"),
            created_at: row.get("created_at"),
        })
    }

    async fn list_orgs(&self) -> Result<Vec<Organization>, StorageError> {
        let rows = sqlx::query(
            "SELECT id, name, slug, created_at FROM organizations ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(rows
            .iter()
            .map(|row| Organization {
                id: row.get("id"),
                name: row.get("name"),
                slug: row.get("slug"),
                created_at: row.get("created_at"),
            })
            .collect())
    }

    async fn delete_org(&self, org_id: &Uuid) -> Result<(), StorageError> {
        let result = sqlx::query("DELETE FROM organizations WHERE id = $1")
            .bind(org_id)
            .execute(&self.pool)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;

        if result.rows_affected() == 0 {
            return Err(StorageError::NotFound(format!("org {org_id}")));
        }
        Ok(())
    }

    // ── Users ─────────────────────────────────────────────────────────────────

    async fn create_user(&self, user: NewUser) -> Result<User, StorageError> {
        let row = sqlx::query(
            "INSERT INTO users (email, name, better_auth_id)
             VALUES ($1, $2, $3)
             RETURNING id, email, name, created_at, better_auth_id",
        )
        .bind(&user.email)
        .bind(&user.name)
        .bind(&user.better_auth_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            if let sqlx::Error::Database(ref db_err) = e {
                if db_err.constraint().is_some() {
                    return StorageError::Conflict(format!(
                        "email '{}' already exists",
                        user.email
                    ));
                }
            }
            StorageError::Database(e.to_string())
        })?;

        Ok(User {
            id: row.get("id"),
            email: row.get("email"),
            name: row.get("name"),
            created_at: row.get("created_at"),
            better_auth_id: row.get("better_auth_id"),
        })
    }

    async fn get_user(&self, user_id: &Uuid) -> Result<User, StorageError> {
        let row = sqlx::query(
            "SELECT id, email, name, created_at, better_auth_id FROM users WHERE id = $1",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?
        .ok_or_else(|| StorageError::NotFound(format!("user {user_id}")))?;

        Ok(User {
            id: row.get("id"),
            email: row.get("email"),
            name: row.get("name"),
            created_at: row.get("created_at"),
            better_auth_id: row.get("better_auth_id"),
        })
    }

    async fn get_user_by_email(&self, email: &str) -> Result<User, StorageError> {
        let row = sqlx::query(
            "SELECT id, email, name, created_at, better_auth_id FROM users WHERE email = $1",
        )
        .bind(email)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?
        .ok_or_else(|| StorageError::NotFound(format!("user with email '{email}'")))?;

        Ok(User {
            id: row.get("id"),
            email: row.get("email"),
            name: row.get("name"),
            created_at: row.get("created_at"),
            better_auth_id: row.get("better_auth_id"),
        })
    }

    async fn get_user_by_external_id(&self, external_id: &str) -> Result<User, StorageError> {
        let row = sqlx::query(
            "SELECT id, email, name, created_at, better_auth_id \
             FROM users WHERE better_auth_id = $1",
        )
        .bind(external_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?
        .ok_or_else(|| {
            StorageError::NotFound(format!("user with better_auth_id '{external_id}'"))
        })?;

        Ok(User {
            id: row.get("id"),
            email: row.get("email"),
            name: row.get("name"),
            created_at: row.get("created_at"),
            better_auth_id: row.get("better_auth_id"),
        })
    }

    // ── Org membership ────────────────────────────────────────────────────────

    async fn add_org_member(
        &self,
        org_id: &Uuid,
        user_id: &Uuid,
        role: OrgRole,
    ) -> Result<OrgMember, StorageError> {
        let row = sqlx::query(
            "INSERT INTO org_members (org_id, user_id, role)
             VALUES ($1, $2, $3)
             RETURNING org_id, user_id, role, created_at",
        )
        .bind(org_id)
        .bind(user_id)
        .bind(role.as_str())
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            if let sqlx::Error::Database(ref db_err) = e {
                if db_err.constraint().is_some() {
                    return StorageError::Conflict(format!(
                        "user {user_id} is already a member of org {org_id}"
                    ));
                }
            }
            StorageError::Database(e.to_string())
        })?;

        Ok(OrgMember {
            org_id: row.get("org_id"),
            user_id: row.get("user_id"),
            role: OrgRole::from_db_str(row.get("role")),
            created_at: row.get("created_at"),
        })
    }

    async fn update_org_member(
        &self,
        org_id: &Uuid,
        user_id: &Uuid,
        role: OrgRole,
    ) -> Result<OrgMember, StorageError> {
        let row = sqlx::query(
            "UPDATE org_members SET role = $3
             WHERE org_id = $1 AND user_id = $2
             RETURNING org_id, user_id, role, created_at",
        )
        .bind(org_id)
        .bind(user_id)
        .bind(role.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?
        .ok_or_else(|| {
            StorageError::NotFound(format!("membership for user {user_id} in org {org_id}"))
        })?;

        Ok(OrgMember {
            org_id: row.get("org_id"),
            user_id: row.get("user_id"),
            role: OrgRole::from_db_str(row.get("role")),
            created_at: row.get("created_at"),
        })
    }

    async fn remove_org_member(&self, org_id: &Uuid, user_id: &Uuid) -> Result<(), StorageError> {
        let result =
            sqlx::query("DELETE FROM org_members WHERE org_id = $1 AND user_id = $2")
                .bind(org_id)
                .bind(user_id)
                .execute(&self.pool)
                .await
                .map_err(|e| StorageError::Database(e.to_string()))?;

        if result.rows_affected() == 0 {
            return Err(StorageError::NotFound(format!(
                "membership for user {user_id} in org {org_id}"
            )));
        }
        Ok(())
    }

    async fn list_org_members(&self, org_id: &Uuid) -> Result<Vec<OrgMember>, StorageError> {
        let rows = sqlx::query(
            "SELECT org_id, user_id, role, created_at
             FROM org_members WHERE org_id = $1
             ORDER BY created_at",
        )
        .bind(org_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(rows
            .iter()
            .map(|row| OrgMember {
                org_id: row.get("org_id"),
                user_id: row.get("user_id"),
                role: OrgRole::from_db_str(row.get("role")),
                created_at: row.get("created_at"),
            })
            .collect())
    }

    // ── Org-scoped repo lookup ────────────────────────────────────────────────

    async fn get_repo_id_in_org(
        &self,
        org_id: &Uuid,
        repo_name: &str,
    ) -> Result<Uuid, StorageError> {
        let row = sqlx::query("SELECT id FROM repos WHERE org_id = $1 AND name = $2")
            .bind(org_id)
            .bind(repo_name)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;

        match row {
            Some(r) => Ok(r.get::<Uuid, _>("id")),
            None => Err(StorageError::NotFound(format!(
                "repo '{}' not found in org",
                repo_name
            ))),
        }
    }

    // ── Repo collaborators ────────────────────────────────────────────────────

    async fn add_collaborator(
        &self,
        repo_id: &Uuid,
        user_id: &Uuid,
        role: RepoRole,
    ) -> Result<RepoCollaborator, StorageError> {
        let row = sqlx::query(
            "INSERT INTO repo_collaborators (repo_id, user_id, role)
             VALUES ($1, $2, $3)
             RETURNING repo_id, user_id, role, created_at",
        )
        .bind(repo_id)
        .bind(user_id)
        .bind(role.as_str())
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            if let sqlx::Error::Database(ref db_err) = e {
                if db_err.constraint().is_some() {
                    return StorageError::Conflict(format!(
                        "user {user_id} is already a collaborator on repo {repo_id}"
                    ));
                }
            }
            StorageError::Database(e.to_string())
        })?;

        Ok(RepoCollaborator {
            repo_id: row.get("repo_id"),
            user_id: row.get("user_id"),
            role: RepoRole::from_db_str(row.get("role")),
            created_at: row.get("created_at"),
        })
    }

    async fn update_collaborator(
        &self,
        repo_id: &Uuid,
        user_id: &Uuid,
        role: RepoRole,
    ) -> Result<RepoCollaborator, StorageError> {
        let row = sqlx::query(
            "UPDATE repo_collaborators SET role = $3
             WHERE repo_id = $1 AND user_id = $2
             RETURNING repo_id, user_id, role, created_at",
        )
        .bind(repo_id)
        .bind(user_id)
        .bind(role.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?
        .ok_or_else(|| {
            StorageError::NotFound(format!(
                "collaborator {user_id} on repo {repo_id}"
            ))
        })?;

        Ok(RepoCollaborator {
            repo_id: row.get("repo_id"),
            user_id: row.get("user_id"),
            role: RepoRole::from_db_str(row.get("role")),
            created_at: row.get("created_at"),
        })
    }

    async fn remove_collaborator(
        &self,
        repo_id: &Uuid,
        user_id: &Uuid,
    ) -> Result<(), StorageError> {
        let result = sqlx::query(
            "DELETE FROM repo_collaborators WHERE repo_id = $1 AND user_id = $2",
        )
        .bind(repo_id)
        .bind(user_id)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        if result.rows_affected() == 0 {
            return Err(StorageError::NotFound(format!(
                "collaborator {user_id} on repo {repo_id}"
            )));
        }
        Ok(())
    }

    async fn list_collaborators(
        &self,
        repo_id: &Uuid,
    ) -> Result<Vec<RepoCollaborator>, StorageError> {
        let rows = sqlx::query(
            "SELECT repo_id, user_id, role, created_at
             FROM repo_collaborators WHERE repo_id = $1
             ORDER BY created_at",
        )
        .bind(repo_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(rows
            .iter()
            .map(|row| RepoCollaborator {
                repo_id: row.get("repo_id"),
                user_id: row.get("user_id"),
                role: RepoRole::from_db_str(row.get("role")),
                created_at: row.get("created_at"),
            })
            .collect())
    }

    async fn list_repo_ids_for_org(&self, org_id: &Uuid) -> Result<Vec<Uuid>, StorageError> {
        let rows = sqlx::query("SELECT id FROM repos WHERE org_id = $1 ORDER BY created_at")
            .bind(org_id)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(rows.iter().map(|r| r.get::<Uuid, _>("id")).collect())
    }

    async fn list_all_repo_ids(&self) -> Result<Vec<Uuid>, StorageError> {
        let rows = sqlx::query("SELECT id FROM repos ORDER BY created_at")
            .fetch_all(&self.pool)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(rows.iter().map(|r| r.get::<Uuid, _>("id")).collect())
    }

    async fn count_collaborator_repos(&self, user_id: &Uuid) -> Result<u64, StorageError> {
        let row =
            sqlx::query("SELECT COUNT(*) AS n FROM repo_collaborators WHERE user_id = $1")
                .bind(user_id)
                .fetch_one(&self.pool)
                .await
                .map_err(|e| StorageError::Database(e.to_string()))?;

        let n: i64 = row.get("n");
        Ok(n as u64)
    }

    async fn resolve_repo_role(
        &self,
        user_id: &Uuid,
        repo_id: &Uuid,
    ) -> Result<Option<RepoRole>, StorageError> {
        // Single query: join repos → org_members (via org_id) and
        // repo_collaborators, both as LEFT JOINs so we always get a row when
        // the repo exists.
        let row = sqlx::query(
            "SELECT om.role  AS org_role,
                    rc.role  AS direct_role
             FROM   repos r
             LEFT JOIN org_members om
                    ON r.org_id IS NOT NULL
                   AND r.org_id   = om.org_id
                   AND om.user_id = $1
             LEFT JOIN repo_collaborators rc
                    ON rc.repo_id  = $2
                   AND rc.user_id  = $1
             WHERE  r.id = $2",
        )
        .bind(user_id)
        .bind(repo_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        let row = match row {
            Some(r) => r,
            // Repo not found — treat as no access.
            None => return Ok(None),
        };

        // Derive an effective role from org membership (owner/admin only).
        let org_derived: Option<RepoRole> = {
            let org_role: Option<&str> = row.get("org_role");
            match org_role {
                Some("owner") => Some(RepoRole::Owner),
                Some("admin") => Some(RepoRole::Admin),
                // org members (role = "member") need a direct collaborator entry.
                _ => None,
            }
        };

        // Direct collaborator role on this specific repo.
        let direct: Option<RepoRole> = {
            let direct_role: Option<&str> = row.get("direct_role");
            direct_role.map(RepoRole::from_db_str)
        };

        // Return the most permissive of the two, or None if neither exists.
        Ok(match (org_derived, direct) {
            (Some(a), Some(b)) => Some(RepoRole::max(a, b)),
            (Some(r), None) | (None, Some(r)) => Some(r),
            (None, None) => None,
        })
    }
}

// ── AttachmentStore ───────────────────────────────────────────────────────────

#[async_trait]
impl AttachmentStore for PostgresStorage {
    async fn create_attachment(
        &self,
        repo_id: &Uuid,
        issue_id: &Uuid,
        attachment: NewIssueAttachment,
    ) -> Result<IssueAttachment, StorageError> {
        let row = sqlx::query(
            r#"
            INSERT INTO issue_attachments
                (repo_id, issue_id, filename, content_type, size_bytes, s3_key, uploaded_by)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            RETURNING id, issue_id, filename, content_type, size_bytes, s3_key, uploaded_by, created_at
            "#,
        )
        .bind(repo_id)
        .bind(issue_id)
        .bind(&attachment.filename)
        .bind(&attachment.content_type)
        .bind(attachment.size_bytes)
        .bind(&attachment.s3_key)
        .bind(&attachment.uploaded_by)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            let msg = e.to_string();
            if msg.contains("unique") || msg.contains("duplicate") {
                StorageError::Conflict(format!(
                    "attachment '{}' already exists on this issue",
                    attachment.filename
                ))
            } else {
                StorageError::Database(msg)
            }
        })?;

        Ok(IssueAttachment {
            id: row.get("id"),
            issue_id: row.get("issue_id"),
            filename: row.get("filename"),
            content_type: row.get("content_type"),
            size_bytes: row.get("size_bytes"),
            s3_key: row.get("s3_key"),
            uploaded_by: row.get("uploaded_by"),
            created_at: row.get("created_at"),
        })
    }

    async fn list_attachments(
        &self,
        repo_id: &Uuid,
        issue_id: &Uuid,
    ) -> Result<Vec<IssueAttachment>, StorageError> {
        let rows = sqlx::query(
            "SELECT id, issue_id, filename, content_type, size_bytes, s3_key, uploaded_by, created_at \
             FROM issue_attachments \
             WHERE repo_id = $1 AND issue_id = $2 \
             ORDER BY created_at ASC",
        )
        .bind(repo_id)
        .bind(issue_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(rows
            .into_iter()
            .map(|row| IssueAttachment {
                id: row.get("id"),
                issue_id: row.get("issue_id"),
                filename: row.get("filename"),
                content_type: row.get("content_type"),
                size_bytes: row.get("size_bytes"),
                s3_key: row.get("s3_key"),
                uploaded_by: row.get("uploaded_by"),
                created_at: row.get("created_at"),
            })
            .collect())
    }

    async fn get_attachment(
        &self,
        repo_id: &Uuid,
        issue_id: &Uuid,
        filename: &str,
    ) -> Result<IssueAttachment, StorageError> {
        let row = sqlx::query(
            "SELECT id, issue_id, filename, content_type, size_bytes, s3_key, uploaded_by, created_at \
             FROM issue_attachments \
             WHERE repo_id = $1 AND issue_id = $2 AND filename = $3",
        )
        .bind(repo_id)
        .bind(issue_id)
        .bind(filename)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?
        .ok_or_else(|| StorageError::NotFound(format!("attachment '{filename}' not found")))?;

        Ok(IssueAttachment {
            id: row.get("id"),
            issue_id: row.get("issue_id"),
            filename: row.get("filename"),
            content_type: row.get("content_type"),
            size_bytes: row.get("size_bytes"),
            s3_key: row.get("s3_key"),
            uploaded_by: row.get("uploaded_by"),
            created_at: row.get("created_at"),
        })
    }

    async fn delete_attachment(
        &self,
        repo_id: &Uuid,
        issue_id: &Uuid,
        filename: &str,
    ) -> Result<(), StorageError> {
        sqlx::query(
            "DELETE FROM issue_attachments WHERE repo_id = $1 AND issue_id = $2 AND filename = $3",
        )
        .bind(repo_id)
        .bind(issue_id)
        .bind(filename)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(())
    }
}

// ── WatcherRegistryStore ──────────────────────────────────────────────────────

/// Returns the current UTC hour as a bucket string `YYYY-MM-DDTHH`.
fn pg_hour_bucket() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H").to_string()
}

/// Maps a Postgres row (8 columns) into a [`Watcher`].
fn row_to_watcher(row: &sqlx::postgres::PgRow) -> Result<Watcher, StorageError> {
    let agent_id: String = row.try_get("agent_id").map_err(|e| StorageError::Database(e.to_string()))?;
    let watch_type: String = row.try_get("watch_type").map_err(|e| StorageError::Database(e.to_string()))?;
    let description: String = row.try_get("description").map_err(|e| StorageError::Database(e.to_string()))?;
    let policy_json: serde_json::Value = row.try_get("policy_json").map_err(|e| StorageError::Database(e.to_string()))?;
    let status: String = row.try_get("status").map_err(|e| StorageError::Database(e.to_string()))?;
    let registered_at: DateTime<Utc> = row.try_get("registered_at").map_err(|e| StorageError::Database(e.to_string()))?;
    let last_discovery_at: Option<DateTime<Utc>> = row.try_get("last_discovery_at").map_err(|e| StorageError::Database(e.to_string()))?;
    let discovery_count: i32 = row.try_get("discovery_count").map_err(|e| StorageError::Database(e.to_string()))?;

    let policy: IssueCreationPolicy = serde_json::from_value(policy_json)
        .unwrap_or_default();

    Ok(Watcher {
        agent_id,
        watch_type: WatchType::from_db_str(&watch_type),
        description,
        issue_creation_policy: policy,
        status: WatcherStatus::from_db_str(&status),
        registered_at,
        last_discovery_at,
        discovery_count: discovery_count as u32,
    })
}

#[async_trait]
impl WatcherRegistryStore for PostgresStorage {
    async fn register_watcher(
        &self,
        repo_id: &Uuid,
        watcher: Watcher,
    ) -> Result<Watcher, StorageError> {
        let policy_json = serde_json::to_value(&watcher.issue_creation_policy)
            .map_err(|e| StorageError::Serialization(e.to_string()))?;
        sqlx::query(
            r#"
            INSERT INTO watchers
                (repo_id, agent_id, watch_type, description, policy_json, status,
                 registered_at, last_discovery_at, discovery_count)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            "#,
        )
        .bind(repo_id)
        .bind(&watcher.agent_id)
        .bind(watcher.watch_type.as_str())
        .bind(&watcher.description)
        .bind(&policy_json)
        .bind(watcher.status.as_str())
        .bind(watcher.registered_at)
        .bind(watcher.last_discovery_at)
        .bind(watcher.discovery_count as i32)
        .execute(&self.pool)
        .await
        .map_err(|e| {
            let msg = e.to_string();
            if msg.contains("unique") || msg.contains("duplicate") || msg.contains("23505") {
                StorageError::Conflict(format!(
                    "watcher '{}' is already registered for this repo",
                    watcher.agent_id
                ))
            } else {
                StorageError::Database(msg)
            }
        })?;

        Ok(watcher)
    }

    async fn get_watcher(
        &self,
        repo_id: &Uuid,
        agent_id: &str,
    ) -> Result<Watcher, StorageError> {
        let row = sqlx::query(
            "SELECT agent_id, watch_type, description, policy_json, status, \
             registered_at, last_discovery_at, discovery_count \
             FROM watchers WHERE repo_id = $1 AND agent_id = $2",
        )
        .bind(repo_id)
        .bind(agent_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?
        .ok_or_else(|| StorageError::NotFound(format!("watcher '{agent_id}' not found")))?;

        row_to_watcher(&row)
    }

    async fn list_watchers(&self, repo_id: &Uuid) -> Result<Vec<Watcher>, StorageError> {
        let rows = sqlx::query(
            "SELECT agent_id, watch_type, description, policy_json, status, \
             registered_at, last_discovery_at, discovery_count \
             FROM watchers WHERE repo_id = $1 \
             ORDER BY registered_at DESC",
        )
        .bind(repo_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        rows.iter().map(row_to_watcher).collect()
    }

    async fn pause_watcher(
        &self,
        repo_id: &Uuid,
        agent_id: &str,
    ) -> Result<Watcher, StorageError> {
        let rows_affected = sqlx::query(
            "UPDATE watchers SET status = 'paused' WHERE repo_id = $1 AND agent_id = $2",
        )
        .bind(repo_id)
        .bind(agent_id)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?
        .rows_affected();

        if rows_affected == 0 {
            return Err(StorageError::NotFound(format!(
                "watcher '{agent_id}' not found"
            )));
        }

        self.get_watcher(repo_id, agent_id).await
    }

    async fn resume_watcher(
        &self,
        repo_id: &Uuid,
        agent_id: &str,
    ) -> Result<Watcher, StorageError> {
        let rows_affected = sqlx::query(
            "UPDATE watchers SET status = 'active' WHERE repo_id = $1 AND agent_id = $2",
        )
        .bind(repo_id)
        .bind(agent_id)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?
        .rows_affected();

        if rows_affected == 0 {
            return Err(StorageError::NotFound(format!(
                "watcher '{agent_id}' not found"
            )));
        }

        self.get_watcher(repo_id, agent_id).await
    }

    async fn prepare_discovery(
        &self,
        repo_id: &Uuid,
        agent_id: &str,
        event: &DiscoveryEventKind,
    ) -> Result<DiscoveryPreparation, StorageError> {
        // Step 1: validate watcher is active.
        let watcher = self.get_watcher(repo_id, agent_id).await?;
        if watcher.status == WatcherStatus::Paused {
            return Err(StorageError::NotFound(format!(
                "{agent_id} is paused — resume before submitting discoveries"
            )));
        }

        // Step 2: rate-limit — increment the per-hour counter.
        let bucket = pg_hour_bucket();
        sqlx::query(
            r#"
            INSERT INTO watcher_rate_limits (repo_id, agent_id, hour_bucket, count)
            VALUES ($1, $2, $3, 1)
            ON CONFLICT (repo_id, agent_id, hour_bucket)
            DO UPDATE SET count = watcher_rate_limits.count + 1
            "#,
        )
        .bind(repo_id)
        .bind(agent_id)
        .bind(&bucket)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        let count: i32 = sqlx::query(
            "SELECT count FROM watcher_rate_limits \
             WHERE repo_id = $1 AND agent_id = $2 AND hour_bucket = $3",
        )
        .bind(repo_id)
        .bind(agent_id)
        .bind(&bucket)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?
        .try_get("count")
        .map_err(|e| StorageError::Database(e.to_string()))?;

        let max = watcher.issue_creation_policy.max_per_hour;
        if count as u32 > max {
            // Roll back the increment before returning.
            let _ = sqlx::query(
                "UPDATE watcher_rate_limits SET count = count - 1 \
                 WHERE repo_id = $1 AND agent_id = $2 AND hour_bucket = $3",
            )
            .bind(repo_id)
            .bind(agent_id)
            .bind(&bucket)
            .execute(&self.pool)
            .await;

            return Err(StorageError::RateLimitExceeded(format!(
                "watcher {agent_id} has submitted {count} discoveries this hour (max {max})"
            )));
        }

        // Step 3: duplicate suppression — find existing open issue for this dedup key.
        let dedup_key = event.dedup_key();
        let existing_issue_id: Option<Uuid> = sqlx::query(
            "SELECT created_issue_id FROM watcher_discoveries \
             WHERE repo_id = $1 AND agent_id = $2 AND dedup_key = $3 \
               AND suppressed = FALSE AND created_issue_id IS NOT NULL \
             ORDER BY received_at DESC \
             LIMIT 1",
        )
        .bind(repo_id)
        .bind(agent_id)
        .bind(&dedup_key)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?
        .and_then(|row| row.try_get::<Option<Uuid>, _>("created_issue_id").ok().flatten());

        let priority = event.default_priority();
        let should_create = watcher.issue_creation_policy.should_auto_create(&priority);

        Ok(DiscoveryPreparation {
            record_id: Uuid::new_v4(),
            dedup_key,
            received_at: chrono::Utc::now(),
            suppressed_with_issue_id: existing_issue_id,
            should_create_issue: should_create,
            priority,
        })
    }

    async fn record_discovery(
        &self,
        repo_id: &Uuid,
        agent_id: &str,
        event: &DiscoveryEventKind,
        record_id: Uuid,
        dedup_key: &str,
        received_at: DateTime<Utc>,
        created_issue_id: Option<Uuid>,
        suppressed: bool,
    ) -> Result<DiscoveryRecord, StorageError> {
        let event_json = serde_json::to_value(event)
            .map_err(|e| StorageError::Serialization(e.to_string()))?;

        sqlx::query(
            r#"
            INSERT INTO watcher_discoveries
                (id, repo_id, agent_id, event_type, event_json, dedup_key,
                 received_at, created_issue_id, suppressed)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            "#,
        )
        .bind(record_id)
        .bind(repo_id)
        .bind(agent_id)
        .bind(event.event_type())
        .bind(&event_json)
        .bind(dedup_key)
        .bind(received_at)
        .bind(created_issue_id)
        .bind(suppressed)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        // Update watcher stats.
        sqlx::query(
            r#"
            UPDATE watchers
            SET last_discovery_at = $1,
                discovery_count = discovery_count + 1
            WHERE repo_id = $2 AND agent_id = $3
            "#,
        )
        .bind(received_at)
        .bind(repo_id)
        .bind(agent_id)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(DiscoveryRecord {
            id: record_id,
            agent_id: agent_id.to_string(),
            event: event.clone(),
            received_at,
            created_issue_id,
            suppressed,
        })
    }
}
