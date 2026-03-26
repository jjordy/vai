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
pub mod postgres;
pub mod s3;
pub mod sqlite;

use std::path::PathBuf;
use std::sync::Arc;

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
    /// Issue IDs that must be closed before this issue is available.
    pub depends_on: Vec<Uuid>,
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
        let mut versions = self.list_versions(repo_id).await?;
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
    /// `user_id` associates the key with a user account (RBAC server mode).
    /// `role_override` caps the key's effective permissions at the given role.
    async fn create_key(
        &self,
        repo_id: Option<&Uuid>,
        name: &str,
        user_id: Option<&Uuid>,
        role_override: Option<&str>,
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
    pub fn from_str(s: &str) -> Self {
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
    pub fn from_str(s: &str) -> Self {
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
    Server(Arc<postgres::PostgresStorage>),
    /// Hosted server mode with an S3-compatible file store.
    ///
    /// All database traits delegate to the Postgres backend; [`FileStore`]
    /// delegates to the S3 backend.
    ServerWithS3(Arc<postgres::PostgresStorage>, Arc<s3::S3FileStore>),
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

    /// Returns connection pool statistics, or `None` for local (SQLite) backends.
    pub fn pool_stats(&self) -> Option<postgres::PoolStats> {
        match self {
            StorageBackend::Server(pg) | StorageBackend::ServerWithS3(pg, _) => {
                Some(pg.pool_stats())
            }
            StorageBackend::Local(_) => None,
        }
    }

    /// Returns the event log store.
    pub fn events(&self) -> Arc<dyn EventStore> {
        match self {
            StorageBackend::Local(s) => Arc::clone(s) as Arc<dyn EventStore>,
            StorageBackend::Server(s) | StorageBackend::ServerWithS3(s, _) => {
                Arc::clone(s) as Arc<dyn EventStore>
            }
        }
    }

    /// Returns the issue store.
    pub fn issues(&self) -> Arc<dyn IssueStore> {
        match self {
            StorageBackend::Local(s) => Arc::clone(s) as Arc<dyn IssueStore>,
            StorageBackend::Server(s) | StorageBackend::ServerWithS3(s, _) => {
                Arc::clone(s) as Arc<dyn IssueStore>
            }
        }
    }

    /// Returns the escalation store.
    pub fn escalations(&self) -> Arc<dyn EscalationStore> {
        match self {
            StorageBackend::Local(s) => Arc::clone(s) as Arc<dyn EscalationStore>,
            StorageBackend::Server(s) | StorageBackend::ServerWithS3(s, _) => {
                Arc::clone(s) as Arc<dyn EscalationStore>
            }
        }
    }

    /// Returns the semantic graph store.
    pub fn graph(&self) -> Arc<dyn GraphStore> {
        match self {
            StorageBackend::Local(s) => Arc::clone(s) as Arc<dyn GraphStore>,
            StorageBackend::Server(s) | StorageBackend::ServerWithS3(s, _) => {
                Arc::clone(s) as Arc<dyn GraphStore>
            }
        }
    }

    /// Returns the version history store.
    pub fn versions(&self) -> Arc<dyn VersionStore> {
        match self {
            StorageBackend::Local(s) => Arc::clone(s) as Arc<dyn VersionStore>,
            StorageBackend::Server(s) | StorageBackend::ServerWithS3(s, _) => {
                Arc::clone(s) as Arc<dyn VersionStore>
            }
        }
    }

    /// Returns the workspace metadata store.
    pub fn workspaces(&self) -> Arc<dyn WorkspaceStore> {
        match self {
            StorageBackend::Local(s) => Arc::clone(s) as Arc<dyn WorkspaceStore>,
            StorageBackend::Server(s) | StorageBackend::ServerWithS3(s, _) => {
                Arc::clone(s) as Arc<dyn WorkspaceStore>
            }
        }
    }

    /// Returns the API key (auth) store.
    pub fn auth(&self) -> Arc<dyn AuthStore> {
        match self {
            StorageBackend::Local(s) => Arc::clone(s) as Arc<dyn AuthStore>,
            StorageBackend::Server(s) | StorageBackend::ServerWithS3(s, _) => {
                Arc::clone(s) as Arc<dyn AuthStore>
            }
        }
    }

    /// Returns the organization and RBAC store.
    ///
    /// Only meaningful for Postgres-backed variants — the local SQLite backend
    /// returns a stub that errors on use (RBAC is not supported in local mode).
    pub fn orgs(&self) -> Arc<dyn OrgStore> {
        match self {
            StorageBackend::Local(s) => Arc::clone(s) as Arc<dyn OrgStore>,
            StorageBackend::Server(s) | StorageBackend::ServerWithS3(s, _) => {
                Arc::clone(s) as Arc<dyn OrgStore>
            }
        }
    }

    /// Returns the file content store.
    ///
    /// - [`StorageBackend::Local`]: filesystem-backed store under `.vai/`.
    /// - [`StorageBackend::Server`]: stub that returns errors (use `ServerWithS3`).
    /// - [`StorageBackend::ServerWithS3`]: S3-backed store with Postgres index.
    pub fn files(&self) -> Arc<dyn FileStore> {
        match self {
            StorageBackend::Local(s) => Arc::clone(s) as Arc<dyn FileStore>,
            StorageBackend::Server(s) => Arc::clone(s) as Arc<dyn FileStore>,
            StorageBackend::ServerWithS3(_, f) => Arc::clone(f) as Arc<dyn FileStore>,
        }
    }
}
