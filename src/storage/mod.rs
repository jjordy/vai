//! Storage abstraction layer — trait definitions for all vai storage backends.
//!
//! # Backends
//!
//! - [`sqlite`] — SQLite/filesystem backend for local CLI mode (see [`sqlite::SqliteStorage`]).
//! - [`postgres`] — Postgres backend for hosted server mode (see [`postgres::PostgresStorage`]).
//! - [`filesystem`] — Standalone filesystem [`FileStore`] (see [`filesystem::FilesystemFileStore`]).
//!
//! This module defines the trait interfaces that decouple business logic from
//! specific storage engines. Two backends are supported:
//!
//! - **SQLite** (local CLI mode): single-file databases under `.vai/`
//! - **Postgres** (hosted server mode): shared multi-tenant database with `repo_id` scoping
//!
//! Every trait method accepts a `repo_id` parameter. In SQLite mode this is the
//! local repo's UUID (used for forward-compatibility). In Postgres mode it scopes
//! all queries to the correct tenant.
//!
//! Use [`StorageBackend`] to construct and access the appropriate backend.

pub mod filesystem;
pub mod pagination;
#[cfg(feature = "postgres")]
pub mod postgres;
#[cfg(feature = "s3")]
pub mod s3;
pub mod sqlite;

pub use pagination::{ListQuery, ListResult, SortDirection, SortField};

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

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
use crate::watcher::{DiscoveryEventKind, DiscoveryPreparation, DiscoveryRecord, Watcher};
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
    /// Testable conditions that define when the issue is complete.
    pub acceptance_criteria: Vec<String>,
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
    /// Testable conditions that define when the issue is complete.
    pub acceptance_criteria: Option<Vec<String>>,
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
    /// Per-conflict detail records (file, entity, content snippets).
    pub conflicts: Vec<crate::escalation::EscalationConflict>,
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
    /// Replaces the workspace's full deletion list with this value when `Some`.
    /// Pass the merged list (existing + new entries) rather than just new ones.
    pub deleted_paths: Option<Vec<String>>,
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

/// Subscription filter used to narrow event queries to only the events a
/// client cares about.
///
/// All non-empty fields are ANDed together; within each field the values are
/// ORed (i.e. "event type is A or B, AND workspace is W1 or W2").
/// An empty `Vec` for any field means "no restriction on that dimension".
#[derive(Debug, Clone, Default)]
pub struct EventFilter {
    /// If non-empty, only return events whose `event_type` is one of these.
    pub event_types: Vec<String>,
    /// If non-empty, only return events whose `workspace_id` is one of these.
    pub workspace_ids: Vec<Uuid>,
    /// If non-empty, only return events whose serialised payload contains at
    /// least one of these strings (used for entity-ID filtering).
    pub entity_ids: Vec<String>,
    /// If non-empty, only return events whose serialised payload contains at
    /// least one of these path strings.
    pub paths: Vec<String>,
}

impl EventFilter {
    /// Returns `true` when every dimension is unconstrained (no filtering).
    pub fn is_empty(&self) -> bool {
        self.event_types.is_empty()
            && self.workspace_ids.is_empty()
            && self.entity_ids.is_empty()
            && self.paths.is_empty()
    }

    /// Returns `true` if `event` satisfies all active filter dimensions.
    pub fn matches(&self, event: &Event) -> bool {
        if !self.event_types.is_empty()
            && !self
                .event_types
                .iter()
                .any(|t| t == event.kind.event_type())
        {
            return false;
        }

        if !self.workspace_ids.is_empty() {
            match event.kind.workspace_id() {
                Some(ws) if self.workspace_ids.contains(&ws) => {}
                _ => return false,
            }
        }

        if !self.entity_ids.is_empty() {
            let data = serde_json::to_string(&event.kind).unwrap_or_default();
            if !self.entity_ids.iter().any(|eid| data.contains(eid.as_str())) {
                return false;
            }
        }

        if !self.paths.is_empty() {
            let data = serde_json::to_string(&event.kind).unwrap_or_default();
            if !self.paths.iter().any(|p| data.contains(p.as_str())) {
                return false;
            }
        }

        true
    }
}

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

    /// Returns events with ID greater than `last_id` that match `filter`.
    ///
    /// Implementations should push the filter conditions to the storage layer
    /// rather than loading all events and filtering in memory.  The default
    /// implementation falls back to [`query_since_id`] + in-memory filtering
    /// for backends that do not support server-side filtering.
    async fn query_since_id_filtered(
        &self,
        repo_id: &Uuid,
        last_id: i64,
        filter: &EventFilter,
    ) -> Result<Vec<Event>, StorageError> {
        if filter.is_empty() {
            return self.query_since_id(repo_id, last_id).await;
        }
        let events = self.query_since_id(repo_id, last_id).await?;
        Ok(events.into_iter().filter(|e| filter.matches(e)).collect())
    }

    /// Returns the total number of events for the repo.
    async fn count(&self, repo_id: &Uuid) -> Result<u64, StorageError>;
}

// ── IssueComment ──────────────────────────────────────────────────────────────

/// Re-exported from `crate::issue` for use by storage trait consumers.
pub use crate::issue::IssueComment;

/// Input for creating a new comment.
#[derive(Debug, Clone)]
pub struct NewIssueComment {
    /// Author username or agent ID.
    pub author: String,
    /// Comment body.
    pub body: String,
    /// Whether the author is a `"human"` or `"agent"`. Defaults to `"human"`.
    pub author_type: String,
    /// Optional structured author identifier (e.g. agent instance ID).
    pub author_id: Option<String>,
}

// ── CommentStore ──────────────────────────────────────────────────────────────

/// Storage for issue comments.
#[async_trait]
pub trait CommentStore: Send + Sync {
    /// Creates a new comment on an issue.
    async fn create_comment(
        &self,
        repo_id: &Uuid,
        issue_id: &Uuid,
        comment: NewIssueComment,
    ) -> Result<IssueComment, StorageError>;

    /// Lists comments for an issue, ordered by `created_at` ascending.
    async fn list_comments(
        &self,
        repo_id: &Uuid,
        issue_id: &Uuid,
    ) -> Result<Vec<IssueComment>, StorageError>;
}

// ── IssueLinkStore ────────────────────────────────────────────────────────────

/// The directional relationship type between two issues.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IssueLinkRelationship {
    /// Source issue blocks target issue.
    Blocks,
    /// Source and target issues are related (symmetric).
    RelatesTo,
    /// Source issue duplicates target issue.
    Duplicates,
}

impl IssueLinkRelationship {
    /// Parse from the string stored in the database.
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "blocks" => Some(Self::Blocks),
            "relates-to" => Some(Self::RelatesTo),
            "duplicates" => Some(Self::Duplicates),
            _ => None,
        }
    }

    /// Serialize to the string stored in the database.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Blocks => "blocks",
            Self::RelatesTo => "relates-to",
            Self::Duplicates => "duplicates",
        }
    }

    /// The inverse relationship label (used for bidirectional responses).
    pub fn inverse_str(&self) -> &'static str {
        match self {
            Self::Blocks => "is-blocked-by",
            Self::RelatesTo => "relates-to",
            Self::Duplicates => "is-duplicated-by",
        }
    }
}

/// A directional link between two issues.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueLink {
    /// The issue that owns this link (the "from" side).
    pub source_id: Uuid,
    /// The related issue (the "to" side).
    pub target_id: Uuid,
    /// The nature of the relationship, from source to target.
    pub relationship: IssueLinkRelationship,
}

/// Input for creating a new issue link.
#[derive(Debug, Clone)]
pub struct NewIssueLink {
    /// The target issue to link to.
    pub target_id: Uuid,
    /// The relationship from the caller's issue to the target.
    pub relationship: IssueLinkRelationship,
}

/// Storage for issue links.
#[async_trait]
pub trait IssueLinkStore: Send + Sync {
    /// Creates a directional link between `source_id` and `target_id`.
    ///
    /// Returns `StorageError::AlreadyExists` if the link already exists.
    async fn create_link(
        &self,
        repo_id: &Uuid,
        source_id: &Uuid,
        link: NewIssueLink,
    ) -> Result<IssueLink, StorageError>;

    /// Returns all links where `issue_id` is either source or target,
    /// in canonical DB direction (source_id is who created the link, target_id is the other end).
    /// Callers must compare source_id/target_id against issue_id to determine direction.
    async fn list_links(
        &self,
        repo_id: &Uuid,
        issue_id: &Uuid,
    ) -> Result<Vec<IssueLink>, StorageError>;

    /// Removes the link between `source_id` and `target_id`.
    ///
    /// Succeeds even if the link did not exist.
    async fn delete_link(
        &self,
        repo_id: &Uuid,
        source_id: &Uuid,
        target_id: &Uuid,
    ) -> Result<(), StorageError>;
}

// ── AttachmentStore ───────────────────────────────────────────────────────────

/// Re-exported from `crate::issue` for use by storage trait consumers.
pub use crate::issue::IssueAttachment;

/// Input for creating a new issue attachment record.
#[derive(Debug, Clone)]
pub struct NewIssueAttachment {
    /// Original filename as uploaded.
    pub filename: String,
    /// MIME content type.
    pub content_type: String,
    /// File size in bytes.
    pub size_bytes: i64,
    /// Storage key used to retrieve content from S3.
    pub s3_key: String,
    /// Username or agent ID that uploaded the file.
    pub uploaded_by: String,
}

/// Storage for issue file attachments.
///
/// Metadata only — actual file bytes live in S3.
#[async_trait]
pub trait AttachmentStore: Send + Sync {
    /// Persists attachment metadata and returns the stored record.
    ///
    /// Returns `StorageError::Conflict` if a file with the same filename
    /// already exists on the issue.
    async fn create_attachment(
        &self,
        repo_id: &Uuid,
        issue_id: &Uuid,
        attachment: NewIssueAttachment,
    ) -> Result<IssueAttachment, StorageError>;

    /// Lists all attachments for an issue, ordered by `created_at` ascending.
    async fn list_attachments(
        &self,
        repo_id: &Uuid,
        issue_id: &Uuid,
    ) -> Result<Vec<IssueAttachment>, StorageError>;

    /// Fetches a single attachment by filename.
    async fn get_attachment(
        &self,
        repo_id: &Uuid,
        issue_id: &Uuid,
        filename: &str,
    ) -> Result<IssueAttachment, StorageError>;

    /// Deletes an attachment record.
    ///
    /// The caller is responsible for also deleting the S3 object.
    /// Succeeds even if the attachment did not exist.
    async fn delete_attachment(
        &self,
        repo_id: &Uuid,
        issue_id: &Uuid,
        filename: &str,
    ) -> Result<(), StorageError>;
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

    /// Lists issues, optionally filtered and paginated.
    async fn list_issues(
        &self,
        repo_id: &Uuid,
        filter: &IssueFilter,
        query: &ListQuery,
    ) -> Result<ListResult<Issue>, StorageError>;

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
        query: &ListQuery,
    ) -> Result<ListResult<Escalation>, StorageError>;

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

    /// Returns versions, paginated and sorted according to `query`.
    ///
    /// `ListQuery::default()` returns all versions in chronological order
    /// (oldest first), preserving the previous behaviour for existing callers.
    async fn list_versions(
        &self,
        repo_id: &Uuid,
        query: &ListQuery,
    ) -> Result<ListResult<VersionMeta>, StorageError>;

    /// Returns versions whose numeric suffix is `> since_num` and `<= head_num`,
    /// in ascending numeric order.
    ///
    /// The default implementation calls [`list_versions`] and filters in memory.
    /// Backends with indexed storage should override this for efficiency.
    async fn list_versions_since(
        &self,
        repo_id: &Uuid,
        since_num: u64,
        head_num: u64,
    ) -> Result<Vec<VersionMeta>, StorageError> {
        fn version_num(id: &str) -> u64 {
            id.strip_prefix('v')
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0)
        }
        let mut versions = self.list_versions(repo_id, &ListQuery::default()).await?.items;
        versions.retain(|v| {
            let n = version_num(&v.version_id);
            n > since_num && n <= head_num
        });
        versions.sort_by_key(|v| version_num(&v.version_id));
        Ok(versions)
    }

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

    /// Lists workspaces, optionally paginated.
    ///
    /// If `include_inactive` is false, discarded and merged workspaces are
    /// excluded. `ListQuery::default()` returns all matching workspaces.
    async fn list_workspaces(
        &self,
        repo_id: &Uuid,
        include_inactive: bool,
        query: &ListQuery,
    ) -> Result<ListResult<WorkspaceMeta>, StorageError>;

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
    /// `user_id` associates the key with a user account (RBAC server mode).
    /// `role_override` caps the key's effective permissions at the given role.
    /// `agent_type` is an optional label for the kind of agent (e.g. `"ci"`, `"worker"`).
    /// `expires_at` is an optional expiry timestamp; `None` means the key never expires.
    async fn create_key(
        &self,
        repo_id: Option<&Uuid>,
        name: &str,
        user_id: Option<&Uuid>,
        role_override: Option<&str>,
        agent_type: Option<&str>,
        expires_at: Option<DateTime<Utc>>,
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

    /// Lists all non-revoked keys belonging to a specific user.
    ///
    /// In SQLite (local) mode this returns all keys (no user concept exists).
    async fn list_keys_by_user(&self, user_id: &Uuid) -> Result<Vec<ApiKey>, StorageError>;

    /// Revokes a key by its record ID. Revoked keys are rejected by `validate_key`.
    async fn revoke_key(&self, id: &str) -> Result<(), StorageError>;

    /// Revokes all non-revoked keys scoped to the given repository.
    ///
    /// Returns the number of keys revoked. Only meaningful in Postgres (server)
    /// mode; SQLite mode returns an error.
    async fn revoke_keys_by_repo(&self, repo_id: &Uuid) -> Result<u64, StorageError>;

    /// Revokes all non-revoked keys owned by the given user.
    ///
    /// Returns the number of keys revoked. Only meaningful in Postgres (server)
    /// mode; SQLite mode returns an error.
    async fn revoke_keys_by_user(&self, user_id: &Uuid) -> Result<u64, StorageError>;

    /// Validates a Better Auth session token and returns the associated user ID.
    ///
    /// Queries the `session` table (Better Auth schema) by the `token` column
    /// and verifies the session has not expired. Returns `StorageError::NotFound`
    /// for invalid or expired sessions.
    ///
    /// Only meaningful in Postgres (server) mode; SQLite mode returns an error.
    async fn validate_session(&self, session_token: &str) -> Result<String, StorageError>;

    /// Fetches a Better Auth user's email and display name by their BA user ID.
    ///
    /// Queries the Better Auth `user` table (camelCase columns). Used during
    /// auto-provisioning to seed the vai user record with the correct identity.
    ///
    /// Returns `(email, name)`. Only meaningful in Postgres mode.
    async fn get_better_auth_user(
        &self,
        ba_user_id: &str,
    ) -> Result<(String, String), StorageError>;

    /// Creates and persists a refresh token for `user_id`.
    ///
    /// The plaintext token is returned with a `rt_` prefix and is shown only
    /// once — only its SHA-256 hash is stored. The token expires at `expires_at`.
    ///
    /// Only meaningful in Postgres (server) mode; SQLite mode returns an error.
    async fn create_refresh_token(
        &self,
        user_id: &Uuid,
        expires_at: DateTime<Utc>,
    ) -> Result<String, StorageError>;

    /// Validates a refresh token and returns the associated `user_id`.
    ///
    /// Hashes the plaintext token, looks it up in the `refresh_tokens` table,
    /// and verifies it has not expired and has not been revoked.
    /// Returns `StorageError::NotFound` for invalid, expired, or revoked tokens.
    ///
    /// Only meaningful in Postgres (server) mode; SQLite mode returns an error.
    async fn validate_refresh_token(&self, token: &str) -> Result<Uuid, StorageError>;

    /// Revokes a refresh token by setting its `revoked_at` timestamp.
    ///
    /// Hashes the plaintext token and marks it as revoked in the database.
    /// Returns `StorageError::NotFound` if the token does not exist or is
    /// already revoked.
    ///
    /// Only meaningful in Postgres (server) mode; SQLite mode returns an error.
    async fn revoke_refresh_token(&self, token: &str) -> Result<(), StorageError>;
}

// ── RBAC types ────────────────────────────────────────────────────────────────

/// Role a user holds within an organization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrgRole {
    /// Full control over the org, including billing and deletion.
    Owner,
    /// Manage members, create repos, resolve escalations.
    Admin,
    /// Basic org membership; access determined per repo.
    Member,
}

impl OrgRole {
    /// Parses a stored role string, defaulting to `Member` on unknown values.
    pub fn from_db_str(s: &str) -> Self {
        match s {
            "owner" => OrgRole::Owner,
            "admin" => OrgRole::Admin,
            _ => OrgRole::Member,
        }
    }

    /// Returns the canonical lowercase string stored in the database.
    pub fn as_str(&self) -> &'static str {
        match self {
            OrgRole::Owner => "owner",
            OrgRole::Admin => "admin",
            OrgRole::Member => "member",
        }
    }
}

/// Role a user holds on a specific repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepoRole {
    /// Full control including deletion and collaborator management.
    Owner,
    /// Manage collaborators, workspaces, issues, and escalations.
    Admin,
    /// Create and submit workspaces; create and close issues.
    Write,
    /// Read-only access to all repo data.
    Read,
}

impl RepoRole {
    /// Parses a stored role string, defaulting to `Read` on unknown values.
    pub fn from_db_str(s: &str) -> Self {
        match s {
            "owner" => RepoRole::Owner,
            "admin" => RepoRole::Admin,
            "write" => RepoRole::Write,
            _ => RepoRole::Read,
        }
    }

    /// Returns the canonical lowercase string stored in the database.
    pub fn as_str(&self) -> &'static str {
        match self {
            RepoRole::Owner => "owner",
            RepoRole::Admin => "admin",
            RepoRole::Write => "write",
            RepoRole::Read => "read",
        }
    }

    /// Numeric rank used to compare privilege levels (higher = more permissive).
    ///
    /// Used by permission resolution to return the most permissive role when a
    /// user has both an org-derived role and a direct collaborator role.
    pub fn rank(&self) -> u8 {
        match self {
            RepoRole::Owner => 4,
            RepoRole::Admin => 3,
            RepoRole::Write => 2,
            RepoRole::Read => 1,
        }
    }

    /// Returns the more permissive of two roles.
    pub fn max(a: Self, b: Self) -> Self {
        if a.rank() >= b.rank() { a } else { b }
    }
}

/// An organization that owns repositories and has members.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Organization {
    /// Stable UUID.
    pub id: Uuid,
    /// Human-readable display name.
    pub name: String,
    /// URL-safe unique identifier, e.g. `"acme-corp"`.
    pub slug: String,
    /// When the organization was created.
    pub created_at: DateTime<Utc>,
}

/// A user in the vai system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    /// Stable UUID.
    pub id: Uuid,
    /// Unique email address.
    pub email: String,
    /// Display name.
    pub name: String,
    /// When the user account was created.
    pub created_at: DateTime<Utc>,
    /// Better Auth user ID if this account was auto-provisioned via session exchange.
    pub better_auth_id: Option<String>,
}

/// A user's membership record in an organization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrgMember {
    /// Organization this membership belongs to.
    pub org_id: Uuid,
    /// The member user.
    pub user_id: Uuid,
    /// Role within the organization.
    pub role: OrgRole,
    /// When the membership was created.
    pub created_at: DateTime<Utc>,
}

/// A user's collaborator record on a specific repository.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoCollaborator {
    /// Repository this record belongs to.
    pub repo_id: Uuid,
    /// The collaborator user.
    pub user_id: Uuid,
    /// Role on the repository.
    pub role: RepoRole,
    /// When the collaborator was added.
    pub created_at: DateTime<Utc>,
}

/// Input for creating a new organization.
#[derive(Debug, Clone)]
pub struct NewOrg {
    /// Human-readable display name.
    pub name: String,
    /// URL-safe unique slug.
    pub slug: String,
}

/// Input for creating a new user.
#[derive(Debug, Clone)]
pub struct NewUser {
    /// Unique email address.
    pub email: String,
    /// Display name.
    pub name: String,
    /// Better Auth user ID to link, when provisioning from a session exchange.
    pub better_auth_id: Option<String>,
}

// ── OrgStore ──────────────────────────────────────────────────────────────────

/// Storage for organizations, users, org memberships, and repo collaborators.
///
/// This is the foundation of the RBAC system. Permission resolution logic
/// (PRD 10.1) queries these tables to compute a user's effective role.
#[async_trait]
pub trait OrgStore: Send + Sync {
    // ── Organizations ──────────────────────────────────────────────────────────

    /// Creates a new organization.
    async fn create_org(&self, org: NewOrg) -> Result<Organization, StorageError>;

    /// Fetches an organization by its UUID.
    async fn get_org(&self, org_id: &Uuid) -> Result<Organization, StorageError>;

    /// Fetches an organization by its URL slug.
    async fn get_org_by_slug(&self, slug: &str) -> Result<Organization, StorageError>;

    /// Lists all organizations (used by server-level admin only).
    async fn list_orgs(&self) -> Result<Vec<Organization>, StorageError>;

    /// Permanently deletes an organization and all its repos (cascade).
    async fn delete_org(&self, org_id: &Uuid) -> Result<(), StorageError>;

    // ── Users ─────────────────────────────────────────────────────────────────

    /// Creates a new user.
    async fn create_user(&self, user: NewUser) -> Result<User, StorageError>;

    /// Fetches a user by UUID.
    async fn get_user(&self, user_id: &Uuid) -> Result<User, StorageError>;

    /// Fetches a user by email address.
    async fn get_user_by_email(&self, email: &str) -> Result<User, StorageError>;

    /// Fetches a vai user by their Better Auth external ID.
    ///
    /// Returns [`StorageError::NotFound`] if no user has been linked to this
    /// Better Auth identity yet (i.e. first login — caller should auto-provision).
    async fn get_user_by_external_id(&self, external_id: &str) -> Result<User, StorageError>;

    // ── Org membership ────────────────────────────────────────────────────────

    /// Adds a user as a member of an organization with the given role.
    async fn add_org_member(
        &self,
        org_id: &Uuid,
        user_id: &Uuid,
        role: OrgRole,
    ) -> Result<OrgMember, StorageError>;

    /// Changes an existing member's role within the organization.
    async fn update_org_member(
        &self,
        org_id: &Uuid,
        user_id: &Uuid,
        role: OrgRole,
    ) -> Result<OrgMember, StorageError>;

    /// Removes a user from an organization.
    async fn remove_org_member(&self, org_id: &Uuid, user_id: &Uuid) -> Result<(), StorageError>;

    /// Lists all members of an organization.
    async fn list_org_members(&self, org_id: &Uuid) -> Result<Vec<OrgMember>, StorageError>;

    // ── Org-scoped repo lookup ────────────────────────────────────────────────

    /// Resolves a repository's UUID from its org and name.
    ///
    /// Used by collaborator endpoints that address repos as
    /// `/api/orgs/:org/repos/:repo/collaborators`.
    async fn get_repo_id_in_org(
        &self,
        org_id: &Uuid,
        repo_name: &str,
    ) -> Result<Uuid, StorageError>;

    /// Lists the UUIDs of all repositories belonging to `org_id`.
    ///
    /// Used during user auto-provisioning to grant the new user a default
    /// collaborator role on every existing repo in the organisation.
    async fn list_repo_ids_for_org(&self, org_id: &Uuid) -> Result<Vec<Uuid>, StorageError>;

    /// Lists the UUIDs of **all** repositories in the system, regardless of org.
    ///
    /// Used during user auto-provisioning to ensure standalone repos (with no
    /// org) are also covered when granting a new user their default role.
    async fn list_all_repo_ids(&self) -> Result<Vec<Uuid>, StorageError>;

    /// Returns the number of repos on which `user_id` has a direct collaborator
    /// entry.  Used to detect users that slipped through auto-provisioning and
    /// have no repo access at all.
    async fn count_collaborator_repos(&self, user_id: &Uuid) -> Result<u64, StorageError>;

    // ── Repo collaborators ────────────────────────────────────────────────────

    /// Grants a user a role on a specific repository.
    async fn add_collaborator(
        &self,
        repo_id: &Uuid,
        user_id: &Uuid,
        role: RepoRole,
    ) -> Result<RepoCollaborator, StorageError>;

    /// Changes an existing collaborator's role on a repository.
    async fn update_collaborator(
        &self,
        repo_id: &Uuid,
        user_id: &Uuid,
        role: RepoRole,
    ) -> Result<RepoCollaborator, StorageError>;

    /// Removes a collaborator from a repository.
    async fn remove_collaborator(&self, repo_id: &Uuid, user_id: &Uuid) -> Result<(), StorageError>;

    /// Lists all collaborators on a repository.
    async fn list_collaborators(
        &self,
        repo_id: &Uuid,
    ) -> Result<Vec<RepoCollaborator>, StorageError>;

    // ── Permission resolution ─────────────────────────────────────────────────

    /// Computes the effective [`RepoRole`] for `user_id` on `repo_id`.
    ///
    /// Resolution order (PRD 10.1):
    /// 1. If the repo belongs to an org and the user is an org **owner** or
    ///    **admin**, they receive `Owner` or `Admin` access respectively on
    ///    every repo in that org.
    /// 2. A direct [`repo_collaborators`](RepoCollaborator) entry overrides or
    ///    supplements the org-derived role — the more permissive of the two is
    ///    returned.
    /// 3. Returns `None` when the user has no access to the repo.
    async fn resolve_repo_role(
        &self,
        user_id: &Uuid,
        repo_id: &Uuid,
    ) -> Result<Option<RepoRole>, StorageError>;
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

// ── MemFileStore ──────────────────────────────────────────────────────────────

/// In-memory [`FileStore`] implementation for testing.
///
/// All operations are backed by a `HashMap` protected by a `Mutex`.
/// Suitable for unit tests and integration tests that need a functional
/// file store without real S3 access.
#[derive(Debug, Default)]
pub struct MemFileStore {
    files: Mutex<HashMap<String, Vec<u8>>>,
}

impl MemFileStore {
    /// Creates a new empty `MemFileStore`.
    pub fn new() -> Self {
        Self::default()
    }
}

/// Computes the SHA-256 hex digest of `data`.
fn mem_sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(data);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

#[async_trait]
impl FileStore for MemFileStore {
    async fn put(
        &self,
        _repo_id: &uuid::Uuid,
        path: &str,
        content: &[u8],
    ) -> Result<String, StorageError> {
        let hash = mem_sha256_hex(content);
        self.files.lock().unwrap().insert(path.to_string(), content.to_vec());
        Ok(hash)
    }

    async fn get(
        &self,
        _repo_id: &uuid::Uuid,
        path: &str,
    ) -> Result<Vec<u8>, StorageError> {
        self.files
            .lock()
            .unwrap()
            .get(path)
            .cloned()
            .ok_or_else(|| StorageError::NotFound(path.to_string()))
    }

    async fn list(
        &self,
        _repo_id: &uuid::Uuid,
        prefix: &str,
    ) -> Result<Vec<FileMetadata>, StorageError> {
        let guard = self.files.lock().unwrap();
        let now = Utc::now();
        let result = guard
            .iter()
            .filter(|(k, _)| k.starts_with(prefix))
            .map(|(k, v)| FileMetadata {
                path: k.clone(),
                size: v.len() as u64,
                content_hash: mem_sha256_hex(v),
                updated_at: now,
            })
            .collect();
        Ok(result)
    }

    async fn delete(
        &self,
        _repo_id: &uuid::Uuid,
        path: &str,
    ) -> Result<(), StorageError> {
        self.files.lock().unwrap().remove(path);
        Ok(())
    }

    async fn exists(
        &self,
        _repo_id: &uuid::Uuid,
        path: &str,
    ) -> Result<bool, StorageError> {
        Ok(self.files.lock().unwrap().contains_key(path))
    }
}

// ── WatcherRegistryStore ──────────────────────────────────────────────────────

/// Storage for watcher registration and discovery event records.
///
/// In local (SQLite) mode this delegates to `.vai/watchers.db`.
/// In server (Postgres) mode this uses the `watchers`, `watcher_discoveries`,
/// and `watcher_rate_limits` tables.
#[async_trait]
pub trait WatcherRegistryStore: Send + Sync {
    /// Register a new watcher agent for a repository.
    ///
    /// Returns `StorageError::Conflict` if a watcher with the same `agent_id`
    /// is already registered for this repo.
    async fn register_watcher(
        &self,
        repo_id: &Uuid,
        watcher: Watcher,
    ) -> Result<Watcher, StorageError>;

    /// Fetch a watcher by agent ID.
    ///
    /// Returns `StorageError::NotFound` if no watcher with that ID exists.
    async fn get_watcher(
        &self,
        repo_id: &Uuid,
        agent_id: &str,
    ) -> Result<Watcher, StorageError>;

    /// List all watchers for a repository, most recently registered first.
    async fn list_watchers(&self, repo_id: &Uuid) -> Result<Vec<Watcher>, StorageError>;

    /// Set a watcher's status to `Paused` and return the updated record.
    async fn pause_watcher(
        &self,
        repo_id: &Uuid,
        agent_id: &str,
    ) -> Result<Watcher, StorageError>;

    /// Set a watcher's status to `Active` and return the updated record.
    async fn resume_watcher(
        &self,
        repo_id: &Uuid,
        agent_id: &str,
    ) -> Result<Watcher, StorageError>;

    /// Phase 1 of a two-phase discovery submission.
    ///
    /// Validates the watcher is active, increments the per-hour rate-limit
    /// counter, and checks for an existing open issue with the same dedup key.
    /// Returns a [`DiscoveryPreparation`] the caller uses to decide whether to
    /// create an issue (via `ctx.storage.issues()`) and to call
    /// [`record_discovery`] afterwards.
    async fn prepare_discovery(
        &self,
        repo_id: &Uuid,
        agent_id: &str,
        event: &DiscoveryEventKind,
    ) -> Result<DiscoveryPreparation, StorageError>;

    /// Phase 2 of a two-phase discovery submission: persist the discovery
    /// record and update watcher statistics.
    ///
    /// `record_id`, `dedup_key`, and `received_at` must come from the
    /// [`DiscoveryPreparation`] returned by [`prepare_discovery`].
    #[allow(clippy::too_many_arguments)]
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
    ) -> Result<DiscoveryRecord, StorageError>;
}

// ── StorageBackend factory ────────────────────────────────────────────────────

/// A constructed, ready-to-use storage backend.
///
/// Wraps either [`sqlite::SqliteStorage`] (for local CLI mode) or
/// [`postgres::PostgresStorage`] (for hosted server mode).  All accessor
/// methods return `Arc<dyn Trait>` so callers are fully decoupled from the
/// concrete implementation.
///
/// # Examples
///
/// ```rust,no_run
/// use vai::storage::StorageBackend;
///
/// // Local CLI mode:
/// let backend = StorageBackend::local(".vai");
/// let events = backend.events();
///
/// // Server mode (async):
/// // let backend = StorageBackend::server("postgres://...", 10).await?;
/// ```
#[derive(Clone, Debug)]
pub enum StorageBackend {
    /// Local CLI mode — SQLite + filesystem under a `.vai/` directory.
    Local(Arc<sqlite::SqliteStorage>),
    /// Hosted server mode — Postgres with multi-tenant `repo_id` scoping.
    ///
    /// File storage falls back to [`postgres::PostgresStorage`]'s stub
    /// `FileStore` impl (returns errors).  Use [`StorageBackend::ServerWithS3`]
    /// for a real file store.
    #[cfg(feature = "postgres")]
    Server(Arc<postgres::PostgresStorage>),
    /// Hosted server mode with an S3-compatible file store.
    ///
    /// All database traits delegate to the Postgres backend; [`FileStore`]
    /// delegates to the S3 backend.
    #[cfg(feature = "s3")]
    ServerWithS3(Arc<postgres::PostgresStorage>, Arc<s3::S3FileStore>),
    /// Hosted server mode with an in-memory file store (for testing only).
    ///
    /// All database traits delegate to the Postgres backend; [`FileStore`]
    /// delegates to an in-memory [`MemFileStore`].  This variant activates
    /// the same `S3MergeFs` code path as [`ServerWithS3`] so tests exercise
    /// the real server-mode merge/submit logic without requiring real S3.
    #[cfg(feature = "postgres")]
    ServerWithMemFs(Arc<postgres::PostgresStorage>, Arc<MemFileStore>),
}

impl StorageBackend {
    /// Creates a local SQLite backend rooted at `vai_dir`.
    pub fn local(vai_dir: impl Into<PathBuf>) -> Self {
        StorageBackend::Local(Arc::new(sqlite::SqliteStorage::new(vai_dir)))
    }

    /// Connects to Postgres at `database_url` and returns a server-mode backend.
    ///
    /// `max_connections` controls the pool size (10 is a reasonable default).
    ///
    /// Run migrations separately via [`postgres::PostgresStorage::migrate`]
    /// before serving requests.
    #[cfg(feature = "postgres")]
    pub async fn server(database_url: &str, max_connections: u32) -> Result<Self, StorageError> {
        let storage = postgres::PostgresStorage::connect(database_url, max_connections).await?;
        Ok(StorageBackend::Server(Arc::new(storage)))
    }

    /// Connects to Postgres and an S3-compatible store, returning a server-mode
    /// backend with a functional [`FileStore`].
    ///
    /// The Postgres pool is shared between the database backend and the S3 file
    /// index.  Run migrations (including `20260323000003_file_index.sql`) via
    /// [`postgres::PostgresStorage::migrate`] before serving requests.
    #[cfg(feature = "s3")]
    pub async fn server_with_s3(
        database_url: &str,
        max_connections: u32,
        s3_config: s3::S3Config,
    ) -> Result<Self, StorageError> {
        let pg = postgres::PostgresStorage::connect(database_url, max_connections).await?;
        let pg = Arc::new(pg);
        let file_store =
            s3::S3FileStore::connect(s3_config, pg.pool().clone()).await?;
        Ok(StorageBackend::ServerWithS3(pg, Arc::new(file_store)))
    }

    /// Connects to Postgres and wraps it with an in-memory file store for testing.
    ///
    /// Activates the same `S3MergeFs` submit path as [`StorageBackend::ServerWithS3`]
    /// so integration tests can verify `current/` updates without real S3.
    #[cfg(feature = "postgres")]
    pub async fn server_with_mem_fs(
        database_url: &str,
        max_connections: u32,
    ) -> Result<Self, StorageError> {
        let pg = postgres::PostgresStorage::connect(database_url, max_connections).await?;
        let pg = Arc::new(pg);
        let mem_fs = Arc::new(MemFileStore::new());
        Ok(StorageBackend::ServerWithMemFs(pg, mem_fs))
    }

    /// Returns connection pool statistics, or `None` for local (SQLite) backends.
    #[cfg(feature = "postgres")]
    pub fn pool_stats(&self) -> Option<postgres::PoolStats> {
        match self {
            #[cfg(feature = "postgres")]
            StorageBackend::Server(pg) | StorageBackend::ServerWithMemFs(pg, _) => {
                Some(pg.pool_stats())
            }
            #[cfg(feature = "s3")]
            StorageBackend::ServerWithS3(pg, _) => Some(pg.pool_stats()),
            StorageBackend::Local(_) => None,
        }
    }

    /// Checks database connectivity.
    ///
    /// Runs `SELECT 1` against Postgres for server-mode backends.  Always
    /// returns `Ok(())` for the local SQLite backend.
    pub async fn ping_database(&self) -> Result<(), String> {
        match self {
            StorageBackend::Local(_) => Ok(()),
            #[cfg(feature = "postgres")]
            StorageBackend::Server(pg) | StorageBackend::ServerWithMemFs(pg, _) => {
                pg.ping().await.map_err(|e| e.to_string())
            }
            #[cfg(feature = "s3")]
            StorageBackend::ServerWithS3(pg, _) => {
                pg.ping().await.map_err(|e| e.to_string())
            }
        }
    }

    /// Checks S3 connectivity by listing a health-check prefix.
    ///
    /// Returns `Ok(())` for backends without an S3 file store.
    pub async fn ping_s3(&self) -> Result<(), String> {
        match self {
            #[cfg(feature = "s3")]
            StorageBackend::ServerWithS3(_, s3) => {
                s3.ping().await.map_err(|e| e.to_string())
            }
            _ => Ok(()),
        }
    }

    /// Returns the event log store.
    pub fn events(&self) -> Arc<dyn EventStore> {
        match self {
            StorageBackend::Local(s) => Arc::clone(s) as Arc<dyn EventStore>,
            #[cfg(feature = "postgres")]
            StorageBackend::Server(s) | StorageBackend::ServerWithMemFs(s, _) => {
                Arc::clone(s) as Arc<dyn EventStore>
            }
            #[cfg(feature = "s3")]
            StorageBackend::ServerWithS3(s, _) => Arc::clone(s) as Arc<dyn EventStore>,
        }
    }

    /// Returns the issue store.
    pub fn issues(&self) -> Arc<dyn IssueStore> {
        match self {
            StorageBackend::Local(s) => Arc::clone(s) as Arc<dyn IssueStore>,
            #[cfg(feature = "postgres")]
            StorageBackend::Server(s) | StorageBackend::ServerWithMemFs(s, _) => {
                Arc::clone(s) as Arc<dyn IssueStore>
            }
            #[cfg(feature = "s3")]
            StorageBackend::ServerWithS3(s, _) => Arc::clone(s) as Arc<dyn IssueStore>,
        }
    }

    /// Returns the issue comment store.
    pub fn comments(&self) -> Arc<dyn CommentStore> {
        match self {
            StorageBackend::Local(s) => Arc::clone(s) as Arc<dyn CommentStore>,
            #[cfg(feature = "postgres")]
            StorageBackend::Server(s) | StorageBackend::ServerWithMemFs(s, _) => {
                Arc::clone(s) as Arc<dyn CommentStore>
            }
            #[cfg(feature = "s3")]
            StorageBackend::ServerWithS3(s, _) => Arc::clone(s) as Arc<dyn CommentStore>,
        }
    }

    /// Returns the issue link store.
    pub fn links(&self) -> Arc<dyn IssueLinkStore> {
        match self {
            StorageBackend::Local(s) => Arc::clone(s) as Arc<dyn IssueLinkStore>,
            #[cfg(feature = "postgres")]
            StorageBackend::Server(s) | StorageBackend::ServerWithMemFs(s, _) => {
                Arc::clone(s) as Arc<dyn IssueLinkStore>
            }
            #[cfg(feature = "s3")]
            StorageBackend::ServerWithS3(s, _) => Arc::clone(s) as Arc<dyn IssueLinkStore>,
        }
    }

    /// Returns the escalation store.
    pub fn escalations(&self) -> Arc<dyn EscalationStore> {
        match self {
            StorageBackend::Local(s) => Arc::clone(s) as Arc<dyn EscalationStore>,
            #[cfg(feature = "postgres")]
            StorageBackend::Server(s) | StorageBackend::ServerWithMemFs(s, _) => {
                Arc::clone(s) as Arc<dyn EscalationStore>
            }
            #[cfg(feature = "s3")]
            StorageBackend::ServerWithS3(s, _) => Arc::clone(s) as Arc<dyn EscalationStore>,
        }
    }

    /// Returns the semantic graph store.
    pub fn graph(&self) -> Arc<dyn GraphStore> {
        match self {
            StorageBackend::Local(s) => Arc::clone(s) as Arc<dyn GraphStore>,
            #[cfg(feature = "postgres")]
            StorageBackend::Server(s) | StorageBackend::ServerWithMemFs(s, _) => {
                Arc::clone(s) as Arc<dyn GraphStore>
            }
            #[cfg(feature = "s3")]
            StorageBackend::ServerWithS3(s, _) => Arc::clone(s) as Arc<dyn GraphStore>,
        }
    }

    /// Returns the version history store.
    pub fn versions(&self) -> Arc<dyn VersionStore> {
        match self {
            StorageBackend::Local(s) => Arc::clone(s) as Arc<dyn VersionStore>,
            #[cfg(feature = "postgres")]
            StorageBackend::Server(s) | StorageBackend::ServerWithMemFs(s, _) => {
                Arc::clone(s) as Arc<dyn VersionStore>
            }
            #[cfg(feature = "s3")]
            StorageBackend::ServerWithS3(s, _) => Arc::clone(s) as Arc<dyn VersionStore>,
        }
    }

    /// Returns the workspace metadata store.
    pub fn workspaces(&self) -> Arc<dyn WorkspaceStore> {
        match self {
            StorageBackend::Local(s) => Arc::clone(s) as Arc<dyn WorkspaceStore>,
            #[cfg(feature = "postgres")]
            StorageBackend::Server(s) | StorageBackend::ServerWithMemFs(s, _) => {
                Arc::clone(s) as Arc<dyn WorkspaceStore>
            }
            #[cfg(feature = "s3")]
            StorageBackend::ServerWithS3(s, _) => Arc::clone(s) as Arc<dyn WorkspaceStore>,
        }
    }

    /// Returns the API key (auth) store.
    pub fn auth(&self) -> Arc<dyn AuthStore> {
        match self {
            StorageBackend::Local(s) => Arc::clone(s) as Arc<dyn AuthStore>,
            #[cfg(feature = "postgres")]
            StorageBackend::Server(s) | StorageBackend::ServerWithMemFs(s, _) => {
                Arc::clone(s) as Arc<dyn AuthStore>
            }
            #[cfg(feature = "s3")]
            StorageBackend::ServerWithS3(s, _) => Arc::clone(s) as Arc<dyn AuthStore>,
        }
    }

    /// Returns the organization and RBAC store.
    ///
    /// Only meaningful for Postgres-backed variants — the local SQLite backend
    /// returns a stub that errors on use (RBAC is not supported in local mode).
    pub fn orgs(&self) -> Arc<dyn OrgStore> {
        match self {
            StorageBackend::Local(s) => Arc::clone(s) as Arc<dyn OrgStore>,
            #[cfg(feature = "postgres")]
            StorageBackend::Server(s) | StorageBackend::ServerWithMemFs(s, _) => {
                Arc::clone(s) as Arc<dyn OrgStore>
            }
            #[cfg(feature = "s3")]
            StorageBackend::ServerWithS3(s, _) => Arc::clone(s) as Arc<dyn OrgStore>,
        }
    }

    /// Returns the issue attachment metadata store.
    ///
    /// Only meaningful for Postgres-backed variants — the local SQLite backend
    /// returns a stub that errors on write and returns empty lists on read.
    pub fn attachments(&self) -> Arc<dyn AttachmentStore> {
        match self {
            StorageBackend::Local(s) => Arc::clone(s) as Arc<dyn AttachmentStore>,
            #[cfg(feature = "postgres")]
            StorageBackend::Server(s) | StorageBackend::ServerWithMemFs(s, _) => {
                Arc::clone(s) as Arc<dyn AttachmentStore>
            }
            #[cfg(feature = "s3")]
            StorageBackend::ServerWithS3(s, _) => Arc::clone(s) as Arc<dyn AttachmentStore>,
        }
    }

    /// Returns the file content store.
    ///
    /// - [`StorageBackend::Local`]: filesystem-backed store under `.vai/`.
    /// - [`StorageBackend::Server`]: stub that returns errors (use `ServerWithS3`).
    /// - [`StorageBackend::ServerWithS3`]: S3-backed store with Postgres index.
    /// - [`StorageBackend::ServerWithMemFs`]: in-memory store (for testing).
    pub fn files(&self) -> Arc<dyn FileStore> {
        match self {
            StorageBackend::Local(s) => Arc::clone(s) as Arc<dyn FileStore>,
            #[cfg(feature = "postgres")]
            StorageBackend::Server(s) => Arc::clone(s) as Arc<dyn FileStore>,
            #[cfg(feature = "s3")]
            StorageBackend::ServerWithS3(_, f) => Arc::clone(f) as Arc<dyn FileStore>,
            #[cfg(feature = "postgres")]
            StorageBackend::ServerWithMemFs(_, f) => Arc::clone(f) as Arc<dyn FileStore>,
        }
    }

    /// Returns the watcher registry store.
    ///
    /// - [`StorageBackend::Local`]: delegates to `.vai/watchers.db` (SQLite).
    /// - [`StorageBackend::Server`] / [`StorageBackend::ServerWithS3`] /
    ///   [`StorageBackend::ServerWithMemFs`]: Postgres-backed.
    pub fn watchers(&self) -> Arc<dyn WatcherRegistryStore> {
        match self {
            StorageBackend::Local(s) => Arc::clone(s) as Arc<dyn WatcherRegistryStore>,
            #[cfg(feature = "postgres")]
            StorageBackend::Server(s) | StorageBackend::ServerWithMemFs(s, _) => {
                Arc::clone(s) as Arc<dyn WatcherRegistryStore>
            }
            #[cfg(feature = "s3")]
            StorageBackend::ServerWithS3(s, _) => Arc::clone(s) as Arc<dyn WatcherRegistryStore>,
        }
    }
}
