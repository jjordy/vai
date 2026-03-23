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
use uuid::Uuid;

use crate::auth::ApiKey;
use crate::escalation::{
    Escalation, EscalationSeverity, EscalationStatus, EscalationType, ResolutionOption,
};
use crate::event_log::{Event, EventKind};
use crate::graph::{Entity, EntityKind, Relationship, RelationshipKind};
use crate::issue::{AgentSource, Issue, IssueFilter, IssuePriority, IssueStatus};
use crate::version::VersionMeta;
use crate::workspace::{WorkspaceMeta, WorkspaceStatus};

use super::{
    AuthStore, EscalationStore, EventStore, FileMetadata, FileStore, GraphStore, IssueStore,
    IssueUpdate, NewEscalation, NewIssue, NewOrg, NewUser, NewVersion, NewWorkspace, OrgMember,
    OrgRole, OrgStore, Organization, RepoCollaborator, RepoRole, StorageError, User, VersionStore,
    WorkspaceStore, WorkspaceUpdate,
};

// ── PostgresStorage ───────────────────────────────────────────────────────────

/// Postgres-backed storage for multi-tenant hosted vai.
///
/// All trait methods accept a `repo_id` parameter and scope every SQL query
/// to that repository.  The underlying connection pool is cheaply cloneable.
#[derive(Clone, Debug)]
pub struct PostgresStorage {
    pool: PgPool,
}

impl PostgresStorage {
    /// Connects to Postgres at `database_url` and returns a new storage handle.
    ///
    /// `max_connections` caps the pool size (10 is suitable for single-server
    /// deployments; increase for high-throughput scenarios).
    pub async fn connect(database_url: &str, max_connections: u32) -> Result<Self, StorageError> {
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .connect(database_url)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;
        Ok(Self { pool })
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

        let row = sqlx::query(
            r#"
            INSERT INTO events (repo_id, event_type, workspace_id, payload)
            VALUES ($1, $2, $3, $4)
            RETURNING id, created_at
            "#,
        )
        .bind(repo_id)
        .bind(&event_type)
        .bind(workspace_id)
        .bind(&payload)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        let id: i64 = row.get("id");
        let created_at: DateTime<Utc> = row.get("created_at");

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
            INSERT INTO issues (id, repo_id, title, body, priority, labels, creator, agent_source)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
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
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        self.get_issue(repo_id, &id).await
    }

    async fn get_issue(&self, repo_id: &Uuid, id: &Uuid) -> Result<Issue, StorageError> {
        let row = sqlx::query(
            "SELECT id, title, body, status, priority, labels, creator, agent_source, \
                    resolution, created_at, updated_at \
             FROM issues WHERE repo_id = $1 AND id = $2",
        )
        .bind(repo_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?
        .ok_or_else(|| StorageError::NotFound(format!("issue {id}")))?;

        row_to_issue(row)
    }

    async fn list_issues(
        &self,
        repo_id: &Uuid,
        filter: &IssueFilter,
    ) -> Result<Vec<Issue>, StorageError> {
        let rows = sqlx::query(
            "SELECT id, title, body, status, priority, labels, creator, agent_source, \
                    resolution, created_at, updated_at \
             FROM issues WHERE repo_id = $1 ORDER BY created_at DESC",
        )
        .bind(repo_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        let mut issues = Vec::new();
        for row in rows {
            let issue = row_to_issue(row)?;
            if filter_matches_issue(&issue, filter) {
                issues.push(issue);
            }
        }
        Ok(issues)
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

        sqlx::query(
            r#"
            UPDATE issues
            SET title = $1, body = $2, priority = $3, labels = $4,
                status = $5, resolution = $6, updated_at = now()
            WHERE repo_id = $7 AND id = $8
            "#,
        )
        .bind(&title)
        .bind(&body)
        .bind(&priority)
        .bind(&labels)
        .bind(&status)
        .bind(&resolution)
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

    let status = IssueStatus::from_str(&status_str).unwrap_or(IssueStatus::Open);
    let priority = IssuePriority::from_str(&priority_str).unwrap_or(IssuePriority::Medium);
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
        created_at,
        updated_at,
    })
}

fn filter_matches_issue(issue: &Issue, filter: &IssueFilter) -> bool {
    if let Some(ref s) = filter.status {
        if issue.status != *s {
            return false;
        }
    }
    if let Some(ref p) = filter.priority {
        if issue.priority != *p {
            return false;
        }
    }
    if let Some(ref label) = filter.label {
        if !issue.labels.iter().any(|l| l.eq_ignore_ascii_case(label)) {
            return false;
        }
    }
    if let Some(ref creator) = filter.creator {
        if &issue.creator != creator {
            return false;
        }
    }
    true
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

        sqlx::query(
            r#"
            INSERT INTO escalations
                (id, repo_id, escalation_type, severity, summary,
                 intents, agents, workspace_ids, affected_entities, resolution_options)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
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
                    affected_entities, resolution_options, resolved, resolution, resolved_by, \
                    resolved_at, created_at \
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
    ) -> Result<Vec<Escalation>, StorageError> {
        let rows = if pending_only {
            sqlx::query(
                "SELECT id, escalation_type, severity, summary, intents, agents, workspace_ids, \
                        affected_entities, resolution_options, resolved, resolution, resolved_by, \
                        resolved_at, created_at \
                 FROM escalations WHERE repo_id = $1 AND resolved = false ORDER BY created_at",
            )
            .bind(repo_id)
            .fetch_all(&self.pool)
            .await
        } else {
            sqlx::query(
                "SELECT id, escalation_type, severity, summary, intents, agents, workspace_ids, \
                        affected_entities, resolution_options, resolved, resolution, resolved_by, \
                        resolved_at, created_at \
                 FROM escalations WHERE repo_id = $1 ORDER BY created_at",
            )
            .bind(repo_id)
            .fetch_all(&self.pool)
            .await
        }
        .map_err(|e| StorageError::Database(e.to_string()))?;

        rows.into_iter().map(row_to_escalation).collect()
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
    let id: Uuid = row.get("id");
    let esc_type_str: String = row.get("escalation_type");
    let severity_str: String = row.get("severity");
    let summary: String = row.get("summary");
    let intents: Vec<String> = row.get("intents");
    let agents: Vec<String> = row.get("agents");
    let workspace_ids: Vec<Uuid> = row.get("workspace_ids");
    let affected_entities: Vec<String> = row.get("affected_entities");
    let resolution_options_val: serde_json::Value = row.get("resolution_options");
    let resolved: bool = row.get("resolved");
    let resolution_str: Option<String> = row.get("resolution");
    let resolved_by: Option<String> = row.get("resolved_by");
    let resolved_at: Option<DateTime<Utc>> = row.get("resolved_at");
    let created_at: DateTime<Utc> = row.get("created_at");

    let escalation_type = EscalationType::from_str(&esc_type_str)
        .unwrap_or(EscalationType::MergeConflict);
    let severity = EscalationSeverity::from_str(&severity_str)
        .unwrap_or(EscalationSeverity::High);
    let resolution_options: Vec<ResolutionOption> =
        serde_json::from_value(resolution_options_val)
            .map_err(|e| StorageError::Serialization(e.to_string()))?;
    let resolution: Option<ResolutionOption> = resolution_str
        .and_then(|s| ResolutionOption::from_str(&s));
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

    async fn list_versions(&self, repo_id: &Uuid) -> Result<Vec<VersionMeta>, StorageError> {
        let rows = sqlx::query(
            "SELECT version_id, parent_version_id, intent, created_by, merge_event_id, created_at \
             FROM versions WHERE repo_id = $1 ORDER BY created_at",
        )
        .bind(repo_id)
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
            "SELECT id, intent, base_version, status, issue_id, created_at, updated_at \
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
    ) -> Result<Vec<WorkspaceMeta>, StorageError> {
        let rows = if include_inactive {
            sqlx::query(
                "SELECT id, intent, base_version, status, issue_id, created_at, updated_at \
                 FROM workspaces WHERE repo_id = $1 ORDER BY created_at",
            )
            .bind(repo_id)
            .fetch_all(&self.pool)
            .await
        } else {
            sqlx::query(
                "SELECT id, intent, base_version, status, issue_id, created_at, updated_at \
                 FROM workspaces WHERE repo_id = $1 \
                 AND status NOT IN ('Discarded', 'Merged') ORDER BY created_at",
            )
            .bind(repo_id)
            .fetch_all(&self.pool)
            .await
        }
        .map_err(|e| StorageError::Database(e.to_string()))?;

        rows.into_iter().map(row_to_workspace).collect()
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

        sqlx::query(
            "UPDATE workspaces SET status = $1, issue_id = $2, updated_at = now() \
             WHERE repo_id = $3 AND id = $4",
        )
        .bind(&status)
        .bind(issue_id)
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
    ) -> Result<(ApiKey, String), StorageError> {
        let id = Uuid::new_v4().to_string();
        let token = random_token(64);
        let key_hash = hash_token(&token);
        let key_prefix = token[..12].to_string();

        sqlx::query(
            r#"
            INSERT INTO api_keys (id, repo_id, name, key_hash, key_prefix)
            VALUES ($1, $2, $3, $4, $5)
            "#,
        )
        .bind(&id)
        .bind(repo_id)
        .bind(name)
        .bind(&key_hash)
        .bind(&key_prefix)
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
        };

        Ok((key, token))
    }

    async fn validate_key(&self, token: &str) -> Result<ApiKey, StorageError> {
        let key_hash = hash_token(token);

        let row = sqlx::query(
            "SELECT id, name, key_prefix, last_used_at, created_at, revoked \
             FROM api_keys WHERE key_hash = $1 AND revoked = false",
        )
        .bind(&key_hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?
        .ok_or_else(|| StorageError::NotFound("invalid or revoked API key".to_string()))?;

        // Update last_used_at asynchronously (best-effort).
        let pool = self.pool.clone();
        let id: String = row.get("id");
        let id_clone = id.clone();
        tokio::spawn(async move {
            let _ = sqlx::query("UPDATE api_keys SET last_used_at = now() WHERE id = $1")
                .bind(&id_clone)
                .execute(&pool)
                .await;
        });

        row_to_api_key(row)
    }

    async fn list_keys(&self, repo_id: Option<&Uuid>) -> Result<Vec<ApiKey>, StorageError> {
        let rows = match repo_id {
            Some(rid) => sqlx::query(
                "SELECT id, name, key_prefix, last_used_at, created_at, revoked \
                 FROM api_keys WHERE repo_id = $1 AND revoked = false ORDER BY created_at",
            )
            .bind(rid)
            .fetch_all(&self.pool)
            .await,
            None => sqlx::query(
                "SELECT id, name, key_prefix, last_used_at, created_at, revoked \
                 FROM api_keys WHERE repo_id IS NULL AND revoked = false ORDER BY created_at",
            )
            .fetch_all(&self.pool)
            .await,
        }
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
}

fn row_to_api_key(row: sqlx::postgres::PgRow) -> Result<ApiKey, StorageError> {
    Ok(ApiKey {
        id: row.get("id"),
        name: row.get("name"),
        key_prefix: row.get("key_prefix"),
        last_used_at: row.get("last_used_at"),
        created_at: row.get("created_at"),
        revoked: row.get("revoked"),
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
            "INSERT INTO users (email, name)
             VALUES ($1, $2)
             RETURNING id, email, name, created_at",
        )
        .bind(&user.email)
        .bind(&user.name)
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
        })
    }

    async fn get_user(&self, user_id: &Uuid) -> Result<User, StorageError> {
        let row =
            sqlx::query("SELECT id, email, name, created_at FROM users WHERE id = $1")
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
        })
    }

    async fn get_user_by_email(&self, email: &str) -> Result<User, StorageError> {
        let row =
            sqlx::query("SELECT id, email, name, created_at FROM users WHERE email = $1")
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
            role: OrgRole::from_str(row.get("role")),
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
            role: OrgRole::from_str(row.get("role")),
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
                role: OrgRole::from_str(row.get("role")),
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
            role: RepoRole::from_str(row.get("role")),
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
            role: RepoRole::from_str(row.get("role")),
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
                role: RepoRole::from_str(row.get("role")),
                created_at: row.get("created_at"),
            })
            .collect())
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
            direct_role.map(RepoRole::from_str)
        };

        // Return the most permissive of the two, or None if neither exists.
        Ok(match (org_derived, direct) {
            (Some(a), Some(b)) => Some(RepoRole::max(a, b)),
            (Some(r), None) | (None, Some(r)) => Some(r),
            (None, None) => None,
        })
    }
}
