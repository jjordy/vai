//! Storage abstraction layer — trait definitions for all vai storage backends.
//!
//! # Backends
//!
//! - [`sqlite`] — SQLite/filesystem backend for local CLI mode (see [`sqlite::SqliteStorage`]).
//!
//! This module defines the trait interfaces that decouple business logic from
//! specific storage engines. Two backends are planned:
//!
//! - **SQLite** (local CLI mode): single-file databases under `.vai/`
//! - **Postgres** (hosted server mode): shared multi-tenant database with `repo_id` scoping
//!
//! Every trait method accepts a `repo_id` parameter. In SQLite mode this is the
//! local repo's UUID (used for forward-compatibility). In Postgres mode it scopes
//! all queries to the correct tenant.

pub mod sqlite;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::auth::ApiKey;
use crate::escalation::{Escalation, EscalationSeverity, EscalationType, ResolutionOption};
use crate::event_log::{Event, EventKind};
use crate::graph::{Entity, Relationship};
use crate::issue::{Issue, IssueFilter, IssuePriority};
use crate::version::VersionMeta;
use crate::workspace::{WorkspaceMeta, WorkspaceStatus};

// ── Error type ────────────────────────────────────────────────────────────────

/// Unified error type for all storage operations.
///
/// Implementations map their internal error types (SQLite, Postgres, I/O) into
/// this common enum so callers need not know which backend is in use.
#[derive(Debug, Error)]
pub enum StorageError {
    /// A requested record was not found.
    #[error("not found: {0}")]
    NotFound(String),

    /// A uniqueness constraint was violated.
    #[error("conflict: {0}")]
    Conflict(String),

    /// An invalid state transition was attempted.
    #[error("invalid state transition: {0}")]
    InvalidTransition(String),

    /// A database-level error (SQLite or Postgres).
    #[error("database error: {0}")]
    Database(String),

    /// An I/O error (filesystem or network).
    #[error("I/O error: {0}")]
    Io(String),

    /// A serialization / deserialization error.
    #[error("serialization error: {0}")]
    Serialization(String),

    /// A rate limit was exceeded.
    #[error("rate limit exceeded: {0}")]
    RateLimitExceeded(String),
}

// ── Input types ───────────────────────────────────────────────────────────────

/// Input for creating a new issue.
#[derive(Debug, Clone)]
pub struct NewIssue {
    /// Short summary.
    pub title: String,
    /// Full description (Markdown).
    pub description: String,
    /// Priority level.
    pub priority: IssuePriority,
    /// Labels to attach.
    pub labels: Vec<String>,
    /// Human username or agent ID creating the issue.
    pub creator: String,
    /// Optional agent discovery metadata (JSON).
    pub agent_source: Option<serde_json::Value>,
}

/// Fields that can be updated on an existing issue.
#[derive(Debug, Clone, Default)]
pub struct IssueUpdate {
    pub title: Option<String>,
    pub description: Option<String>,
    pub priority: Option<IssuePriority>,
    pub labels: Option<Vec<String>>,
    pub status: Option<crate::issue::IssueStatus>,
    pub resolution: Option<String>,
    /// ID of the workspace linked to this issue.
    pub workspace_id: Option<Uuid>,
}

/// Input for creating a new escalation.
#[derive(Debug, Clone)]
pub struct NewEscalation {
    pub escalation_type: EscalationType,
    pub severity: EscalationSeverity,
    pub summary: String,
    pub intents: Vec<String>,
    pub agents: Vec<String>,
    pub workspace_ids: Vec<Uuid>,
    pub affected_entities: Vec<String>,
    pub resolution_options: Vec<ResolutionOption>,
}

/// Input for creating a new version.
#[derive(Debug, Clone)]
pub struct NewVersion {
    /// Version identifier, e.g. `"v3"`.
    pub version_id: String,
    /// Parent version ID, if any.
    pub parent_version_id: Option<String>,
    /// Intent description for this version.
    pub intent: String,
    /// Agent or user who created this version.
    pub created_by: String,
    /// ID of the merge event that produced this version, if any.
    pub merge_event_id: Option<u64>,
}

/// Input for creating a new workspace.
#[derive(Debug, Clone)]
pub struct NewWorkspace {
    /// Explicit ID to assign; if `None` a new UUID v4 is generated.
    pub id: Option<Uuid>,
    /// Agent's stated intent.
    pub intent: String,
    /// Version ID that was HEAD when this workspace was created.
    pub base_version: String,
    /// Optional issue this workspace addresses.
    pub issue_id: Option<Uuid>,
}

/// Fields that can be updated on an existing workspace.
#[derive(Debug, Clone, Default)]
pub struct WorkspaceUpdate {
    pub status: Option<WorkspaceStatus>,
    pub issue_id: Option<Uuid>,
}

/// Metadata about a stored file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileMetadata {
    /// Path relative to the repository root.
    pub path: String,
    /// File size in bytes.
    pub size: u64,
    /// SHA-256 hex digest of the file content.
    pub content_hash: String,
    /// When the file was last written.
    pub updated_at: DateTime<Utc>,
}

// ── EventStore ────────────────────────────────────────────────────────────────

/// Append-only event log storage.
///
/// The event log is vai's source of truth. Every action in the system is
/// recorded as an immutable, append-only event.
#[async_trait]
pub trait EventStore: Send + Sync {
    /// Appends an event to the log and returns the persisted envelope.
    async fn append(&self, repo_id: &Uuid, event: EventKind) -> Result<Event, StorageError>;

    /// Returns all events of the given type (e.g. `"WorkspaceCreated"`).
    async fn query_by_type(
        &self,
        repo_id: &Uuid,
        event_type: &str,
    ) -> Result<Vec<Event>, StorageError>;

    /// Returns all events associated with the given workspace.
    async fn query_by_workspace(
        &self,
        repo_id: &Uuid,
        workspace_id: &Uuid,
    ) -> Result<Vec<Event>, StorageError>;

    /// Returns all events whose timestamp falls within `[from, to]`.
    async fn query_by_time_range(
        &self,
        repo_id: &Uuid,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<Event>, StorageError>;

    /// Returns all events with an ID greater than `last_id`, ordered by ID.
    ///
    /// Used for streaming / replay from a known position.
    async fn query_since_id(
        &self,
        repo_id: &Uuid,
        last_id: i64,
    ) -> Result<Vec<Event>, StorageError>;

    /// Returns the total number of events for the repo.
    async fn count(&self, repo_id: &Uuid) -> Result<u64, StorageError>;
}

// ── IssueStore ────────────────────────────────────────────────────────────────

/// CRUD storage for issues.
#[async_trait]
pub trait IssueStore: Send + Sync {
    /// Creates a new issue.
    async fn create_issue(
        &self,
        repo_id: &Uuid,
        issue: NewIssue,
    ) -> Result<Issue, StorageError>;

    /// Fetches a single issue by ID.
    async fn get_issue(
        &self,
        repo_id: &Uuid,
        id: &Uuid,
    ) -> Result<Issue, StorageError>;

    /// Lists issues, optionally filtered.
    async fn list_issues(
        &self,
        repo_id: &Uuid,
        filter: &IssueFilter,
    ) -> Result<Vec<Issue>, StorageError>;

    /// Applies partial updates to an issue.
    async fn update_issue(
        &self,
        repo_id: &Uuid,
        id: &Uuid,
        update: IssueUpdate,
    ) -> Result<Issue, StorageError>;

    /// Closes an issue with the given resolution string.
    async fn close_issue(
        &self,
        repo_id: &Uuid,
        id: &Uuid,
        resolution: &str,
    ) -> Result<Issue, StorageError>;
}

// ── EscalationStore ───────────────────────────────────────────────────────────

/// Storage for escalations requiring human operator attention.
#[async_trait]
pub trait EscalationStore: Send + Sync {
    /// Creates a new escalation.
    async fn create_escalation(
        &self,
        repo_id: &Uuid,
        esc: NewEscalation,
    ) -> Result<Escalation, StorageError>;

    /// Fetches a single escalation by ID.
    async fn get_escalation(
        &self,
        repo_id: &Uuid,
        id: &Uuid,
    ) -> Result<Escalation, StorageError>;

    /// Lists escalations. If `pending_only` is true, only unresolved ones are returned.
    async fn list_escalations(
        &self,
        repo_id: &Uuid,
        pending_only: bool,
    ) -> Result<Vec<Escalation>, StorageError>;

    /// Marks an escalation as resolved with the operator's chosen resolution.
    async fn resolve_escalation(
        &self,
        repo_id: &Uuid,
        id: &Uuid,
        resolution: ResolutionOption,
        resolved_by: &str,
    ) -> Result<Escalation, StorageError>;
}

// ── GraphStore ────────────────────────────────────────────────────────────────

/// Storage for the semantic graph (entities and relationships).
///
/// The graph is fully rebuildable by re-parsing source files; its stored
/// state is a performance cache that avoids re-parsing on every query.
#[async_trait]
pub trait GraphStore: Send + Sync {
    /// Inserts or updates a batch of entities (upsert by ID).
    async fn upsert_entities(
        &self,
        repo_id: &Uuid,
        entities: Vec<Entity>,
    ) -> Result<(), StorageError>;

    /// Inserts or updates a batch of relationships (upsert by ID).
    async fn upsert_relationships(
        &self,
        repo_id: &Uuid,
        rels: Vec<Relationship>,
    ) -> Result<(), StorageError>;

    /// Fetches a single entity by its stable ID.
    async fn get_entity(
        &self,
        repo_id: &Uuid,
        id: &str,
    ) -> Result<Entity, StorageError>;

    /// Lists entities, optionally restricted to a single source file.
    async fn list_entities(
        &self,
        repo_id: &Uuid,
        file_path: Option<&str>,
    ) -> Result<Vec<Entity>, StorageError>;

    /// Returns all relationships where `from_entity_id` is the source.
    async fn get_relationships(
        &self,
        repo_id: &Uuid,
        from_entity_id: &str,
    ) -> Result<Vec<Relationship>, StorageError>;

    /// Removes all entities and relationships associated with `file_path`.
    ///
    /// Used before re-parsing a file to avoid stale graph data.
    async fn clear_file(
        &self,
        repo_id: &Uuid,
        file_path: &str,
    ) -> Result<(), StorageError>;
}

// ── VersionStore ──────────────────────────────────────────────────────────────

/// Storage for version history.
///
/// Versions are labeled snapshots of the codebase created after successful
/// merges. The history is a linear chain (v1 → v2 → v3 → …).
#[async_trait]
pub trait VersionStore: Send + Sync {
    /// Persists a new version record.
    async fn create_version(
        &self,
        repo_id: &Uuid,
        version: NewVersion,
    ) -> Result<VersionMeta, StorageError>;

    /// Fetches a version by its string ID (e.g. `"v3"`).
    async fn get_version(
        &self,
        repo_id: &Uuid,
        version_id: &str,
    ) -> Result<VersionMeta, StorageError>;

    /// Returns all versions in chronological order (oldest first).
    async fn list_versions(
        &self,
        repo_id: &Uuid,
    ) -> Result<Vec<VersionMeta>, StorageError>;

    /// Returns the current HEAD version ID, or `None` if no versions exist yet.
    async fn read_head(&self, repo_id: &Uuid) -> Result<Option<String>, StorageError>;

    /// Advances HEAD to `version_id`.
    async fn advance_head(
        &self,
        repo_id: &Uuid,
        version_id: &str,
    ) -> Result<(), StorageError>;
}

// ── WorkspaceStore ────────────────────────────────────────────────────────────

/// Storage for workspace metadata.
///
/// Workspace file overlays are stored separately via [`FileStore`].
#[async_trait]
pub trait WorkspaceStore: Send + Sync {
    /// Creates a new workspace.
    async fn create_workspace(
        &self,
        repo_id: &Uuid,
        ws: NewWorkspace,
    ) -> Result<WorkspaceMeta, StorageError>;

    /// Fetches a workspace by ID.
    async fn get_workspace(
        &self,
        repo_id: &Uuid,
        id: &Uuid,
    ) -> Result<WorkspaceMeta, StorageError>;

    /// Lists workspaces. If `include_inactive` is false, discarded and merged
    /// workspaces are excluded.
    async fn list_workspaces(
        &self,
        repo_id: &Uuid,
        include_inactive: bool,
    ) -> Result<Vec<WorkspaceMeta>, StorageError>;

    /// Applies partial updates to a workspace's metadata.
    async fn update_workspace(
        &self,
        repo_id: &Uuid,
        id: &Uuid,
        update: WorkspaceUpdate,
    ) -> Result<WorkspaceMeta, StorageError>;

    /// Marks a workspace as discarded.
    async fn discard_workspace(
        &self,
        repo_id: &Uuid,
        id: &Uuid,
    ) -> Result<(), StorageError>;
}

// ── AuthStore ─────────────────────────────────────────────────────────────────

/// Storage for API keys used to authenticate requests.
#[async_trait]
pub trait AuthStore: Send + Sync {
    /// Creates a new API key with the given human-readable name.
    ///
    /// Returns the key metadata and the plaintext secret (shown only once).
    /// `repo_id` is `None` for server-level keys.
    async fn create_key(
        &self,
        repo_id: Option<&Uuid>,
        name: &str,
    ) -> Result<(ApiKey, String), StorageError>;

    /// Validates a plaintext API token and returns the associated key metadata.
    ///
    /// Updates `last_used_at` as a side effect.
    async fn validate_key(&self, token: &str) -> Result<ApiKey, StorageError>;

    /// Lists all non-revoked keys. `repo_id` is `None` to list server-level keys.
    async fn list_keys(
        &self,
        repo_id: Option<&Uuid>,
    ) -> Result<Vec<ApiKey>, StorageError>;

    /// Revokes a key by its record ID. Revoked keys are rejected by `validate_key`.
    async fn revoke_key(&self, id: &str) -> Result<(), StorageError>;
}

// ── FileStore ─────────────────────────────────────────────────────────────────

/// Content storage for source files and workspace overlays.
///
/// The filesystem implementation writes to `.vai/` directories. The S3
/// implementation writes to a bucket scoped by `repo_id`.
#[async_trait]
pub trait FileStore: Send + Sync {
    /// Writes `content` to `path` and returns the SHA-256 hex digest.
    async fn put(
        &self,
        repo_id: &Uuid,
        path: &str,
        content: &[u8],
    ) -> Result<String, StorageError>;

    /// Reads the content of the file at `path`.
    async fn get(
        &self,
        repo_id: &Uuid,
        path: &str,
    ) -> Result<Vec<u8>, StorageError>;

    /// Lists files whose path starts with `prefix`, returning metadata only.
    async fn list(
        &self,
        repo_id: &Uuid,
        prefix: &str,
    ) -> Result<Vec<FileMetadata>, StorageError>;

    /// Deletes the file at `path`.
    async fn delete(
        &self,
        repo_id: &Uuid,
        path: &str,
    ) -> Result<(), StorageError>;

    /// Returns `true` if a file exists at `path`.
    async fn exists(
        &self,
        repo_id: &Uuid,
        path: &str,
    ) -> Result<bool, StorageError>;
}
