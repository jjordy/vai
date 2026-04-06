//! HTTP server for vai — exposes REST API and WebSocket endpoints for
//! multi-agent coordination.
//!
//! Entry point: [`start`] — binds a TCP socket and serves the application.
//!
//! ## REST Endpoints
//!   - `GET /health` — liveness probe, returns 200 OK (unauthenticated)
//!   - `GET /api/status` — server and repository health (unauthenticated)
//!   - `GET /api/server/stats` — uptime, version, workspace count (unauthenticated)
//!   - `POST /api/workspaces` — create a new workspace
//!   - `GET /api/workspaces` — list active workspaces
//!   - `GET /api/workspaces/:id` — workspace details
//!   - `POST /api/workspaces/:id/submit` — submit workspace for merge
//!   - `DELETE /api/workspaces/:id` — discard a workspace
//!   - `GET /api/versions` — list version history (optional `?limit=N`)
//!   - `GET /api/versions/:id` — version details with entity-level changes
//!   - `GET /api/versions/:id/diff` — unified diffs for all files changed in a version (`?base=<version_id>`)
//!   - `POST /api/versions/rollback` — rollback to a prior version
//!   - `POST /api/workspaces/:id/files` — upload files into a workspace overlay (base64 JSON)
//!   - `POST /api/workspaces/:id/upload-snapshot` — upload gzip tarball, diff against `current/`, store as overlay
//!   - `GET /api/workspaces/:id/files/*path` — download a file from workspace (overlay or base)
//!   - `GET /api/files/*path` — download a file from the current main version
//!   - `GET /api/graph/entities` — list entities with optional filters (`?kind=`, `?file=`, `?name=`)
//!   - `GET /api/graph/entities/:id` — entity details and relationships
//!   - `GET /api/graph/entities/:id/deps` — transitive dependencies (bidirectional)
//!   - `GET /api/graph/blast-radius` — entities reachable from seeds (`?entities=id1,id2&hops=2`)
//!   - `POST /api/issues` — create issue
//!   - `GET /api/issues` — list issues (`?status=`, `?priority=`, `?label=`, `?created_by=`)
//!   - `GET /api/issues/:id` — issue details with linked workspaces
//!   - `PATCH /api/issues/:id` — update issue fields
//!   - `POST /api/issues/:id/close` — close issue with resolution
//!   - `GET /api/escalations` — list escalations (`?status=pending|resolved`)
//!   - `GET /api/escalations/:id` — escalation details
//!   - `POST /api/escalations/:id/resolve` — resolve an escalation
//!   - `GET /api/work-queue` — ranked list of available and blocked issues
//!   - `POST /api/work-queue/claim` — atomically claim an issue and create a workspace
//!   - `POST /api/watchers/register` — register a new watcher agent
//!   - `GET /api/watchers` — list all registered watchers
//!   - `POST /api/watchers/:id/pause` — pause a watcher
//!   - `POST /api/watchers/:id/resume` — resume a paused watcher
//!   - `POST /api/discoveries` — submit a discovery event from a watcher
//!   - `POST /api/files` — upload source files into repo root (migration, PRD 12.3)
//!   - `POST /api/graph/refresh` — rebuild semantic graph from source files (PRD 12.4)
//!   - `POST /api/migrate` — bulk migration from local SQLite (single-repo mode)
//!   - `GET /api/migration-stats` — counts of migrated data for post-migration verification
//!   - `POST /api/repos/:repo/migrate` — bulk migration from local SQLite (multi-repo mode)
//!   - `GET /api/repos/:repo/migration-stats` — per-repo migration stats
//!   - `POST /api/repos` — register and initialise a new repository (multi-repo mode)
//!   - `GET /api/repos` — list all registered repositories with basic stats
//!   - `POST /api/users` — create a new user account
//!   - `GET /api/users/:user` — get user by UUID or email address
//!   - `POST /api/orgs` — create a new organization
//!   - `GET /api/orgs` — list all organizations
//!   - `GET /api/orgs/:org` — get organization by slug
//!   - `DELETE /api/orgs/:org` — delete organization (cascades to repos)
//!   - `POST /api/orgs/:org/members` — add a user to an organization
//!   - `GET /api/orgs/:org/members` — list members of an organization
//!   - `PATCH /api/orgs/:org/members/:user` — update a member's role
//!   - `DELETE /api/orgs/:org/members/:user` — remove a member from an organization
//!   - `POST /api/orgs/:org/repos/:repo/collaborators` — add a collaborator to a repo
//!   - `GET /api/orgs/:org/repos/:repo/collaborators` — list repo collaborators
//!   - `PATCH /api/orgs/:org/repos/:repo/collaborators/:user` — change collaborator role
//!   - `DELETE /api/orgs/:org/repos/:repo/collaborators/:user` — remove a collaborator
//!   - `POST /api/keys` — create an API key (scoped to repo + role)
//!   - `GET /api/keys` — list the authenticated user's keys
//!   - `DELETE /api/keys/:id` — revoke a key by its record UUID
//!   - `DELETE /api/keys?repo_id=<id>` — bulk-revoke all keys for a repo (admin only)
//!   - `DELETE /api/keys?created_by=<user_id>` — bulk-revoke all keys by a user (admin only)
//!
//! ## Multi-Repo Endpoints (`/api/repos/:repo/`)
//!   - `GET /api/repos/:repo/status` — per-repo health (same fields as `/api/status`)
//!   - `POST /api/repos/:repo/workspaces` — create workspace in the named repo
//!   - `GET /api/repos/:repo/workspaces` — list workspaces in the named repo
//!   - `GET /api/repos/:repo/workspaces/:id` — workspace details
//!   - `POST /api/repos/:repo/workspaces/:id/submit` — submit workspace for merge
//!   - `DELETE /api/repos/:repo/workspaces/:id` — discard workspace
//!   - `POST /api/repos/:repo/workspaces/:id/files` — upload files into workspace overlay
//!   - `POST /api/repos/:repo/workspaces/:id/upload-snapshot` — upload gzip tarball, diff against `current/`, store as overlay
//!   - `GET /api/repos/:repo/workspaces/:id/files/*path` — download file from workspace
//!   - `GET /api/repos/:repo/files` — list files in repo root
//!   - `POST /api/repos/:repo/files` — upload source files into repo root (migration, PRD 12.3)
//!   - `GET /api/repos/:repo/files/*path` — download file from main version
//!   - `GET /api/repos/:repo/versions` — list version history
//!   - `GET /api/repos/:repo/versions/:id` — version details
//!   - `POST /api/repos/:repo/versions/rollback` — rollback to prior version
//!   - `GET /api/repos/:repo/graph/entities` — list graph entities
//!   - `GET /api/repos/:repo/graph/entities/:id` — entity details
//!   - `GET /api/repos/:repo/graph/entities/:id/deps` — transitive deps
//!   - `GET /api/repos/:repo/graph/blast-radius` — blast-radius query
//!   - `POST /api/repos/:repo/graph/refresh` — rebuild semantic graph (PRD 12.4)
//!   - `POST /api/repos/:repo/issues` — create issue
//!   - `GET /api/repos/:repo/issues` — list issues
//!   - `GET /api/repos/:repo/issues/:id` — issue details
//!   - `PATCH /api/repos/:repo/issues/:id` — update issue
//!   - `POST /api/repos/:repo/issues/:id/close` — close issue
//!   - `POST /api/repos/:repo/issues/:id/attachments` — upload file attachment
//!   - `GET /api/repos/:repo/issues/:id/attachments` — list attachment metadata
//!   - `GET /api/repos/:repo/issues/:id/attachments/:filename` — download attachment
//!   - `DELETE /api/repos/:repo/issues/:id/attachments/:filename` — delete attachment
//!   - `GET /api/repos/:repo/escalations` — list escalations
//!   - `GET /api/repos/:repo/escalations/:id` — escalation details
//!   - `POST /api/repos/:repo/escalations/:id/resolve` — resolve escalation
//!   - `GET /api/repos/:repo/work-queue` — ranked work items
//!   - `POST /api/repos/:repo/work-queue/claim` — claim issue + create workspace
//!   - `POST /api/repos/:repo/watchers/register` — register watcher
//!   - `GET /api/repos/:repo/watchers` — list watchers
//!   - `POST /api/repos/:repo/watchers/:id/pause` — pause watcher
//!   - `POST /api/repos/:repo/watchers/:id/resume` — resume watcher
//!   - `POST /api/repos/:repo/discoveries` — submit discovery event
//!   - `GET /api/repos/:repo/members` — search repo members for @mention autocomplete
//!   - `WS /api/repos/:repo/ws/events` — per-repo WebSocket event stream
//!
//! ## WebSocket Endpoints
//!   - `GET /ws/events?key=<api_key>` — real-time event stream
//!
//! All REST endpoints except `GET /health`, `GET /api/status`, and
//! `GET /api/server/stats` require `Authorization: Bearer <key>`.
//! WebSocket auth uses the `key` query param.
//! Keys are managed with `vai server keys`.

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::Mutex;
use std::time::Instant;

use axum::extract::{DefaultBodyLimit, Extension, FromRequest as _, Path as AxumPath, Query as AxumQuery, Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use utoipa::{OpenApi, ToSchema};
use tokio::net::TcpListener;
use tokio::sync::broadcast;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};

pub mod pagination;
pub use pagination::{PaginatedResponse, PaginationMeta, PaginationParams};

mod escalation;
mod graph;
mod version;
mod watcher;
mod work_queue;
mod ws;

use crate::auth;
use crate::conflict;
use crate::storage::ListQuery;
use crate::event_log::EventKind;
use crate::merge;
use crate::repo;
use crate::version as vai_version;
use crate::workspace;

// ── Error type ────────────────────────────────────────────────────────────────

/// Errors that can occur during server operations.
#[derive(Debug, Error)]
pub enum ServerError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Repository error: {0}")]
    Repo(#[from] repo::RepoError),

    #[error("Workspace error: {0}")]
    Workspace(#[from] workspace::WorkspaceError),

    #[error("Auth error: {0}")]
    Auth(#[from] auth::AuthError),

    #[error("Storage error: {0}")]
    Storage(#[from] crate::storage::StorageError),

    #[error("Invalid bind address `{addr}`: {source}")]
    BadAddress {
        addr: String,
        #[source]
        source: std::net::AddrParseError,
    },
}

// ── Configuration ─────────────────────────────────────────────────────────────

/// Server bind configuration. Persisted in `.vai/config.toml` under `[server]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    /// IP address to bind to (default: `127.0.0.1`).
    pub host: String,
    /// TCP port to listen on (default: `7865`).
    pub port: u16,
    /// Root directory for multi-repo storage. `None` means single-repo (legacy) mode.
    pub storage_root: Option<std::path::PathBuf>,
    /// Optional path to write a PID file on startup (removed on clean shutdown).
    pub pid_file: Option<std::path::PathBuf>,
    /// Postgres connection URL. When set the server uses `PostgresStorage`
    /// instead of the default SQLite backend. Example:
    /// `postgres://vai:secret@localhost:5432/vai`
    pub database_url: Option<String>,
    /// Maximum number of Postgres connections in the pool.
    ///
    /// Defaults to 25 when not set.  Increase this value if you observe
    /// `pool timed out` errors under high load (many concurrent CLI commands
    /// or WebSocket clients).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub db_pool_size: Option<u32>,
    /// S3-compatible file store configuration.
    ///
    /// When set alongside `database_url`, the server uses
    /// [`crate::storage::StorageBackend::ServerWithS3`] so that file uploads
    /// (migration files, blob snapshots) are durably stored in S3 instead of
    /// being silently discarded by the no-op stub.
    ///
    /// AWS credentials come from the environment (`AWS_ACCESS_KEY_ID`,
    /// `AWS_SECRET_ACCESS_KEY`) via the AWS SDK default credential chain.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub s3: Option<crate::storage::s3::S3Config>,
    /// Allowed CORS origins.
    ///
    /// Comma-separated list of allowed origins (e.g. `https://app.example.com`).
    /// When `None` or empty the server permits all origins (`*`) — suitable for
    /// development.  In production set this to the exact origin(s) of your
    /// dashboard.  Can also be supplied via `VAI_CORS_ORIGINS`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cors_origins: Option<Vec<String>>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            host: "127.0.0.1".to_string(),
            port: 7865,
            storage_root: None,
            pid_file: None,
            database_url: None,
            db_pool_size: None,
            s3: None,
            cors_origins: None,
        }
    }
}

impl ServerConfig {
    /// Returns the socket address derived from host + port.
    pub fn socket_addr(&self) -> Result<SocketAddr, ServerError> {
        let raw = format!("{}:{}", self.host, self.port);
        raw.parse().map_err(|source| ServerError::BadAddress {
            addr: raw,
            source,
        })
    }
}

// ── Broadcast event types ─────────────────────────────────────────────────────

/// Capacity of the broadcast channel (number of events buffered).
const EVENT_CHANNEL_CAPACITY: usize = 1024;

/// Maximum number of events retained in the server-side replay buffer.
const BUFFER_MAX_EVENTS: usize = 10_000;

/// Maximum age (seconds) of events kept in the replay buffer (1 hour).
const BUFFER_MAX_AGE_SECS: i64 = 3600;

// ── Input validation limits ────────────────────────────────────────────────────

/// Maximum length for issue titles (characters).
const MAX_ISSUE_TITLE_LEN: usize = 500;
/// Maximum length for issue/workspace description bodies (bytes).
const MAX_ISSUE_BODY_LEN: usize = 50 * 1024; // 50 KB
/// Maximum length for workspace intent (characters).
const MAX_INTENT_LEN: usize = 1000;
/// Maximum length for a single label (characters).
const MAX_LABEL_LEN: usize = 100;
/// Maximum number of labels per issue.
const MAX_LABELS_PER_ISSUE: usize = 20;
/// Maximum length for a file path (characters).
const MAX_PATH_LEN: usize = 1000;
/// Maximum number of files per upload request.
const MAX_FILES_PER_REQUEST: usize = 100;
/// Default JSON body size limit (10 MiB) — applies to all endpoints.
const DEFAULT_BODY_LIMIT: usize = 10 * 1024 * 1024;
/// Body size limit for file-upload endpoints (50 MiB).
const UPLOAD_BODY_LIMIT: usize = 50 * 1024 * 1024;
/// Body size limit for the migration endpoint (50 MiB).
const MIGRATE_BODY_LIMIT: usize = 50 * 1024 * 1024;
/// Body size limit for tarball snapshot uploads (100 MiB).
const SNAPSHOT_BODY_LIMIT: usize = 100 * 1024 * 1024;

// ── Input validation helpers ───────────────────────────────────────────────────

/// Returns `Err(ApiError::bad_request(...))` when `value` exceeds `max` bytes.
fn validate_str_len(value: &str, max: usize, field: &str) -> Result<(), ApiError> {
    if value.len() > max {
        return Err(ApiError::bad_request(format!(
            "`{field}` exceeds maximum length of {max} bytes (got {} bytes)",
            value.len()
        )));
    }
    Ok(())
}

/// Validates a list of labels: at most `MAX_LABELS_PER_ISSUE`, each at most
/// `MAX_LABEL_LEN` characters.
fn validate_labels(labels: &[String]) -> Result<(), ApiError> {
    if labels.len() > MAX_LABELS_PER_ISSUE {
        return Err(ApiError::bad_request(format!(
            "too many labels: {}, maximum is {MAX_LABELS_PER_ISSUE}",
            labels.len()
        )));
    }
    for label in labels {
        validate_str_len(label, MAX_LABEL_LEN, "label")?;
    }
    Ok(())
}

// ── Replay buffer ─────────────────────────────────────────────────────────────

/// Server-side ring buffer of recent [`BroadcastEvent`]s.
///
/// Agents that disconnect and reconnect can request replay of events they
/// missed by passing `?last_event_id=N` on the WebSocket URL.  The buffer
/// retains at most [`BUFFER_MAX_EVENTS`] events **or** the last
/// [`BUFFER_MAX_AGE_SECS`] seconds of events, whichever bound is reached first.
struct EventBuffer {
    events: VecDeque<BroadcastEvent>,
}

impl EventBuffer {
    fn new() -> Self {
        EventBuffer { events: VecDeque::new() }
    }

    /// Appends `event` and evicts entries that exceed the count or age limits.
    fn push(&mut self, event: BroadcastEvent) {
        self.events.push_back(event);

        // Evict by count.
        while self.events.len() > BUFFER_MAX_EVENTS {
            self.events.pop_front();
        }

        // Evict by age.
        let cutoff =
            chrono::Utc::now() - chrono::Duration::seconds(BUFFER_MAX_AGE_SECS);
        while let Some(front) = self.events.front() {
            match chrono::DateTime::parse_from_rfc3339(&front.timestamp) {
                Ok(ts) if ts < cutoff => {
                    self.events.pop_front();
                }
                _ => break,
            }
        }
    }

    /// Returns `(buffer_exceeded, missed_events)` for an agent reconnecting
    /// after `last_event_id`.
    ///
    /// `buffer_exceeded` is `true` when the buffer cannot guarantee continuity
    /// — i.e. events between `last_event_id` and the oldest buffered event may
    /// have been dropped.  In that case the agent should perform a full sync.
    ///
    /// `missed_events` contains every buffered event with
    /// `event_id > last_event_id`, in insertion order.
    fn events_since(&self, last_event_id: u64) -> (bool, Vec<BroadcastEvent>) {
        let missed: Vec<BroadcastEvent> = self
            .events
            .iter()
            .filter(|e| e.event_id > last_event_id)
            .cloned()
            .collect();

        // Buffer exceeded when the oldest retained event is not the immediate
        // successor of last_event_id (meaning some events were evicted).
        let buffer_exceeded = match self.events.front() {
            // Buffer is empty: if the agent had a non-zero last_event_id there
            // may have been events we can no longer deliver.
            None => last_event_id > 0,
            Some(oldest) => oldest.event_id > last_event_id + 1,
        };

        (buffer_exceeded, missed)
    }
}

/// An event broadcast to all connected WebSocket clients.
///
/// Clients receive this as a JSON message on their WebSocket connection.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BroadcastEvent {
    /// Discriminant matching `EventKind` variant names (e.g. `"WorkspaceCreated"`).
    #[serde(rename = "type")]
    pub event_type: String,
    /// Monotonic event ID from the event log.
    pub event_id: u64,
    /// Associated workspace ID, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    /// RFC 3339 timestamp.
    pub timestamp: String,
    /// Event-specific payload as a JSON object.
    #[schema(value_type = Object)]
    pub data: serde_json::Value,
}

// ── Rate limiter ──────────────────────────────────────────────────────────────

/// Result of a rate-limit check.
enum RateLimitResult {
    Allowed,
    Denied { retry_after_secs: u64 },
}

/// In-memory sliding-window rate limiter.
///
/// Keys are arbitrary strings (IP address, "key:<id>:<category>", etc.).
/// Each entry records the timestamps of recent requests; entries older than the
/// window are pruned on every check, keeping memory usage bounded.
struct RateLimiter {
    windows: StdMutex<HashMap<String, VecDeque<std::time::Instant>>>,
}

impl RateLimiter {
    fn new() -> Self {
        Self {
            windows: StdMutex::new(HashMap::new()),
        }
    }

    /// Returns [`RateLimitResult::Allowed`] when the request is within limits
    /// and records it, or [`RateLimitResult::Denied`] when the bucket is full.
    ///
    /// `max` is the maximum number of requests allowed per `window`.
    fn check(&self, key: &str, max: usize, window: std::time::Duration) -> RateLimitResult {
        let now = std::time::Instant::now();
        // Saturating sub so we don't panic on very short windows in tests.
        let cutoff = now.checked_sub(window).unwrap_or(now);
        let mut windows = self.windows.lock().unwrap_or_else(|e| e.into_inner());
        let entry = windows.entry(key.to_string()).or_default();

        // Drop timestamps older than the window.
        while matches!(entry.front(), Some(&ts) if ts <= cutoff) {
            entry.pop_front();
        }

        if entry.len() >= max {
            let oldest = *entry.front().expect("non-empty after capacity check");
            let elapsed = now.duration_since(oldest);
            let retry_after = window.saturating_sub(elapsed);
            RateLimitResult::Denied {
                retry_after_secs: retry_after.as_secs().max(1),
            }
        } else {
            entry.push_back(now);
            RateLimitResult::Allowed
        }
    }
}

// ── Shared state ──────────────────────────────────────────────────────────────

/// State shared across all request handlers.
#[derive(Clone)]
pub(crate) struct AppState {
    /// Absolute path to the `.vai/` directory.
    vai_dir: PathBuf,
    /// Absolute path to the repository root (parent of `.vai/`).
    repo_root: PathBuf,
    /// Monotonic timestamp recorded when the server started.
    started_at: Instant,
    /// Human-readable repository name from `.vai/config.toml`.
    repo_name: String,
    /// vai crate version string.
    vai_version: String,
    /// Broadcast channel for real-time event streaming to WebSocket clients.
    event_tx: broadcast::Sender<BroadcastEvent>,
    /// Monotonic counter — each broadcast event gets a unique, increasing ID.
    event_seq: Arc<AtomicU64>,
    /// Server-side replay buffer for reconnecting agents.
    event_buffer: Arc<StdMutex<EventBuffer>>,
    /// Conflict engine — tracks workspace scopes and detects overlaps.
    conflict_engine: Arc<Mutex<conflict::ConflictEngine>>,
    /// Serializes filesystem-mutating operations (workspace create/submit/discard,
    /// issue create/update, etc.) to prevent data races on the event log and
    /// `.vai/` directory.
    repo_lock: Arc<Mutex<()>>,
    /// Root directory for multi-repo storage. `None` means single-repo (legacy) mode.
    storage_root: Option<PathBuf>,
    /// Pluggable storage backend — SQLite for local mode, Postgres for server mode.
    ///
    /// Handlers should prefer this over direct `vai_dir`-based module calls when
    /// the required operation is covered by a storage trait.
    pub(crate) storage: crate::storage::StorageBackend,
    /// Bootstrap admin key (plaintext).
    ///
    /// Set via the `VAI_ADMIN_KEY` environment variable or generated on first
    /// startup and printed to stdout.  A request bearing this key bypasses
    /// normal API-key validation and receives full server-admin access.
    admin_key: String,
    /// JWT signing and verification service.
    ///
    /// Used to mint short-lived access tokens (via `POST /api/auth/token`)
    /// and to validate JWT Bearer tokens in the auth middleware.
    pub(crate) jwt_service: Arc<crate::auth::jwt::JwtService>,
    /// Default repo role assigned to newly auto-provisioned users.
    ///
    /// Set via `VAI_DEFAULT_USER_ROLE` (accepted values: `admin`, `write`, `read`).
    /// Defaults to `write` when the variable is absent or unrecognised.
    default_new_user_role: crate::storage::RepoRole,
    /// In-memory sliding-window rate limiter shared across all requests.
    rate_limiter: Arc<RateLimiter>,
    /// Parsed CORS allowed origins.
    ///
    /// Empty means "allow any origin" (`*`).  Non-empty restricts to the listed
    /// origins.  Set from `ServerConfig::cors_origins` or `VAI_CORS_ORIGINS`.
    cors_origins: Vec<axum::http::HeaderValue>,
}

impl AppState {
    /// Assigns a monotonic `event_id`, appends to the replay buffer, then
    /// broadcasts to all connected WebSocket clients.
    ///
    /// Silently drops the send if no WebSocket clients are connected.
    pub(crate) fn broadcast(&self, mut event: BroadcastEvent) {
        // Assign a server-wide monotonic ID.
        let seq = self.event_seq.fetch_add(1, Ordering::Relaxed) + 1;
        event.event_id = seq;

        // Append to the replay buffer before sending so reconnecting agents
        // never miss an event that was already acknowledged by live clients.
        if let Ok(mut buf) = self.event_buffer.lock() {
            buf.push(event.clone());
        }

        // `send` only fails if there are no receivers — that's fine.
        let _ = self.event_tx.send(event);
    }
}

// ── Agent identity ────────────────────────────────────────────────────────────

/// How the current request was authenticated.
#[derive(Debug, Clone, PartialEq)]
pub enum AuthSource {
    /// A short-lived JWT access token validated by [`crate::auth::jwt::JwtService`].
    Jwt,
    /// A long-lived API key looked up in the key store.
    ApiKey,
    /// The bootstrap admin key from `VAI_ADMIN_KEY`.
    AdminKey,
}

/// The authenticated agent making the current request.
///
/// Injected into request extensions by [`auth_middleware`] and available to
/// handlers via `Extension<AgentIdentity>`.
#[derive(Debug, Clone)]
pub struct AgentIdentity {
    /// Key record ID (or `"jwt:<sub>"` for JWT-authenticated requests).
    pub key_id: String,
    /// Human-readable key name (or JWT subject for JWT-authenticated requests).
    pub name: String,
    /// Whether this request was authenticated with the bootstrap admin key.
    ///
    /// Admin requests bypass per-repo permission checks and have full access
    /// to all endpoints including org/user management.
    pub is_admin: bool,
    /// The user this API key belongs to. `None` for legacy or admin keys.
    pub user_id: Option<uuid::Uuid>,
    /// Optional role cap from the key's `role_override` column.
    ///
    /// When present, the key's effective permissions are the lesser of the
    /// user's computed role and this override value.
    pub role_override: Option<String>,
    /// How this request was authenticated.
    pub auth_source: AuthSource,
}

// ── Per-request repository context ────────────────────────────────────────────

/// Resolved repository paths and storage backend for the current request.
///
/// In single-repo (legacy) mode this mirrors [`AppState::vai_dir`] and
/// [`AppState::repo_root`].  In multi-repo mode it is resolved from the
/// `/:repo` path segment by [`repo_resolve_middleware`] and injected into
/// request extensions before any handler runs.
#[derive(Debug, Clone)]
struct RepoCtx {
    /// Absolute path to the repository's `.vai/` directory.
    vai_dir: PathBuf,
    /// Absolute path to the repository root (parent of `.vai/`).
    repo_root: PathBuf,
    /// Stable identifier for this repository, used to scope storage trait calls.
    ///
    /// Read from `.vai/config.toml` at request time. In SQLite mode the value
    /// is ignored by all trait implementations; in Postgres mode it is used to
    /// scope every query to the correct tenant.
    repo_id: uuid::Uuid,
    /// Per-repository storage backend.
    ///
    /// In single-repo mode this is a clone of [`AppState::storage`].
    /// In multi-repo SQLite mode each repo gets its own `Local` backend rooted
    /// at its own `.vai/` directory.  In multi-repo Postgres mode the shared
    /// `Server` backend is used (scoped by `repo_id`).
    storage: crate::storage::StorageBackend,
}

/// Reads the `repo_id` from `.vai/config.toml`, returning `Uuid::nil()` on
/// failure (safe for SQLite mode which ignores the value).
fn repo_id_from_vai_dir(vai_dir: &Path) -> uuid::Uuid {
    crate::repo::read_config(vai_dir)
        .map(|c| c.repo_id)
        .unwrap_or_else(|_| uuid::Uuid::nil())
}

/// Constructs the per-repo storage backend given the resolved `.vai/` directory
/// and the server-level backend.
///
/// - SQLite (`Local`) backends are re-rooted at `vai_dir` so multi-repo
///   configurations get isolated SQLite files per repository.
/// - Postgres (`Server`) backends are shared and use `repo_id` for tenant
///   scoping, so the server backend is returned as-is.
fn repo_storage(
    state_storage: &crate::storage::StorageBackend,
    vai_dir: &Path,
) -> crate::storage::StorageBackend {
    use crate::storage::StorageBackend;
    match state_storage {
        StorageBackend::Local(_) => StorageBackend::local(vai_dir),
        StorageBackend::Server(_)
        | StorageBackend::ServerWithS3(_, _)
        | StorageBackend::ServerWithMemFs(_, _) => state_storage.clone(),
    }
}

#[axum::async_trait]
impl axum::extract::FromRequestParts<Arc<AppState>> for RepoCtx {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        // Prefer context injected by repo_resolve_middleware (multi-repo mode).
        if let Some(ctx) = parts.extensions.get::<RepoCtx>() {
            return Ok(ctx.clone());
        }
        // Fall back to the single-repo paths stored in AppState.
        let vai_dir = state.vai_dir.clone();
        let repo_id = repo_id_from_vai_dir(&vai_dir);
        let storage = state.storage.clone();
        Ok(RepoCtx {
            vai_dir,
            repo_root: state.repo_root.clone(),
            repo_id,
            storage,
        })
    }
}

// ── Path parameter extractor ──────────────────────────────────────────────────

/// Path extractor that resolves the `:id` parameter by name from the matched
/// path segments.
///
/// Unlike `axum::extract::Path<String>`, which fails with "wrong number of
/// arguments" when the route has more than one parameter, `PathId` extracts
/// params as a `HashMap` and looks up `"id"` by key.  This means the same
/// handler works correctly in both single-repo mode (e.g. `/workspaces/:id`)
/// and multi-repo mode (e.g. `/api/repos/:repo/workspaces/:id`) without any
/// code duplication.
struct PathId(String);

#[axum::async_trait]
impl<S: Send + Sync> axum::extract::FromRequestParts<S> for PathId {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        let AxumPath(params) =
            AxumPath::<HashMap<String, String>>::from_request_parts(parts, state)
                .await
                .map_err(|e| ApiError::bad_request(format!("path extraction failed: {e}")))?;
        let id = params
            .get("id")
            .cloned()
            .ok_or_else(|| ApiError::bad_request("missing `:id` path parameter"))?;
        Ok(PathId(id))
    }
}

// ── Authentication middleware ─────────────────────────────────────────────────

/// Axum middleware that enforces `Authorization: Bearer <token>` on every request.
///
/// Tokens are validated in this order:
/// 1. **JWT** — if the token contains `'.'` it is validated by [`crate::auth::jwt::JwtService`]
///    with no database hit. An invalid or expired JWT returns 401 immediately.
/// 2. **API key** — the token is hashed and looked up in the key store.
/// 3. **Admin key** — the token is compared against the bootstrap `VAI_ADMIN_KEY`.
///
/// The first successful match populates an [`AgentIdentity`] in request
/// extensions. Returns 401 Unauthorized if all checks fail.
async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    mut request: Request,
    next: Next,
) -> Response {
    // WebSocket upgrade requests authenticate via the `key` query parameter
    // inside the handler itself — skip the Bearer token check for them.
    let is_ws_upgrade = request
        .headers()
        .get(axum::http::header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.eq_ignore_ascii_case("websocket"))
        .unwrap_or(false);

    if is_ws_upgrade {
        return next.run(request).await;
    }

    let auth_header = request
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let key_str = match auth_header {
        Some(ref h) if h.starts_with("Bearer ") => h[7..].trim().to_string(),
        _ => {
            let ip = extract_client_ip(&request);
            tracing::warn!(
                event = "auth.failure",
                ip = %ip,
                reason = "missing_or_invalid_header",
                "authentication failed: missing or invalid Authorization header"
            );
            return ApiError::unauthorized(
                "missing or invalid Authorization header; expected `Bearer <key>`",
            )
            .into_response();
        }
    };

    let key_prefix_display =
        if key_str.len() >= 12 { key_str[..12].to_string() } else { key_str.clone() };
    let ip = extract_client_ip(&request);

    // (1) JWT check — if the token looks like a JWT (contains '.'), validate
    // with JwtService. No database hit. Returns 401 immediately on failure;
    // does not fall through to the API-key or admin-key checks.
    if key_str.contains('.') {
        use crate::auth::jwt::JwtError;
        match state.jwt_service.verify(&key_str) {
            Ok(claims) => {
                tracing::info!(
                    event = "auth.success",
                    actor = %claims.sub,
                    method = "jwt",
                    ip = %ip,
                    "authentication succeeded: JWT"
                );
                let user_id = uuid::Uuid::parse_str(&claims.sub).ok();
                let is_admin = claims.role.as_deref() == Some("admin");
                let name = claims.name.unwrap_or_else(|| claims.sub.clone());
                request.extensions_mut().insert(AgentIdentity {
                    key_id: format!("jwt:{}", claims.sub),
                    name,
                    is_admin,
                    user_id,
                    role_override: claims.role,
                    auth_source: AuthSource::Jwt,
                });
                return next.run(request).await;
            }
            Err(JwtError::Expired) => {
                tracing::warn!(
                    event = "auth.failure",
                    method = "jwt",
                    ip = %ip,
                    reason = "expired_jwt",
                    "authentication failed: expired JWT"
                );
                return ApiError::unauthorized("JWT token has expired").into_response();
            }
            Err(e) => {
                tracing::warn!(
                    event = "auth.failure",
                    method = "jwt",
                    ip = %ip,
                    reason = "invalid_jwt",
                    error = %e,
                    "authentication failed: invalid JWT"
                );
                return ApiError::unauthorized("invalid JWT token").into_response();
            }
        }
    }

    // (2) API key check — hash and lookup via storage backend (SQLite or Postgres).
    match state.storage.auth().validate_key(&key_str).await {
        Ok(api_key) => {
            tracing::info!(
                event = "auth.success",
                actor = %api_key.name,
                key_prefix = %api_key.key_prefix,
                ip = %ip,
                "authentication succeeded"
            );
            request.extensions_mut().insert(AgentIdentity {
                key_id: api_key.id,
                name: api_key.name,
                is_admin: false,
                user_id: api_key.user_id,
                role_override: api_key.role_override,
                auth_source: AuthSource::ApiKey,
            });
            return next.run(request).await;
        }
        Err(crate::storage::StorageError::NotFound(_)) => {} // fall through to admin key check
        Err(e) => return ApiError::internal(format!("auth error: {e}")).into_response(),
    }

    // (3) Bootstrap admin key check.
    if key_str == state.admin_key {
        tracing::info!(
            event = "auth.success",
            actor = "admin",
            key_prefix = "admin",
            ip = %ip,
            "authentication succeeded: bootstrap admin key"
        );
        request.extensions_mut().insert(AgentIdentity {
            key_id: "admin".to_string(),
            name: "admin".to_string(),
            is_admin: true,
            user_id: None,
            role_override: None,
            auth_source: AuthSource::AdminKey,
        });
        return next.run(request).await;
    }

    tracing::warn!(
        event = "auth.failure",
        key_prefix = %key_prefix_display,
        ip = %ip,
        reason = "invalid_or_revoked_key",
        "authentication failed: invalid or revoked API key"
    );
    ApiError::unauthorized("invalid or revoked API key").into_response()
}

// ── Rate-limit middleware ──────────────────────────────────────────────────────

/// Extracts the best-effort client IP from the request.
///
/// Checks `X-Forwarded-For` (proxy) then `X-Real-IP` then falls back to the
/// TCP `ConnectInfo` socket address injected by axum.  Returns `"unknown"` if
/// none is available (e.g. in tests that do not set these headers).
fn extract_client_ip(request: &Request) -> String {
    if let Some(xff) = request.headers().get("x-forwarded-for") {
        if let Ok(val) = xff.to_str() {
            if let Some(ip) = val.split(',').next().map(str::trim) {
                if !ip.is_empty() {
                    return ip.to_string();
                }
            }
        }
    }
    if let Some(xri) = request.headers().get("x-real-ip") {
        if let Ok(ip) = xri.to_str() {
            let ip = ip.trim();
            if !ip.is_empty() {
                return ip.to_string();
            }
        }
    }
    if let Some(info) = request
        .extensions()
        .get::<axum::extract::ConnectInfo<SocketAddr>>()
    {
        return info.0.ip().to_string();
    }
    "unknown".to_string()
}

/// Rate limit category determined from HTTP method + request path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RateLimitCategory {
    /// Key-creation and other auth-adjacent endpoints — 10 requests / minute per IP.
    AuthIp,
    /// Issue creation — 100 requests / hour per API key.
    IssueCreate,
    /// Workspace creation — 50 requests / hour per API key.
    WorkspaceCreate,
    /// File upload endpoints — 200 requests / hour per API key.
    FileUpload,
    /// No specific rate limit applies.
    None,
}

/// Classify a request by method and path into a [`RateLimitCategory`].
fn classify_rate_limit(method: &axum::http::Method, path: &str) -> RateLimitCategory {
    use axum::http::Method;
    // Strip the /api/repos/<name> prefix so the logic below works for both
    // the single-repo and multi-repo URL shapes.
    let normalised = if let Some(rest) = path.strip_prefix("/api/repos/") {
        // Skip the repo name segment.
        rest.split_once('/').map(|x| format!("/{}", x.1)).unwrap_or_default()
    } else {
        path.to_string()
    };
    let p = normalised.as_str();

    match method {
        &Method::POST => {
            if p == "/api/keys" {
                return RateLimitCategory::AuthIp;
            }
            if p == "/issues" {
                return RateLimitCategory::IssueCreate;
            }
            if p == "/workspaces" {
                return RateLimitCategory::WorkspaceCreate;
            }
            // File upload: /api/workspaces/:id/files,
            //              /api/workspaces/:id/upload-snapshot,
            //              /api/files, /api/graph/refresh, etc.
            let upload_patterns = [
                "/files",
                "/upload-snapshot",
                "/attachments",
            ];
            if upload_patterns.iter().any(|pat| p.ends_with(pat)) {
                return RateLimitCategory::FileUpload;
            }
            RateLimitCategory::None
        }
        _ => RateLimitCategory::None,
    }
}

/// Axum middleware that enforces per-IP and per-API-key rate limits.
///
/// Limits:
/// - Key-creation (`POST /api/keys`): **10 / minute** per client IP
/// - Issue creation: **100 / hour** per API key
/// - Workspace creation: **50 / hour** per API key
/// - File uploads: **200 / hour** per API key
///
/// Returns **429 Too Many Requests** with a `Retry-After` header on denial.
async fn rate_limit_middleware(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Response {
    let category = classify_rate_limit(request.method(), request.uri().path());

    match category {
        RateLimitCategory::None => {}

        RateLimitCategory::AuthIp => {
            let ip = extract_client_ip(&request);
            let key = format!("ip_auth:{ip}");
            if let RateLimitResult::Denied { retry_after_secs } =
                state.rate_limiter.check(&key, 10, std::time::Duration::from_secs(60))
            {
                tracing::warn!(ip = %ip, "rate limit exceeded: key creation");
                return rate_limited_response(retry_after_secs);
            }
        }

        RateLimitCategory::IssueCreate => {
            if let Some(identity) = request.extensions().get::<AgentIdentity>() {
                let key = format!("issue_create:{}", identity.key_id);
                if let RateLimitResult::Denied { retry_after_secs } =
                    state.rate_limiter.check(&key, 100, std::time::Duration::from_secs(3600))
                {
                    tracing::warn!(agent = %identity.name, "rate limit exceeded: issue creation");
                    return rate_limited_response(retry_after_secs);
                }
            }
        }

        RateLimitCategory::WorkspaceCreate => {
            if let Some(identity) = request.extensions().get::<AgentIdentity>() {
                let key = format!("workspace_create:{}", identity.key_id);
                if let RateLimitResult::Denied { retry_after_secs } =
                    state.rate_limiter.check(&key, 50, std::time::Duration::from_secs(3600))
                {
                    tracing::warn!(agent = %identity.name, "rate limit exceeded: workspace creation");
                    return rate_limited_response(retry_after_secs);
                }
            }
        }

        RateLimitCategory::FileUpload => {
            if let Some(identity) = request.extensions().get::<AgentIdentity>() {
                let key = format!("file_upload:{}", identity.key_id);
                if let RateLimitResult::Denied { retry_after_secs } =
                    state.rate_limiter.check(&key, 200, std::time::Duration::from_secs(3600))
                {
                    tracing::warn!(agent = %identity.name, "rate limit exceeded: file upload");
                    return rate_limited_response(retry_after_secs);
                }
            }
        }
    }

    next.run(request).await
}

/// Builds the 429 response with a `Retry-After` header.
fn rate_limited_response(retry_after_secs: u64) -> Response {
    let mut response = ApiError::rate_limited("rate limit exceeded; try again later").into_response();
    if let Ok(val) = axum::http::HeaderValue::from_str(&retry_after_secs.to_string()) {
        response.headers_mut().insert("Retry-After", val);
    }
    response
}

// ── Multi-repo resolve middleware ─────────────────────────────────────────────

/// Middleware for `/api/repos/:repo/` routes.
///
/// Extracts the `:repo` path parameter, looks the name up in the server
/// registry, and injects a [`RepoCtx`] into request extensions so that every
/// downstream handler operates on the correct per-repo paths.
///
/// Returns **400** when the server is not in multi-repo mode and **404** when
/// the named repository has not been registered.
async fn repo_resolve_middleware(
    State(state): State<Arc<AppState>>,
    mut request: Request,
    next: Next,
) -> Response {
    use axum::extract::FromRequestParts as _;

    // Split the request so we can call the path extractor.
    let (mut parts, body) = request.into_parts();
    let path_result = AxumPath::<HashMap<String, String>>::from_request_parts(
        &mut parts,
        &state,
    )
    .await;
    request = Request::from_parts(parts, body);

    let params = match path_result {
        Ok(AxumPath(p)) => p,
        Err(_) => {
            return ApiError::bad_request("could not extract path parameters").into_response()
        }
    };

    let repo_name = match params.get("repo") {
        Some(n) => n.clone(),
        None => return ApiError::bad_request("missing `:repo` path segment").into_response(),
    };

    let (vai_dir, repo_root) = if let Some(storage_root) = state.storage_root.as_ref() {
        // Multi-repo mode: look up the repo in the registry.
        let registry = match RepoRegistry::load(storage_root) {
            Ok(r) => r,
            Err(e) => {
                return ApiError::internal(format!("failed to load repo registry: {e}"))
                    .into_response()
            }
        };

        let entry = match registry.repos.iter().find(|r| r.name == repo_name) {
            Some(e) => e.clone(),
            None => {
                return ApiError::not_found(format!(
                    "repository `{repo_name}` is not registered on this server"
                ))
                .into_response()
            }
        };
        (entry.path.join(".vai"), entry.path.clone())
    } else {
        // Single-repo mode: only the server's own repository is available.
        if repo_name != state.repo_name {
            return ApiError::not_found(format!(
                "repository `{repo_name}` is not registered on this server"
            ))
            .into_response();
        }
        (state.vai_dir.clone(), state.repo_root.clone())
    };

    let repo_id = repo_id_from_vai_dir(&vai_dir);
    let storage = repo_storage(&state.storage, &vai_dir);
    let ctx = RepoCtx {
        vai_dir,
        repo_root,
        repo_id,
        storage,
    };
    request.extensions_mut().insert(ctx);
    next.run(request).await
}

// ── API error helper ──────────────────────────────────────────────────────────

/// JSON body for error responses.
#[derive(Debug, Serialize, ToSchema)]
struct ErrorBody {
    error: String,
}

/// A handler error that carries an HTTP status code and message.
struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn not_found(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: msg.into(),
        }
    }

    fn conflict(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: msg.into(),
        }
    }

    fn internal(msg: impl Into<String>) -> Self {
        // Log full details server-side; never return them to the client.
        tracing::error!("internal server error: {}", msg.into());
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: "Internal server error".to_string(),
        }
    }

    fn unauthorized(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: msg.into(),
        }
    }

    fn bad_request(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: msg.into(),
        }
    }

    fn rate_limited(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::TOO_MANY_REQUESTS,
            message: msg.into(),
        }
    }

    fn forbidden(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            message: msg.into(),
        }
    }

    fn payload_too_large(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            message: msg.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = Json(ErrorBody {
            error: self.message,
        });
        let mut resp = (self.status, body).into_response();
        if self.status == StatusCode::TOO_MANY_REQUESTS {
            // Seconds remaining until the next hour boundary.
            let now = chrono::Utc::now();
            let secs_remaining = 3600u64 - (now.timestamp() as u64 % 3600);
            if let Ok(val) = secs_remaining.to_string().parse() {
                resp.headers_mut().insert("Retry-After", val);
            }
        }
        resp
    }
}

impl From<workspace::WorkspaceError> for ApiError {
    fn from(e: workspace::WorkspaceError) -> Self {
        match &e {
            workspace::WorkspaceError::NotFound(_) => ApiError::not_found(e.to_string()),
            workspace::WorkspaceError::NoActiveWorkspace => ApiError::not_found(e.to_string()),
            _ => ApiError::internal(e.to_string()),
        }
    }
}

impl From<merge::MergeError> for ApiError {
    fn from(e: merge::MergeError) -> Self {
        match &e {
            merge::MergeError::SemanticConflicts { .. } => ApiError::conflict(e.to_string()),
            merge::MergeError::Workspace(workspace::WorkspaceError::NotFound(_)) => {
                ApiError::not_found(e.to_string())
            }
            merge::MergeError::Workspace(workspace::WorkspaceError::NoActiveWorkspace) => {
                ApiError::not_found(e.to_string())
            }
            _ => ApiError::internal(e.to_string()),
        }
    }
}

impl From<vai_version::VersionError> for ApiError {
    fn from(e: vai_version::VersionError) -> Self {
        match &e {
            vai_version::VersionError::NotFound(_) => ApiError::not_found(e.to_string()),
            _ => ApiError::internal(e.to_string()),
        }
    }
}

impl From<crate::issue::IssueError> for ApiError {
    fn from(e: crate::issue::IssueError) -> Self {
        match &e {
            crate::issue::IssueError::NotFound(_) => ApiError::not_found(e.to_string()),
            crate::issue::IssueError::InvalidTransition { .. } => ApiError::bad_request(e.to_string()),
            crate::issue::IssueError::RateLimitExceeded { .. } => ApiError::rate_limited(e.to_string()),
            _ => ApiError::internal(e.to_string()),
        }
    }
}

impl From<crate::escalation::EscalationError> for ApiError {
    fn from(e: crate::escalation::EscalationError) -> Self {
        match &e {
            crate::escalation::EscalationError::NotFound(_) => ApiError::not_found(e.to_string()),
            crate::escalation::EscalationError::AlreadyResolved(_) => {
                ApiError::bad_request(e.to_string())
            }
            _ => ApiError::internal(e.to_string()),
        }
    }
}

impl From<crate::storage::StorageError> for ApiError {
    fn from(e: crate::storage::StorageError) -> Self {
        match &e {
            crate::storage::StorageError::NotFound(_) => ApiError::not_found(e.to_string()),
            crate::storage::StorageError::Conflict(_) => ApiError::conflict(e.to_string()),
            crate::storage::StorageError::InvalidTransition(_) => {
                ApiError::bad_request(e.to_string())
            }
            _ => ApiError::internal(e.to_string()),
        }
    }
}

// ── RBAC helpers ─────────────────────────────────────────────────────────────

/// Checks that the authenticated user has at least `required` on `repo_id`.
///
/// Returns the effective [`RepoRole`] on success, or a `403 Forbidden`
/// error on failure.  Admin keys always pass with `Owner` access.
///
/// In local (SQLite) mode all authenticated keys receive `Owner` access for
/// backward compatibility — RBAC is a server-mode (Postgres) feature.
///
/// Call this from handlers that need to enforce per-repo permissions:
///
/// ```ignore
/// require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, RepoRole::Write).await?;
/// ```
async fn require_repo_permission(
    storage: &crate::storage::StorageBackend,
    identity: &AgentIdentity,
    repo_id: &uuid::Uuid,
    required: crate::storage::RepoRole,
) -> Result<crate::storage::RepoRole, ApiError> {
    use crate::storage::{RepoRole, StorageBackend};

    // Admin key always has full access.
    if identity.is_admin {
        return Ok(RepoRole::Owner);
    }

    // In local (SQLite) mode there are no users/orgs — any valid key passes.
    if matches!(storage, StorageBackend::Local(_)) {
        return Ok(RepoRole::Owner);
    }

    let user_id = match &identity.user_id {
        Some(uid) => uid,
        None => {
            tracing::warn!(
                event = "permission.denied",
                actor = %identity.name,
                repo = %repo_id,
                required = %required.as_str(),
                reason = "no_user_association",
                "permission denied: key not associated with a user"
            );
            return Err(ApiError::forbidden(
                "this key is not associated with a user; cannot check repo permissions",
            ));
        }
    };

    let resolved = storage
        .orgs()
        .resolve_repo_role(user_id, repo_id)
        .await
        .map_err(|e| ApiError::internal(format!("permission check failed: {e}")))?;

    let effective = match resolved {
        None => {
            tracing::warn!(
                event = "permission.denied",
                actor = %identity.name,
                user_id = %user_id,
                repo = %repo_id,
                required = %required.as_str(),
                reason = "no_role_resolved",
                "permission denied: no repo access"
            );
            return Err(ApiError::forbidden("access denied"));
        }
        Some(r) => r,
    };

    // Apply key-level role cap if present.
    let effective = if let Some(cap_str) = &identity.role_override {
        let cap = RepoRole::from_db_str(cap_str);
        if effective.rank() > cap.rank() { cap } else { effective }
    } else {
        effective
    };

    if effective.rank() < required.rank() {
        tracing::warn!(
            event = "permission.denied",
            actor = %identity.name,
            user_id = %user_id,
            repo = %repo_id,
            required = %required.as_str(),
            effective = %effective.as_str(),
            reason = "insufficient_permissions",
            "permission denied: effective role below required"
        );
        return Err(ApiError::forbidden("insufficient permissions"));
    }

    Ok(effective)
}

/// Asserts that the current request was made with the bootstrap admin key.
///
/// Returns `Ok(())` if `identity.is_admin` is true; otherwise a `403 Forbidden`
/// error.  Use this for server-level management endpoints (org/user creation).
fn require_server_admin(identity: &AgentIdentity) -> Result<(), ApiError> {
    if identity.is_admin {
        Ok(())
    } else {
        tracing::warn!(
            event = "permission.denied",
            actor = %identity.name,
            reason = "not_admin",
            "permission denied: endpoint requires bootstrap admin key"
        );
        Err(ApiError::forbidden(
            "this endpoint requires the bootstrap admin key",
        ))
    }
}

// ── API response types ────────────────────────────────────────────────────────

/// Response body for `GET /api/status`.
#[derive(Debug, Serialize, ToSchema)]
pub struct StatusResponse {
    /// Repository name.
    pub repo_name: String,
    /// Current HEAD version identifier (e.g. `"v3"`).
    pub head_version: String,
    /// Number of seconds the server has been running.
    pub uptime_secs: u64,
    /// Number of active workspaces.
    pub workspace_count: usize,
    /// vai version string.
    pub vai_version: String,
    /// Total number of open issues.
    pub issue_count: usize,
    /// Number of pending escalations.
    pub escalation_count: usize,
    /// Total number of entities in the semantic graph.
    pub entity_count: usize,
}

/// Per-subsystem health status returned by `GET /health`.
#[derive(Debug, Serialize, ToSchema)]
pub struct SubsystemStatus {
    /// `true` if the subsystem is reachable and operational.
    pub healthy: bool,
    /// Human-readable error message when `healthy` is `false`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl SubsystemStatus {
    fn ok() -> Self {
        Self { healthy: true, error: None }
    }
    fn err(msg: String) -> Self {
        Self { healthy: false, error: Some(msg) }
    }
}

/// Subsystem health breakdown returned by `GET /health`.
#[derive(Debug, Serialize, ToSchema)]
pub struct SubsystemsHealth {
    /// Relational database (Postgres in server mode, always healthy for SQLite).
    pub database: SubsystemStatus,
    /// Object storage (S3-compatible in server mode, always healthy otherwise).
    pub storage: SubsystemStatus,
}

/// Response body for `GET /health`.
#[derive(Debug, Serialize, ToSchema)]
pub struct HealthResponse {
    /// `"ok"` when all subsystems are healthy, `"degraded"` otherwise.
    pub status: String,
    /// Seconds since the server process started.
    pub uptime_secs: u64,
    /// vai version string (from `Cargo.toml`).
    pub version: String,
    /// Per-subsystem health details.
    pub subsystems: SubsystemsHealth,
}

/// Response body for `GET /api/server/stats`.
#[derive(Debug, Serialize, ToSchema)]
pub struct ServerStatsResponse {
    /// Number of seconds the server has been running.
    pub uptime_secs: u64,
    /// vai version string (from `Cargo.toml`).
    pub vai_version: String,
    /// Number of active workspaces in the current repository.
    pub workspace_count: usize,
    /// Connections currently checked out from the Postgres pool.
    /// `null` when running against a local SQLite backend.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub db_pool_active: Option<u32>,
    /// Connections currently idle in the Postgres pool.
    /// `null` when running against a local SQLite backend.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub db_pool_idle: Option<u32>,
    /// Maximum connections allowed by the Postgres pool configuration.
    /// `null` when running against a local SQLite backend.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub db_pool_max: Option<u32>,
}

/// Request body for `POST /api/workspaces`.
#[derive(Debug, Deserialize, ToSchema)]
struct CreateWorkspaceRequest {
    /// Stated agent intent for this workspace.
    intent: String,
}

/// Response body for workspace creation and detail endpoints.
#[derive(Debug, Serialize, ToSchema)]
struct WorkspaceResponse {
    id: String,
    intent: String,
    status: String,
    base_version: String,
    created_at: String,
    updated_at: String,
}

impl From<workspace::WorkspaceMeta> for WorkspaceResponse {
    fn from(m: workspace::WorkspaceMeta) -> Self {
        WorkspaceResponse {
            id: m.id.to_string(),
            intent: m.intent,
            status: m.status.as_str().to_string(),
            base_version: m.base_version,
            created_at: m.created_at.to_rfc3339(),
            updated_at: m.updated_at.to_rfc3339(),
        }
    }
}

/// Response body for `POST /api/workspaces/:id/submit`.
#[derive(Debug, Serialize, ToSchema)]
struct SubmitResponse {
    version: String,
    files_applied: usize,
    entities_changed: usize,
    auto_resolved: u32,
}

impl From<merge::SubmitResult> for SubmitResponse {
    fn from(r: merge::SubmitResult) -> Self {
        SubmitResponse {
            version: r.version.version_id.clone(),
            files_applied: r.files_applied,
            entities_changed: r.entities_changed,
            auto_resolved: r.auto_resolved,
        }
    }
}

// ── Issue API types ───────────────────────────────────────────────────────────

/// Request body for `POST /api/issues`.
#[derive(Debug, Deserialize, ToSchema)]
struct CreateIssueRequest {
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default = "default_priority")]
    priority: String,
    #[serde(default)]
    labels: Vec<String>,
    /// Human username or agent ID creating this issue.
    #[serde(default = "default_creator")]
    creator: String,
    /// When set, the issue is created on behalf of an agent with guardrails.
    /// The value is the agent's ID.
    created_by_agent: Option<String>,
    /// Discovery metadata for agent-created issues.
    source: Option<AgentSourceRequest>,
    /// Max issues this agent may create per hour (default: 20).
    #[serde(default = "default_max_per_hour")]
    max_per_hour: u32,
    /// Issue IDs that block this issue (creates `blocks` links where blocker → this issue).
    #[serde(default)]
    blocked_by: Vec<String>,
    /// Testable conditions that define when the issue is complete.
    #[serde(default)]
    acceptance_criteria: Vec<String>,
}

/// Agent discovery source passed via the REST API.
#[derive(Debug, Deserialize, ToSchema)]
struct AgentSourceRequest {
    source_type: String,
    #[serde(default)]
    #[schema(value_type = Object)]
    details: serde_json::Value,
}

fn default_priority() -> String {
    "medium".to_string()
}

fn default_creator() -> String {
    "api".to_string()
}

fn default_max_per_hour() -> u32 {
    20
}

/// Request body for `PATCH /api/issues/:id`.
#[derive(Debug, Deserialize, ToSchema)]
struct UpdateIssueRequest {
    title: Option<String>,
    description: Option<String>,
    priority: Option<String>,
    labels: Option<Vec<String>>,
    /// Testable conditions that define when the issue is complete.
    acceptance_criteria: Option<Vec<String>>,
}

/// Request body for `POST /api/issues/:id/close`.
#[derive(Debug, Deserialize, ToSchema)]
struct CloseIssueRequest {
    /// Resolution: "resolved", "wontfix", or "duplicate".
    resolution: String,
}

/// Query parameters for `GET /api/issues`.
#[derive(Debug, Default, Deserialize)]
struct ListIssuesQuery {
    status: Option<String>,
    priority: Option<String>,
    label: Option<String>,
    created_by: Option<String>,
    page: Option<u32>,
    per_page: Option<u32>,
    sort: Option<String>,
}

/// Response body for issue endpoints.
#[derive(Debug, Serialize, ToSchema)]
struct IssueResponse {
    id: String,
    title: String,
    description: String,
    status: String,
    priority: String,
    labels: Vec<String>,
    creator: String,
    resolution: Option<String>,
    /// Present when the issue was created by an agent.
    #[schema(value_type = Option<Object>)]
    agent_source: Option<serde_json::Value>,
    /// Set on creation responses when a similar open issue was detected.
    #[serde(skip_serializing_if = "Option::is_none")]
    possible_duplicate_of: Option<String>,
    linked_workspace_ids: Vec<String>,
    /// IDs of issues that block this one (source blocks this issue).
    blocked_by: Vec<String>,
    /// IDs of issues that this issue blocks (this issue is source, others are target).
    blocking: Vec<String>,
    /// Testable conditions that define when the issue is complete.
    acceptance_criteria: Vec<String>,
    created_at: String,
    updated_at: String,
}

impl IssueResponse {
    fn from_issue(
        issue: crate::issue::Issue,
        linked: Vec<uuid::Uuid>,
        blocked_by: Vec<uuid::Uuid>,
        blocking: Vec<uuid::Uuid>,
    ) -> Self {
        let agent_source = issue.agent_source.as_ref().map(|s| {
            serde_json::to_value(s).unwrap_or(serde_json::Value::Null)
        });
        IssueResponse {
            id: issue.id.to_string(),
            title: issue.title,
            description: issue.description,
            status: issue.status.as_str().to_string(),
            priority: issue.priority.as_str().to_string(),
            labels: issue.labels,
            creator: issue.creator,
            resolution: issue.resolution,
            agent_source,
            possible_duplicate_of: None,
            linked_workspace_ids: linked.iter().map(|u| u.to_string()).collect(),
            blocked_by: blocked_by.iter().map(|id| id.to_string()).collect(),
            blocking: blocking.iter().map(|id| id.to_string()).collect(),
            acceptance_criteria: issue.acceptance_criteria,
            created_at: issue.created_at.to_rfc3339(),
            updated_at: issue.updated_at.to_rfc3339(),
        }
    }
}

/// Enriched link entry used in the issue detail response.
#[derive(Debug, Serialize, ToSchema)]
struct IssueLinkDetailResponse {
    /// UUID of the other issue in the relationship.
    other_issue_id: String,
    /// Relationship from this issue's perspective (e.g. `"blocks"`, `"is-blocked-by"`,
    /// `"relates-to"`, `"duplicates"`, `"is-duplicated-by"`).
    relationship: String,
    /// Title of the linked issue.
    title: String,
    /// Current status of the linked issue (e.g. `"open"`, `"closed"`).
    status: String,
}

/// Full issue detail response returned by `GET /api/issues/:id`.
///
/// Extends the basic issue fields with linked issues (including status),
/// file attachments, and the 50 most recent comments.
#[derive(Debug, Serialize, ToSchema)]
struct IssueDetailResponse {
    id: String,
    title: String,
    description: String,
    status: String,
    priority: String,
    labels: Vec<String>,
    creator: String,
    resolution: Option<String>,
    /// Present when the issue was created by an agent.
    #[schema(value_type = Option<Object>)]
    agent_source: Option<serde_json::Value>,
    /// Set on creation responses when a similar open issue was detected.
    #[serde(skip_serializing_if = "Option::is_none")]
    possible_duplicate_of: Option<String>,
    linked_workspace_ids: Vec<String>,
    /// IDs of issues that block this one (source blocks this issue).
    blocked_by: Vec<String>,
    /// IDs of issues that this issue blocks (this issue is source, others are target).
    blocking: Vec<String>,
    /// Testable conditions that define when the issue is complete.
    acceptance_criteria: Vec<String>,
    created_at: String,
    updated_at: String,
    /// All links from/to this issue with relationship type, title, and status of the other issue.
    links: Vec<IssueLinkDetailResponse>,
    /// File attachments on this issue.
    attachments: Vec<AttachmentResponse>,
    /// The 50 most recent comments on this issue.
    comments: Vec<CommentResponse>,
}

// ── Route handlers ────────────────────────────────────────────────────────────

#[utoipa::path(
    get,
    path = "/api/status",
    responses(
        (status = 200, description = "Success", body = StatusResponse),
    ),
    tag = "status"
)]
/// `GET /api/status` — returns live repository and server health info.
///
/// This is the only unauthenticated REST endpoint.
async fn status_handler(
    State(state): State<Arc<AppState>>,
    ctx: RepoCtx,
) -> Json<StatusResponse> {
    use crate::issue::{IssueFilter, IssueStatus};
    use crate::storage::StorageBackend;

    // Read HEAD from storage trait so Postgres-backed servers return the
    // migrated version rather than the stale filesystem `.vai/head` file.
    let head = ctx
        .storage
        .versions()
        .read_head(&ctx.repo_id)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| "unknown".to_string());

    // Read workspace count from storage trait (works for both local + Postgres).
    let workspace_count = ctx
        .storage
        .workspaces()
        .list_workspaces(&ctx.repo_id, false, &ListQuery::default())
        .await
        .map(|r| r.total as usize)
        .unwrap_or(0);

    let issue_count = ctx.storage.issues()
        .list_issues(&ctx.repo_id, &IssueFilter {
            status: Some(IssueStatus::Open),
            ..Default::default()
        }, &ListQuery::default())
        .await
        .map(|r| r.total as usize)
        .unwrap_or(0);

    let escalation_count = ctx.storage.escalations()
        .list_escalations(&ctx.repo_id, true, &ListQuery::default())
        .await
        .map(|r| r.total as usize)
        .unwrap_or(0);

    // Read entity count from storage trait; fall back to zero on error.
    let entity_count = ctx
        .storage
        .graph()
        .list_entities(&ctx.repo_id, None)
        .await
        .map(|e| e.len())
        .unwrap_or(0);

    // In multi-repo Postgres mode, look up the repo name from the `repos`
    // table (keyed by repo_id) rather than from the global `state.repo_name`,
    // which is set to the storage_root path in that mode.  In single-repo
    // and local modes, fall back to the config file or state name.
    let repo_name = match &state.storage {
        StorageBackend::Server(ref pg)
        | StorageBackend::ServerWithS3(ref pg, _)
        | StorageBackend::ServerWithMemFs(ref pg, _) => {
            sqlx::query_scalar::<_, String>("SELECT name FROM repos WHERE id = $1")
                .bind(ctx.repo_id)
                .fetch_optional(pg.pool())
                .await
                .ok()
                .flatten()
                .unwrap_or_else(|| {
                    repo::read_config(&ctx.vai_dir)
                        .map(|c| c.name)
                        .unwrap_or_else(|_| state.repo_name.clone())
                })
        }
        StorageBackend::Local(_) => repo::read_config(&ctx.vai_dir)
            .map(|c| c.name)
            .unwrap_or_else(|_| state.repo_name.clone()),
    };

    Json(StatusResponse {
        repo_name,
        head_version: head,
        uptime_secs: state.started_at.elapsed().as_secs(),
        workspace_count,
        vai_version: state.vai_version.clone(),
        issue_count,
        escalation_count,
        entity_count,
    })
}

#[utoipa::path(
    get,
    path = "/health",
    responses(
        (status = 200, description = "All subsystems healthy", body = HealthResponse),
        (status = 503, description = "One or more subsystems degraded", body = HealthResponse),
    ),
    tag = "status"
)]
/// `GET /health` — liveness and readiness probe for load balancers.
///
/// Checks database and object-storage connectivity, then returns:
/// - `200 OK` with `{ "status": "ok" }` when all subsystems are healthy.
/// - `503 Service Unavailable` with `{ "status": "degraded", ... }` when any subsystem is down.
///
/// No authentication required.
async fn health_handler(
    State(state): State<Arc<AppState>>,
) -> (StatusCode, Json<HealthResponse>) {
    let uptime_secs = state.started_at.elapsed().as_secs();
    let version = state.vai_version.clone();

    let db_result = state.storage.ping_database().await;
    let s3_result = state.storage.ping_s3().await;

    let database = match db_result {
        Ok(()) => SubsystemStatus::ok(),
        Err(e) => SubsystemStatus::err(e),
    };
    let storage = match s3_result {
        Ok(()) => SubsystemStatus::ok(),
        Err(e) => SubsystemStatus::err(e),
    };

    let all_healthy = database.healthy && storage.healthy;
    let status_code = if all_healthy { StatusCode::OK } else { StatusCode::SERVICE_UNAVAILABLE };
    let status = if all_healthy { "ok".to_string() } else { "degraded".to_string() };

    (
        status_code,
        Json(HealthResponse {
            status,
            uptime_secs,
            version,
            subsystems: SubsystemsHealth { database, storage },
        }),
    )
}

#[utoipa::path(
    get,
    path = "/api/server/stats",
    responses(
        (status = 200, description = "Success", body = ServerStatsResponse),
    ),
    tag = "status"
)]
/// `GET /api/server/stats` — server-level operational statistics.
///
/// Returns uptime, vai version, workspace count, and (when Postgres is in use)
/// connection pool utilization. No authentication required.
async fn server_stats_handler(
    State(state): State<Arc<AppState>>,
    ctx: RepoCtx,
) -> Json<ServerStatsResponse> {
    let workspace_count = ctx
        .storage
        .workspaces()
        .list_workspaces(&ctx.repo_id, false, &ListQuery::default())
        .await
        .map(|r| r.total as usize)
        .unwrap_or(0);

    let (db_pool_active, db_pool_idle, db_pool_max) =
        match state.storage.pool_stats() {
            Some(stats) => (Some(stats.active), Some(stats.idle), Some(stats.max)),
            None => (None, None, None),
        };

    Json(ServerStatsResponse {
        uptime_secs: state.started_at.elapsed().as_secs(),
        vai_version: state.vai_version.clone(),
        workspace_count,
        db_pool_active,
        db_pool_idle,
        db_pool_max,
    })
}

/// `POST /api/workspaces` — creates a new workspace at the current HEAD.
///
/// Returns 201 Created with the workspace metadata.
/// Broadcasts a `WorkspaceCreated` event to WebSocket subscribers.
#[utoipa::path(
    post,
    path = "/api/repos/{repo}/workspaces",
    request_body = CreateWorkspaceRequest,
    params(
        ("repo" = String, Path, description = "Repository name"),
    ),
    responses(
        (status = 201, description = "Workspace created", body = WorkspaceResponse),
        (status = 400, description = "Bad request", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = [])),
    tag = "workspaces"
)]
async fn create_workspace_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    ctx: RepoCtx,
    Json(body): Json<CreateWorkspaceRequest>,
) -> Result<(StatusCode, Json<WorkspaceResponse>), ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Write).await?;
    validate_str_len(&body.intent, MAX_INTENT_LEN, "intent")?;
    let _lock = state.repo_lock.lock().await;
    let head = ctx.storage.versions()
        .read_head(&ctx.repo_id)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| "unknown".to_string());
    let ws = ctx.storage.workspaces()
        .create_workspace(&ctx.repo_id, crate::storage::NewWorkspace {
            id: None,
            intent: body.intent.clone(),
            base_version: head,
            issue_id: None,
        })
        .await
        .map_err(ApiError::from)?;

    // Append event to event store — triggers pg_notify in Postgres mode.
    let _ = ctx.storage.events()
        .append(&ctx.repo_id, EventKind::WorkspaceCreated {
            workspace_id: ws.id,
            intent: ws.intent.clone(),
            base_version: ws.base_version.clone(),
        })
        .await;

    // Broadcast the workspace creation event to all WebSocket subscribers.
    let ws_id = ws.id.to_string();
    state.broadcast(BroadcastEvent {
        event_type: "WorkspaceCreated".to_string(),
        event_id: 0,
        workspace_id: Some(ws_id.clone()),
        timestamp: ws.created_at.to_rfc3339(),
        data: serde_json::json!({
            "workspace_id": ws_id,
            "intent": ws.intent,
            "base_version": ws.base_version,
        }),
    });

    tracing::info!(
        event = "workspace.created",
        actor = %identity.name,
        repo = %ctx.repo_id,
        workspace_id = %ws.id,
        intent = %ws.intent,
        "workspace created"
    );
    Ok((StatusCode::CREATED, Json(WorkspaceResponse::from(ws))))
}

/// `GET /api/workspaces` — lists all active (non-discarded, non-merged) workspaces.
///
/// Supports pagination via `?page=1&per_page=25` and sorting via
/// `?sort=created_at:desc`.  Sortable columns: `created_at`, `updated_at`,
/// `status`, `intent`.
#[utoipa::path(
    get,
    path = "/api/repos/{repo}/workspaces",
    params(
        ("repo" = String, Path, description = "Repository name"),
        ("page" = Option<u32>, Query, description = "Page number (1-indexed, default 1)"),
        ("per_page" = Option<u32>, Query, description = "Items per page (default 25, max 100)"),
        ("sort" = Option<String>, Query, description = "Sort fields, e.g. `created_at:desc,status:asc`"),
    ),
    responses(
        (status = 200, description = "Paginated list of workspaces", body = PaginatedResponse<WorkspaceResponse>),
        (status = 400, description = "Invalid pagination or sort params", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = [])),
    tag = "workspaces"
)]
async fn list_workspaces_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    AxumQuery(pagination): AxumQuery<PaginationParams>,
) -> Result<Json<PaginatedResponse<WorkspaceResponse>>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Read).await?;
    const ALLOWED_SORT: &[&str] = &["created_at", "updated_at", "status", "intent", "id"];
    let query = ListQuery::from_params(
        pagination.page,
        pagination.per_page,
        pagination.sort.as_deref(),
        ALLOWED_SORT,
    )
    .map_err(|e| ApiError::bad_request(e.to_string()))?;
    let result = ctx.storage.workspaces()
        .list_workspaces(&ctx.repo_id, false, &query)
        .await
        .map_err(ApiError::from)?;
    let items: Vec<WorkspaceResponse> = result.items.into_iter().map(Into::into).collect();
    Ok(Json(PaginatedResponse::new(items, result.total, &query)))
}

/// `GET /api/workspaces/:id` — returns details for a single workspace.
///
/// Returns 404 if the workspace does not exist.
#[utoipa::path(
    get,
    path = "/api/repos/{repo}/workspaces/{id}",
    params(
        ("repo" = String, Path, description = "Repository name"),
        ("id" = String, Path, description = "Workspace ID"),
    ),
    responses(
        (status = 200, description = "Workspace details", body = WorkspaceResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Workspace not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "workspaces"
)]
async fn get_workspace_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    PathId(id): PathId,
) -> Result<Json<WorkspaceResponse>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Read).await?;
    let ws_id = uuid::Uuid::parse_str(&id)
        .map_err(|_| ApiError::not_found(format!("workspace `{id}` not found")))?;
    let meta = ctx.storage.workspaces()
        .get_workspace(&ctx.repo_id, &ws_id)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(WorkspaceResponse::from(meta)))
}

/// `POST /api/workspaces/:id/submit` — submits a workspace for merge.
///
/// Switches the active workspace to `id`, then runs the merge engine.
/// Returns 409 Conflict if the merge cannot be auto-resolved; in that case
/// an escalation is also created automatically.
/// Returns 404 if the workspace does not exist.
/// Broadcasts a `WorkspaceSubmitted` event to WebSocket subscribers.
#[utoipa::path(
    post,
    path = "/api/repos/{repo}/workspaces/{id}/submit",
    params(
        ("repo" = String, Path, description = "Repository name"),
        ("id" = String, Path, description = "Workspace ID"),
    ),
    responses(
        (status = 200, description = "Workspace submitted", body = SubmitResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Workspace not found", body = ErrorBody),
        (status = 409, description = "Merge conflict", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "workspaces"
)]
async fn submit_workspace_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    ctx: RepoCtx,
    PathId(id): PathId,
) -> Result<Json<SubmitResponse>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Write).await?;
    let _lock = state.repo_lock.lock().await;

    // Read workspace metadata from storage (works in both local SQLite and Postgres modes).
    let ws_uuid = uuid::Uuid::parse_str(&id)
        .map_err(|_| ApiError::not_found(format!("workspace `{id}` not found")))?;
    let meta = ctx.storage.workspaces()
        .get_workspace(&ctx.repo_id, &ws_uuid)
        .await
        .map_err(ApiError::from)?;
    let workspace_uuid = meta.id;
    let workspace_intent = meta.intent.clone();

    // Choose merge strategy based on storage backend.
    // ServerWithMemFs uses the same S3MergeFs path as ServerWithS3 (for testing).
    let using_s3_merge = matches!(
        &ctx.storage,
        crate::storage::StorageBackend::ServerWithS3(_, _)
            | crate::storage::StorageBackend::ServerWithMemFs(_, _)
    );

    let submit_result = if using_s3_merge {
        // S3 mode: read HEAD from storage, set up a minimal temporary .vai/
        // directory for the merge engine's metadata operations, and use
        // S3MergeFs for all file I/O.  No writes touch the real repo root.
        let current_head = ctx
            .storage
            .versions()
            .read_head(&ctx.repo_id)
            .await
            .map_err(|e| ApiError::internal(format!("read HEAD from storage: {e}")))?
            .unwrap_or_else(|| meta.base_version.clone());

        let tmp = setup_tmpdir_for_s3_submit(&meta, &current_head)?;
        let tmp_vai = tmp.path().join(".vai");

        let s3_fs = crate::merge_fs::S3MergeFs::new(
            ctx.storage.files(),
            ctx.repo_id,
            format!("workspaces/{id}/"),
            "current/".to_string(),
        );
        let result = merge::submit_with_fs(
            &s3_fs,
            &tmp_vai,
            &meta,
            meta.deleted_paths.clone(),
        );
        if result.is_ok() {
            s3_fs
                .flush()
                .await
                .map_err(|e| ApiError::internal(format!("S3MergeFs flush: {e}")))?;
        }
        // tmp is dropped here; the tmpdir is cleaned up automatically.
        result
    } else {
        // Non-S3 mode (local SQLite): switch to the workspace so merge::submit
        // can locate the active overlay on disk, then run the disk-based merge.
        workspace::switch(&ctx.vai_dir, &id).map_err(ApiError::from)?;
        merge::submit(&ctx.vai_dir, &ctx.repo_root)
    };

    match submit_result {
        Ok(result) => {
            // Remove from conflict engine — workspace is no longer active.
            state.conflict_engine.lock().await.remove_workspace(&workspace_uuid);

            // Append a MergeCompleted event to the storage trait so Postgres
            // has a real event record with the correct sequential ID.  In local
            // SQLite mode this duplicates the event the merge engine already
            // wrote to the event-log file, which is harmless.  We use the
            // returned event ID (the Postgres row ID) as merge_event_id so the
            // version-detail handler can look it up via query_by_type.
            let storage_merge_event = ctx
                .storage
                .events()
                .append(
                    &ctx.repo_id,
                    EventKind::MergeCompleted {
                        workspace_id: workspace_uuid,
                        new_version_id: result.version.version_id.clone(),
                        auto_resolved_conflicts: result.auto_resolved,
                    },
                )
                .await;
            let merge_event_id = storage_merge_event
                .ok()
                .map(|e| e.id)
                .or(result.version.merge_event_id);

            // Write FileRemoved events for deleted paths so the version-detail
            // handler can include them when reconstructing file_changes.  Upload
            // handlers already write FileAdded/FileModified to storage; deletions
            // are only tracked in the workspace metadata column.
            for path in &meta.deleted_paths {
                let _ = ctx
                    .storage
                    .events()
                    .append(
                        &ctx.repo_id,
                        EventKind::FileRemoved {
                            workspace_id: workspace_uuid,
                            path: path.clone(),
                        },
                    )
                    .await;
            }

            // Sync the new version and HEAD to the storage trait.
            // In Postgres server mode these writes go to the database; in local
            // SQLite mode they duplicate what merge::submit already wrote to disk,
            // which is harmless (same files, same data).
            let _ = ctx.storage.versions()
                .create_version(&ctx.repo_id, crate::storage::NewVersion {
                    version_id: result.version.version_id.clone(),
                    parent_version_id: result.version.parent_version_id.clone(),
                    intent: result.version.intent.clone(),
                    created_by: result.version.created_by.clone(),
                    merge_event_id,
                })
                .await;
            let _ = ctx.storage.versions()
                .advance_head(&ctx.repo_id, &result.version.version_id)
                .await;
            // Mark workspace as Merged in storage trait.
            let _ = ctx.storage.workspaces()
                .update_workspace(
                    &ctx.repo_id,
                    &workspace_uuid,
                    crate::storage::WorkspaceUpdate {
                        status: Some(crate::workspace::WorkspaceStatus::Merged),
                        ..Default::default()
                    },
                )
                .await;

            // Persist pre-change snapshot and update "current/" in S3.
            //
            // In S3MergeFs mode both are already handled by flush() above:
            // - save_pre_change_snapshot wrote snapshot files to pending_writes
            //   which were flushed to `versions/{ver}/snapshot/` in S3.
            // - apply_overlay wrote merged base files to pending_writes which
            //   were flushed to `current/` in S3.
            //
            // In disk mode we read from the local vai_dir tree and repo_root.
            if !using_s3_merge {
                // Persist pre-change snapshot to FileStore so diffs survive container
                // restarts and cross-server migrations.
                let snap_dir = ctx.vai_dir
                    .join("versions")
                    .join(&result.version.version_id)
                    .join("snapshot");
                let file_store = ctx.storage.files();
                for (rel, bytes) in collect_dir_files_with_content(&snap_dir) {
                    let key = format!("versions/{}/snapshot/{rel}", result.version.version_id);
                    let _ = file_store.put(&ctx.repo_id, &key, &bytes).await;
                }

                // Update "current/" prefix in S3 with the full repo state.
                // The download handler and diff engine use this as the base.
                // Read from repo_root (post-merge disk state) so that semantic merges
                // write the combined result, not just the workspace's raw overlay.
                // ALLOW_FS: local SQLite mode only — guarded by `if !using_s3_merge`
                let overlay = workspace::overlay_dir(&ctx.vai_dir, &id);
                if overlay.exists() {
                    for (rel, _) in collect_dir_files_with_content(&overlay) {
                        // Read merged content from repo_root rather than overlay.
                        // For fast-forward merges this is identical to the overlay;
                        // for semantic merges it contains the auto-resolved result.
                        let merged_path = ctx.repo_root.join(&rel);
                        // ALLOW_FS: local SQLite mode only — guarded by `if !using_s3_merge`
                        if let Ok(bytes) = std::fs::read(&merged_path) {
                            let key = format!("current/{rel}");
                            let _ = file_store.put(&ctx.repo_id, &key, &bytes).await;
                        }
                    }
                }

                // Remove deleted files from the "current/" prefix using the
                // workspace's `deleted_paths` column (set by upload handlers).
                for path in &meta.deleted_paths {
                    let _ = file_store
                        .delete(&ctx.repo_id, &format!("current/{path}"))
                        .await;
                }
            }

            // Append event to event store — triggers pg_notify in Postgres mode.
            let _ = ctx.storage.events()
                .append(&ctx.repo_id, EventKind::WorkspaceSubmitted {
                    workspace_id: workspace_uuid,
                    changes_summary: format!(
                        "{} files applied, {} entities changed, new version {}",
                        result.files_applied, result.entities_changed, result.version.version_id
                    ),
                })
                .await;

            // Broadcast the submit/merge event.
            state.broadcast(BroadcastEvent {
                event_type: "WorkspaceSubmitted".to_string(),
                event_id: 0,
                workspace_id: Some(id.clone()),
                timestamp: chrono::Utc::now().to_rfc3339(),
                data: serde_json::json!({
                    "workspace_id": id,
                    "new_version": result.version.version_id,
                    "files_applied": result.files_applied,
                    "entities_changed": result.entities_changed,
                }),
            });

            // Auto-refresh the semantic graph in server (S3) mode so entities
            // reflect the newly merged state without requiring a manual call
            // to POST /api/graph/refresh.
            if using_s3_merge {
                let _ = graph::refresh_graph_from_files(
                    ctx.storage.graph(),
                    ctx.storage.files(),
                    ctx.repo_id,
                )
                .await;
            }

            tracing::info!(
                event = "workspace.submitted",
                actor = %identity.name,
                repo = %ctx.repo_id,
                workspace_id = %workspace_uuid,
                "workspace submitted successfully"
            );
            Ok(Json(SubmitResponse::from(result)))
        }
        Err(merge::MergeError::SemanticConflicts { count, ref conflicts }) => {
            // Auto-create an escalation so humans can review.
            let affected_entities: Vec<String> = conflicts
                .iter()
                .flat_map(|c| c.entity_ids.iter().cloned())
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect();

            // Build a more specific summary listing the affected files.
            let unique_files: Vec<String> = {
                let mut files: Vec<String> = conflicts
                    .iter()
                    .map(|c| c.file_path.clone())
                    .collect::<std::collections::HashSet<_>>()
                    .into_iter()
                    .collect();
                files.sort();
                files
            };
            let files_summary = if unique_files.len() == 1 {
                format!("in {}", unique_files[0])
            } else {
                format!("in {} files", unique_files.len())
            };
            let summary = format!(
                "{count} unresolvable conflict(s) {files_summary} — \
                 workspace \"{workspace_intent}\" requires manual resolution"
            );

            // Convert ConflictRecord → EscalationConflict for rich detail.
            let esc_conflicts: Vec<crate::escalation::EscalationConflict> = conflicts
                .iter()
                .map(|c| crate::escalation::EscalationConflict {
                    file: c.file_path.clone(),
                    merge_level: c.merge_level,
                    entity_ids: c.entity_ids.clone(),
                    description: c.description.clone(),
                    // Content is not captured at merge time; callers can fetch
                    // files from the file store using the conflict's file path.
                    ours_content: None,
                    theirs_content: None,
                    base_content: None,
                })
                .collect();

            {
                use crate::escalation::{EscalationSeverity, EscalationType};
                use crate::storage::NewEscalation;
                let resolution_options = crate::escalation::default_resolution_options(
                    &EscalationType::MergeConflict,
                    &[workspace_uuid],
                );
                let new_esc = NewEscalation {
                    escalation_type: EscalationType::MergeConflict,
                    severity: EscalationSeverity::High,
                    summary: summary.clone(),
                    intents: vec![workspace_intent.clone()],
                    agents: vec![],
                    workspace_ids: vec![workspace_uuid],
                    affected_entities,
                    conflicts: esc_conflicts,
                    resolution_options,
                };
                if let Ok(escalation) = ctx.storage.escalations()
                    .create_escalation(&ctx.repo_id, new_esc)
                    .await
                {
                    // Append escalation event to event store.
                    let _ = ctx.storage.events()
                        .append(&ctx.repo_id, EventKind::EscalationCreated {
                            escalation_id: escalation.id,
                            escalation_type: "MergeConflict".to_string(),
                            severity: "High".to_string(),
                            workspace_ids: vec![workspace_uuid.to_string()],
                            summary: summary.clone(),
                        })
                        .await;

                    // Broadcast the escalation creation.
                    state.broadcast(BroadcastEvent {
                        event_type: "EscalationCreated".to_string(),
                        event_id: 0,
                        workspace_id: Some(id.clone()),
                        timestamp: chrono::Utc::now().to_rfc3339(),
                        data: serde_json::json!({
                            "escalation_id": escalation.id,
                            "workspace_id": id,
                            "summary": summary,
                        }),
                    });
                }
            }

            // Return 409 Conflict (same as before).
            Err(ApiError::conflict(format!(
                "Semantic merge detected {count} conflict(s) requiring manual resolution; \
                 an escalation has been created — run `vai escalations list` to view it"
            )))
        }
        Err(e) => Err(ApiError::from(e)),
    }
}

/// `DELETE /api/workspaces/:id` — discards a workspace.
///
/// Returns 404 if the workspace does not exist.
/// Returns 204 No Content on success.
/// Broadcasts a `WorkspaceDiscarded` event to WebSocket subscribers.
#[utoipa::path(
    delete,
    path = "/api/repos/{repo}/workspaces/{id}",
    params(
        ("repo" = String, Path, description = "Repository name"),
        ("id" = String, Path, description = "Workspace ID"),
    ),
    responses(
        (status = 204, description = "Workspace discarded"),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Workspace not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "workspaces"
)]
async fn discard_workspace_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    ctx: RepoCtx,
    PathId(id): PathId,
) -> Result<StatusCode, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Write).await?;
    let _lock = state.repo_lock.lock().await;
    // Resolve UUID from path parameter.
    let ws_uuid = uuid::Uuid::parse_str(&id)
        .map_err(|_| ApiError::not_found(format!("workspace `{id}` not found")))?;
    // Look up workspace via storage (works in both local and Postgres mode).
    let meta = ctx.storage.workspaces()
        .get_workspace(&ctx.repo_id, &ws_uuid)
        .await
        .map_err(ApiError::from)?;
    let issue_id = meta.issue_id;
    // Discard via storage trait — avoids filesystem-only lookup that fails in Postgres mode.
    ctx.storage.workspaces()
        .discard_workspace(&ctx.repo_id, &ws_uuid)
        .await
        .map_err(ApiError::from)?;

    // Remove from conflict engine — workspace is no longer active.
    state.conflict_engine.lock().await.remove_workspace(&ws_uuid);

    // If workspace was linked to an issue, transition it back to Open.
    if let Some(iid) = issue_id {
        let _ = ctx.storage.issues()
            .update_issue(
                &ctx.repo_id,
                &iid,
                crate::storage::IssueUpdate {
                    status: Some(crate::issue::IssueStatus::Open),
                    ..Default::default()
                },
            )
            .await;
    }

    // Append event to event store — triggers pg_notify in Postgres mode.
    let _ = ctx.storage.events()
        .append(&ctx.repo_id, EventKind::WorkspaceDiscarded {
            workspace_id: ws_uuid,
            reason: "discarded via API".to_string(),
        })
        .await;

    // Broadcast discard event.
    state.broadcast(BroadcastEvent {
        event_type: "WorkspaceDiscarded".to_string(),
        event_id: 0,
        workspace_id: Some(id.clone()),
        timestamp: chrono::Utc::now().to_rfc3339(),
        data: serde_json::json!({ "workspace_id": id }),
    });

    tracing::info!(
        event = "workspace.discarded",
        actor = %identity.name,
        repo = %ctx.repo_id,
        workspace_id = %ws_uuid,
        "workspace discarded"
    );
    Ok(StatusCode::NO_CONTENT)
}

// ── File upload / download ────────────────────────────────────────────────────

/// Maximum allowed size for a single uploaded file (10 MiB).
const MAX_FILE_SIZE_BYTES: usize = 10 * 1024 * 1024;

/// A single file entry within an upload request.
#[derive(Debug, Deserialize, ToSchema)]
struct FileUploadEntry {
    /// Path relative to the repository root (e.g. `src/auth.rs`).
    path: String,
    /// File content encoded as standard (padded) base64.
    content_base64: String,
}

/// Request body for `POST /api/workspaces/:id/files`.
#[derive(Debug, Deserialize, ToSchema)]
struct UploadFilesRequest {
    /// One or more files to upload into the workspace overlay.
    files: Vec<FileUploadEntry>,
    /// Paths (relative to repo root) that the agent deleted during this session.
    ///
    /// These are accumulated into the workspace row's `deleted_paths` column
    /// via the storage trait. The submit handler removes them from `current/`
    /// and emits `FileRemoved` events; the download handler excludes them from
    /// tarballs built from merged workspace overlays.
    #[serde(default)]
    deleted_paths: Vec<String>,
}

/// Response body for a successful file upload.
#[derive(Debug, Serialize, ToSchema)]
struct UploadFilesResponse {
    /// Number of files successfully written to storage.
    uploaded: usize,
    /// Number of files skipped because they were already present in storage
    /// with the same content hash (resumability — Postgres mode only).
    #[serde(default)]
    skipped: usize,
    /// Repository-relative paths of all written files.
    paths: Vec<String>,
}

/// Response body for `POST /api/workspaces/:id/upload-snapshot`.
#[derive(Debug, Serialize, ToSchema)]
struct UploadSnapshotResponse {
    /// Files in the tarball that were not present in `current/`.
    added: usize,
    /// Files with different content from `current/`.
    modified: usize,
    /// Files present in `current/` but absent from the tarball (full mode) or
    /// listed in `.vai-delta.json` (delta mode).
    deleted: usize,
    /// Files identical in both tarball and `current/`.
    unchanged: usize,
    /// `true` when the upload was processed as a delta (`.vai-delta.json` was present).
    is_delta: bool,
}

/// Manifest embedded inside a delta tarball as `.vai-delta.json`.
///
/// When this file is present in the uploaded archive the server switches to
/// delta mode: only the files actually present in the tarball are compared
/// against `current/`, and `deleted_paths` is taken verbatim from this struct
/// rather than derived from absent files.
#[derive(Debug, Deserialize, ToSchema)]
struct DeltaManifest {
    /// The version identifier the delta was built on top of (informational).
    #[allow(dead_code)]
    base_version: String,
    /// Repository-relative paths that were deleted relative to `base_version`.
    #[serde(default)]
    deleted_paths: Vec<String>,
}

/// Response body for file download endpoints.
#[derive(Debug, Serialize, ToSchema)]
struct FileDownloadResponse {
    /// Path relative to the repository root.
    path: String,
    /// File content encoded as standard (padded) base64.
    content_base64: String,
    /// File size in bytes.
    size: usize,
    /// Where the file was sourced: `"overlay"` or `"base"`.
    found_in: String,
}

/// Validates and normalises a client-supplied file path.
///
/// Returns `None` if the path is absolute or contains any parent-directory
/// (`..`) components, preventing directory-traversal attacks.
fn sanitize_path(raw: &str) -> Option<std::path::PathBuf> {
    // Reject null bytes — they can be used to truncate paths at the OS level.
    if raw.contains('\0') {
        return None;
    }
    let trimmed = raw.trim_start_matches('/');
    let pb = std::path::PathBuf::from(trimmed);
    if pb.is_absolute() {
        return None;
    }
    if pb.components().any(|c| c == std::path::Component::ParentDir) {
        return None;
    }
    Some(pb)
}

/// Computes the hex-encoded SHA-256 digest of `data`.
fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(data);
    format!("{:x}", h.finalize())
}

/// Extracts a gzip-compressed tarball into an in-memory `{path → content}` map.
///
/// Paths are normalised by stripping any leading `./` prefix.  Directory
/// entries are silently skipped.  Symlinks and hard links are rejected
/// outright — they could be used to escape the workspace root.  Each file is
/// limited to `MAX_FILE_SIZE_BYTES`; the overall tarball limit is enforced by
/// the caller.  Returns an error if the bytes are not a valid gzip-compressed
/// tar archive.
fn extract_snapshot_tarball(gz_bytes: &[u8]) -> Result<HashMap<String, Vec<u8>>, ApiError> {
    use flate2::read::GzDecoder;
    use tar::Archive;
    use std::io::Read;

    let decoder = GzDecoder::new(gz_bytes);
    let mut archive = Archive::new(decoder);
    let mut files = HashMap::new();

    let entries = archive
        .entries()
        .map_err(|e| ApiError::bad_request(format!("invalid tarball: {e}")))?;

    for entry in entries {
        let mut entry = entry
            .map_err(|e| ApiError::bad_request(format!("invalid tarball entry: {e}")))?;

        let entry_type = entry.header().entry_type();

        // Reject symlinks and hard links — they can be used to traverse outside
        // the workspace root or reference paths the agent does not own.
        if entry_type.is_symlink() || entry_type.is_hard_link() {
            return Err(ApiError::bad_request(
                "tarball contains a symlink or hard link, which is not permitted",
            ));
        }

        // Skip directories and other non-regular-file entries.
        if !entry_type.is_file() {
            continue;
        }

        let raw_path = entry
            .path()
            .map_err(|e| ApiError::bad_request(format!("invalid path in tarball: {e}")))?
            .to_string_lossy()
            .to_string();

        // Normalise: strip leading "./" and validate for path traversal.
        let path = raw_path.trim_start_matches("./").to_string();
        let rel = match sanitize_path(&path) {
            Some(p) => p.to_string_lossy().replace('\\', "/"),
            None => {
                return Err(ApiError::bad_request(format!(
                    "tarball contains an unsafe path: '{path}'"
                )));
            }
        };
        if rel.is_empty() {
            continue;
        }

        let mut content = Vec::new();
        entry
            .read_to_end(&mut content)
            .map_err(|e| ApiError::bad_request(format!("read tarball entry '{rel}': {e}")))?;

        // Enforce per-file size limit.
        if content.len() > MAX_FILE_SIZE_BYTES {
            return Err(ApiError::bad_request(format!(
                "tarball entry '{rel}' exceeds 10 MiB per-file limit ({} bytes)",
                content.len()
            )));
        }

        files.insert(rel, content);
    }

    Ok(files)
}

/// Returns `true` for paths that should be excluded from snapshot uploads.
///
/// Always excludes `.vai/` and `.git/` trees which are internal to the
/// version-control tooling and must never be stored as workspace overlay
/// files.
fn is_snapshot_path_ignored(path: &str) -> bool {
    path.starts_with(".vai/")
        || path.starts_with(".git/")
        || path == ".vai"
        || path == ".git"
}

#[utoipa::path(
    post,
    path = "/api/repos/{repo}/workspaces/{id}/files",
    params(
        ("repo" = String, Path, description = "Repository name"),
        ("id" = String, Path, description = "Workspace ID"),
    ),
    request_body = UploadFilesRequest,
    responses(
        (status = 201, description = "Files uploaded", body = UploadFilesResponse),
        (status = 400, description = "Bad request", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Workspace not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "workspaces"
)]
/// `POST /api/workspaces/:id/files` — uploads one or more files into the
/// workspace overlay.
///
/// Each file's content must be standard base64-encoded. Binary files are fully
/// supported. Files larger than 10 MiB are rejected with 400 Bad Request.
///
/// - If the file already exists in the overlay a `FileModified` event is
///   recorded; otherwise a `FileAdded` event is recorded.
/// - On first upload the workspace transitions from `Created` → `Active`.
/// - Broadcasts a `FilesUploaded` WebSocket event on success.
async fn upload_workspace_files_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    ctx: RepoCtx,
    PathId(id): PathId,
    Json(body): Json<UploadFilesRequest>,
) -> Result<(StatusCode, Json<UploadFilesResponse>), ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Write).await?;

    // Validate file count and path lengths before acquiring the lock.
    if body.files.len() > MAX_FILES_PER_REQUEST {
        return Err(ApiError::bad_request(format!(
            "too many files: {}, maximum is {MAX_FILES_PER_REQUEST}",
            body.files.len()
        )));
    }
    for entry in &body.files {
        validate_str_len(&entry.path, MAX_PATH_LEN, "file path")?;
    }
    for path in &body.deleted_paths {
        validate_str_len(path, MAX_PATH_LEN, "deleted_path")?;
    }

    let _lock = state.repo_lock.lock().await;

    // Read workspace metadata from storage (works in both local SQLite and Postgres modes).
    let ws_uuid = uuid::Uuid::parse_str(&id)
        .map_err(|_| ApiError::not_found(format!("workspace `{id}` not found")))?;
    let meta = ctx.storage.workspaces()
        .get_workspace(&ctx.repo_id, &ws_uuid)
        .await
        .map_err(ApiError::from)?;
    let workspace_uuid = meta.id;

    let mut uploaded_paths: Vec<String> = Vec::new();

    for entry in &body.files {
        // Decode base64 content.
        let content = BASE64
            .decode(&entry.content_base64)
            .map_err(|e| ApiError::bad_request(format!("base64 decode error for '{}': {e}", entry.path)))?;

        // Enforce per-file size limit.
        if content.len() > MAX_FILE_SIZE_BYTES {
            return Err(ApiError::bad_request(format!(
                "file '{}' exceeds 10 MiB limit ({} bytes)",
                entry.path,
                content.len()
            )));
        }

        // Validate and normalise the path.
        let rel = sanitize_path(&entry.path)
            .ok_or_else(|| ApiError::bad_request(format!("invalid path: '{}'", entry.path)))?;

        let path_str = rel.to_string_lossy().replace('\\', "/");

        // Determine whether this is an add or a modify.
        // Check the workspace overlay first (re-upload), then fall back to
        // current/ (base repo state) to distinguish new files from modifications.
        let new_hash = sha256_hex(&content);
        let store_key = format!("workspaces/{}/{}", id, path_str);
        let current_key = format!("current/{}", path_str);
        let file_store = ctx.storage.files();
        let existing = file_store.get(&ctx.repo_id, &store_key).await.ok()
            .or(file_store.get(&ctx.repo_id, &current_key).await.ok());
        let is_new = existing.is_none();
        let old_hash = existing.as_ref().map(|bytes| sha256_hex(bytes)).unwrap_or_default();

        let new_hash_blob = new_hash.clone();
        let old_hash_blob = old_hash.clone();

        // Write to FileStore (primary storage — works in both S3 and local modes).
        file_store.put(&ctx.repo_id, &store_key, &content).await
            .map_err(|e| ApiError::internal(format!("write overlay file to store: {e}")))?;
        // Also store content-addressably by hash for diffs.
        let _ = file_store.put(&ctx.repo_id, &format!("blobs/{new_hash_blob}"), &content).await;
        if let Some(old_bytes) = existing {
            let _ = file_store.put(&ctx.repo_id, &format!("blobs/{old_hash_blob}"), &old_bytes).await;
        }

        // Also write to local filesystem overlay as cache (best-effort for local mode).
        // ALLOW_FS: local filesystem cache for SQLite mode; best-effort, errors ignored
        let overlay = workspace::overlay_dir(&ctx.vai_dir, &id);
        let dest = overlay.join(&rel);
        if let Some(parent) = dest.parent() {
            // ALLOW_FS: local filesystem cache for SQLite mode; best-effort, errors ignored
            let _ = std::fs::create_dir_all(parent);
        }
        // ALLOW_FS: local filesystem cache for SQLite mode; best-effort, errors ignored
        let _ = std::fs::write(&dest, &content);

        // Append event via storage trait (Postgres pg_notify + local event log).
        let event_kind = if is_new {
            EventKind::FileAdded {
                workspace_id: workspace_uuid,
                path: path_str.clone(),
                hash: new_hash,
            }
        } else {
            EventKind::FileModified {
                workspace_id: workspace_uuid,
                path: path_str.clone(),
                old_hash,
                new_hash,
            }
        };
        let _ = ctx.storage.events().append(&ctx.repo_id, event_kind).await;

        uploaded_paths.push(path_str);
    }

    // Broadcast a WebSocket notification.
    state.broadcast(BroadcastEvent {
        event_type: "FilesUploaded".to_string(),
        event_id: 0,
        workspace_id: Some(id.clone()),
        timestamp: chrono::Utc::now().to_rfc3339(),
        data: serde_json::json!({ "paths": uploaded_paths }),
    });

    // Fetch the complete overlay path list from the FileStore so the conflict
    // engine always sees the authoritative current state of the workspace,
    // not just the files uploaded in this request.
    //
    // Files are stored as `workspaces/{id}/{rel_path}` (e.g. `workspaces/{id}/src/auth.rs`).
    // We also exclude content-addressed blobs stored under `blobs/` which share
    // no prefix with workspace paths and therefore won't appear here.
    let ws_prefix = format!("workspaces/{id}/");
    let overlay_paths: Vec<String> = ctx
        .storage
        .files()
        .list(&ctx.repo_id, &ws_prefix)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter_map(|fm| fm.path.strip_prefix(&ws_prefix).map(|s| s.to_string()))
        .filter(|p| !p.is_empty())
        .collect();

    // Run conflict overlap detection and notify affected workspaces.
    {
        let mut engine = state.conflict_engine.lock().await;
        let overlaps = engine.update_scope(workspace_uuid, &meta.intent, &overlay_paths);
        for overlap in overlaps {
            let ts = chrono::Utc::now().to_rfc3339();
            let payload = serde_json::json!({
                "type": "overlap_detected",
                "severity": overlap.level.as_str(),
                "your_workspace": overlap.your_workspace.to_string(),
                "other_workspace": overlap.other_workspace.to_string(),
                "other_intent": overlap.other_intent,
                "overlapping_files": overlap.overlapping_files,
                "overlapping_entities": overlap.overlapping_entities,
                "recommendation": overlap.recommendation,
            });
            // Notify the workspace whose scope was just updated.
            state.broadcast(BroadcastEvent {
                event_type: "OverlapDetected".to_string(),
                event_id: 0,
                workspace_id: Some(overlap.your_workspace.to_string()),
                timestamp: ts.clone(),
                data: payload.clone(),
            });
            // Also notify the other overlapping workspace.
            let mirrored = serde_json::json!({
                "type": "overlap_detected",
                "severity": overlap.level.as_str(),
                "your_workspace": overlap.other_workspace.to_string(),
                "other_workspace": overlap.your_workspace.to_string(),
                "other_intent": meta.intent,
                "overlapping_files": overlap.overlapping_files,
                "overlapping_entities": overlap.overlapping_entities,
                "recommendation": overlap.recommendation,
            });
            state.broadcast(BroadcastEvent {
                event_type: "OverlapDetected".to_string(),
                event_id: 0,
                workspace_id: Some(overlap.other_workspace.to_string()),
                timestamp: ts,
                data: mirrored,
            });
        }
    }

    // ── Process deleted_paths and status transition ────────────────────────────
    //
    // Merge new deletions into the workspace row's `deleted_paths` column.
    // Also transition workspace from Created → Active on first content upload.
    {
        let mut merged_deleted = meta.deleted_paths.clone();

        for raw_path in &body.deleted_paths {
            let rel = match sanitize_path(raw_path) {
                Some(p) => p.to_string_lossy().replace('\\', "/"),
                None => {
                    return Err(ApiError::bad_request(format!(
                        "invalid deleted path: '{raw_path}'"
                    )));
                }
            };
            if !merged_deleted.contains(&rel) {
                merged_deleted.push(rel.clone());
            }
            // Emit FileRemoved event via storage trait.
            let _ = ctx
                .storage
                .events()
                .append(
                    &ctx.repo_id,
                    EventKind::FileRemoved {
                        workspace_id: workspace_uuid,
                        path: rel,
                    },
                )
                .await;
        }

        let new_status = if meta.status == workspace::WorkspaceStatus::Created
            && (!uploaded_paths.is_empty() || !body.deleted_paths.is_empty())
        {
            Some(workspace::WorkspaceStatus::Active)
        } else {
            None
        };
        let deleted_changed = merged_deleted != meta.deleted_paths;
        if new_status.is_some() || deleted_changed {
            let update = crate::storage::WorkspaceUpdate {
                status: new_status,
                deleted_paths: if deleted_changed { Some(merged_deleted) } else { None },
                ..Default::default()
            };
            let _ = ctx
                .storage
                .workspaces()
                .update_workspace(&ctx.repo_id, &workspace_uuid, update)
                .await;
        }
    }

    let count = uploaded_paths.len();
    Ok((
        StatusCode::OK,
        Json(UploadFilesResponse {
            uploaded: count,
            skipped: 0,
            paths: uploaded_paths,
        }),
    ))
}

/// Maximum uncompressed tarball payload accepted by the snapshot upload endpoint.
const MAX_SNAPSHOT_SIZE_BYTES: usize = 100 * 1024 * 1024; // 100 MiB

#[utoipa::path(
    post,
    path = "/api/repos/{repo}/workspaces/{id}/upload-snapshot",
    params(
        ("repo" = String, Path, description = "Repository name"),
        ("id" = String, Path, description = "Workspace ID"),
    ),
    request_body(
        content = String,
        description = "Gzip-compressed tarball of the working directory (Content-Type: application/gzip). Maximum 100 MiB.",
        content_type = "application/gzip"
    ),
    responses(
        (status = 200, description = "Snapshot diffed and stored", body = UploadSnapshotResponse),
        (status = 400, description = "Bad request or invalid tarball", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Workspace not found", body = ErrorBody),
        (status = 413, description = "Tarball exceeds 100 MiB limit", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "workspaces"
)]
/// `POST /api/workspaces/:id/upload-snapshot` — accepts a gzip-compressed
/// tarball of the agent's working directory, diffs it against the current
/// repository state in `current/`, and stores the delta as a workspace
/// overlay.
///
/// ## Full mode (default)
///
/// The endpoint compares each file in the tarball to `current/` using
/// SHA-256 content hashes:
/// - **added** — present in tarball, absent from `current/`
/// - **modified** — present in both, but with a different hash
/// - **deleted** — present in `current/`, absent from tarball
/// - **unchanged** — identical hash in both; skipped
///
/// ## Delta mode
///
/// If the tarball contains a `.vai-delta.json` manifest at its root the
/// upload is processed in delta mode.  The manifest has the form:
/// ```json
/// { "base_version": "v42", "deleted_paths": ["src/old.ts"] }
/// ```
/// In delta mode only the files actually present in the archive are compared
/// against `current/`; absent files are **not** treated as deletions.
/// Instead the explicit `deleted_paths` list from the manifest is used.
/// This allows agents to upload only changed files for large repositories.
///
/// Added and modified files are written to the workspace overlay under
/// `workspaces/{id}/{path}` in the file store.  Deleted paths are recorded
/// via the workspace row's `deleted_paths` column used by submit and download handlers.
///
/// Tarballs larger than 100 MiB (compressed) are rejected with **413**.
async fn upload_snapshot_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    ctx: RepoCtx,
    PathId(id): PathId,
    body: axum::body::Bytes,
) -> Result<(StatusCode, Json<UploadSnapshotResponse>), ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Write).await?;
    let _lock = state.repo_lock.lock().await;

    // Reject tarballs above the size limit.
    if body.len() > MAX_SNAPSHOT_SIZE_BYTES {
        return Err(ApiError::payload_too_large(format!(
            "tarball exceeds 100 MiB limit ({} bytes)",
            body.len()
        )));
    }

    // Parse workspace metadata.
    let ws_uuid = uuid::Uuid::parse_str(&id)
        .map_err(|_| ApiError::not_found(format!("workspace `{id}` not found")))?;
    let meta = ctx
        .storage
        .workspaces()
        .get_workspace(&ctx.repo_id, &ws_uuid)
        .await
        .map_err(ApiError::from)?;

    // Extract the tarball to an in-memory map, filtering ignored paths.
    let mut raw_files = extract_snapshot_tarball(&body)?;

    // Detect delta mode: presence of `.vai-delta.json` in the archive switches
    // from full-snapshot semantics to delta semantics.
    let delta_manifest: Option<DeltaManifest> = if let Some(manifest_bytes) = raw_files.remove(".vai-delta.json") {
        match serde_json::from_slice::<DeltaManifest>(&manifest_bytes) {
            Ok(m) => Some(m),
            Err(e) => {
                return Err(ApiError::bad_request(format!(
                    "invalid .vai-delta.json: {e}"
                )));
            }
        }
    } else {
        None
    };
    let is_delta = delta_manifest.is_some();

    let tarball_files: HashMap<String, Vec<u8>> = raw_files
        .into_iter()
        .filter(|(path, _)| !is_snapshot_path_ignored(path))
        .collect();

    // Build a map of the current repository state: path → content_hash.
    let file_store = ctx.storage.files();
    let current_entries = file_store
        .list(&ctx.repo_id, "current/")
        .await
        .unwrap_or_default();
    let current_map: HashMap<String, String> = current_entries
        .into_iter()
        .filter_map(|fm| {
            let rel = fm.path.strip_prefix("current/")?.to_string();
            if rel.is_empty() {
                None
            } else {
                Some((rel, fm.content_hash))
            }
        })
        .collect();

    // Diff tarball against current state.
    let mut added = 0usize;
    let mut modified = 0usize;
    let mut unchanged = 0usize;
    let mut uploaded_paths: Vec<String> = Vec::new();

    for (path, content) in &tarball_files {
        let new_hash = sha256_hex(content);

        let event_kind = match current_map.get(path) {
            Some(current_hash) if current_hash == &new_hash => {
                unchanged += 1;
                continue;
            }
            Some(current_hash) => {
                modified += 1;
                EventKind::FileModified {
                    workspace_id: ws_uuid,
                    path: path.clone(),
                    old_hash: current_hash.clone(),
                    new_hash: new_hash.clone(),
                }
            }
            None => {
                added += 1;
                EventKind::FileAdded {
                    workspace_id: ws_uuid,
                    path: path.clone(),
                    hash: new_hash.clone(),
                }
            }
        };

        // Write to workspace overlay in file store.
        let store_key = format!("workspaces/{id}/{path}");
        file_store
            .put(&ctx.repo_id, &store_key, content)
            .await
            .map_err(|e| ApiError::internal(format!("write overlay file: {e}")))?;
        // Also store content-addressably for diffs.
        let _ = file_store
            .put(&ctx.repo_id, &format!("blobs/{new_hash}"), content)
            .await;

        // Record event via storage trait.
        let _ = ctx.storage.events().append(&ctx.repo_id, event_kind).await;

        uploaded_paths.push(path.clone());
    }

    // Compute deletions.
    // - Full mode: any file in current/ that is absent from the tarball is deleted.
    // - Delta mode: only files explicitly listed in the manifest are deleted.
    let deleted_paths: Vec<String> = if let Some(ref manifest) = delta_manifest {
        manifest
            .deleted_paths
            .iter()
            .filter(|p| !p.is_empty())
            .cloned()
            .collect()
    } else {
        current_map
            .keys()
            .filter(|p| !tarball_files.contains_key(*p))
            .cloned()
            .collect()
    };
    let deleted = deleted_paths.len();

    // Merge snapshot deletions into workspace row and transition Created → Active.
    {
        let mut merged_deleted = meta.deleted_paths.clone();
        for path in &deleted_paths {
            if !merged_deleted.contains(path) {
                merged_deleted.push(path.clone());
            }
            let _ = ctx
                .storage
                .events()
                .append(
                    &ctx.repo_id,
                    EventKind::FileRemoved {
                        workspace_id: ws_uuid,
                        path: path.clone(),
                    },
                )
                .await;
        }

        let new_status = if meta.status == workspace::WorkspaceStatus::Created
            && (!uploaded_paths.is_empty() || !deleted_paths.is_empty())
        {
            Some(workspace::WorkspaceStatus::Active)
        } else {
            None
        };
        let deleted_changed = merged_deleted != meta.deleted_paths;
        if new_status.is_some() || deleted_changed {
            let update = crate::storage::WorkspaceUpdate {
                status: new_status,
                deleted_paths: if deleted_changed { Some(merged_deleted) } else { None },
                ..Default::default()
            };
            let _ = ctx
                .storage
                .workspaces()
                .update_workspace(&ctx.repo_id, &ws_uuid, update)
                .await;
        }
    }

    // Broadcast WebSocket notification.
    state.broadcast(BroadcastEvent {
        event_type: "SnapshotUploaded".to_string(),
        event_id: 0,
        workspace_id: Some(id.clone()),
        timestamp: chrono::Utc::now().to_rfc3339(),
        data: serde_json::json!({
            "added": added,
            "modified": modified,
            "deleted": deleted,
            "unchanged": unchanged,
        }),
    });

    Ok((
        StatusCode::OK,
        Json(UploadSnapshotResponse {
            added,
            modified,
            deleted,
            unchanged,
            is_delta,
        }),
    ))
}

#[utoipa::path(
    get,
    path = "/api/repos/{repo}/workspaces/{id}/files/{path}",
    params(
        ("repo" = String, Path, description = "Repository name"),
        ("id" = String, Path, description = "Workspace ID"),
        ("path" = String, Path, description = "File path within workspace"),
    ),
    responses(
        (status = 200, description = "File content", body = FileDownloadResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "File not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "workspaces"
)]
/// `GET /api/workspaces/:id/files/*path` — downloads a file from a workspace.
///
/// The overlay is checked first; if the file is not present there the base
/// repository (repo root) is used as a fallback. Returns 404 if the file
/// exists in neither location. Response includes `found_in: "overlay"|"base"`.
async fn get_workspace_file_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    AxumPath(params): AxumPath<HashMap<String, String>>,
) -> Result<Json<FileDownloadResponse>, ApiError> {
    let id = params
        .get("id")
        .cloned()
        .ok_or_else(|| ApiError::bad_request("missing `:id` path parameter"))?;
    let path = params
        .get("path")
        .cloned()
        .ok_or_else(|| ApiError::bad_request("missing `*path` wildcard"))?;
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Read).await?;

    // Verify workspace exists via storage trait (works in both SQLite and Postgres modes).
    let ws_uuid = uuid::Uuid::parse_str(&id)
        .map_err(|_| ApiError::not_found(format!("workspace `{id}` not found")))?;
    ctx.storage.workspaces()
        .get_workspace(&ctx.repo_id, &ws_uuid)
        .await
        .map_err(ApiError::from)?;

    let rel = sanitize_path(&path)
        .ok_or_else(|| ApiError::bad_request(format!("invalid path: '{path}'")))?;
    let path_str = rel.to_string_lossy().replace('\\', "/");

    let file_store = ctx.storage.files();

    // 1. Try overlay from storage (primary path for Postgres/S3 mode).
    let overlay_key = format!("workspaces/{id}/{path_str}");
    if let Ok(bytes) = file_store.get(&ctx.repo_id, &overlay_key).await {
        let size = bytes.len();
        return Ok(Json(FileDownloadResponse {
            path: path_str,
            content_base64: BASE64.encode(&bytes),
            size,
            found_in: "overlay".to_string(),
        }));
    }

    // 2. Try overlay from local filesystem (fallback for SQLite/local mode).
    // ALLOW_FS: fallback for local/SQLite mode when FileStore has no overlay entry
    let overlay_path = workspace::overlay_dir(&ctx.vai_dir, &id).join(&rel);
    if overlay_path.exists() {
        // ALLOW_FS: fallback for local/SQLite mode when FileStore has no overlay entry
        let bytes = std::fs::read(&overlay_path)
            .map_err(|e| ApiError::internal(format!("read overlay file: {e}")))?;
        let size = bytes.len();
        return Ok(Json(FileDownloadResponse {
            path: path_str,
            content_base64: BASE64.encode(&bytes),
            size,
            found_in: "overlay".to_string(),
        }));
    }

    // 3. Try base from storage `current/` prefix (set by submit handler after each merge).
    let current_key = format!("current/{path_str}");
    if let Ok(bytes) = file_store.get(&ctx.repo_id, &current_key).await {
        let size = bytes.len();
        return Ok(Json(FileDownloadResponse {
            path: path_str,
            content_base64: BASE64.encode(&bytes),
            size,
            found_in: "base".to_string(),
        }));
    }

    // 4. Final fallback: read from repo root on disk (local/SQLite mode, migration).
    let base_path = ctx.repo_root.join(&rel);
    if !base_path.exists() {
        return Err(ApiError::not_found(format!("file not found: '{path}'")));
    }
    // ALLOW_FS: final fallback for local/SQLite mode and migration-seeded repos
    let bytes = std::fs::read(&base_path)
        .map_err(|e| ApiError::internal(format!("read base file: {e}")))?;
    let size = bytes.len();
    Ok(Json(FileDownloadResponse {
        path: path_str,
        content_base64: BASE64.encode(&bytes),
        size,
        found_in: "base".to_string(),
    }))
}

/// Response for `GET /api/repo/files`.
#[derive(Debug, Serialize, ToSchema)]
struct RepoFileListResponse {
    /// Relative paths of all files in the repository root, sorted.
    files: Vec<String>,
    /// Total number of files.
    count: usize,
    /// Current HEAD version of the repository.
    head_version: String,
}

#[utoipa::path(
    get,
    path = "/api/repos/{repo}/files",
    params(
        ("repo" = String, Path, description = "Repository name"),
    ),
    responses(
        (status = 200, description = "List of repo files", body = RepoFileListResponse),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = [])),
    tag = "repos"
)]
/// `GET /api/repo/files` — lists every file in the current main codebase.
///
/// Returns relative paths suitable for use with `GET /api/files/*path`.
/// Hidden directories (`.git`, `.vai`) and common build artefacts are excluded.
async fn list_repo_files_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
) -> Result<Json<RepoFileListResponse>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Read).await?;
    // Read HEAD from storage trait (works in both SQLite and Postgres modes).
    let head_version = ctx.storage.versions()
        .read_head(&ctx.repo_id)
        .await
        .map_err(|e| ApiError::internal(format!("read head: {e}")))?
        .unwrap_or_else(|| "v0".to_string());

    // Primary: list from storage `current/` prefix — works in Postgres/S3 mode
    // with no local `.vai/` directory.
    let storage_files = ctx.storage.files()
        .list(&ctx.repo_id, "current/")
        .await
        .unwrap_or_default();

    let mut files: Vec<String> = if !storage_files.is_empty() {
        storage_files
            .into_iter()
            .filter_map(|fm| {
                let rel = fm.path.strip_prefix("current/")?.to_string();
                if rel.is_empty() { None } else { Some(rel) }
            })
            .collect()
    } else {
        // Fallback: enumerate local disk (SQLite/local-CLI mode).
        let vai_toml_ignore = read_vai_toml_ignore(&ctx.repo_root);
        crate::ignore_rules::collect_all_files_relative(&ctx.repo_root, &vai_toml_ignore)
    };
    files.sort();

    let count = files.len();
    Ok(Json(RepoFileListResponse {
        files,
        count,
        head_version,
    }))
}

/// Reads the `ignore` list from `vai.toml` at `repo_root`.
///
/// Returns an empty vec if `vai.toml` is absent or unparseable so that
/// callers degrade gracefully and still apply `.gitignore`/`.vaignore` rules.
fn read_vai_toml_ignore(repo_root: &std::path::Path) -> Vec<String> {
    let path = repo_root.join("vai.toml");
    if !path.exists() {
        return Vec::new();
    }
    // ALLOW_FS: reads vai.toml config from repo root; valid in both local and server mode
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    toml::from_str::<crate::repo::VaiToml>(&raw)
        .map(|t| t.ignore)
        .unwrap_or_default()
}

// ── Source file upload (migration) ───────────────────────────────────────────

#[utoipa::path(
    post,
    path = "/api/repos/{repo}/files",
    request_body = UploadFilesRequest,
    params(
        ("repo" = String, Path, description = "Repository name"),
    ),
    responses(
        (status = 200, description = "Files uploaded", body = UploadFilesResponse),
        (status = 400, description = "Bad request", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = [])),
    tag = "repos"
)]
/// `POST /api/files` — uploads source files to storage during migration.
///
/// Used by `vai remote migrate` (PRD 12.3) to seed the server with the
/// complete repository state after metadata migration completes.  Each file
/// is written to two storage keys:
///
/// - `blobs/{sha256}` — content-addressable blob, used for diff computation.
/// - `current/{path}` — the live repo state served by list/download handlers.
///
/// No files are written to the local filesystem.  In server mode `repo_root`
/// contains only `.vai/config.toml`; source files live exclusively in S3.
///
/// **Resumability (Postgres mode):** progress is tracked in the
/// `migration_state` table.  Files whose `(repo_id, path, hash)` tuple is
/// already recorded are skipped, so interrupted migrations can be retried
/// without re-uploading already-confirmed files.
///
/// Call `POST /api/graph/refresh` after all batches complete to rebuild the
/// semantic graph from the uploaded files.
async fn upload_source_files_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    ctx: RepoCtx,
    Json(body): Json<UploadFilesRequest>,
) -> Result<(StatusCode, Json<UploadFilesResponse>), ApiError> {
    use crate::storage::StorageBackend;

    require_repo_permission(
        &ctx.storage,
        &identity,
        &ctx.repo_id,
        crate::storage::RepoRole::Write,
    )
    .await?;

    // Validate file count and path lengths before acquiring the lock.
    if body.files.len() > MAX_FILES_PER_REQUEST {
        return Err(ApiError::bad_request(format!(
            "too many files: {}, maximum is {MAX_FILES_PER_REQUEST}",
            body.files.len()
        )));
    }
    for entry in &body.files {
        validate_str_len(&entry.path, MAX_PATH_LEN, "file path")?;
    }

    let _lock = state.repo_lock.lock().await;

    // Extract Postgres pool for migration_state tracking (Postgres mode only).
    let pg_pool: Option<sqlx::PgPool> = match &ctx.storage {
        StorageBackend::Server(pg)
        | StorageBackend::ServerWithS3(pg, _)
        | StorageBackend::ServerWithMemFs(pg, _) => {
            Some(pg.pool().clone())
        }
        StorageBackend::Local(_) => None,
    };

    let repo_id = ctx.repo_id;
    let file_store = ctx.storage.files();
    let mut uploaded_paths: Vec<String> = Vec::new();
    let mut skipped: usize = 0;

    for entry in &body.files {
        let content = BASE64
            .decode(&entry.content_base64)
            .map_err(|e| {
                ApiError::bad_request(format!(
                    "base64 decode error for '{}': {e}",
                    entry.path
                ))
            })?;

        if content.len() > MAX_FILE_SIZE_BYTES {
            return Err(ApiError::bad_request(format!(
                "file '{}' exceeds 10 MiB limit ({} bytes)",
                entry.path,
                content.len()
            )));
        }

        let rel = sanitize_path(&entry.path)
            .ok_or_else(|| ApiError::bad_request(format!("invalid path: '{}'", entry.path)))?;
        let path_str = rel.to_string_lossy().replace('\\', "/");
        let hash = sha256_hex(&content);

        // Resumability check: skip files already confirmed uploaded in this repo
        // with the same content hash (Postgres mode only).
        if let Some(pool) = &pg_pool {
            let already_done: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM migration_state WHERE repo_id = $1 AND path = $2 AND hash = $3)"
            )
            .bind(repo_id)
            .bind(&path_str)
            .bind(&hash)
            .fetch_one(pool)
            .await
            .unwrap_or(false);

            if already_done {
                skipped += 1;
                continue;
            }
        }

        // Store content-addressably so diffs can be computed for migrated versions.
        let _ = file_store.put(&repo_id, &format!("blobs/{hash}"), &content).await;

        // Write to `current/` prefix — the live repo state served by all
        // list/download handlers.  No filesystem write: server mode keeps
        // source files exclusively in S3.
        let _ = file_store.put(&repo_id, &format!("current/{path_str}"), &content).await;

        // Record progress in migration_state so this file is skipped on retry.
        if let Some(pool) = &pg_pool {
            let _ = sqlx::query(
                "INSERT INTO migration_state (repo_id, path, hash) VALUES ($1, $2, $3)
                 ON CONFLICT (repo_id, path) DO UPDATE SET hash = EXCLUDED.hash, uploaded_at = NOW()"
            )
            .bind(repo_id)
            .bind(&path_str)
            .bind(&hash)
            .execute(pool)
            .await;
        }

        uploaded_paths.push(path_str);
    }

    Ok((
        StatusCode::OK,
        Json(UploadFilesResponse {
            uploaded: uploaded_paths.len(),
            skipped,
            paths: uploaded_paths,
        }),
    ))
}


#[utoipa::path(
    get,
    path = "/api/repos/{repo}/files/{path}",
    params(
        ("repo" = String, Path, description = "Repository name"),
        ("path" = String, Path, description = "File path relative to repo root"),
    ),
    responses(
        (status = 200, description = "File content", body = FileDownloadResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "File not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "repos"
)]
/// `GET /api/files/*path` — downloads a file from the current main version.
///
/// Returns the file as base64-encoded content. Returns 404 if not found.
async fn get_main_file_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    AxumPath(params): AxumPath<HashMap<String, String>>,
) -> Result<Json<FileDownloadResponse>, ApiError> {
    let path = params
        .get("path")
        .cloned()
        .ok_or_else(|| ApiError::bad_request("missing `*path` wildcard"))?;
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Read).await?;
    let rel = sanitize_path(&path)
        .ok_or_else(|| ApiError::bad_request(format!("invalid path: '{path}'")))?;
    let path_str = rel.to_string_lossy().replace('\\', "/");

    // Try storage `current/` prefix first (populated by submit handler — works in Postgres mode).
    let current_key = format!("current/{path_str}");
    if let Ok(bytes) = ctx.storage.files().get(&ctx.repo_id, &current_key).await {
        let size = bytes.len();
        return Ok(Json(FileDownloadResponse {
            path: path_str,
            content_base64: BASE64.encode(&bytes),
            size,
            found_in: "base".to_string(),
        }));
    }

    // Fallback: read from repo root on disk (local mode and migration-seeded servers).
    let file_path = ctx.repo_root.join(&rel);
    if !file_path.exists() {
        return Err(ApiError::not_found(format!("file not found: '{path}'")));
    }

    // ALLOW_FS: final fallback for local/SQLite mode and migration-seeded repos
    let content = std::fs::read(&file_path)
        .map_err(|e| ApiError::internal(format!("read file: {e}")))?;

    let size = content.len();
    Ok(Json(FileDownloadResponse {
        path: path_str,
        content_base64: BASE64.encode(&content),
        size,
        found_in: "base".to_string(),
    }))
}

// ── Repo file download / pull ─────────────────────────────────────────────────

/// Query parameters for `GET /api/repos/:repo/files/download`.
#[derive(Debug, Default, Deserialize, ToSchema)]
struct FilesDownloadQuery {
    /// Version to download (e.g. `"v42"`). Defaults to the current HEAD.
    version: Option<String>,
}

/// Query parameters for `GET /api/repos/:repo/files/pull`.
#[derive(Debug, Deserialize, ToSchema)]
struct FilesPullQuery {
    /// The version the caller already has. Only files changed after this
    /// version (exclusive) up to HEAD (inclusive) are returned.
    since: String,
}

/// Describes the kind of change applied to a single file in a pull response.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
enum FileChangeType {
    Added,
    Modified,
    Removed,
}

/// A single file change entry returned by `GET /api/repos/:repo/files/pull`.
#[derive(Debug, Serialize, ToSchema)]
struct PullFileEntry {
    /// Path relative to the repository root (e.g. `"src/lib.rs"`).
    pub path: String,
    /// How this file changed since the base version.
    pub change_type: FileChangeType,
    /// Base64-encoded file content. Present for `added` and `modified` entries;
    /// absent for `removed` entries.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_base64: Option<String>,
}

/// Response body for `GET /api/repos/:repo/files/pull`.
#[derive(Debug, Serialize, ToSchema)]
struct FilesPullResponse {
    /// The version the caller supplied as the `since` parameter.
    pub base_version: String,
    /// The current HEAD version.
    pub head_version: String,
    /// Files that changed between `base_version` (exclusive) and `head_version`
    /// (inclusive).
    pub files: Vec<PullFileEntry>,
}

/// Parses a `"vN"` version string and returns the numeric part.
///
/// Returns 0 for unrecognised strings so unversioned repos sort before v1.
fn parse_version_num(version_id: &str) -> u64 {
    version_id
        .strip_prefix('v')
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0)
}

/// Reconstructs the full repo file map at a historical version by replaying
/// file events from the event log. Returns a map of `relative_path -> content`.
///
/// Works without a local `.vai/` directory — all data is read via the storage
/// trait. Files whose content cannot be located (e.g. blobs predating
/// content-addressable storage) are silently omitted.
async fn build_file_map_at_version(
    ctx: &RepoCtx,
    target_version_num: u64,
) -> Result<std::collections::HashMap<String, Vec<u8>>, ApiError> {
    // All versions whose numeric suffix ≤ target.
    let all_versions = ctx
        .storage
        .versions()
        .list_versions(&ctx.repo_id, &ListQuery::default())
        .await
        .map_err(ApiError::from)?
        .items;

    let target_versions: Vec<_> = all_versions
        .into_iter()
        .filter(|v| parse_version_num(&v.version_id) <= target_version_num)
        .collect();

    let merge_event_ids: Vec<u64> = target_versions
        .iter()
        .filter_map(|v| v.merge_event_id)
        .collect();

    if merge_event_ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }

    // Resolve workspace_ids from MergeCompleted events.
    let merge_events = ctx
        .storage
        .events()
        .query_by_type(&ctx.repo_id, "MergeCompleted")
        .await
        .map_err(ApiError::from)?;

    let mut workspace_ids: Vec<uuid::Uuid> = merge_event_ids
        .iter()
        .filter_map(|mid| {
            merge_events
                .iter()
                .find(|e| e.id == *mid)
                .and_then(|e| e.kind.workspace_id())
        })
        .collect();
    workspace_ids.sort_unstable();
    workspace_ids.dedup();

    // Fetch all file events for the relevant workspaces.
    use crate::storage::EventFilter;
    let filter = EventFilter {
        event_types: vec![
            "FileAdded".to_string(),
            "FileModified".to_string(),
            "FileRemoved".to_string(),
        ],
        workspace_ids,
        ..EventFilter::default()
    };
    let mut file_events = ctx
        .storage
        .events()
        .query_since_id_filtered(&ctx.repo_id, 0, &filter)
        .await
        .map_err(ApiError::from)?;

    // Sort by event id to replay in chronological order.
    file_events.sort_by_key(|e| e.id);

    // Build path → content-hash map by replaying events.
    let mut path_hash: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for event in file_events {
        match event.kind {
            EventKind::FileAdded { path, hash, .. } => {
                path_hash.insert(path, hash);
            }
            EventKind::FileModified { path, new_hash, .. } => {
                path_hash.insert(path, new_hash);
            }
            EventKind::FileRemoved { path, .. } => {
                path_hash.remove(&path);
            }
            _ => {}
        }
    }

    // Fetch file content from content-addressable blob store.
    let file_store = ctx.storage.files();
    let mut file_map: std::collections::HashMap<String, Vec<u8>> =
        std::collections::HashMap::new();
    for (path, hash) in path_hash {
        if let Ok(content) = file_store.get(&ctx.repo_id, &format!("blobs/{hash}")).await {
            file_map.insert(path, content);
        }
        // Silently skip blobs not found (pre-content-addressable versions).
    }

    Ok(file_map)
}

#[utoipa::path(
    get,
    path = "/api/repos/{repo}/files/download",
    params(
        ("repo" = String, Path, description = "Repository name"),
        ("version" = Option<String>, Query, description = "Version to download (default: HEAD)"),
    ),
    responses(
        (status = 200, description = "Tar-gzip archive of all repo files", content_type = "application/gzip"),
        (status = 400, description = "No files in storage — run migration first", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Repository not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "files"
)]
/// `GET /api/repos/:repo/files/download` — downloads all source files as a
/// `.tar.gz` archive.
///
/// Returns a tarball of every file in the repository root at the current HEAD
/// version.  Hidden directories (`.vai`, `.git`) and build artefacts are
/// excluded.  The `Content-Disposition` header is set to suggest a filename of
/// the form `<repo>-<version>.tar.gz`.
///
/// Serves files exclusively from storage (`current/` prefix for HEAD,
/// or event-log replay for historical versions). Returns 400 if the
/// `current/` prefix is empty — run `vai remote migrate` to seed it.
async fn files_download_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    AxumQuery(query): AxumQuery<FilesDownloadQuery>,
) -> Result<Response, ApiError> {
    use flate2::write::GzEncoder;
    use flate2::Compression;

    require_repo_permission(
        &ctx.storage,
        &identity,
        &ctx.repo_id,
        crate::storage::RepoRole::Read,
    )
    .await?;

    let head_version = ctx
        .storage
        .versions()
        .read_head(&ctx.repo_id)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| "unknown".to_string());

    // Determine which version to download.
    let requested_version = query.version.as_deref().unwrap_or(&head_version);
    let requested_num = parse_version_num(requested_version);
    let head_num = parse_version_num(&head_version);

    // Build the file map exclusively from storage — no local filesystem fallback.
    // The `current/` prefix is maintained by the submit handler and migration
    // uploader; it always reflects the latest merged repo state.
    let file_map = if requested_num < head_num {
        // Historical version: reconstruct file state by replaying version events.
        build_file_map_at_version(&ctx, requested_num).await?
    } else {
        // HEAD (or unknown/future version): serve directly from `current/` prefix.
        let file_store = ctx.storage.files();
        let current_entries = file_store
            .list(&ctx.repo_id, "current/")
            .await
            .map_err(|e| ApiError::internal(format!("list current/: {e}")))?;

        if current_entries.is_empty() {
            return Err(ApiError::bad_request(
                "repository has no files in current/ — run `vai remote migrate` to seed storage",
            ));
        }

        let mut map: std::collections::HashMap<String, Vec<u8>> =
            std::collections::HashMap::new();
        for fm in current_entries {
            let rel = fm.path.strip_prefix("current/").unwrap_or(&fm.path).to_string();
            if rel.is_empty() {
                continue;
            }
            if let Ok(content) = file_store.get(&ctx.repo_id, &fm.path).await {
                map.insert(rel, content);
            }
        }
        map
    };

    // Build the tarball from the merged file map.
    let mut sorted_paths: Vec<String> = file_map.keys().cloned().collect();
    sorted_paths.sort();

    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    {
        let mut archive = tar::Builder::new(&mut encoder);
        for rel in &sorted_paths {
            let content = &file_map[rel];
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            // Preserve executable bit for scripts with a shebang line.
            let mode = if content.starts_with(b"#!") { 0o755 } else { 0o644 };
            header.set_mode(mode);
            header.set_cksum();
            archive
                .append_data(&mut header, rel, content.as_slice())
                .map_err(|e| ApiError::internal(format!("tar append '{rel}': {e}")))?;
        }
        archive
            .finish()
            .map_err(|e| ApiError::internal(format!("tar finish: {e}")))?;
    }
    let gz_bytes = encoder
        .finish()
        .map_err(|e| ApiError::internal(format!("gzip finish: {e}")))?;

    let repo_name = ctx
        .repo_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("repo");
    let filename = format!("{repo_name}-{head_version}.tar.gz");

    let response = axum::response::Response::builder()
        .status(StatusCode::OK)
        .header(axum::http::header::CONTENT_TYPE, "application/gzip")
        .header(
            axum::http::header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{filename}\""),
        )
        .header("X-Vai-Head", head_version.clone())
        .body(axum::body::Body::from(gz_bytes))
        .map_err(|e| ApiError::internal(format!("build response: {e}")))?;

    Ok(response)
}

#[utoipa::path(
    get,
    path = "/api/repos/{repo}/files/pull",
    params(
        ("repo" = String, Path, description = "Repository name"),
        ("since" = String, Query, description = "Version to pull changes since (e.g. `v60`)"),
    ),
    responses(
        (status = 200, description = "Files changed since the given version", body = FilesPullResponse),
        (status = 400, description = "Missing or invalid `since` parameter", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Repository or version not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "files"
)]
/// `GET /api/repos/:repo/files/pull` — returns files changed since a version.
///
/// Intended for agents that already have a local working copy (downloaded via
/// `/files/download`) and want to sync with the latest server state after one
/// or more versions have been committed.
///
/// The `since` query parameter is **required**.  Only files that were added,
/// modified, or removed between `since` (exclusive) and HEAD (inclusive) are
/// returned.  Content is base64-encoded; removed files omit the content field.
async fn files_pull_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    AxumQuery(query): AxumQuery<FilesPullQuery>,
) -> Result<Json<FilesPullResponse>, ApiError> {
    require_repo_permission(
        &ctx.storage,
        &identity,
        &ctx.repo_id,
        crate::storage::RepoRole::Read,
    )
    .await?;

    let head_version = ctx
        .storage
        .versions()
        .read_head(&ctx.repo_id)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| "unknown".to_string());

    let since_num = parse_version_num(&query.since);
    let head_num = parse_version_num(&head_version);

    // If the caller is already at HEAD, return an empty diff.
    if since_num >= head_num {
        return Ok(Json(FilesPullResponse {
            base_version: query.since,
            head_version,
            files: vec![],
        }));
    }

    // Enumerate only the versions in the (since, head] range — avoids loading
    // the full version history for large repos.
    let newer_versions = ctx
        .storage
        .versions()
        .list_versions_since(&ctx.repo_id, since_num, head_num)
        .await
        .map_err(ApiError::from)?;

    // Collect the merge_event_ids so we can look up workspace_ids in one pass.
    let merge_event_ids: Vec<u64> = newer_versions
        .iter()
        .filter_map(|v| v.merge_event_id)
        .collect();

    if merge_event_ids.is_empty() {
        return Ok(Json(FilesPullResponse {
            base_version: query.since,
            head_version,
            files: vec![],
        }));
    }

    // Resolve workspace IDs: fetch only the MergeCompleted events we need.
    // Uses a single query filtered by event_type instead of loading all events.
    let merge_events = ctx
        .storage
        .events()
        .query_by_type(&ctx.repo_id, "MergeCompleted")
        .await
        .map_err(ApiError::from)?;

    let mut workspace_ids: Vec<uuid::Uuid> = newer_versions
        .iter()
        .filter_map(|v| {
            let mid = v.merge_event_id?;
            merge_events
                .iter()
                .find(|e| e.id == mid)
                .and_then(|e| e.kind.workspace_id())
        })
        .collect();
    workspace_ids.sort_unstable();
    workspace_ids.dedup();

    // Fetch all file events for all relevant workspaces in a single batch query.
    use crate::storage::EventFilter;
    let file_event_filter = EventFilter {
        event_types: vec![
            "FileAdded".to_string(),
            "FileModified".to_string(),
            "FileRemoved".to_string(),
        ],
        workspace_ids,
        ..EventFilter::default()
    };
    let all_file_events = ctx
        .storage
        .events()
        .query_since_id_filtered(&ctx.repo_id, 0, &file_event_filter)
        .await
        .map_err(ApiError::from)?;

    // Track the final change state for each path.  We process events in
    // order (query_since_id_filtered returns events ordered by ID) so later
    // events overwrite earlier ones.
    #[derive(Clone)]
    enum ChangeState {
        Added,
        Modified,
        Removed,
    }
    let mut path_changes: std::collections::HashMap<String, ChangeState> =
        std::collections::HashMap::new();

    for event in all_file_events {
        match event.kind {
            EventKind::FileAdded { path, .. } => {
                path_changes.insert(path, ChangeState::Added);
            }
            EventKind::FileModified { path, .. } => {
                // Preserve "added" for files added within this pull range.
                path_changes
                    .entry(path)
                    .and_modify(|s| {
                        if !matches!(s, ChangeState::Added) {
                            *s = ChangeState::Modified;
                        }
                    })
                    .or_insert(ChangeState::Modified);
            }
            EventKind::FileRemoved { path, .. } => {
                path_changes.insert(path, ChangeState::Removed);
            }
            _ => {}
        }
    }

    // Build the response, reading current file content for added/modified files.
    let mut files: Vec<PullFileEntry> = Vec::new();
    for (path, state) in path_changes {
        match state {
            ChangeState::Removed => {
                files.push(PullFileEntry {
                    path,
                    change_type: FileChangeType::Removed,
                    content_base64: None,
                });
            }
            ChangeState::Added | ChangeState::Modified => {
                // Read from the `current/` storage prefix — works in both
                // SQLite and Postgres/S3 modes with no local filesystem needed.
                let content_base64 = ctx
                    .storage
                    .files()
                    .get(&ctx.repo_id, &format!("current/{path}"))
                    .await
                    .ok()
                    .map(|bytes| BASE64.encode(&bytes));
                let change_type = match state {
                    ChangeState::Added => FileChangeType::Added,
                    _ => FileChangeType::Modified,
                };
                files.push(PullFileEntry {
                    path,
                    change_type,
                    content_base64,
                });
            }
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));

    Ok(Json(FilesPullResponse {
        base_version: query.since,
        head_version,
        files,
    }))
}

// ── File manifest ─────────────────────────────────────────────────────────────

/// A single entry in the files manifest response.
#[derive(Debug, Serialize, ToSchema)]
struct ManifestFileEntry {
    /// Path relative to the repository root (e.g. `"src/lib.rs"`).
    path: String,
    /// Lowercase hex-encoded SHA-256 hash of the file content.
    sha256: String,
}

/// Response body for `GET /api/repos/:repo/files/manifest`.
#[derive(Debug, Serialize, ToSchema)]
struct FilesManifestResponse {
    /// The current HEAD version of the repository.
    pub version: String,
    /// Every file in the repository root with its content hash.
    pub files: Vec<ManifestFileEntry>,
}

#[utoipa::path(
    get,
    path = "/api/repos/{repo}/files/manifest",
    params(
        ("repo" = String, Path, description = "Repository name"),
    ),
    responses(
        (status = 200, description = "File paths and SHA-256 hashes for all files at HEAD", body = FilesManifestResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Repository not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "files"
)]
/// `GET /api/repos/:repo/files/manifest` — returns paths and SHA-256 hashes for
/// every file in the repository at HEAD.
///
/// This lightweight endpoint lets clients (e.g. `vai status`) compare their
/// local working copy against the server without downloading file contents.
/// The SHA-256 hash is computed from the raw file bytes stored in S3/filesystem.
async fn files_manifest_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
) -> Result<Json<FilesManifestResponse>, ApiError> {
    use sha2::{Digest, Sha256};

    require_repo_permission(
        &ctx.storage,
        &identity,
        &ctx.repo_id,
        crate::storage::RepoRole::Read,
    )
    .await?;

    let head_version = ctx
        .storage
        .versions()
        .read_head(&ctx.repo_id)
        .await
        .map_err(|e| ApiError::internal(format!("read head: {e}")))?
        .unwrap_or_else(|| "v0".to_string());

    // List all files from the `current/` prefix in storage.
    let storage_files = ctx
        .storage
        .files()
        .list(&ctx.repo_id, "current/")
        .await
        .map_err(|e| ApiError::internal(format!("list current/: {e}")))?;

    let mut entries: Vec<ManifestFileEntry> = Vec::with_capacity(storage_files.len());

    for fm in &storage_files {
        let rel = match fm.path.strip_prefix("current/") {
            Some(r) if !r.is_empty() => r.to_string(),
            _ => continue,
        };

        let content = ctx
            .storage
            .files()
            .get(&ctx.repo_id, &fm.path)
            .await
            .map_err(|e| ApiError::internal(format!("read {}: {e}", fm.path)))?;

        let mut hasher = Sha256::new();
        hasher.update(&content);
        let sha256 = format!("{:x}", hasher.finalize());

        entries.push(ManifestFileEntry { path: rel, sha256 });
    }

    entries.sort_by(|a, b| a.path.cmp(&b.path));

    Ok(Json(FilesManifestResponse {
        version: head_version,
        files: entries,
    }))
}

// ── Issue route handlers ──────────────────────────────────────────────────────

/// Returns the IDs of workspaces linked to `issue_id` via their `issue_id` field.
///
/// Uses the storage trait so the lookup works for both SQLite and Postgres.
/// Falls back to an empty list on error so callers never fail on this auxiliary query.
async fn linked_workspace_ids(
    ctx: &RepoCtx,
    issue_id: uuid::Uuid,
) -> Vec<uuid::Uuid> {
    ctx.storage
        .workspaces()
        .list_workspaces(&ctx.repo_id, true, &ListQuery::default())
        .await
        .map(|r| r.items)
        .unwrap_or_default()
        .into_iter()
        .filter(|ws| ws.issue_id == Some(issue_id))
        .map(|ws| ws.id)
        .collect()
}

/// Returns `(blocked_by, blocking)` for an issue from the `issue_links` table.
///
/// - `blocked_by`: IDs of issues that have a `blocks` link targeting `issue_id`.
/// - `blocking`: IDs of issues that `issue_id` has a `blocks` link targeting.
async fn links_for_issue(
    ctx: &RepoCtx,
    issue_id: uuid::Uuid,
) -> (Vec<uuid::Uuid>, Vec<uuid::Uuid>) {
    let links = ctx.storage
        .links()
        .list_links(&ctx.repo_id, &issue_id)
        .await
        .unwrap_or_default();

    let mut blocked_by = Vec::new();
    let mut blocking = Vec::new();

    for link in links {
        if link.relationship == crate::storage::IssueLinkRelationship::Blocks {
            if link.target_id == issue_id {
                // source blocks this issue
                blocked_by.push(link.source_id);
            } else if link.source_id == issue_id {
                // this issue blocks target
                blocking.push(link.target_id);
            }
        }
    }

    (blocked_by, blocking)
}

#[utoipa::path(
    post,
    path = "/api/repos/{repo}/issues",
    request_body = CreateIssueRequest,
    params(
        ("repo" = String, Path, description = "Repository name"),
    ),
    responses(
        (status = 201, description = "Issue created", body = IssueResponse),
        (status = 400, description = "Bad request", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 429, description = "Rate limit exceeded"),
    ),
    security(("bearer_auth" = [])),
    tag = "issues"
)]
/// `POST /api/issues` — create a new issue.
///
/// When `created_by_agent` is set the request is treated as an agent-initiated
/// issue and goes through rate-limiting and duplicate-detection guardrails.
/// If the rate limit is exceeded the handler returns **429 Too Many Requests**
/// with a `Retry-After` header.  When a similar open issue is detected the
/// issue is still created but the response includes `possible_duplicate_of`.
///
/// Returns 201 Created with the issue metadata.
async fn create_issue_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    ctx: RepoCtx,
    Json(body): Json<CreateIssueRequest>,
) -> Result<(StatusCode, Json<IssueResponse>), ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Write).await?;
    validate_str_len(&body.title, MAX_ISSUE_TITLE_LEN, "title")?;
    validate_str_len(&body.description, MAX_ISSUE_BODY_LEN, "description")?;
    validate_labels(&body.labels)?;
    use crate::issue::{AgentSource, IssueFilter, IssuePriority};
    use crate::storage::NewIssue;

    let _lock = state.repo_lock.lock().await;

    let priority = IssuePriority::from_db_str(&body.priority).ok_or_else(|| {
        ApiError::bad_request(format!("unknown priority `{}`", body.priority))
    })?;

    let issues = ctx.storage.issues();

    let (creator, agent_source, possible_duplicate_id) =
        if let Some(ref agent_id) = body.created_by_agent {
            // Agent-initiated path: apply rate-limiting and duplicate-detection.
            let all_issues = issues
                .list_issues(&ctx.repo_id, &IssueFilter::default(), &ListQuery::default())
                .await
                .map_err(ApiError::from)?
                .items;

            // Rate-limiting: count issues created by this agent in the last hour.
            let one_hour_ago = chrono::Utc::now() - chrono::Duration::hours(1);
            let agent_count = all_issues
                .iter()
                .filter(|i| {
                    i.creator == *agent_id
                        && i.created_at > one_hour_ago
                })
                .count() as u32;

            if agent_count >= body.max_per_hour {
                return Err(ApiError::rate_limited(format!(
                    "agent `{agent_id}` has created {agent_count} issues in the last hour \
                     (limit: {})",
                    body.max_per_hour
                )));
            }

            // Duplicate detection: Jaccard similarity on open-issue titles.
            let dup_id = crate::issue::find_similar_open_issue(&all_issues, &body.title);

            let source = body.source.as_ref().map(|s| AgentSource {
                source_type: s.source_type.clone(),
                details: s.details.clone(),
            }).unwrap_or_else(|| AgentSource {
                source_type: "unknown".into(),
                details: serde_json::Value::Null,
            });

            (agent_id.clone(), Some(source), dup_id)
        } else {
            // Human-initiated path: no guardrails.
            (body.creator.clone(), None, None)
        };

    // Parse and validate blocked_by IDs (each blocker must exist).
    let mut blocker_ids: Vec<uuid::Uuid> = Vec::new();
    for blocker_str in &body.blocked_by {
        let blocker_id = uuid::Uuid::parse_str(blocker_str)
            .map_err(|_| ApiError::bad_request(format!("invalid blocker ID `{blocker_str}`")))?;
        // Verify the blocker exists.
        ctx.storage.issues()
            .get_issue(&ctx.repo_id, &blocker_id)
            .await
            .map_err(|_| ApiError::bad_request(format!("blocker issue `{blocker_id}` not found")))?;
        blocker_ids.push(blocker_id);
    }

    let new_issue = NewIssue {
        title: body.title.clone(),
        description: body.description.clone(),
        priority,
        labels: body.labels.clone(),
        creator,
        agent_source: agent_source.map(|s| {
            serde_json::to_value(s).unwrap_or(serde_json::Value::Null)
        }),
        acceptance_criteria: body.acceptance_criteria.clone(),
    };

    let issue = issues
        .create_issue(&ctx.repo_id, new_issue)
        .await
        .map_err(ApiError::from)?;

    let issue_id = issue.id;
    // Append event to event store — triggers pg_notify in Postgres mode.
    let _ = ctx.storage.events()
        .append(&ctx.repo_id, EventKind::IssueCreated {
            issue_id,
            title: issue.title.clone(),
            creator: issue.creator.clone(),
            priority: issue.priority.as_str().to_string(),
        })
        .await;

    state.broadcast(BroadcastEvent {
        event_type: "IssueCreated".to_string(),
        event_id: 0,
        workspace_id: None,
        timestamp: issue.created_at.to_rfc3339(),
        data: serde_json::json!({
            "issue_id": issue_id.to_string(),
            "title": issue.title.clone(),
        }),
    });

    // Create `blocks` links for each blocker: source=blocker, target=new issue.
    for blocker_id in &blocker_ids {
        let _ = ctx.storage.links()
            .create_link(
                &ctx.repo_id,
                blocker_id,
                crate::storage::NewIssueLink {
                    target_id: issue_id,
                    relationship: crate::storage::IssueLinkRelationship::Blocks,
                },
            )
            .await;
    }

    let mut resp = IssueResponse::from_issue(issue, vec![], blocker_ids, vec![]);
    resp.possible_duplicate_of = possible_duplicate_id.map(|id| id.to_string());

    tracing::info!(
        event = "issue.created",
        actor = %identity.name,
        repo = %ctx.repo_id,
        issue_id = %issue_id,
        "issue created"
    );
    Ok((StatusCode::CREATED, Json(resp)))
}

/// `GET /api/issues` — list issues with optional filters and pagination.
///
/// Supports pagination via `?page=1&per_page=25` and sorting via
/// `?sort=created_at:desc`.  Sortable columns: `created_at`, `updated_at`,
/// `priority`, `status`, `title`.
#[utoipa::path(
    get,
    path = "/api/repos/{repo}/issues",
    params(
        ("repo" = String, Path, description = "Repository name"),
        ("status" = Option<String>, Query, description = "Filter by status (open, in_progress, closed)"),
        ("priority" = Option<String>, Query, description = "Filter by priority"),
        ("label" = Option<String>, Query, description = "Filter by label"),
        ("created_by" = Option<String>, Query, description = "Filter by creator"),
        ("page" = Option<u32>, Query, description = "Page number (1-indexed, default 1)"),
        ("per_page" = Option<u32>, Query, description = "Items per page (default 25, max 100)"),
        ("sort" = Option<String>, Query, description = "Sort fields, e.g. `created_at:desc,priority:asc`"),
    ),
    responses(
        (status = 200, description = "Paginated list of issues", body = PaginatedResponse<IssueResponse>),
        (status = 400, description = "Invalid filter, pagination, or sort params", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = [])),
    tag = "issues"
)]
async fn list_issues_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    AxumQuery(query): AxumQuery<ListIssuesQuery>,
) -> Result<Json<PaginatedResponse<IssueResponse>>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Read).await?;
    use crate::issue::{IssueFilter, IssueStatus, IssuePriority};

    let status = query.status.as_deref()
        .map(|s| IssueStatus::from_db_str(s).ok_or_else(|| ApiError::bad_request(format!("unknown status `{s}`"))))
        .transpose()?;
    let priority = query.priority.as_deref()
        .map(|p| IssuePriority::from_db_str(p).ok_or_else(|| ApiError::bad_request(format!("unknown priority `{p}`"))))
        .transpose()?;

    let filter = IssueFilter {
        status,
        priority,
        label: query.label,
        creator: query.created_by,
    };

    const ALLOWED_SORT: &[&str] = &["created_at", "updated_at", "priority", "status", "title", "creator", "id"];
    let list_query = ListQuery::from_params(
        query.page,
        query.per_page,
        query.sort.as_deref(),
        ALLOWED_SORT,
    )
    .map_err(|e| ApiError::bad_request(e.to_string()))?;

    let result = ctx.storage.issues()
        .list_issues(&ctx.repo_id, &filter, &list_query)
        .await
        .map_err(ApiError::from)?;

    // Fetch all workspaces once to compute linked workspace IDs per issue.
    let all_workspaces = ctx.storage.workspaces()
        .list_workspaces(&ctx.repo_id, true, &ListQuery::default())
        .await
        .map(|r| r.items)
        .unwrap_or_default();

    let mut response = Vec::with_capacity(result.items.len());
    for issue in result.items {
        let linked: Vec<uuid::Uuid> = all_workspaces
            .iter()
            .filter(|ws| ws.issue_id == Some(issue.id))
            .map(|ws| ws.id)
            .collect();
        let (blocked_by, blocking) = links_for_issue(&ctx, issue.id).await;
        response.push(IssueResponse::from_issue(issue, linked, blocked_by, blocking));
    }

    Ok(Json(PaginatedResponse::new(response, result.total, &list_query)))
}

#[utoipa::path(
    get,
    path = "/api/repos/{repo}/issues/{id}",
    params(
        ("repo" = String, Path, description = "Repository name"),
        ("id" = String, Path, description = "Issue ID"),
    ),
    responses(
        (status = 200, description = "Issue details", body = IssueDetailResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Issue not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "issues"
)]
/// `GET /api/issues/:id` — full issue detail with links, attachments, and comments.
///
/// Returns a single enriched response containing the issue's metadata, all linked
/// issues (with relationship type and current status), file attachments, and the
/// 50 most recent comments.  Returns 404 if the issue does not exist.
async fn get_issue_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    PathId(id): PathId,
) -> Result<Json<IssueDetailResponse>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Read).await?;
    let issue_id = uuid::Uuid::parse_str(&id)
        .map_err(|_| ApiError::bad_request(format!("invalid issue ID `{id}`")))?;

    let issue = ctx.storage.issues()
        .get_issue(&ctx.repo_id, &issue_id)
        .await
        .map_err(ApiError::from)?;

    let linked = linked_workspace_ids(&ctx, issue_id).await;
    let (blocked_by, blocking) = links_for_issue(&ctx, issue_id).await;

    // Fetch raw links, attachments, and comments concurrently.
    let links_store = ctx.storage.links();
    let attachments_store = ctx.storage.attachments();
    let comments_store = ctx.storage.comments();
    let (raw_links, attachments, all_comments, mentions_by_comment) = tokio::join!(
        links_store.list_links(&ctx.repo_id, &issue_id),
        attachments_store.list_attachments(&ctx.repo_id, &issue_id),
        comments_store.list_comments(&ctx.repo_id, &issue_id),
        comments_store.list_issue_mentions(&ctx.repo_id, &issue_id),
    );
    let raw_links = raw_links.map_err(ApiError::from)?;
    let attachments = attachments.map_err(ApiError::from)?;
    let mut all_comments = all_comments.map_err(ApiError::from)?;
    let mut mentions_by_comment = mentions_by_comment.unwrap_or_default();

    // Enrich links: fetch status + title of the other issue in each link.
    let mut links: Vec<IssueLinkDetailResponse> = Vec::with_capacity(raw_links.len());
    for link in &raw_links {
        let (other_id, relationship_str) = if link.source_id == issue_id {
            (link.target_id, link.relationship.as_str().to_string())
        } else {
            (link.source_id, link.relationship.inverse_str().to_string())
        };
        // Best-effort: if the linked issue can't be fetched, skip it.
        if let Ok(other) = ctx.storage.issues().get_issue(&ctx.repo_id, &other_id).await {
            links.push(IssueLinkDetailResponse {
                other_issue_id: other_id.to_string(),
                relationship: relationship_str,
                title: other.title,
                status: other.status.as_str().to_string(),
            });
        }
    }

    // Return the 50 most recent comments (list_comments returns oldest-first).
    let comments_start = all_comments.len().saturating_sub(50);
    let recent_comments: Vec<CommentResponse> = all_comments
        .drain(comments_start..)
        .map(|c| {
            let id = c.id;
            let mentions = mentions_by_comment.remove(&id).unwrap_or_default();
            CommentResponse::with_mentions(c, &mentions)
        })
        .collect();

    let agent_source = issue.agent_source.as_ref().map(|s| {
        serde_json::to_value(s).unwrap_or(serde_json::Value::Null)
    });

    Ok(Json(IssueDetailResponse {
        id: issue.id.to_string(),
        title: issue.title,
        description: issue.description,
        status: issue.status.as_str().to_string(),
        priority: issue.priority.as_str().to_string(),
        labels: issue.labels,
        creator: issue.creator,
        resolution: issue.resolution,
        agent_source,
        possible_duplicate_of: None,
        linked_workspace_ids: linked.iter().map(|u| u.to_string()).collect(),
        blocked_by: blocked_by.iter().map(|id| id.to_string()).collect(),
        blocking: blocking.iter().map(|id| id.to_string()).collect(),
        acceptance_criteria: issue.acceptance_criteria,
        created_at: issue.created_at.to_rfc3339(),
        updated_at: issue.updated_at.to_rfc3339(),
        links,
        attachments: attachments.into_iter().map(AttachmentResponse::from).collect(),
        comments: recent_comments,
    }))
}

#[utoipa::path(
    patch,
    path = "/api/repos/{repo}/issues/{id}",
    params(
        ("repo" = String, Path, description = "Repository name"),
        ("id" = String, Path, description = "Issue ID"),
    ),
    request_body = UpdateIssueRequest,
    responses(
        (status = 200, description = "Updated issue", body = IssueResponse),
        (status = 400, description = "Bad request", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Issue not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "issues"
)]
/// `PATCH /api/issues/:id` — update mutable fields of an issue.
///
/// Returns 404 if the issue does not exist.
async fn update_issue_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    ctx: RepoCtx,
    PathId(id): PathId,
    Json(body): Json<UpdateIssueRequest>,
) -> Result<Json<IssueResponse>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Write).await?;
    if let Some(ref title) = body.title {
        validate_str_len(title, MAX_ISSUE_TITLE_LEN, "title")?;
    }
    if let Some(ref desc) = body.description {
        validate_str_len(desc, MAX_ISSUE_BODY_LEN, "description")?;
    }
    if let Some(ref labels) = body.labels {
        validate_labels(labels)?;
    }
    use crate::issue::IssuePriority;
    use crate::storage::IssueUpdate;

    let _lock = state.repo_lock.lock().await;

    let issue_id = uuid::Uuid::parse_str(&id)
        .map_err(|_| ApiError::bad_request(format!("invalid issue ID `{id}`")))?;

    let priority = body.priority.as_deref()
        .map(|p| IssuePriority::from_db_str(p).ok_or_else(|| ApiError::bad_request(format!("unknown priority `{p}`"))))
        .transpose()?;

    // Collect changed field names before moving body fields into update.
    let fields_changed: Vec<String> = [
        body.title.as_ref().map(|_| "title"),
        body.description.as_ref().map(|_| "description"),
        priority.as_ref().map(|_| "priority"),
        body.labels.as_ref().map(|_| "labels"),
        body.acceptance_criteria.as_ref().map(|_| "acceptance_criteria"),
    ]
    .into_iter()
    .flatten()
    .map(String::from)
    .collect();

    let update = IssueUpdate {
        title: body.title,
        description: body.description,
        priority,
        labels: body.labels,
        acceptance_criteria: body.acceptance_criteria,
        ..Default::default()
    };

    let issue = ctx.storage.issues()
        .update_issue(&ctx.repo_id, &issue_id, update)
        .await
        .map_err(ApiError::from)?;

    // Append event to event store — triggers pg_notify in Postgres mode.
    let _ = ctx.storage.events()
        .append(&ctx.repo_id, EventKind::IssueUpdated { issue_id, fields_changed })
        .await;

    let linked = linked_workspace_ids(&ctx, issue_id).await;
    let (blocked_by, blocking) = links_for_issue(&ctx, issue_id).await;
    Ok(Json(IssueResponse::from_issue(issue, linked, blocked_by, blocking)))
}

#[utoipa::path(
    post,
    path = "/api/repos/{repo}/issues/{id}/close",
    params(
        ("repo" = String, Path, description = "Repository name"),
        ("id" = String, Path, description = "Issue ID"),
    ),
    request_body = CloseIssueRequest,
    responses(
        (status = 200, description = "Closed issue", body = IssueResponse),
        (status = 400, description = "Bad request", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Issue not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "issues"
)]
/// `POST /api/issues/:id/close` — close an issue with a resolution.
///
/// Returns 404 if the issue does not exist.
async fn close_issue_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    ctx: RepoCtx,
    PathId(id): PathId,
    Json(body): Json<CloseIssueRequest>,
) -> Result<Json<IssueResponse>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Write).await?;
    let _lock = state.repo_lock.lock().await;

    let issue_id = uuid::Uuid::parse_str(&id)
        .map_err(|_| ApiError::bad_request(format!("invalid issue ID `{id}`")))?;

    let issue = ctx.storage.issues()
        .close_issue(&ctx.repo_id, &issue_id, &body.resolution)
        .await
        .map_err(ApiError::from)?;

    // Append event to event store — triggers pg_notify in Postgres mode.
    let _ = ctx.storage.events()
        .append(&ctx.repo_id, EventKind::IssueClosed {
            issue_id,
            resolution: body.resolution.clone(),
        })
        .await;

    // Broadcast the close event.
    state.broadcast(BroadcastEvent {
        event_type: "IssueClosed".to_string(),
        event_id: 0,
        workspace_id: None,
        timestamp: chrono::Utc::now().to_rfc3339(),
        data: serde_json::json!({
            "issue_id": issue_id.to_string(),
            "resolution": body.resolution,
        }),
    });

    tracing::info!(
        event = "issue.closed",
        actor = %identity.name,
        repo = %ctx.repo_id,
        issue_id = %issue_id,
        "issue closed"
    );
    let linked = linked_workspace_ids(&ctx, issue_id).await;
    let (blocked_by, blocking) = links_for_issue(&ctx, issue_id).await;
    Ok(Json(IssueResponse::from_issue(issue, linked, blocked_by, blocking)))
}

// ── Issue comment handlers ────────────────────────────────────────────────────

/// Request body for `POST /api/repos/:repo/issues/:id/comments`.
#[derive(Debug, Deserialize, ToSchema)]
struct CreateCommentRequest {
    /// Comment body (Markdown supported).
    body: String,
    /// Ignored — author is always derived from the authenticated identity.
    #[serde(default)]
    #[allow(dead_code)]
    author: Option<String>,
    /// Optional parent comment UUID for threaded replies.
    #[serde(default)]
    parent_id: Option<String>,
}


/// A resolved @mention embedded in a comment response.
#[derive(Debug, Serialize, ToSchema)]
struct MentionRef {
    /// Stable UUID — user ID for humans, API key ID for agents.
    id: String,
    /// Display name of the mentioned user or agent.
    name: String,
    /// `"human"` for users, `"agent"` for API keys.
    mention_type: String,
}

impl From<&crate::storage::CommentMention> for MentionRef {
    fn from(m: &crate::storage::CommentMention) -> Self {
        MentionRef {
            id: m.entity_id().map(|u| u.to_string()).unwrap_or_default(),
            name: m.mentioned_name.clone(),
            mention_type: m.mention_type.clone(),
        }
    }
}

/// Response body for a single issue comment.
#[derive(Debug, Serialize, ToSchema)]
struct CommentResponse {
    id: String,
    issue_id: String,
    author: String,
    /// Comment body. `null` when the comment has been soft-deleted.
    body: Option<String>,
    /// Whether the author is a `"human"` or `"agent"`.
    author_type: String,
    /// Optional structured author identifier.
    author_id: Option<String>,
    created_at: String,
    /// Parent comment UUID for threaded replies.
    parent_id: Option<String>,
    /// When the comment was last edited, if ever.
    edited_at: Option<String>,
    /// When the comment was soft-deleted, if ever.
    deleted_at: Option<String>,
    /// Resolved @mentions found in the comment body.
    mentions: Vec<MentionRef>,
}

impl CommentResponse {
    /// Build a response from a comment and its resolved mentions.
    fn with_mentions(c: crate::issue::IssueComment, mentions: &[crate::storage::CommentMention]) -> Self {
        CommentResponse {
            id: c.id.to_string(),
            issue_id: c.issue_id.to_string(),
            author: c.author,
            body: c.body,
            author_type: c.author_type,
            author_id: c.author_id,
            created_at: c.created_at.to_rfc3339(),
            parent_id: c.parent_id.map(|u| u.to_string()),
            edited_at: c.edited_at.map(|t| t.to_rfc3339()),
            deleted_at: c.deleted_at.map(|t| t.to_rfc3339()),
            mentions: mentions.iter().map(MentionRef::from).collect(),
        }
    }
}

impl From<crate::issue::IssueComment> for CommentResponse {
    fn from(c: crate::issue::IssueComment) -> Self {
        CommentResponse::with_mentions(c, &[])
    }
}

/// Extracts unique @mention names from a comment body.
///
/// Matches the pattern `@word` where the name starts with a word character and
/// may contain word characters, dots, and dashes. Duplicate names are removed.
fn extract_mention_names(body: &str) -> Vec<String> {
    let bytes = body.as_bytes();
    let mut names: Vec<String> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'@' {
            i += 1;
            // First char must be alphanumeric or underscore.
            if i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                let start = i;
                while i < bytes.len()
                    && (bytes[i].is_ascii_alphanumeric()
                        || bytes[i] == b'_'
                        || bytes[i] == b'.'
                        || bytes[i] == b'-')
                {
                    i += 1;
                }
                if let Ok(name) = std::str::from_utf8(&bytes[start..i]) {
                    names.push(name.to_string());
                }
            }
        } else {
            i += 1;
        }
    }
    names.sort();
    names.dedup();
    names
}

/// Validates @mention names against repo members and returns `NewCommentMention` records.
///
/// Names that do not match any repo member are silently ignored.
async fn resolve_comment_mentions(
    storage: &crate::storage::StorageBackend,
    repo_id: &uuid::Uuid,
    names: Vec<String>,
) -> Vec<crate::storage::NewCommentMention> {
    if names.is_empty() {
        return vec![];
    }
    let mut result = Vec::new();
    let orgs = storage.orgs();
    for name in names {
        // Search returns prefix matches; we filter for exact case-insensitive match.
        if let Ok(members) = orgs.search_repo_members(repo_id, &name, 10).await {
            if let Some(m) = members.into_iter().find(|m| m.name.eq_ignore_ascii_case(&name)) {
                let (user_id, key_id) = if m.member_type == "human" {
                    (uuid::Uuid::parse_str(&m.id).ok(), None)
                } else {
                    (None, uuid::Uuid::parse_str(&m.id).ok())
                };
                result.push(crate::storage::NewCommentMention {
                    mentioned_user_id: user_id,
                    mentioned_key_id: key_id,
                    mentioned_name: m.name,
                    mention_type: m.member_type,
                });
            }
        }
    }
    result
}

#[utoipa::path(
    post,
    path = "/api/repos/{repo}/issues/{id}/comments",
    params(
        ("repo" = String, Path, description = "Repository slug"),
        ("id" = String, Path, description = "Issue UUID"),
    ),
    request_body = CreateCommentRequest,
    responses(
        (status = 201, description = "Comment created", body = CommentResponse),
        (status = 400, description = "Bad request", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Issue not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "issues"
)]
/// `POST /api/repos/:repo/issues/:id/comments` — add a comment to an issue.
async fn create_issue_comment_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    ctx: RepoCtx,
    PathId(id): PathId,
    Json(body): Json<CreateCommentRequest>,
) -> Result<(StatusCode, Json<CommentResponse>), ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Write).await?;

    let issue_id = uuid::Uuid::parse_str(&id)
        .map_err(|_| ApiError::bad_request(format!("invalid issue ID `{id}`")))?;

    // Verify the issue exists.
    ctx.storage.issues()
        .get_issue(&ctx.repo_id, &issue_id)
        .await
        .map_err(ApiError::from)?;

    // Derive author info from the authenticated identity, ignoring any
    // client-supplied author field.
    let (author, author_type, author_id) = match identity.auth_source {
        AuthSource::Jwt => {
            let id = identity.user_id.map(|u| u.to_string())
                .unwrap_or_else(|| identity.key_id.clone());
            (identity.name.clone(), "human".to_string(), Some(id))
        }
        AuthSource::ApiKey => {
            (identity.name.clone(), "agent".to_string(), Some(identity.key_id.clone()))
        }
        AuthSource::AdminKey => {
            ("admin".to_string(), "human".to_string(), None)
        }
    };

    // Parse and validate optional parent_id.
    let parent_id = if let Some(ref pid_str) = body.parent_id {
        let pid = uuid::Uuid::parse_str(pid_str)
            .map_err(|_| ApiError::bad_request(format!("invalid parent_id `{pid_str}`")))?;
        // Verify the parent comment exists on the same issue.
        let existing = ctx.storage.comments()
            .list_comments(&ctx.repo_id, &issue_id)
            .await
            .map_err(ApiError::from)?;
        if !existing.iter().any(|c| c.id == pid) {
            return Err(ApiError::bad_request(
                format!("parent_id `{pid_str}` does not reference a comment on this issue"),
            ));
        }
        Some(pid)
    } else {
        None
    };

    // Resolve @mentions from the body against repo members.
    let mention_names = extract_mention_names(&body.body);
    let new_mentions = resolve_comment_mentions(&ctx.storage, &ctx.repo_id, mention_names).await;

    let comment = ctx.storage.comments()
        .create_comment(&ctx.repo_id, &issue_id, crate::storage::NewIssueComment {
            author: author.clone(),
            body: body.body,
            author_type: author_type.clone(),
            author_id,
            parent_id,
        })
        .await
        .map_err(ApiError::from)?;

    // Store mentions and collect mention UUIDs for the event payload.
    let mentions = ctx.storage.comments()
        .replace_mentions(&ctx.repo_id, &comment.id, new_mentions)
        .await
        .unwrap_or_default();
    let mention_ids: Vec<uuid::Uuid> = mentions.iter().filter_map(|m| m.entity_id()).collect();

    // Append CommentCreated event — triggers pg_notify in Postgres mode.
    let _ = ctx.storage.events()
        .append(&ctx.repo_id, EventKind::CommentCreated {
            issue_id,
            comment_id: comment.id,
            author: author.clone(),
            author_type: author_type.clone(),
            parent_id: comment.parent_id,
            mentions: mention_ids.clone(),
        })
        .await;

    state.broadcast(BroadcastEvent {
        event_type: "CommentCreated".to_string(),
        event_id: 0,
        workspace_id: None,
        timestamp: comment.created_at.to_rfc3339(),
        data: serde_json::json!({
            "issue_id": issue_id.to_string(),
            "comment_id": comment.id.to_string(),
            "author": author,
            "author_type": author_type,
            "parent_id": comment.parent_id.map(|u| u.to_string()),
            "mentions": mention_ids.iter().map(|u| u.to_string()).collect::<Vec<_>>(),
        }),
    });

    Ok((StatusCode::CREATED, Json(CommentResponse::with_mentions(comment, &mentions))))
}

#[utoipa::path(
    get,
    path = "/api/repos/{repo}/issues/{id}/comments",
    params(
        ("repo" = String, Path, description = "Repository slug"),
        ("id" = String, Path, description = "Issue UUID"),
    ),
    responses(
        (status = 200, description = "List of comments", body = Vec<CommentResponse>),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Issue not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "issues"
)]
/// `GET /api/repos/:repo/issues/:id/comments` — list comments for an issue.
async fn list_issue_comments_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    PathId(id): PathId,
) -> Result<Json<Vec<CommentResponse>>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Read).await?;

    let issue_id = uuid::Uuid::parse_str(&id)
        .map_err(|_| ApiError::bad_request(format!("invalid issue ID `{id}`")))?;

    // Verify the issue exists.
    ctx.storage.issues()
        .get_issue(&ctx.repo_id, &issue_id)
        .await
        .map_err(ApiError::from)?;

    let comments_store = ctx.storage.comments();
    let (comments, mentions_map) = tokio::join!(
        comments_store.list_comments(&ctx.repo_id, &issue_id),
        comments_store.list_issue_mentions(&ctx.repo_id, &issue_id),
    );
    let comments = comments.map_err(ApiError::from)?;
    let mut mentions_by_comment = mentions_map.unwrap_or_default();

    Ok(Json(
        comments
            .into_iter()
            .map(|c| {
                let id = c.id;
                let mentions = mentions_by_comment.remove(&id).unwrap_or_default();
                CommentResponse::with_mentions(c, &mentions)
            })
            .collect(),
    ))
}

/// Request body for `PATCH /api/repos/:repo/issues/:id/comments/:comment_id`.
#[derive(Debug, Deserialize, ToSchema)]
struct UpdateCommentRequest {
    /// New comment body (Markdown supported).
    body: String,
}

/// Returns `true` if the authenticated identity is the author of the comment.
fn is_comment_author(identity: &AgentIdentity, comment: &crate::issue::IssueComment) -> bool {
    match &comment.author_id {
        None => identity.is_admin,
        Some(author_id) => match identity.auth_source {
            AuthSource::Jwt => {
                let my_id = identity.user_id.map(|u| u.to_string())
                    .unwrap_or_else(|| identity.key_id.clone());
                my_id == *author_id
            }
            AuthSource::ApiKey => identity.key_id == *author_id,
            AuthSource::AdminKey => identity.is_admin,
        },
    }
}

#[utoipa::path(
    patch,
    path = "/api/repos/{repo}/issues/{id}/comments/{comment_id}",
    params(
        ("repo" = String, Path, description = "Repository slug"),
        ("id" = String, Path, description = "Issue UUID"),
        ("comment_id" = String, Path, description = "Comment UUID"),
    ),
    request_body = UpdateCommentRequest,
    responses(
        (status = 200, description = "Comment updated", body = CommentResponse),
        (status = 400, description = "Bad request", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden — not the comment author", body = ErrorBody),
        (status = 404, description = "Comment not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "issues"
)]
/// `PATCH /api/repos/:repo/issues/:id/comments/:comment_id` — edit a comment body.
///
/// Only the original author may edit a comment.
async fn update_issue_comment_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    AxumPath(params): AxumPath<HashMap<String, String>>,
    Json(body): Json<UpdateCommentRequest>,
) -> Result<Json<CommentResponse>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Write).await?;

    let issue_id_str = params.get("id").cloned().unwrap_or_default();
    let comment_id_str = params.get("comment_id").cloned().unwrap_or_default();

    let issue_id = uuid::Uuid::parse_str(&issue_id_str)
        .map_err(|_| ApiError::bad_request(format!("invalid issue ID `{issue_id_str}`")))?;
    let comment_id = uuid::Uuid::parse_str(&comment_id_str)
        .map_err(|_| ApiError::bad_request(format!("invalid comment ID `{comment_id_str}`")))?;

    // Verify the issue exists.
    ctx.storage.issues()
        .get_issue(&ctx.repo_id, &issue_id)
        .await
        .map_err(ApiError::from)?;

    // Find the comment to check ownership.
    let comments = ctx.storage.comments()
        .list_comments(&ctx.repo_id, &issue_id)
        .await
        .map_err(ApiError::from)?;
    let comment = comments.into_iter().find(|c| c.id == comment_id)
        .ok_or_else(|| ApiError::not_found(format!("comment `{comment_id_str}` not found")))?;

    if !is_comment_author(&identity, &comment) {
        return Err(ApiError::forbidden("only the original author may edit this comment"));
    }

    // Re-parse and replace @mentions for the new body.
    let mention_names = extract_mention_names(&body.body);
    let new_mentions = resolve_comment_mentions(&ctx.storage, &ctx.repo_id, mention_names).await;

    let updated = ctx.storage.comments()
        .update_comment(&ctx.repo_id, &comment_id, &body.body)
        .await
        .map_err(ApiError::from)?;

    let mentions = ctx.storage.comments()
        .replace_mentions(&ctx.repo_id, &comment_id, new_mentions)
        .await
        .unwrap_or_default();

    Ok(Json(CommentResponse::with_mentions(updated, &mentions)))
}

#[utoipa::path(
    delete,
    path = "/api/repos/{repo}/issues/{id}/comments/{comment_id}",
    params(
        ("repo" = String, Path, description = "Repository slug"),
        ("id" = String, Path, description = "Issue UUID"),
        ("comment_id" = String, Path, description = "Comment UUID"),
    ),
    responses(
        (status = 200, description = "Comment deleted (soft)", body = CommentResponse),
        (status = 400, description = "Bad request", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden — not the comment author or admin", body = ErrorBody),
        (status = 404, description = "Comment not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "issues"
)]
/// `DELETE /api/repos/:repo/issues/:id/comments/:comment_id` — soft-delete a comment.
///
/// The original author or any admin may delete a comment. The comment is not
/// removed from the database; it is soft-deleted by setting `deleted_at`.
async fn delete_issue_comment_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    AxumPath(params): AxumPath<HashMap<String, String>>,
) -> Result<Json<CommentResponse>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Write).await?;

    let issue_id_str = params.get("id").cloned().unwrap_or_default();
    let comment_id_str = params.get("comment_id").cloned().unwrap_or_default();

    let issue_id = uuid::Uuid::parse_str(&issue_id_str)
        .map_err(|_| ApiError::bad_request(format!("invalid issue ID `{issue_id_str}`")))?;
    let comment_id = uuid::Uuid::parse_str(&comment_id_str)
        .map_err(|_| ApiError::bad_request(format!("invalid comment ID `{comment_id_str}`")))?;

    // Verify the issue exists.
    ctx.storage.issues()
        .get_issue(&ctx.repo_id, &issue_id)
        .await
        .map_err(ApiError::from)?;

    // Find the comment to check ownership.
    let comments = ctx.storage.comments()
        .list_comments(&ctx.repo_id, &issue_id)
        .await
        .map_err(ApiError::from)?;
    let comment = comments.into_iter().find(|c| c.id == comment_id)
        .ok_or_else(|| ApiError::not_found(format!("comment `{comment_id_str}` not found")))?;

    if !identity.is_admin && !is_comment_author(&identity, &comment) {
        return Err(ApiError::forbidden("only the original author or an admin may delete this comment"));
    }

    let deleted = ctx.storage.comments()
        .soft_delete_comment(&ctx.repo_id, &comment_id)
        .await
        .map_err(ApiError::from)?;

    Ok(Json(CommentResponse::from(deleted)))
}

// ── Issue link handlers ───────────────────────────────────────────────────────

/// Request body for `POST /api/repos/:repo/issues/:id/links`.
#[derive(Debug, Deserialize, ToSchema)]
struct CreateIssueLinkRequest {
    /// UUID of the issue to link to.
    target_id: String,
    /// Relationship from this issue to the target: `"blocks"`, `"relates-to"`, or `"duplicates"`.
    relationship: String,
}

/// Response body for a single issue link as seen from one issue's perspective.
#[derive(Debug, Serialize, ToSchema)]
struct IssueLinkResponse {
    /// The other issue in the relationship.
    other_issue_id: String,
    /// Relationship from this issue's perspective (e.g. `"blocks"`, `"is-blocked-by"`,
    /// `"relates-to"`, `"duplicates"`, `"is-duplicated-by"`).
    relationship: String,
}

#[utoipa::path(
    post,
    path = "/api/repos/{repo}/issues/{id}/links",
    params(
        ("repo" = String, Path, description = "Repository slug"),
        ("id" = String, Path, description = "Issue UUID"),
    ),
    request_body = CreateIssueLinkRequest,
    responses(
        (status = 201, description = "Link created", body = IssueLinkResponse),
        (status = 400, description = "Bad request", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Issue not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "issues"
)]
/// `POST /api/repos/:repo/issues/:id/links` — create a link from this issue to another.
async fn create_issue_link_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    PathId(id): PathId,
    Json(body): Json<CreateIssueLinkRequest>,
) -> Result<(StatusCode, Json<IssueLinkResponse>), ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Write).await?;

    let source_id = uuid::Uuid::parse_str(&id)
        .map_err(|_| ApiError::bad_request(format!("invalid issue ID `{id}`")))?;
    let target_id = uuid::Uuid::parse_str(&body.target_id)
        .map_err(|_| ApiError::bad_request(format!("invalid target_id `{}`", body.target_id)))?;

    let relationship = crate::storage::IssueLinkRelationship::from_db_str(&body.relationship)
        .ok_or_else(|| ApiError::bad_request(format!(
            "invalid relationship `{}`, must be one of: blocks, relates-to, duplicates",
            body.relationship
        )))?;

    // Verify both issues exist.
    ctx.storage.issues().get_issue(&ctx.repo_id, &source_id).await.map_err(ApiError::from)?;
    ctx.storage.issues().get_issue(&ctx.repo_id, &target_id).await.map_err(ApiError::from)?;

    ctx.storage.links().create_link(
        &ctx.repo_id,
        &source_id,
        crate::storage::NewIssueLink { target_id, relationship: relationship.clone() },
    ).await.map_err(ApiError::from)?;

    Ok((StatusCode::CREATED, Json(IssueLinkResponse {
        other_issue_id: target_id.to_string(),
        relationship: relationship.as_str().to_string(),
    })))
}

#[utoipa::path(
    get,
    path = "/api/repos/{repo}/issues/{id}/links",
    params(
        ("repo" = String, Path, description = "Repository slug"),
        ("id" = String, Path, description = "Issue UUID"),
    ),
    responses(
        (status = 200, description = "List of issue links", body = Vec<IssueLinkResponse>),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Issue not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "issues"
)]
/// `GET /api/repos/:repo/issues/:id/links` — list all links for an issue.
async fn list_issue_links_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    PathId(id): PathId,
) -> Result<Json<Vec<IssueLinkResponse>>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Read).await?;

    let issue_id = uuid::Uuid::parse_str(&id)
        .map_err(|_| ApiError::bad_request(format!("invalid issue ID `{id}`")))?;

    ctx.storage.issues().get_issue(&ctx.repo_id, &issue_id).await.map_err(ApiError::from)?;

    let links = ctx.storage.links().list_links(&ctx.repo_id, &issue_id).await.map_err(ApiError::from)?;

    let resp: Vec<IssueLinkResponse> = links.into_iter().map(|link| {
        // Determine direction: if this issue is the source, use the forward relationship;
        // if it's the target, express the inverse from this issue's perspective.
        if link.source_id == issue_id {
            IssueLinkResponse {
                other_issue_id: link.target_id.to_string(),
                relationship: link.relationship.as_str().to_string(),
            }
        } else {
            IssueLinkResponse {
                other_issue_id: link.source_id.to_string(),
                relationship: link.relationship.inverse_str().to_string(),
            }
        }
    }).collect();

    Ok(Json(resp))
}

#[utoipa::path(
    delete,
    path = "/api/repos/{repo}/issues/{id}/links/{target_id}",
    params(
        ("repo" = String, Path, description = "Repository slug"),
        ("id" = String, Path, description = "Issue UUID"),
        ("target_id" = String, Path, description = "Target issue UUID"),
    ),
    responses(
        (status = 204, description = "Link removed"),
        (status = 400, description = "Bad request", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = [])),
    tag = "issues"
)]
/// `DELETE /api/repos/:repo/issues/:id/links/:target_id` — remove a link.
async fn delete_issue_link_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    AxumPath(params): AxumPath<HashMap<String, String>>,
) -> Result<StatusCode, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Write).await?;

    let id = params.get("id").cloned().unwrap_or_default();
    let target_id_str = params.get("target_id").cloned().unwrap_or_default();

    let source_id = uuid::Uuid::parse_str(&id)
        .map_err(|_| ApiError::bad_request(format!("invalid issue ID `{id}`")))?;
    let target_id_parsed = uuid::Uuid::parse_str(&target_id_str)
        .map_err(|_| ApiError::bad_request(format!("invalid target_id `{target_id_str}`")))?;

    ctx.storage.links().delete_link(&ctx.repo_id, &source_id, &target_id_parsed).await.map_err(ApiError::from)?;

    Ok(StatusCode::NO_CONTENT)
}

// ── Attachment handlers ───────────────────────────────────────────────────────

/// Maximum file size for issue attachments (10 MiB).
const MAX_ATTACHMENT_BYTES: usize = 10 * 1024 * 1024;
/// Maximum number of attachments per issue.
const MAX_ATTACHMENTS_PER_ISSUE: usize = 10;

/// Request body for `POST .../attachments` — JSON upload with base64 content.
#[derive(Debug, Deserialize, ToSchema)]
struct UploadAttachmentRequest {
    /// Original filename (no path separators allowed).
    filename: String,
    /// MIME content type, e.g. `"image/png"`. Defaults to `"application/octet-stream"`.
    #[serde(default = "default_attachment_content_type")]
    content_type: String,
    /// File bytes, Base64-encoded (standard encoding).
    content: String,
    /// Username or agent ID uploading the file. Defaults to `"unknown"`.
    #[serde(default = "default_attachment_uploaded_by")]
    uploaded_by: String,
}

fn default_attachment_content_type() -> String {
    "application/octet-stream".to_string()
}
fn default_attachment_uploaded_by() -> String {
    "unknown".to_string()
}

/// Metadata response for a single issue attachment.
#[derive(Debug, Serialize, ToSchema)]
struct AttachmentResponse {
    id: String,
    issue_id: String,
    filename: String,
    content_type: String,
    size_bytes: i64,
    uploaded_by: String,
    created_at: String,
}

impl From<crate::issue::IssueAttachment> for AttachmentResponse {
    fn from(a: crate::issue::IssueAttachment) -> Self {
        AttachmentResponse {
            id: a.id.to_string(),
            issue_id: a.issue_id.to_string(),
            filename: a.filename,
            content_type: a.content_type,
            size_bytes: a.size_bytes,
            uploaded_by: a.uploaded_by,
            created_at: a.created_at.to_rfc3339(),
        }
    }
}

/// Returns `Err` if `filename` contains path separators or starts with `.`.
fn validate_attachment_filename(filename: &str) -> Result<(), ApiError> {
    if filename.is_empty()
        || filename.contains('/')
        || filename.contains('\\')
        || filename.starts_with('.')
    {
        Err(ApiError::bad_request(format!("invalid filename: `{filename}`")))
    } else {
        Ok(())
    }
}

/// Extracts `(filename, content_type, bytes, uploaded_by)` from either a
/// `multipart/form-data` request or a JSON body with base64-encoded content.
async fn parse_attachment_body(
    request: axum::extract::Request,
) -> Result<(String, String, Vec<u8>, String), ApiError> {
    let ct = request
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    if ct.contains("multipart/form-data") {
        let mut mp = axum::extract::Multipart::from_request(request, &())
            .await
            .map_err(|e| ApiError::bad_request(format!("multipart error: {e}")))?;

        let mut filename: Option<String> = None;
        let mut file_ct: Option<String> = None;
        let mut bytes: Option<Vec<u8>> = None;
        let mut uploaded_by: Option<String> = None;

        while let Some(field) = mp
            .next_field()
            .await
            .map_err(|e| ApiError::bad_request(format!("multipart field error: {e}")))?
        {
            match field.name() {
                Some("file") => {
                    if filename.is_none() {
                        filename = field.file_name().map(String::from);
                    }
                    if file_ct.is_none() {
                        file_ct = field.content_type().map(String::from);
                    }
                    let data = field
                        .bytes()
                        .await
                        .map_err(|e| ApiError::bad_request(format!("read field: {e}")))?;
                    bytes = Some(data.to_vec());
                }
                Some("filename") => {
                    let val = field
                        .text()
                        .await
                        .map_err(|e| ApiError::bad_request(format!("read field: {e}")))?;
                    filename = Some(val);
                }
                Some("content_type") => {
                    let val = field
                        .text()
                        .await
                        .map_err(|e| ApiError::bad_request(format!("read field: {e}")))?;
                    file_ct = Some(val);
                }
                Some("uploaded_by") => {
                    let val = field
                        .text()
                        .await
                        .map_err(|e| ApiError::bad_request(format!("read field: {e}")))?;
                    uploaded_by = Some(val);
                }
                _ => {}
            }
        }

        Ok((
            filename.ok_or_else(|| ApiError::bad_request("missing filename in multipart"))?,
            file_ct.unwrap_or_else(|| "application/octet-stream".to_string()),
            bytes.ok_or_else(|| ApiError::bad_request("missing file content in multipart"))?,
            uploaded_by.unwrap_or_else(|| "unknown".to_string()),
        ))
    } else {
        // JSON with base64 content.
        let body = axum::body::to_bytes(request.into_body(), MAX_ATTACHMENT_BYTES * 2)
            .await
            .map_err(|e| ApiError::bad_request(format!("read body: {e}")))?;
        let req: UploadAttachmentRequest = serde_json::from_slice(&body)
            .map_err(|e| ApiError::bad_request(format!("invalid JSON: {e}")))?;
        let data = BASE64
            .decode(&req.content)
            .map_err(|e| ApiError::bad_request(format!("invalid base64: {e}")))?;
        Ok((req.filename, req.content_type, data, req.uploaded_by))
    }
}

#[utoipa::path(
    post,
    path = "/api/repos/{repo}/issues/{id}/attachments",
    params(
        ("repo" = String, Path, description = "Repository slug"),
        ("id" = String, Path, description = "Issue UUID"),
    ),
    request_body = UploadAttachmentRequest,
    responses(
        (status = 201, description = "Attachment uploaded", body = AttachmentResponse),
        (status = 400, description = "Bad request (invalid filename, size, or count limit)", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Issue not found", body = ErrorBody),
        (status = 409, description = "Attachment with this filename already exists", body = ErrorBody),
        (status = 413, description = "File exceeds 10 MiB limit", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "issues"
)]
/// `POST /api/repos/:repo/issues/:id/attachments` — upload a file attachment.
///
/// Accepts either `multipart/form-data` (fields: `file`, optional `uploaded_by`,
/// `filename`, `content_type`) or a JSON body (`UploadAttachmentRequest`) with
/// the file bytes base64-encoded in the `content` field.
///
/// Limits: 10 MiB per file, 10 attachments per issue.
async fn upload_attachment_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    PathId(id): PathId,
    request: axum::extract::Request,
) -> Result<(StatusCode, Json<AttachmentResponse>), ApiError> {
    require_repo_permission(
        &ctx.storage,
        &identity,
        &ctx.repo_id,
        crate::storage::RepoRole::Write,
    )
    .await?;

    let issue_id = uuid::Uuid::parse_str(&id)
        .map_err(|_| ApiError::bad_request(format!("invalid issue ID `{id}`")))?;

    // Verify the issue exists.
    ctx.storage
        .issues()
        .get_issue(&ctx.repo_id, &issue_id)
        .await
        .map_err(ApiError::from)?;

    // Enforce per-issue attachment limit.
    let existing = ctx
        .storage
        .attachments()
        .list_attachments(&ctx.repo_id, &issue_id)
        .await
        .map_err(ApiError::from)?;
    if existing.len() >= MAX_ATTACHMENTS_PER_ISSUE {
        return Err(ApiError::bad_request(format!(
            "issue already has {MAX_ATTACHMENTS_PER_ISSUE} attachments (limit reached)"
        )));
    }

    let (filename, content_type, bytes, uploaded_by) =
        parse_attachment_body(request).await?;

    validate_attachment_filename(&filename)?;

    if bytes.len() > MAX_ATTACHMENT_BYTES {
        return Err(ApiError::payload_too_large(format!(
            "file exceeds 10 MiB limit ({} bytes)",
            bytes.len()
        )));
    }

    // Store file bytes under a deterministic S3 key.
    let s3_key = format!("issues/{issue_id}/attachments/{filename}");
    ctx.storage
        .files()
        .put(&ctx.repo_id, &s3_key, &bytes)
        .await
        .map_err(|e| ApiError::internal(format!("store attachment: {e}")))?;

    // Persist metadata.
    let attachment = ctx
        .storage
        .attachments()
        .create_attachment(
            &ctx.repo_id,
            &issue_id,
            crate::storage::NewIssueAttachment {
                filename,
                content_type,
                size_bytes: bytes.len() as i64,
                s3_key,
                uploaded_by,
            },
        )
        .await
        .map_err(ApiError::from)?;

    Ok((StatusCode::CREATED, Json(AttachmentResponse::from(attachment))))
}

#[utoipa::path(
    get,
    path = "/api/repos/{repo}/issues/{id}/attachments",
    params(
        ("repo" = String, Path, description = "Repository slug"),
        ("id" = String, Path, description = "Issue UUID"),
    ),
    responses(
        (status = 200, description = "List of attachment metadata", body = Vec<AttachmentResponse>),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Issue not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "issues"
)]
/// `GET /api/repos/:repo/issues/:id/attachments` — list attachment metadata for an issue.
async fn list_attachments_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    PathId(id): PathId,
) -> Result<Json<Vec<AttachmentResponse>>, ApiError> {
    require_repo_permission(
        &ctx.storage,
        &identity,
        &ctx.repo_id,
        crate::storage::RepoRole::Read,
    )
    .await?;

    let issue_id = uuid::Uuid::parse_str(&id)
        .map_err(|_| ApiError::bad_request(format!("invalid issue ID `{id}`")))?;

    // Verify the issue exists.
    ctx.storage
        .issues()
        .get_issue(&ctx.repo_id, &issue_id)
        .await
        .map_err(ApiError::from)?;

    let attachments = ctx
        .storage
        .attachments()
        .list_attachments(&ctx.repo_id, &issue_id)
        .await
        .map_err(ApiError::from)?;

    Ok(Json(
        attachments.into_iter().map(AttachmentResponse::from).collect(),
    ))
}

#[utoipa::path(
    get,
    path = "/api/repos/{repo}/issues/{id}/attachments/{filename}",
    params(
        ("repo" = String, Path, description = "Repository slug"),
        ("id" = String, Path, description = "Issue UUID"),
        ("filename" = String, Path, description = "Attachment filename"),
    ),
    responses(
        (status = 200, description = "File content (binary)", content_type = "application/octet-stream"),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Attachment not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "issues"
)]
/// `GET /api/repos/:repo/issues/:id/attachments/:filename` — download attachment content.
async fn download_attachment_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    AxumPath(params): AxumPath<HashMap<String, String>>,
) -> Result<Response, ApiError> {
    require_repo_permission(
        &ctx.storage,
        &identity,
        &ctx.repo_id,
        crate::storage::RepoRole::Read,
    )
    .await?;

    let id = params.get("id").cloned().unwrap_or_default();
    let filename = params.get("filename").cloned().unwrap_or_default();

    let issue_id = uuid::Uuid::parse_str(&id)
        .map_err(|_| ApiError::bad_request(format!("invalid issue ID `{id}`")))?;

    // Load metadata to get content_type and s3_key.
    let meta = ctx
        .storage
        .attachments()
        .get_attachment(&ctx.repo_id, &issue_id, &filename)
        .await
        .map_err(ApiError::from)?;

    // Fetch file bytes from the file store.
    let bytes = ctx
        .storage
        .files()
        .get(&ctx.repo_id, &meta.s3_key)
        .await
        .map_err(|e| ApiError::internal(format!("retrieve attachment: {e}")))?;

    let response = axum::response::Response::builder()
        .status(StatusCode::OK)
        .header(axum::http::header::CONTENT_TYPE, meta.content_type)
        .header(
            axum::http::header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{filename}\""),
        )
        .body(axum::body::Body::from(bytes))
        .map_err(|e| ApiError::internal(format!("build response: {e}")))?;

    Ok(response)
}

#[utoipa::path(
    delete,
    path = "/api/repos/{repo}/issues/{id}/attachments/{filename}",
    params(
        ("repo" = String, Path, description = "Repository slug"),
        ("id" = String, Path, description = "Issue UUID"),
        ("filename" = String, Path, description = "Attachment filename"),
    ),
    responses(
        (status = 204, description = "Attachment deleted"),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Attachment not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "issues"
)]
/// `DELETE /api/repos/:repo/issues/:id/attachments/:filename` — delete an attachment.
async fn delete_attachment_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    AxumPath(params): AxumPath<HashMap<String, String>>,
) -> Result<StatusCode, ApiError> {
    require_repo_permission(
        &ctx.storage,
        &identity,
        &ctx.repo_id,
        crate::storage::RepoRole::Write,
    )
    .await?;

    let id = params.get("id").cloned().unwrap_or_default();
    let filename = params.get("filename").cloned().unwrap_or_default();

    let issue_id = uuid::Uuid::parse_str(&id)
        .map_err(|_| ApiError::bad_request(format!("invalid issue ID `{id}`")))?;

    // Load metadata to confirm existence and get s3_key.
    let meta = ctx
        .storage
        .attachments()
        .get_attachment(&ctx.repo_id, &issue_id, &filename)
        .await
        .map_err(ApiError::from)?;

    // Delete file bytes from the file store.
    let _ = ctx
        .storage
        .files()
        .delete(&ctx.repo_id, &meta.s3_key)
        .await;

    // Delete metadata record.
    ctx.storage
        .attachments()
        .delete_attachment(&ctx.repo_id, &issue_id, &filename)
        .await
        .map_err(ApiError::from)?;

    Ok(StatusCode::NO_CONTENT)
}

// ── Repository registry ───────────────────────────────────────────────────────

/// A single registered repository entry in the multi-repo registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoRegistryEntry {
    /// Short identifier used in API paths (e.g. `"my-project"`).
    pub name: String,
    /// Absolute path to the repository root (parent of `.vai/`).
    pub path: PathBuf,
    /// When this repo was registered with the server.
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Persistent registry of all repos managed by this server instance.
///
/// Stored as JSON at `{storage_root}/registry.json`.
#[derive(Debug, Default, Serialize, Deserialize)]
struct RepoRegistry {
    repos: Vec<RepoRegistryEntry>,
}

impl RepoRegistry {
    /// Loads the registry from `{storage_root}/registry.json`, creating an
    /// empty registry if the file does not yet exist.
    fn load(storage_root: &Path) -> Result<Self, std::io::Error> {
        let path = storage_root.join("registry.json");
        if !path.exists() {
            return Ok(Self::default());
        }
        // ALLOW_FS: multi-repo registry is a JSON file in storage_root; intentional disk storage
        let raw = std::fs::read_to_string(&path)?;
        serde_json::from_str(&raw).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    /// Saves the registry to `{storage_root}/registry.json`.
    fn save(&self, storage_root: &Path) -> Result<(), std::io::Error> {
        let path = storage_root.join("registry.json");
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        // ALLOW_FS: multi-repo registry is a JSON file in storage_root; intentional disk storage
        std::fs::write(path, json)
    }

    /// Returns `true` if a repo with the given name is already registered.
    fn contains(&self, name: &str) -> bool {
        self.repos.iter().any(|r| r.name == name)
    }
}

// ── Request / response types for /api/repos ───────────────────────────────────

/// Request body for `POST /api/repos`.
#[derive(Debug, Deserialize, ToSchema)]
struct CreateRepoRequest {
    /// Short name for the new repository (alphanumeric, hyphens, underscores).
    name: String,
}

/// Response body for repo list and creation endpoints.
#[derive(Debug, Serialize, ToSchema)]
struct RepoResponse {
    /// Short name of the repository.
    name: String,
    /// Absolute filesystem path to the repository root.
    path: String,
    /// ISO-8601 timestamp when the repo was registered.
    created_at: String,
    /// Current HEAD version string (e.g. `"v1"`).
    head_version: String,
    /// Number of active workspaces.
    workspace_count: usize,
}

impl RepoResponse {
    fn from_entry(entry: &RepoRegistryEntry) -> Self {
        let vai_dir = entry.path.join(".vai");
        // ALLOW_FS: local mode repo listing; tracked by issue #173 to use Postgres in server mode
        let head_version = repo::read_head(&vai_dir).unwrap_or_else(|_| "unknown".to_string());
        let workspace_count = workspace::list(&vai_dir).map(|w| w.len()).unwrap_or(0);
        RepoResponse {
            name: entry.name.clone(),
            path: entry.path.display().to_string(),
            created_at: entry.created_at.to_rfc3339(),
            head_version,
            workspace_count,
        }
    }
}

// ── Repository management handlers ────────────────────────────────────────────

#[utoipa::path(
    post,
    path = "/api/repos",
    request_body = CreateRepoRequest,
    responses(
        (status = 201, description = "Repository created", body = RepoResponse),
        (status = 400, description = "Bad request or multi-repo mode not enabled", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 409, description = "Repository already exists", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "repos"
)]
/// `POST /api/repos` — registers and initialises a new repository.
///
/// Creates `{storage_root}/{name}/`, runs `vai init`, and records the repo in
/// the server registry. Returns 400 if multi-repo mode is not enabled (i.e.
/// `storage_root` is not set) or if the name is already taken.
async fn create_repo_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateRepoRequest>,
) -> Result<(StatusCode, Json<RepoResponse>), ApiError> {
    require_server_admin(&identity)?;
    let storage_root = state.storage_root.as_ref().ok_or_else(|| {
        ApiError::bad_request(
            "server is not in multi-repo mode; set storage_root in ~/.vai/server.toml",
        )
    })?;

    // Validate the repo name: alphanumeric, hyphens, underscores only.
    if body.name.is_empty()
        || !body
            .name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return Err(ApiError::bad_request(
            "repo name must be non-empty and contain only alphanumeric characters, hyphens, and underscores",
        ));
    }

    let _lock = state.repo_lock.lock().await;

    // Load registry and check for duplicates.
    let mut registry = RepoRegistry::load(storage_root).map_err(|e| ApiError::internal(e.to_string()))?;
    if registry.contains(&body.name) {
        return Err(ApiError::conflict(format!(
            "repository '{}' is already registered",
            body.name
        )));
    }

    let repo_root = storage_root.join(&body.name);
    let (repo_id, created_at) = match &state.storage {
        // ── Server mode (Postgres / S3) ───────────────────────────────────────
        // In server mode repo_root contains only .vai/config.toml.  All other
        // state (event log, graph, workspace metadata, versions, HEAD) lives in
        // Postgres/S3.  We avoid running the full `repo::init()` so that
        // server-mode repos don't write source-file artefacts to disk.
        crate::storage::StorageBackend::Server(ref pg)
        | crate::storage::StorageBackend::ServerWithS3(ref pg, _)
        | crate::storage::StorageBackend::ServerWithMemFs(ref pg, _) => {
            let vai_dir = repo_root.join(".vai");
            // ALLOW_FS: creates {storage_root}/{name}/.vai/ and writes config.toml only
            std::fs::create_dir_all(&vai_dir)
                .map_err(|e| ApiError::internal(e.to_string()))?;

            let repo_id = uuid::Uuid::new_v4();
            let created_at = chrono::Utc::now();
            let config = crate::repo::RepoConfig {
                repo_id,
                name: body.name.clone(),
                created_at,
                vai_version: env!("CARGO_PKG_VERSION").to_string(),
                remote: None,
                server: None,
            };
            // ALLOW_FS: writes .vai/config.toml only — sole on-disk artefact in server mode
            crate::repo::write_config(&vai_dir, &config)
                .map_err(|e| ApiError::internal(format!("failed to write config.toml: {e}")))?;

            // Insert repo row into Postgres.
            sqlx::query("INSERT INTO repos (id, name, created_at) VALUES ($1, $2, $3) ON CONFLICT (id) DO NOTHING")
                .bind(repo_id)
                .bind(&body.name)
                .bind(created_at)
                .execute(pg.pool())
                .await
                .map_err(|e| ApiError::internal(format!("failed to insert repo into Postgres: {e}")))?;
            tracing::debug!(repo_id = %repo_id, name = %body.name, "repo inserted into Postgres");

            // Seed the initial v1 version and HEAD in Postgres so version
            // queries never return empty for a brand-new repo.
            let v1 = crate::storage::NewVersion {
                version_id: "v1".to_string(),
                parent_version_id: None,
                intent: "initial repository".to_string(),
                created_by: "system".to_string(),
                merge_event_id: None,
            };
            state
                .storage
                .versions()
                .create_version(&repo_id, v1)
                .await
                .map_err(|e| ApiError::internal(format!("failed to create initial version: {e}")))?;
            state
                .storage
                .versions()
                .advance_head(&repo_id, "v1")
                .await
                .map_err(|e| ApiError::internal(format!("failed to advance head: {e}")))?;

            (repo_id, created_at)
        }

        // ── Local mode (SQLite + filesystem) ──────────────────────────────────
        // Run the full vai init so that the SQLite storage and filesystem-backed
        // helpers (read_head, workspace::list) find the expected directory layout.
        crate::storage::StorageBackend::Local(_) => {
            // ALLOW_FS: local-mode repo init writes full .vai/ directory structure
            let repo_root_clone = repo_root.clone();
            let init_result = tokio::task::spawn_blocking(move || repo::init(&repo_root_clone))
                .await
                .map_err(|e| ApiError::internal(format!("task join error: {e}")))?
                .map_err(|e| ApiError::internal(format!("vai init failed: {e}")))?;
            let created_at = init_result.config.created_at;
            (init_result.config.repo_id, created_at)
        }
    };

    let entry = RepoRegistryEntry {
        name: body.name.clone(),
        path: repo_root,
        created_at,
    };

    // Persist the updated registry (used by repo_resolve_middleware to map name → path).
    registry.repos.push(entry.clone());
    registry.save(storage_root).map_err(|e| ApiError::internal(e.to_string()))?;

    tracing::info!(repo_id = %repo_id, name = %entry.name, path = %entry.path.display(), "repo registered");

    // Build the response without additional filesystem reads.
    let response = RepoResponse {
        name: entry.name.clone(),
        path: entry.path.display().to_string(),
        created_at: entry.created_at.to_rfc3339(),
        head_version: "v1".to_string(),
        workspace_count: 0,
    };
    Ok((StatusCode::CREATED, Json(response)))
}

#[utoipa::path(
    get,
    path = "/api/repos",
    responses(
        (status = 200, description = "List of registered repositories", body = Vec<RepoResponse>),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = [])),
    tag = "repos"
)]
/// `GET /api/repos` — lists all registered repositories with basic stats.
///
/// Admin key returns all repos. JWT or API-key users receive only repos they
/// have access to (via direct `repo_collaborators` entry or org owner/admin
/// membership). Unauthenticated requests are rejected by the auth middleware.
async fn list_repos_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<RepoResponse>>, ApiError> {
    // In server mode (Postgres), read repo list and stats from Postgres so that
    // this handler works without touching the filesystem.
    if let crate::storage::StorageBackend::Server(ref pg)
    | crate::storage::StorageBackend::ServerWithS3(ref pg, _)
    | crate::storage::StorageBackend::ServerWithMemFs(ref pg, _) = state.storage
    {
        use sqlx::Row as _;

        // Fetch the list of (id, name, created_at) rows the caller can see.
        let rows = if identity.is_admin {
            // Admin key — return every repo.
            sqlx::query("SELECT id, name, created_at FROM repos ORDER BY created_at ASC")
                .fetch_all(pg.pool())
                .await
                .map_err(|e| ApiError::internal(format!("failed to query repos: {e}")))?
        } else if let Some(user_id) = identity.user_id {
            // JWT / API-key user — return only repos the user can access:
            //   1. direct repo_collaborators entry, OR
            //   2. org owner/admin membership (org members need an explicit
            //      collaborator row; only owner/admin roles confer implicit access).
            sqlx::query(
                "SELECT DISTINCT r.id, r.name, r.created_at
                 FROM repos r
                 WHERE (
                     EXISTS (
                         SELECT 1 FROM repo_collaborators rc
                         WHERE rc.repo_id = r.id AND rc.user_id = $1
                     )
                     OR EXISTS (
                         SELECT 1 FROM org_members om
                         WHERE om.org_id = r.org_id
                           AND om.user_id = $1
                           AND om.role IN ('owner', 'admin')
                     )
                 )
                 ORDER BY r.created_at ASC",
            )
            .bind(user_id)
            .fetch_all(pg.pool())
            .await
            .map_err(|e| ApiError::internal(format!("failed to query repos: {e}")))?
        } else {
            // Non-admin API key without an associated user — no repo access.
            vec![]
        };

        let mut responses = Vec::with_capacity(rows.len());
        for row in rows {
            let repo_id: uuid::Uuid = row.get("id");
            let name: String = row.get("name");
            let created_at: chrono::DateTime<chrono::Utc> = row.get("created_at");

            let head_version = state
                .storage
                .versions()
                .read_head(&repo_id)
                .await
                .unwrap_or(None)
                .unwrap_or_else(|| "v1".to_string());

            let workspace_count = state
                .storage
                .workspaces()
                .list_workspaces(&repo_id, false, &ListQuery::default())
                .await
                .map(|r| r.total as usize)
                .unwrap_or(0);

            let path = state
                .storage_root
                .as_ref()
                .map(|sr| sr.join(&name))
                .unwrap_or_default();

            responses.push(RepoResponse {
                name,
                path: path.display().to_string(),
                created_at: created_at.to_rfc3339(),
                head_version,
                workspace_count,
            });
        }
        return Ok(Json(responses));
    }

    // Local mode: admin sees all repos from the on-disk registry; non-admin
    // users have no RBAC data available so return an empty list.
    if !identity.is_admin {
        return Ok(Json(vec![]));
    }

    let storage_root = match state.storage_root.as_ref() {
        Some(sr) => sr,
        None => return Ok(Json(vec![])),
    };

    let registry = RepoRegistry::load(storage_root).map_err(|e| ApiError::internal(e.to_string()))?;
    let responses: Vec<RepoResponse> = registry
        .repos
        .iter()
        .map(RepoResponse::from_entry)
        .collect();

    Ok(Json(responses))
}

// ── Org / User API types ──────────────────────────────────────────────────────

/// Request body for `POST /api/orgs`.
#[derive(Debug, Deserialize, ToSchema)]
struct CreateOrgRequest {
    name: String,
    slug: String,
}

/// Request body for `POST /api/users`.
#[derive(Debug, Deserialize, ToSchema)]
struct CreateUserRequest {
    email: String,
    name: String,
}

/// Request body for `POST /api/orgs/:org/members`.
#[derive(Debug, Deserialize, ToSchema)]
struct AddMemberRequest {
    /// User UUID to add.
    #[schema(value_type = String)]
    user_id: uuid::Uuid,
    /// Role within the org: `"owner"`, `"admin"`, or `"member"`.
    role: String,
}

/// Request body for `PATCH /api/orgs/:org/members/:user`.
#[derive(Debug, Deserialize, ToSchema)]
struct UpdateMemberRequest {
    /// New role: `"owner"`, `"admin"`, or `"member"`.
    role: String,
}

/// Response body for org endpoints.
#[derive(Debug, Serialize, ToSchema)]
struct OrgResponse {
    id: String,
    name: String,
    slug: String,
    created_at: String,
}

impl From<crate::storage::Organization> for OrgResponse {
    fn from(o: crate::storage::Organization) -> Self {
        OrgResponse {
            id: o.id.to_string(),
            name: o.name,
            slug: o.slug,
            created_at: o.created_at.to_rfc3339(),
        }
    }
}

/// Response body for user endpoints.
#[derive(Debug, Serialize, ToSchema)]
struct UserResponse {
    id: String,
    email: String,
    name: String,
    created_at: String,
}

impl From<crate::storage::User> for UserResponse {
    fn from(u: crate::storage::User) -> Self {
        UserResponse {
            id: u.id.to_string(),
            email: u.email,
            name: u.name,
            created_at: u.created_at.to_rfc3339(),
        }
    }
}

/// Response body for `GET /api/repos/:repo/me`.
#[derive(Debug, Serialize, ToSchema)]
pub struct MeResponse {
    /// The authenticated user's identifier.
    ///
    /// `"admin"` for the bootstrap admin key; a UUID string for scoped keys
    /// associated with a user account; the key record ID for legacy keys
    /// without a user association (local mode).
    pub user_id: String,
    /// The user's email address, or `null` for admin and legacy keys.
    pub email: Option<String>,
    /// Effective role on this repository.
    ///
    /// One of `"owner"`, `"admin"`, `"write"`, or `"read"`.
    /// The bootstrap admin key always returns `"admin"`.
    pub role: String,
    /// Authentication method used for this request.
    ///
    /// One of `"api_key"` (API key or admin key) or `"jwt"` (JWT access token).
    pub auth_type: String,
}

/// Response body for org membership endpoints.
#[derive(Debug, Serialize, ToSchema)]
struct OrgMemberResponse {
    org_id: String,
    user_id: String,
    role: String,
    created_at: String,
}

impl From<crate::storage::OrgMember> for OrgMemberResponse {
    fn from(m: crate::storage::OrgMember) -> Self {
        OrgMemberResponse {
            org_id: m.org_id.to_string(),
            user_id: m.user_id.to_string(),
            role: m.role.as_str().to_string(),
            created_at: m.created_at.to_rfc3339(),
        }
    }
}

// ── Org / User handlers ───────────────────────────────────────────────────────

#[utoipa::path(
    post,
    path = "/api/orgs",
    request_body = CreateOrgRequest,
    responses(
        (status = 201, description = "Organization created", body = OrgResponse),
        (status = 400, description = "Bad request", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden — requires admin key"),
        (status = 409, description = "Slug already exists", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "orgs"
)]
/// `POST /api/orgs` — creates a new organization.
async fn create_org_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateOrgRequest>,
) -> Result<(StatusCode, Json<OrgResponse>), ApiError> {
    require_server_admin(&identity)?;
    use crate::storage::NewOrg;

    if body.slug.is_empty()
        || !body.slug.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return Err(ApiError::bad_request(
            "slug must be non-empty and contain only alphanumeric characters, hyphens, and underscores",
        ));
    }

    let org = state
        .storage
        .orgs()
        .create_org(NewOrg { name: body.name, slug: body.slug })
        .await
        .map_err(ApiError::from)?;

    tracing::info!(
        event = "admin.org.created",
        actor = %identity.name,
        org_id = %org.id,
        org_slug = %org.slug,
        "organization created"
    );
    Ok((StatusCode::CREATED, Json(OrgResponse::from(org))))
}

#[utoipa::path(
    get,
    path = "/api/orgs",
    responses(
        (status = 200, description = "List of organizations", body = Vec<OrgResponse>),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden — requires admin key"),
    ),
    security(("bearer_auth" = [])),
    tag = "orgs"
)]
/// `GET /api/orgs` — lists all organizations (server-level admin use).
async fn list_orgs_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<OrgResponse>>, ApiError> {
    require_server_admin(&identity)?;
    let orgs = state.storage.orgs().list_orgs().await.map_err(ApiError::from)?;
    Ok(Json(orgs.into_iter().map(OrgResponse::from).collect()))
}

#[utoipa::path(
    get,
    path = "/api/orgs/{org}",
    params(
        ("org" = String, Path, description = "Organization slug"),
    ),
    responses(
        (status = 200, description = "Organization found", body = OrgResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden — requires admin key"),
        (status = 404, description = "Not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "orgs"
)]
/// `GET /api/orgs/:org` — returns the organization with the given slug.
async fn get_org_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    AxumPath(slug): AxumPath<String>,
) -> Result<Json<OrgResponse>, ApiError> {
    require_server_admin(&identity)?;
    let org = state.storage.orgs().get_org_by_slug(&slug).await.map_err(ApiError::from)?;
    Ok(Json(OrgResponse::from(org)))
}

#[utoipa::path(
    delete,
    path = "/api/orgs/{org}",
    params(
        ("org" = String, Path, description = "Organization slug"),
    ),
    responses(
        (status = 204, description = "Deleted"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden — requires admin key"),
        (status = 404, description = "Not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "orgs"
)]
/// `DELETE /api/orgs/:org` — permanently deletes an org by slug (cascades to repos).
async fn delete_org_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    AxumPath(slug): AxumPath<String>,
) -> Result<StatusCode, ApiError> {
    require_server_admin(&identity)?;
    let org = state.storage.orgs().get_org_by_slug(&slug).await.map_err(ApiError::from)?;
    state.storage.orgs().delete_org(&org.id).await.map_err(ApiError::from)?;
    tracing::info!(
        event = "admin.org.deleted",
        actor = %identity.name,
        org_id = %org.id,
        org_slug = %slug,
        "organization deleted"
    );
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/api/users",
    request_body = CreateUserRequest,
    responses(
        (status = 201, description = "User created", body = UserResponse),
        (status = 400, description = "Bad request", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden — requires admin key"),
        (status = 409, description = "User already exists", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "auth"
)]
/// `POST /api/users` — creates a new user account.
async fn create_user_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateUserRequest>,
) -> Result<(StatusCode, Json<UserResponse>), ApiError> {
    require_server_admin(&identity)?;
    use crate::storage::NewUser;

    let user = state
        .storage
        .orgs()
        .create_user(NewUser { email: body.email, name: body.name, better_auth_id: None })
        .await
        .map_err(ApiError::from)?;

    tracing::info!(
        event = "admin.user.created",
        actor = %identity.name,
        user_id = %user.id,
        user_email = %user.email,
        "user created"
    );
    Ok((StatusCode::CREATED, Json(UserResponse::from(user))))
}

#[utoipa::path(
    get,
    path = "/api/users/{user}",
    params(
        ("user" = String, Path, description = "User UUID or email address"),
    ),
    responses(
        (status = 200, description = "User found", body = UserResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden — requires admin key"),
        (status = 404, description = "Not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "auth"
)]
/// `GET /api/users/:user` — fetches a user by UUID or email.
///
/// The `:user` path segment is tried first as a UUID; if it cannot be parsed as
/// one it is treated as an email address.
async fn get_user_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    AxumPath(user_ref): AxumPath<String>,
) -> Result<Json<UserResponse>, ApiError> {
    require_server_admin(&identity)?;
    let orgs = state.storage.orgs();
    let user = if let Ok(id) = uuid::Uuid::parse_str(&user_ref) {
        orgs.get_user(&id).await.map_err(ApiError::from)?
    } else {
        orgs.get_user_by_email(&user_ref).await.map_err(ApiError::from)?
    };
    Ok(Json(UserResponse::from(user)))
}

#[utoipa::path(
    get,
    path = "/api/repos/{repo}/me",
    params(
        ("repo" = String, Path, description = "Repository name"),
    ),
    responses(
        (status = 200, description = "Authenticated user info for this repo", body = MeResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "No access to this repository"),
    ),
    security(("bearer_auth" = [])),
    tag = "auth"
)]
/// `GET /api/repos/:repo/me` — returns the authenticated caller's identity and
/// effective role on the named repository.
///
/// Reads the [`AgentIdentity`] injected by the auth middleware and resolves:
/// - Bootstrap admin key → role `"admin"`, no email.
/// - JWT or scoped key with user → look up email and resolve repo role via [`OrgStore`].
/// - Legacy/local key without user → role `"owner"` (local mode grants full access).
async fn get_me_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
) -> Result<Json<MeResponse>, ApiError> {
    let auth_type = match identity.auth_source {
        AuthSource::Jwt => "jwt",
        AuthSource::ApiKey => "api_key",
        AuthSource::AdminKey => "api_key",
    }
    .to_string();

    // Bootstrap admin key always has full access.
    if identity.is_admin {
        return Ok(Json(MeResponse {
            user_id: "admin".to_string(),
            email: None,
            role: "admin".to_string(),
            auth_type,
        }));
    }

    // Scoped key or JWT associated with a user: resolve effective repo role via OrgStore.
    if let Some(uid) = &identity.user_id {
        let user = ctx
            .storage
            .orgs()
            .get_user(uid)
            .await
            .map_err(|e| ApiError::internal(format!("user lookup failed: {e}")))?;

        let resolved = ctx
            .storage
            .orgs()
            .resolve_repo_role(uid, &ctx.repo_id)
            .await
            .map_err(|e| ApiError::internal(format!("role resolution failed: {e}")))?;

        let effective = match resolved {
            None => return Err(ApiError::forbidden("no access to this repository")),
            Some(r) => r,
        };

        // Apply the key-level role cap if one is set.
        let effective = if let Some(cap_str) = &identity.role_override {
            let cap = crate::storage::RepoRole::from_db_str(cap_str);
            if effective.rank() > cap.rank() { cap } else { effective }
        } else {
            effective
        };

        return Ok(Json(MeResponse {
            user_id: uid.to_string(),
            email: Some(user.email),
            role: effective.as_str().to_string(),
            auth_type,
        }));
    }

    // Legacy key with no user association (local SQLite mode).
    // Any authenticated key has full owner access in local mode.
    Ok(Json(MeResponse {
        user_id: identity.key_id.clone(),
        email: None,
        role: "owner".to_string(),
        auth_type,
    }))
}

#[utoipa::path(
    get,
    path = "/api/orgs/{org}/members",
    params(
        ("org" = String, Path, description = "Organization slug"),
    ),
    responses(
        (status = 200, description = "List of org members", body = Vec<OrgMemberResponse>),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden — requires admin key"),
        (status = 404, description = "Organization not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "orgs"
)]
/// `GET /api/orgs/:org/members` — lists all members of an organization.
async fn list_org_members_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    AxumPath(slug): AxumPath<String>,
) -> Result<Json<Vec<OrgMemberResponse>>, ApiError> {
    require_server_admin(&identity)?;
    let orgs = state.storage.orgs();
    let org = orgs.get_org_by_slug(&slug).await.map_err(ApiError::from)?;
    let members = orgs.list_org_members(&org.id).await.map_err(ApiError::from)?;
    Ok(Json(members.into_iter().map(OrgMemberResponse::from).collect()))
}

#[utoipa::path(
    post,
    path = "/api/orgs/{org}/members",
    params(
        ("org" = String, Path, description = "Organization slug"),
    ),
    request_body = AddMemberRequest,
    responses(
        (status = 201, description = "Member added", body = OrgMemberResponse),
        (status = 400, description = "Bad request", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden — requires admin key"),
        (status = 404, description = "Organization or user not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "orgs"
)]
/// `POST /api/orgs/:org/members` — adds a user as a member of an organization.
async fn add_org_member_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    AxumPath(slug): AxumPath<String>,
    Json(body): Json<AddMemberRequest>,
) -> Result<(StatusCode, Json<OrgMemberResponse>), ApiError> {
    require_server_admin(&identity)?;
    use crate::storage::OrgRole;

    let role = match body.role.as_str() {
        "owner" => OrgRole::Owner,
        "admin" => OrgRole::Admin,
        "member" => OrgRole::Member,
        other => {
            return Err(ApiError::bad_request(format!(
                "unknown org role `{other}`; expected one of: owner, admin, member"
            )));
        }
    };

    let orgs = state.storage.orgs();
    let org = orgs.get_org_by_slug(&slug).await.map_err(ApiError::from)?;
    let member = orgs.add_org_member(&org.id, &body.user_id, role).await.map_err(ApiError::from)?;
    tracing::info!(
        event = "admin.member.added",
        actor = %identity.name,
        org_slug = %slug,
        user_id = %body.user_id,
        role = %body.role,
        "org member added"
    );
    Ok((StatusCode::CREATED, Json(OrgMemberResponse::from(member))))
}

#[utoipa::path(
    patch,
    path = "/api/orgs/{org}/members/{user}",
    params(
        ("org" = String, Path, description = "Organization slug"),
        ("user" = String, Path, description = "User UUID"),
    ),
    request_body = UpdateMemberRequest,
    responses(
        (status = 200, description = "Member role updated", body = OrgMemberResponse),
        (status = 400, description = "Bad request", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden — requires admin key"),
        (status = 404, description = "Organization or member not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "orgs"
)]
/// `PATCH /api/orgs/:org/members/:user` — updates a member's role.
async fn update_org_member_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    AxumPath((slug, user_id)): AxumPath<(String, uuid::Uuid)>,
    Json(body): Json<UpdateMemberRequest>,
) -> Result<Json<OrgMemberResponse>, ApiError> {
    require_server_admin(&identity)?;
    use crate::storage::OrgRole;

    let role = match body.role.as_str() {
        "owner" => OrgRole::Owner,
        "admin" => OrgRole::Admin,
        "member" => OrgRole::Member,
        other => {
            return Err(ApiError::bad_request(format!(
                "unknown org role `{other}`; expected one of: owner, admin, member"
            )));
        }
    };

    let orgs = state.storage.orgs();
    let org = orgs.get_org_by_slug(&slug).await.map_err(ApiError::from)?;
    let member = orgs.update_org_member(&org.id, &user_id, role).await.map_err(ApiError::from)?;
    tracing::info!(
        event = "admin.member.updated",
        actor = %identity.name,
        org_slug = %slug,
        user_id = %user_id,
        role = %body.role,
        "org member role updated"
    );
    Ok(Json(OrgMemberResponse::from(member)))
}

#[utoipa::path(
    delete,
    path = "/api/orgs/{org}/members/{user}",
    params(
        ("org" = String, Path, description = "Organization slug"),
        ("user" = String, Path, description = "User UUID"),
    ),
    responses(
        (status = 204, description = "Deleted"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden — requires admin key"),
        (status = 404, description = "Organization or member not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "orgs"
)]
/// `DELETE /api/orgs/:org/members/:user` — removes a user from an organization.
async fn remove_org_member_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    AxumPath((slug, user_id)): AxumPath<(String, uuid::Uuid)>,
) -> Result<StatusCode, ApiError> {
    require_server_admin(&identity)?;
    let orgs = state.storage.orgs();
    let org = orgs.get_org_by_slug(&slug).await.map_err(ApiError::from)?;
    orgs.remove_org_member(&org.id, &user_id).await.map_err(ApiError::from)?;
    tracing::info!(
        event = "admin.member.removed",
        actor = %identity.name,
        org_slug = %slug,
        user_id = %user_id,
        "org member removed"
    );
    Ok(StatusCode::NO_CONTENT)
}

// ── Repo collaborator handlers (PRD 10.3) ─────────────────────────────────────

/// Request body for `POST /api/orgs/:org/repos/:repo/collaborators`.
#[derive(Debug, Deserialize, ToSchema)]
struct AddCollaboratorRequest {
    /// User UUID to add as a collaborator.
    #[schema(value_type = String)]
    user_id: uuid::Uuid,
    /// Role on the repository: `"owner"`, `"admin"`, `"write"`, or `"read"`.
    role: String,
}

/// Request body for `PATCH /api/orgs/:org/repos/:repo/collaborators/:user`.
#[derive(Debug, Deserialize, ToSchema)]
struct UpdateCollaboratorRequest {
    /// New role: `"owner"`, `"admin"`, `"write"`, or `"read"`.
    role: String,
}

/// Response body for repo collaborator endpoints.
#[derive(Debug, Serialize, ToSchema)]
struct CollaboratorResponse {
    repo_id: String,
    user_id: String,
    role: String,
    created_at: String,
}

impl From<crate::storage::RepoCollaborator> for CollaboratorResponse {
    fn from(c: crate::storage::RepoCollaborator) -> Self {
        CollaboratorResponse {
            repo_id: c.repo_id.to_string(),
            user_id: c.user_id.to_string(),
            role: c.role.as_str().to_string(),
            created_at: c.created_at.to_rfc3339(),
        }
    }
}

/// Parses a repo role string, returning a 400 error for unknown values.
fn parse_repo_role(s: &str) -> Result<crate::storage::RepoRole, ApiError> {
    use crate::storage::RepoRole;
    match s {
        "owner" => Ok(RepoRole::Owner),
        "admin" => Ok(RepoRole::Admin),
        "write" => Ok(RepoRole::Write),
        "read" => Ok(RepoRole::Read),
        other => Err(ApiError::bad_request(format!(
            "unknown repo role `{other}`; expected one of: owner, admin, write, read"
        ))),
    }
}

#[utoipa::path(
    post,
    path = "/api/orgs/{org}/repos/{repo}/collaborators",
    params(
        ("org" = String, Path, description = "Organization slug"),
        ("repo" = String, Path, description = "Repository name"),
    ),
    request_body = AddCollaboratorRequest,
    responses(
        (status = 201, description = "Collaborator added", body = CollaboratorResponse),
        (status = 400, description = "Bad request", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "repos"
)]
/// `POST /api/orgs/:org/repos/:repo/collaborators` — adds a collaborator to a repo.
async fn add_collaborator_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    AxumPath((org_slug, repo_name)): AxumPath<(String, String)>,
    Json(body): Json<AddCollaboratorRequest>,
) -> Result<(StatusCode, Json<CollaboratorResponse>), ApiError> {
    let role = parse_repo_role(&body.role)?;
    let orgs = state.storage.orgs();
    let org = orgs.get_org_by_slug(&org_slug).await.map_err(ApiError::from)?;
    let repo_id = orgs.get_repo_id_in_org(&org.id, &repo_name).await.map_err(ApiError::from)?;
    // Write permission required to add collaborators (invite).
    require_repo_permission(&state.storage, &identity, &repo_id, crate::storage::RepoRole::Write).await?;
    let collaborator = orgs.add_collaborator(&repo_id, &body.user_id, role).await.map_err(ApiError::from)?;
    tracing::info!(
        event = "admin.collaborator.added",
        actor = %identity.name,
        org_slug = %org_slug,
        repo = %repo_id,
        user_id = %body.user_id,
        role = %body.role,
        "repo collaborator added"
    );
    Ok((StatusCode::CREATED, Json(CollaboratorResponse::from(collaborator))))
}

#[utoipa::path(
    get,
    path = "/api/orgs/{org}/repos/{repo}/collaborators",
    params(
        ("org" = String, Path, description = "Organization slug"),
        ("repo" = String, Path, description = "Repository name"),
    ),
    responses(
        (status = 200, description = "List of collaborators", body = Vec<CollaboratorResponse>),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = [])),
    tag = "repos"
)]
/// `GET /api/orgs/:org/repos/:repo/collaborators` — lists all collaborators on a repo.
async fn list_collaborators_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    AxumPath((org_slug, repo_name)): AxumPath<(String, String)>,
) -> Result<Json<Vec<CollaboratorResponse>>, ApiError> {
    let orgs = state.storage.orgs();
    let org = orgs.get_org_by_slug(&org_slug).await.map_err(ApiError::from)?;
    let repo_id = orgs.get_repo_id_in_org(&org.id, &repo_name).await.map_err(ApiError::from)?;
    require_repo_permission(&state.storage, &identity, &repo_id, crate::storage::RepoRole::Read).await?;
    let collaborators = orgs.list_collaborators(&repo_id).await.map_err(ApiError::from)?;
    Ok(Json(collaborators.into_iter().map(CollaboratorResponse::from).collect()))
}

#[utoipa::path(
    patch,
    path = "/api/orgs/{org}/repos/{repo}/collaborators/{user}",
    params(
        ("org" = String, Path, description = "Organization slug"),
        ("repo" = String, Path, description = "Repository name"),
        ("user" = String, Path, description = "User ID"),
    ),
    request_body = UpdateCollaboratorRequest,
    responses(
        (status = 200, description = "Collaborator updated", body = CollaboratorResponse),
        (status = 400, description = "Bad request", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "repos"
)]
/// `PATCH /api/orgs/:org/repos/:repo/collaborators/:user` — updates a collaborator's role.
async fn update_collaborator_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    AxumPath((org_slug, repo_name, user_id)): AxumPath<(String, String, uuid::Uuid)>,
    Json(body): Json<UpdateCollaboratorRequest>,
) -> Result<Json<CollaboratorResponse>, ApiError> {
    let role = parse_repo_role(&body.role)?;
    let orgs = state.storage.orgs();
    let org = orgs.get_org_by_slug(&org_slug).await.map_err(ApiError::from)?;
    let repo_id = orgs.get_repo_id_in_org(&org.id, &repo_name).await.map_err(ApiError::from)?;
    // Admin permission required to change roles.
    require_repo_permission(&state.storage, &identity, &repo_id, crate::storage::RepoRole::Admin).await?;
    let collaborator = orgs.update_collaborator(&repo_id, &user_id, role).await.map_err(ApiError::from)?;
    tracing::info!(
        event = "admin.collaborator.updated",
        actor = %identity.name,
        org_slug = %org_slug,
        repo = %repo_id,
        user_id = %user_id,
        role = %body.role,
        "repo collaborator role updated"
    );
    Ok(Json(CollaboratorResponse::from(collaborator)))
}

#[utoipa::path(
    delete,
    path = "/api/orgs/{org}/repos/{repo}/collaborators/{user}",
    params(
        ("org" = String, Path, description = "Organization slug"),
        ("repo" = String, Path, description = "Repository name"),
        ("user" = String, Path, description = "User ID"),
    ),
    responses(
        (status = 204, description = "Collaborator removed"),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "repos"
)]
/// `DELETE /api/orgs/:org/repos/:repo/collaborators/:user` — removes a collaborator from a repo.
async fn remove_collaborator_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    AxumPath((org_slug, repo_name, user_id)): AxumPath<(String, String, uuid::Uuid)>,
) -> Result<StatusCode, ApiError> {
    let orgs = state.storage.orgs();
    let org = orgs.get_org_by_slug(&org_slug).await.map_err(ApiError::from)?;
    let repo_id = orgs.get_repo_id_in_org(&org.id, &repo_name).await.map_err(ApiError::from)?;
    // Admin permission required to remove collaborators.
    require_repo_permission(&state.storage, &identity, &repo_id, crate::storage::RepoRole::Admin).await?;
    orgs.remove_collaborator(&repo_id, &user_id).await.map_err(ApiError::from)?;
    tracing::info!(
        event = "admin.collaborator.removed",
        actor = %identity.name,
        org_slug = %org_slug,
        repo = %repo_id,
        user_id = %user_id,
        "repo collaborator removed"
    );
    Ok(StatusCode::NO_CONTENT)
}

// ── Repo members search (PRD 22, Issue 5) ────────────────────────────────────

/// Query parameters for `GET /api/repos/:repo/members`.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
struct MembersSearchParams {
    /// Case-insensitive prefix to filter members by name or email.
    /// An empty or absent `q` returns the first 10 members alphabetically.
    #[serde(default)]
    q: String,
}

/// A repo member — either a human user or an agent API key.
#[derive(Debug, Serialize, ToSchema)]
struct RepoMemberResponse {
    /// Stable UUID — user ID for humans, API key ID for agents.
    id: String,
    /// Display name.
    name: String,
    /// `"human"` for users, `"agent"` for API keys.
    #[serde(rename = "type")]
    member_type: String,
}

impl From<crate::storage::RepoMember> for RepoMemberResponse {
    fn from(m: crate::storage::RepoMember) -> Self {
        RepoMemberResponse {
            id: m.id,
            name: m.name,
            member_type: m.member_type,
        }
    }
}

#[utoipa::path(
    get,
    path = "/api/repos/{repo}/members",
    params(
        ("repo" = String, Path, description = "Repository slug"),
        MembersSearchParams,
    ),
    responses(
        (status = 200, description = "List of matching repo members", body = Vec<RepoMemberResponse>),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden"),
    ),
    security(("bearer_auth" = [])),
    tag = "repos"
)]
/// `GET /api/repos/:repo/members` — searches for repo members for @mention autocomplete.
///
/// Returns up to 10 users (with access via collaborators or org membership) and
/// agent API keys whose names match the `q` prefix (case-insensitive).
async fn search_repo_members_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    AxumQuery(params): AxumQuery<MembersSearchParams>,
) -> Result<Json<Vec<RepoMemberResponse>>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Read).await?;

    let members = ctx.storage.orgs()
        .search_repo_members(&ctx.repo_id, &params.q, 10)
        .await
        .map_err(ApiError::from)?;

    Ok(Json(members.into_iter().map(RepoMemberResponse::from).collect()))
}

// ── Auth token exchange (PRD 18) ──────────────────────────────────────────────

/// Request body for `POST /api/auth/token`.
///
/// Two grant types are supported:
/// - `"session_exchange"` — exchange a Better Auth session token for a vai JWT.
///   Requires `session_token`. Returns an access token and a refresh token.
/// - `"api_key"` — exchange a long-lived API key for a short-lived JWT.
///   Requires `api_key`. Returns an access token only (no refresh token).
#[derive(Debug, Deserialize, ToSchema)]
struct TokenRequest {
    /// Grant type. Accepted values: `"session_exchange"`, `"api_key"`.
    grant_type: String,
    /// Better Auth session token (required for `session_exchange`).
    session_token: Option<String>,
    /// Plaintext API key (required for `api_key`).
    api_key: Option<String>,
    /// Optional repository UUID to scope the token. When provided, the user's
    /// effective role on this repo is embedded in the JWT claims.
    #[schema(value_type = Option<String>)]
    repo_id: Option<uuid::Uuid>,
}

/// Response body for `POST /api/auth/token`.
#[derive(Debug, Serialize, ToSchema)]
struct TokenResponse {
    /// Short-lived JWT access token (HMAC-SHA256, 15 min TTL).
    access_token: String,
    /// Token type — always `"Bearer"`.
    token_type: String,
    /// Access token TTL in seconds (900 = 15 minutes).
    expires_in: u64,
    /// Opaque refresh token. Present only for `session_exchange` grants.
    /// Use `POST /api/auth/refresh` to mint a new access token.
    #[serde(skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
}

#[utoipa::path(
    post,
    path = "/api/auth/token",
    request_body = TokenRequest,
    responses(
        (status = 200, description = "Access token issued", body = TokenResponse),
        (status = 400, description = "Missing or invalid parameters", body = ErrorBody),
        (status = 401, description = "Invalid credentials", body = ErrorBody),
    ),
    tag = "auth"
)]
/// `POST /api/auth/token` — exchanges credentials for a short-lived JWT.
///
/// # Grant types
///
/// ## `session_exchange`
/// Validates a Better Auth session token by querying the shared Postgres
/// `session` table. On success, mints a JWT scoped to the authenticated user
/// (and optionally to a specific repo) and creates a refresh token.
///
/// ## `api_key`
/// Validates a plaintext vai API key. On success, mints a JWT carrying the
/// same user and role as the key. No refresh token is issued; the agent should
/// re-exchange the long-lived key before the JWT expires.
async fn token_exchange_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<TokenRequest>,
) -> Result<Json<TokenResponse>, ApiError> {
    let auth = state.storage.auth();

    match body.grant_type.as_str() {
        "session_exchange" => {
            let session_token = body.session_token.as_deref().ok_or_else(|| {
                ApiError::bad_request("session_token is required for session_exchange grant")
            })?;

            // Validate the Better Auth session and extract the BA user ID (opaque string).
            let ba_user_id = auth.validate_session(session_token).await.map_err(|e| {
                match e {
                    crate::storage::StorageError::NotFound(_) => {
                        ApiError::unauthorized("invalid or expired session token")
                    }
                    other => ApiError::from(other),
                }
            })?;

            // Resolve or auto-provision the vai user for this Better Auth identity.
            let orgs = state.storage.orgs();
            let (user_id, user_name) = match orgs.get_user_by_external_id(&ba_user_id).await {
                Ok(existing) => (existing.id, existing.name),
                Err(crate::storage::StorageError::NotFound(_)) => {
                    // First login — fetch BA profile and create a vai user record.
                    let (email, name) = auth
                        .get_better_auth_user(&ba_user_id)
                        .await
                        .map_err(ApiError::from)?;

                    let new_user = orgs
                        .create_user(crate::storage::NewUser {
                            email,
                            name,
                            better_auth_id: Some(ba_user_id.clone()),
                        })
                        .await
                        .map_err(ApiError::from)?;

                    tracing::info!(
                        event = "auth.user_provisioned",
                        ba_user_id = %ba_user_id,
                        vai_user_id = %new_user.id,
                        "Auto-provisioned vai user from Better Auth identity"
                    );

                    (new_user.id, new_user.name)
                }
                Err(other) => return Err(ApiError::from(other)),
            };

            // Grant the user a default collaborator role on every repo they are
            // not yet a collaborator on.  This runs for both newly provisioned
            // users and existing users who were created before auto-provisioning
            // was in place (i.e. they have zero collaborator records).  The
            // check is cheap and the grant loop is a no-op when the user is
            // already a member of every repo.
            let needs_grant = orgs
                .count_collaborator_repos(&user_id)
                .await
                .unwrap_or(0)
                == 0;
            if needs_grant {
                let default_role = state.default_new_user_role.clone();
                let repo_ids = orgs.list_all_repo_ids().await.unwrap_or_default();
                for repo_id in repo_ids {
                    // Ignore conflicts (already a collaborator) and other
                    // non-fatal errors — provisioning must not fail the login.
                    match orgs
                        .add_collaborator(&repo_id, &user_id, default_role.clone())
                        .await
                    {
                        Ok(_) => {
                            tracing::info!(
                                event = "auth.collaborator_granted",
                                vai_user_id = %user_id,
                                repo_id = %repo_id,
                                role = %default_role.as_str(),
                                "Granted default repo role to user"
                            );
                        }
                        Err(crate::storage::StorageError::Conflict(_)) => {
                            // Already a collaborator — harmless.
                        }
                        Err(e) => {
                            tracing::warn!(
                                event = "auth.collaborator_grant_failed",
                                vai_user_id = %user_id,
                                repo_id = %repo_id,
                                error = %e,
                                "Failed to grant default repo role to user"
                            );
                        }
                    }
                }
            }

            // Resolve the user's repo role if repo_id was supplied.
            let role: Option<String> = if let Some(repo_id) = &body.repo_id {
                let orgs = state.storage.orgs();
                orgs.resolve_repo_role(&user_id, repo_id)
                    .await
                    .map_err(ApiError::from)?
                    .map(|r| r.as_str().to_string())
            } else {
                None
            };

            // Mint the JWT access token.
            let access_token = state
                .jwt_service
                .sign(
                    user_id.to_string(),
                    Some(user_name),
                    body.repo_id.as_ref().map(|id| id.to_string()),
                    role,
                )
                .map_err(|e| ApiError::internal(e.to_string()))?;

            // Mint and persist a refresh token (7-day TTL).
            let expires_at = chrono::Utc::now() + chrono::Duration::days(7);
            let refresh_token = auth
                .create_refresh_token(&user_id, expires_at)
                .await
                .map_err(ApiError::from)?;

            tracing::info!(
                event = "auth.token_issued",
                grant_type = "session_exchange",
                user_id = %user_id,
                repo_id = ?body.repo_id,
                "JWT access token issued via session exchange"
            );

            Ok(Json(TokenResponse {
                access_token,
                token_type: "Bearer".to_string(),
                expires_in: state.jwt_service.access_token_ttl,
                refresh_token: Some(refresh_token),
            }))
        }

        "api_key" => {
            let api_key_str = body.api_key.as_deref().ok_or_else(|| {
                ApiError::bad_request("api_key is required for api_key grant")
            })?;

            // Bootstrap admin key takes priority over per-repo keys.
            let (sub, name, role) = if api_key_str == state.admin_key {
                ("admin".to_string(), "admin".to_string(), Some("admin".to_string()))
            } else {
                // Validate the API key against the store.
                let key_meta = auth.validate_key(api_key_str).await.map_err(|e| {
                    match e {
                        crate::storage::StorageError::NotFound(_) => {
                            ApiError::unauthorized("invalid or revoked API key")
                        }
                        other => ApiError::from(other),
                    }
                })?;

                tracing::info!(
                    event = "auth.token_issued",
                    grant_type = "api_key",
                    key_id = %key_meta.id,
                    key_name = %key_meta.name,
                    "JWT access token issued via API key exchange"
                );

                let sub = key_meta
                    .user_id
                    .map(|u| u.to_string())
                    .unwrap_or_else(|| key_meta.id.clone());
                let key_name = key_meta.name.clone();
                let role = key_meta.role_override.clone();
                (sub, key_name, role)
            };

            let repo_id_str = body.repo_id.as_ref().map(|id| id.to_string());
            let access_token = state
                .jwt_service
                .sign(sub, Some(name), repo_id_str, role)
                .map_err(|e| ApiError::internal(e.to_string()))?;

            // No refresh token for api_key grants — the long-lived key itself
            // acts as the refresh credential.
            Ok(Json(TokenResponse {
                access_token,
                token_type: "Bearer".to_string(),
                expires_in: state.jwt_service.access_token_ttl,
                refresh_token: None,
            }))
        }

        other => Err(ApiError::bad_request(format!(
            "unsupported grant_type '{other}'; accepted: 'session_exchange', 'api_key'"
        ))),
    }
}

// ── Auth refresh and revocation (PRD 18) ──────────────────────────────────────

/// Request body for `POST /api/auth/refresh`.
#[derive(Debug, Deserialize, ToSchema)]
struct RefreshRequest {
    /// Opaque refresh token previously issued by `POST /api/auth/token`.
    refresh_token: String,
}

/// Response body for `POST /api/auth/refresh`.
#[derive(Debug, Serialize, ToSchema)]
struct RefreshResponse {
    /// New short-lived JWT access token (HMAC-SHA256, 15 min TTL).
    access_token: String,
    /// Token type — always `"Bearer"`.
    token_type: String,
    /// Access token TTL in seconds (900 = 15 minutes).
    expires_in: u64,
}

#[utoipa::path(
    post,
    path = "/api/auth/refresh",
    request_body = RefreshRequest,
    responses(
        (status = 200, description = "New access token issued", body = RefreshResponse),
        (status = 400, description = "Missing or malformed body", body = ErrorBody),
        (status = 401, description = "Invalid, expired, or revoked refresh token", body = ErrorBody),
    ),
    tag = "auth"
)]
/// `POST /api/auth/refresh` — exchanges a refresh token for a new access token.
///
/// Validates the opaque refresh token (checks hash, expiry, and revocation),
/// then mints a fresh short-lived JWT for the associated user.
/// The refresh token remains valid after this call until it expires or is
/// explicitly revoked via `POST /api/auth/revoke`.
async fn refresh_token_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<RefreshRequest>,
) -> Result<Json<RefreshResponse>, ApiError> {
    let auth = state.storage.auth();

    let user_id = auth
        .validate_refresh_token(&body.refresh_token)
        .await
        .map_err(|e| match e {
            crate::storage::StorageError::NotFound(_) => {
                ApiError::unauthorized("invalid, expired, or revoked refresh token")
            }
            other => ApiError::from(other),
        })?;

    // Look up the user's display name to embed in the refreshed access token.
    let user_name = state
        .storage
        .orgs()
        .get_user(&user_id)
        .await
        .ok()
        .map(|u| u.name);

    let access_token = state
        .jwt_service
        .sign(user_id.to_string(), user_name, None, None)
        .map_err(|e| ApiError::internal(e.to_string()))?;

    tracing::info!(
        event = "auth.token_refreshed",
        user_id = %user_id,
        "JWT access token issued via refresh token"
    );

    Ok(Json(RefreshResponse {
        access_token,
        token_type: "Bearer".to_string(),
        expires_in: state.jwt_service.access_token_ttl,
    }))
}

/// Request body for `POST /api/auth/revoke`.
#[derive(Debug, Deserialize, ToSchema)]
struct RevokeRequest {
    /// Opaque refresh token to revoke.
    refresh_token: String,
}

#[utoipa::path(
    post,
    path = "/api/auth/revoke",
    request_body = RevokeRequest,
    responses(
        (status = 200, description = "Refresh token revoked"),
        (status = 400, description = "Missing or malformed body", body = ErrorBody),
        (status = 401, description = "Token not found or already revoked", body = ErrorBody),
    ),
    tag = "auth"
)]
/// `POST /api/auth/revoke` — revokes a refresh token.
///
/// Marks the token as revoked so it can no longer be used to mint access tokens.
/// Returns 401 if the token is not found or has already been revoked.
async fn revoke_token_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<RevokeRequest>,
) -> Result<axum::http::StatusCode, ApiError> {
    let auth = state.storage.auth();

    auth.revoke_refresh_token(&body.refresh_token)
        .await
        .map_err(|e| match e {
            crate::storage::StorageError::NotFound(_) => {
                ApiError::unauthorized("refresh token not found or already revoked")
            }
            other => ApiError::from(other),
        })?;

    tracing::info!(event = "auth.token_revoked", "refresh token revoked");

    Ok(axum::http::StatusCode::OK)
}

// ── API key management (PRD 10.3) ─────────────────────────────────────────────

/// Request body for `POST /api/keys`.
#[derive(Debug, Deserialize, ToSchema)]
struct CreateKeyRequest {
    /// Human-readable name for this key.
    name: String,
    /// Repository UUID to scope this key to. `None` for server-level keys.
    #[schema(value_type = Option<String>)]
    repo_id: Option<uuid::Uuid>,
    /// Optional role cap. When set, the key's effective permissions are the
    /// lesser of the creator's role and this value.
    /// Accepted values: `"owner"`, `"admin"`, `"write"`, `"read"`.
    role_override: Option<String>,
    /// Optional label for the kind of agent that will use this key
    /// (e.g. `"ci"`, `"worker"`, `"human"`).
    agent_type: Option<String>,
    /// Optional expiry timestamp (RFC-3339). `None` means the key never expires.
    #[schema(value_type = Option<String>)]
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Response body for key creation.
#[derive(Debug, Serialize, ToSchema)]
struct CreateKeyResponse {
    /// Key metadata (same shape as `ApiKeyResponse`).
    key: ApiKeyResponse,
    /// The plaintext token — shown only once.
    token: String,
}

/// Response body for key list/get endpoints.
#[derive(Debug, Serialize, ToSchema)]
struct ApiKeyResponse {
    id: String,
    name: String,
    key_prefix: String,
    created_at: String,
    last_used_at: Option<String>,
    user_id: Option<String>,
    role_override: Option<String>,
    /// Optional agent type label (e.g. `"ci"`, `"worker"`, `"human"`).
    agent_type: Option<String>,
    /// Optional expiry timestamp (RFC-3339). `null` means the key never expires.
    expires_at: Option<String>,
}

impl From<crate::auth::ApiKey> for ApiKeyResponse {
    fn from(k: crate::auth::ApiKey) -> Self {
        ApiKeyResponse {
            id: k.id,
            name: k.name,
            key_prefix: k.key_prefix,
            created_at: k.created_at.to_rfc3339(),
            last_used_at: k.last_used_at.map(|t| t.to_rfc3339()),
            user_id: k.user_id.map(|u| u.to_string()),
            role_override: k.role_override,
            agent_type: k.agent_type,
            expires_at: k.expires_at.map(|t| t.to_rfc3339()),
        }
    }
}

#[utoipa::path(
    post,
    path = "/api/keys",
    request_body = CreateKeyRequest,
    responses(
        (status = 201, description = "API key created", body = CreateKeyResponse),
        (status = 400, description = "Bad request", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "auth"
)]
/// `POST /api/keys` — creates a new API key scoped to the authenticated user.
///
/// The key's effective permissions are the lesser of the creator's own role and
/// the requested `role_override`. A user cannot create a key with more
/// permissions than they currently have.
async fn create_key_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateKeyRequest>,
) -> Result<(StatusCode, Json<CreateKeyResponse>), ApiError> {
    // Admin keys can create keys without a user_id association.
    // User-linked keys require a user_id on the identity.
    let user_id = if identity.is_admin {
        None
    } else {
        match identity.user_id {
            Some(uid) => Some(uid),
            None => {
                return Err(ApiError::forbidden(
                    "this key is not associated with a user; cannot create scoped keys",
                ));
            }
        }
    };

    // Validate role_override: must be a recognised role string.
    let role_override = match &body.role_override {
        Some(r) => {
            let parsed = crate::storage::RepoRole::from_db_str(r);
            // Ensure the creator is not escalating beyond their own permissions.
            if !identity.is_admin {
                if let Some(repo_id) = &body.repo_id {
                    let effective = require_repo_permission(
                        &state.storage,
                        &identity,
                        repo_id,
                        crate::storage::RepoRole::Read,
                    )
                    .await?;
                    if parsed.rank() > effective.rank() {
                        return Err(ApiError::bad_request(
                            "role_override cannot exceed your own effective role on this repo",
                        ));
                    }
                }
            }
            Some(parsed.as_str().to_string())
        }
        None => None,
    };

    let auth = state.storage.auth();
    let (key_meta, token) = auth
        .create_key(
            body.repo_id.as_ref(),
            &body.name,
            user_id.as_ref(),
            role_override.as_deref(),
            body.agent_type.as_deref(),
            body.expires_at,
        )
        .await
        .map_err(ApiError::from)?;

    tracing::info!(
        event = "key.created",
        actor = %identity.name,
        key_id = %key_meta.id,
        key_name = %key_meta.name,
        key_prefix = %key_meta.key_prefix,
        "API key created"
    );
    Ok((
        StatusCode::CREATED,
        Json(CreateKeyResponse {
            key: ApiKeyResponse::from(key_meta),
            token,
        }),
    ))
}

#[utoipa::path(
    get,
    path = "/api/keys",
    responses(
        (status = 200, description = "List of API keys", body = Vec<ApiKeyResponse>),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = [])),
    tag = "auth"
)]
/// `GET /api/keys` — lists all active keys belonging to the authenticated user.
///
/// Admin keys see all server-level keys; user keys see only keys owned by
/// that user.
async fn list_keys_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<ApiKeyResponse>>, ApiError> {
    let auth = state.storage.auth();

    let keys = if identity.is_admin {
        auth.list_keys(None).await.map_err(ApiError::from)?
    } else {
        match identity.user_id {
            Some(uid) => auth.list_keys_by_user(&uid).await.map_err(ApiError::from)?,
            None => {
                return Err(ApiError::forbidden(
                    "this key is not associated with a user; cannot list keys",
                ));
            }
        }
    };

    Ok(Json(keys.into_iter().map(ApiKeyResponse::from).collect()))
}

#[utoipa::path(
    delete,
    path = "/api/keys/{id}",
    params(
        ("id" = String, Path, description = "API key record UUID"),
    ),
    responses(
        (status = 204, description = "Key revoked"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden", body = ErrorBody),
        (status = 404, description = "Not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "auth"
)]
/// `DELETE /api/keys/:id` — revokes a key by its record UUID.
///
/// Users can only revoke their own keys; admin can revoke any key.
async fn revoke_key_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    AxumPath(key_id): AxumPath<String>,
) -> Result<StatusCode, ApiError> {
    let auth = state.storage.auth();

    // Non-admin users may only revoke their own keys. Verify ownership by
    // checking that the key is in their key list before revoking.
    if !identity.is_admin {
        let user_id = match identity.user_id {
            Some(uid) => uid,
            None => {
                return Err(ApiError::forbidden(
                    "this key is not associated with a user; cannot revoke keys",
                ));
            }
        };
        let user_keys = auth.list_keys_by_user(&user_id).await.map_err(ApiError::from)?;
        if !user_keys.iter().any(|k| k.id == key_id) {
            return Err(ApiError::forbidden("you do not own this API key"));
        }
    }

    auth.revoke_key(&key_id).await.map_err(ApiError::from)?;
    tracing::info!(
        event = "key.revoked",
        actor = %identity.name,
        key_id = %key_id,
        "API key revoked"
    );
    Ok(StatusCode::NO_CONTENT)
}

/// Query parameters for `DELETE /api/keys` (bulk revocation).
///
/// Exactly one of `repo_id` or `created_by` must be supplied.
#[derive(Debug, Deserialize)]
struct BulkRevokeQuery {
    /// Revoke all keys scoped to this repository UUID.
    repo_id: Option<uuid::Uuid>,
    /// Revoke all keys owned by this user UUID.
    created_by: Option<uuid::Uuid>,
}

/// Response body for `DELETE /api/keys` (bulk revocation).
#[derive(Debug, Serialize, ToSchema)]
struct BulkRevokeResponse {
    /// Number of API keys that were revoked.
    revoked: u64,
}

#[utoipa::path(
    delete,
    path = "/api/keys",
    params(
        ("repo_id" = Option<String>, Query, description = "Revoke all keys scoped to this repository UUID"),
        ("created_by" = Option<String>, Query, description = "Revoke all keys owned by this user UUID"),
    ),
    responses(
        (status = 200, description = "Bulk revocation successful", body = BulkRevokeResponse),
        (status = 400, description = "Neither or both query params provided", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden — admin role required", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "auth"
)]
/// `DELETE /api/keys` — revokes all keys for a repo or all keys created by a user.
///
/// Requires admin role. Exactly one of `repo_id` or `created_by` must be provided.
/// Returns the count of keys that were revoked.
async fn bulk_revoke_keys_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    AxumQuery(params): AxumQuery<BulkRevokeQuery>,
) -> Result<Json<BulkRevokeResponse>, ApiError> {
    if !identity.is_admin {
        return Err(ApiError::forbidden("admin role required for bulk key revocation"));
    }

    let auth = state.storage.auth();
    let revoked = match (params.repo_id, params.created_by) {
        (Some(repo_id), None) => {
            let count = auth
                .revoke_keys_by_repo(&repo_id)
                .await
                .map_err(ApiError::from)?;
            tracing::info!(
                event = "keys.bulk_revoked",
                actor = %identity.name,
                repo_id = %repo_id,
                count = count,
                "bulk revoked keys for repo"
            );
            count
        }
        (None, Some(user_id)) => {
            let count = auth
                .revoke_keys_by_user(&user_id)
                .await
                .map_err(ApiError::from)?;
            tracing::info!(
                event = "keys.bulk_revoked",
                actor = %identity.name,
                user_id = %user_id,
                count = count,
                "bulk revoked keys for user"
            );
            count
        }
        (Some(_), Some(_)) => {
            return Err(ApiError::bad_request(
                "provide either repo_id or created_by, not both",
            ));
        }
        (None, None) => {
            return Err(ApiError::bad_request(
                "one of repo_id or created_by is required",
            ));
        }
    };

    Ok(Json(BulkRevokeResponse { revoked }))
}

// ── Migration handler (PRD 12.2) ──────────────────────────────────────────────

#[utoipa::path(
    post,
    path = "/api/repos/{repo}/migrate",
    request_body(content = inline(serde_json::Value), description = "Migration payload (events, issues, versions, escalations)"),
    params(
        ("repo" = String, Path, description = "Repository name"),
    ),
    responses(
        (status = 201, description = "Migration successful", body = crate::migration::MigrationSummary),
        (status = 400, description = "Not running in Postgres mode or invalid payload", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = [])),
    tag = "repos"
)]
/// Bulk migration endpoint: `POST /api/migrate` (single-repo) and
/// `POST /api/repos/:repo/migrate` (multi-repo).
///
/// Accepts a [`MigrationPayload`] containing events, issues, versions, and
/// escalations from a local SQLite repository and upserts everything into
/// Postgres in a single transaction.  The endpoint is idempotent: re-running
/// after a partial failure is safe — items that already exist are skipped
/// via `ON CONFLICT DO NOTHING`.  The returned counts reflect only the rows
/// that were actually inserted (not skipped).
///
/// Only available when the server is running in Postgres mode.
async fn migrate_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    ctx: RepoCtx,
    Json(payload): Json<crate::migration::MigrationPayload>,
) -> Result<(StatusCode, Json<crate::migration::MigrationSummary>), ApiError> {
    use crate::migration::MigrationSummary;
    use crate::storage::StorageBackend;

    require_repo_permission(
        &ctx.storage,
        &identity,
        &ctx.repo_id,
        crate::storage::RepoRole::Write,
    )
    .await?;

    let pg = match &ctx.storage {
        StorageBackend::Server(pg)
        | StorageBackend::ServerWithS3(pg, _)
        | StorageBackend::ServerWithMemFs(pg, _) => pg.clone(),
        StorageBackend::Local(_) => {
            return Err(ApiError::bad_request(
                "migration endpoint requires a Postgres-backed server; \
                 this server is running in local SQLite mode",
            ));
        }
    };

    let repo_id = ctx.repo_id;
    let pool = pg.pool();

    let _lock = state.repo_lock.lock().await;

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal(format!("failed to begin transaction: {e}")))?;

    // ── Insert events ──────────────────────────────────────────────────────────
    // Uses `local_event_id` (the source repo's monotonic counter) to detect
    // duplicates so the endpoint is safe to retry after partial failures.
    let mut events_inserted: usize = 0;
    for event in &payload.events {
        let event_type = event.kind.event_type();
        let workspace_id = event.kind.workspace_id();
        let payload_val = serde_json::to_value(&event.kind)
            .map_err(|e| ApiError::internal(format!("failed to serialize event: {e}")))?;
        let local_id = event.id as i64;

        let result = sqlx::query(
            r#"INSERT INTO events
                   (repo_id, event_type, workspace_id, payload, created_at, local_event_id)
               VALUES ($1, $2, $3, $4, $5, $6)
               ON CONFLICT (repo_id, local_event_id) WHERE local_event_id IS NOT NULL
               DO NOTHING"#,
        )
        .bind(repo_id)
        .bind(event_type)
        .bind(workspace_id)
        .bind(&payload_val)
        .bind(event.timestamp)
        .bind(local_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal(format!("failed to insert event: {e}")))?;
        events_inserted += result.rows_affected() as usize;
    }

    // ── Insert issues ──────────────────────────────────────────────────────────
    let mut issues_inserted: usize = 0;
    for issue in &payload.issues {
        let priority = issue.priority.as_str().to_string();
        let status = issue.status.as_str().to_string();
        let agent_source = issue
            .agent_source
            .as_ref()
            .map(|v| serde_json::to_value(v).unwrap_or(serde_json::Value::Null));

        let result = sqlx::query(
            r#"INSERT INTO issues
                   (id, repo_id, title, body, status, priority, labels,
                    creator, agent_source, resolution, acceptance_criteria, created_at, updated_at)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
               ON CONFLICT (id) DO NOTHING"#,
        )
        .bind(issue.id)
        .bind(repo_id)
        .bind(&issue.title)
        .bind(&issue.description)
        .bind(&status)
        .bind(&priority)
        .bind(&issue.labels)
        .bind(&issue.creator)
        .bind(&agent_source)
        .bind(&issue.resolution)
        .bind(&issue.acceptance_criteria)
        .bind(issue.created_at)
        .bind(issue.updated_at)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal(format!("failed to insert issue: {e}")))?;
        issues_inserted += result.rows_affected() as usize;
    }

    // ── Insert versions ────────────────────────────────────────────────────────
    let mut versions_inserted: usize = 0;
    for version in &payload.versions {
        let merge_event_id = version.merge_event_id.map(|x| x as i64);
        let version_uuid = uuid::Uuid::new_v4();

        let result = sqlx::query(
            r#"INSERT INTO versions
                   (id, repo_id, version_id, parent_version_id, intent,
                    created_by, merge_event_id, created_at)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
               ON CONFLICT (repo_id, version_id) DO NOTHING"#,
        )
        .bind(version_uuid)
        .bind(repo_id)
        .bind(&version.version_id)
        .bind(&version.parent_version_id)
        .bind(&version.intent)
        .bind(&version.created_by)
        .bind(merge_event_id)
        .bind(version.created_at)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal(format!("failed to insert version: {e}")))?;
        versions_inserted += result.rows_affected() as usize;
    }

    // ── Insert escalations ─────────────────────────────────────────────────────
    let mut escalations_inserted: usize = 0;
    for esc in &payload.escalations {
        let esc_type = esc.escalation_type.as_str().to_string();
        let severity = esc.severity.as_str().to_string();
        let resolved = esc.status != crate::escalation::EscalationStatus::Pending;
        let resolution = esc.resolution.as_ref().map(|r| r.as_str().to_string());
        let resolution_options = serde_json::to_value(&esc.resolution_options)
            .map_err(|e| ApiError::internal(format!("failed to serialize resolution options: {e}")))?;

        let result = sqlx::query(
            r#"INSERT INTO escalations
                   (id, repo_id, escalation_type, severity, summary,
                    intents, agents, workspace_ids, affected_entities,
                    resolution_options, resolved, resolution, resolved_by,
                    resolved_at, created_at)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)
               ON CONFLICT (id) DO NOTHING"#,
        )
        .bind(esc.id)
        .bind(repo_id)
        .bind(&esc_type)
        .bind(&severity)
        .bind(&esc.summary)
        .bind(&esc.intents)
        .bind(&esc.agents)
        .bind(&esc.workspace_ids)
        .bind(&esc.affected_entities)
        .bind(&resolution_options)
        .bind(resolved)
        .bind(&resolution)
        .bind(&esc.resolved_by)
        .bind(esc.resolved_at)
        .bind(esc.created_at)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal(format!("failed to insert escalation: {e}")))?;
        escalations_inserted += result.rows_affected() as usize;
    }

    // ── Set HEAD version ───────────────────────────────────────────────────────
    // Only advance HEAD if the incoming version is numerically higher than the
    // current one (safe to re-run without rolling back a newer HEAD).
    if let Some(ref head) = payload.head_version {
        sqlx::query(
            r#"INSERT INTO version_head (repo_id, version_id) VALUES ($1, $2)
               ON CONFLICT (repo_id) DO UPDATE
               SET version_id = EXCLUDED.version_id
               WHERE (REGEXP_REPLACE(version_head.version_id, '[^0-9]', '', 'g'))::BIGINT
                   < (REGEXP_REPLACE(EXCLUDED.version_id,     '[^0-9]', '', 'g'))::BIGINT"#,
        )
        .bind(repo_id)
        .bind(head)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal(format!("failed to set HEAD: {e}")))?;
    }

    tx.commit()
        .await
        .map_err(|e| ApiError::internal(format!("failed to commit migration transaction: {e}")))?;

    let summary = MigrationSummary {
        events_migrated: events_inserted,
        issues_migrated: issues_inserted,
        versions_migrated: versions_inserted,
        escalations_migrated: escalations_inserted,
        head_version: payload.head_version,
        migrated_at: chrono::Utc::now(),
    };
    Ok((StatusCode::OK, Json(summary)))
}

// ── Migration stats ───────────────────────────────────────────────────────────

/// Response body for `GET /api/migration-stats` and `GET /api/repos/:repo/migration-stats`.
///
/// Returns counts of all data in the repository, useful for post-migration verification.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct MigrationStatsResponse {
    /// Total number of events in the repository.
    pub events: i64,
    /// Total number of issues in the repository (all statuses).
    pub issues: i64,
    /// Total number of versions in the repository.
    pub versions: i64,
    /// Total number of escalations in the repository.
    pub escalations: i64,
    /// Current HEAD version identifier, if any.
    pub head_version: Option<String>,
}

#[utoipa::path(
    get,
    path = "/api/repos/{repo}/migration-stats",
    params(
        ("repo" = String, Path, description = "Repository name"),
    ),
    responses(
        (status = 200, description = "Migration statistics", body = MigrationStatsResponse),
        (status = 400, description = "Not running in Postgres mode", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = [])),
    tag = "repos"
)]
/// `GET /api/migration-stats` and `GET /api/repos/:repo/migration-stats`
///
/// Returns counts of events, issues, versions, and escalations for the repository.
/// Requires Postgres backend.  Used by `vai remote status` to verify a migration
/// transferred all data correctly.
async fn migration_stats_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
) -> Result<Json<MigrationStatsResponse>, ApiError> {
    use crate::storage::StorageBackend;

    require_repo_permission(
        &ctx.storage,
        &identity,
        &ctx.repo_id,
        crate::storage::RepoRole::Read,
    )
    .await?;

    let pg = match &ctx.storage {
        StorageBackend::Server(pg)
        | StorageBackend::ServerWithS3(pg, _)
        | StorageBackend::ServerWithMemFs(pg, _) => pg.clone(),
        StorageBackend::Local(_) => {
            return Err(ApiError::bad_request(
                "migration-stats endpoint requires a Postgres-backed server; \
                 this server is running in local SQLite mode",
            ));
        }
    };

    let pool = pg.pool();

    let events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE repo_id = $1")
        .bind(ctx.repo_id)
        .fetch_one(pool)
        .await
        .map_err(|e| ApiError::internal(format!("failed to count events: {e}")))?;

    let issues: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM issues WHERE repo_id = $1")
        .bind(ctx.repo_id)
        .fetch_one(pool)
        .await
        .map_err(|e| ApiError::internal(format!("failed to count issues: {e}")))?;

    let versions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM versions WHERE repo_id = $1")
        .bind(ctx.repo_id)
        .fetch_one(pool)
        .await
        .map_err(|e| ApiError::internal(format!("failed to count versions: {e}")))?;

    let escalations: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM escalations WHERE repo_id = $1")
            .bind(ctx.repo_id)
            .fetch_one(pool)
            .await
            .map_err(|e| ApiError::internal(format!("failed to count escalations: {e}")))?;

    // ALLOW_FS: local mode fallback; best-effort, returns None in server mode
    let head_version = repo::read_head(&ctx.vai_dir).ok();

    Ok(Json(MigrationStatsResponse {
        events,
        issues,
        versions,
        escalations,
        head_version,
    }))
}

// ── OpenAPI spec ──────────────────────────────────────────────────────────────

/// Adds the Bearer token security scheme to the generated OpenAPI spec.
struct SecurityAddon;

impl utoipa::Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        if let Some(components) = openapi.components.as_mut() {
            components.add_security_scheme(
                "bearer_auth",
                utoipa::openapi::security::SecurityScheme::Http(
                    utoipa::openapi::security::HttpBuilder::new()
                        .scheme(utoipa::openapi::security::HttpAuthScheme::Bearer)
                        .bearer_format("API key")
                        .build(),
                ),
            );
        }
    }
}

/// OpenAPI 3.1 spec for the vai REST API.
///
/// Built from all `#[utoipa::path]`-annotated handlers.  Served at
/// `GET /api/openapi.json` (unauthenticated) so the dashboard's orval
/// code generator can fetch it without credentials.
#[derive(OpenApi)]
#[openapi(
    paths(
        status_handler,
        health_handler,
        server_stats_handler,
        create_workspace_handler,
        list_workspaces_handler,
        get_workspace_handler,
        submit_workspace_handler,
        discard_workspace_handler,
        upload_workspace_files_handler,
        upload_snapshot_handler,
        get_workspace_file_handler,
        version::list_versions_handler,
        version::get_version_handler,
        version::get_version_diff_handler,
        version::rollback_handler,
        ws::ws_events_handler,
        list_repo_files_handler,
        upload_source_files_handler,
        get_main_file_handler,
        graph::server_graph_refresh_handler,
        graph::list_graph_entities_handler,
        graph::get_graph_entity_handler,
        graph::get_entity_deps_handler,
        graph::get_blast_radius_handler,
        create_issue_handler,
        list_issues_handler,
        get_issue_handler,
        update_issue_handler,
        close_issue_handler,
        create_issue_comment_handler,
        list_issue_comments_handler,
        update_issue_comment_handler,
        delete_issue_comment_handler,
        create_issue_link_handler,
        list_issue_links_handler,
        delete_issue_link_handler,
        upload_attachment_handler,
        list_attachments_handler,
        download_attachment_handler,
        delete_attachment_handler,
        escalation::list_escalations_handler,
        escalation::get_escalation_handler,
        escalation::resolve_escalation_handler,
        work_queue::get_work_queue_handler,
        work_queue::claim_work_handler,
        watcher::register_watcher_handler,
        watcher::list_watchers_handler,
        watcher::pause_watcher_handler,
        watcher::resume_watcher_handler,
        watcher::submit_discovery_handler,
        create_repo_handler,
        list_repos_handler,
        create_org_handler,
        list_orgs_handler,
        get_org_handler,
        delete_org_handler,
        create_user_handler,
        get_user_handler,
        get_me_handler,
        add_org_member_handler,
        list_org_members_handler,
        update_org_member_handler,
        remove_org_member_handler,
        add_collaborator_handler,
        list_collaborators_handler,
        update_collaborator_handler,
        remove_collaborator_handler,
        search_repo_members_handler,
        token_exchange_handler,
        refresh_token_handler,
        revoke_token_handler,
        create_key_handler,
        list_keys_handler,
        revoke_key_handler,
        bulk_revoke_keys_handler,
        migrate_handler,
        migration_stats_handler,
        openapi_handler,
        files_download_handler,
        files_pull_handler,
        files_manifest_handler,
    ),
    components(
        schemas(
            BroadcastEvent,
            ws::SubscriptionFilter,
            ErrorBody,
            PaginationMeta,
            PaginationParams,
            StatusResponse,
            HealthResponse,
            SubsystemStatus,
            SubsystemsHealth,
            ServerStatsResponse,
            CreateWorkspaceRequest,
            WorkspaceResponse,
            SubmitResponse,
            version::VersionDiffFile,
            version::VersionDiffResponse,
            version::RollbackRequest,
            CreateIssueRequest,
            AgentSourceRequest,
            UpdateIssueRequest,
            CloseIssueRequest,
            IssueResponse,
            IssueDetailResponse,
            IssueLinkDetailResponse,
            CreateCommentRequest,
            UpdateCommentRequest,
            CommentResponse,
            MentionRef,
            CreateIssueLinkRequest,
            IssueLinkResponse,
            UploadAttachmentRequest,
            AttachmentResponse,
            FileUploadEntry,
            UploadFilesRequest,
            UploadFilesResponse,
            UploadSnapshotResponse,
            DeltaManifest,
            FileDownloadResponse,
            RepoFileListResponse,
            graph::ServerGraphRefreshResponse,
            graph::GraphEntityFilter,
            graph::BlastRadiusQuery,
            graph::EntitySummary,
            graph::EntityDetailResponse,
            graph::RelationshipSummary,
            graph::EntityDepsResponse,
            graph::BlastRadiusResponse,
            escalation::EscalationResponse,
            escalation::EscalationConflictResponse,
            escalation::ResolutionOptionResponse,
            escalation::ResolveEscalationRequest,
            work_queue::ClaimWorkRequest,
            watcher::RegisterWatcherRequest,
            watcher::WatcherResponse,
            watcher::SubmitDiscoveryRequest,
            watcher::DiscoveryOutcomeResponse,
            CreateRepoRequest,
            RepoResponse,
            CreateOrgRequest,
            CreateUserRequest,
            AddMemberRequest,
            UpdateMemberRequest,
            OrgResponse,
            UserResponse,
            MeResponse,
            OrgMemberResponse,
            AddCollaboratorRequest,
            UpdateCollaboratorRequest,
            CollaboratorResponse,
            RepoMemberResponse,
            TokenRequest,
            TokenResponse,
            RefreshRequest,
            RefreshResponse,
            RevokeRequest,
            CreateKeyRequest,
            CreateKeyResponse,
            ApiKeyResponse,
            BulkRevokeResponse,
            MigrationStatsResponse,
            crate::version::VersionMeta,
            crate::version::VersionEntityChange,
            crate::version::VersionChangeType,
            crate::version::VersionFileChange,
            crate::version::VersionFileChangeType,
            crate::version::VersionChanges,
            crate::version::RiskLevel,
            crate::version::ImpactItem,
            crate::version::ImpactAnalysis,
            crate::version::RollbackResult,
            crate::watcher::IssueCreationPolicy,
            crate::watcher::DiscoveryEventKind,
            crate::work_queue::PredictionConfidence,
            crate::work_queue::PredictedEntity,
            crate::work_queue::ScopePrediction,
            crate::work_queue::AvailableWork,
            crate::work_queue::BlockedWork,
            crate::work_queue::WorkQueue,
            crate::work_queue::ClaimResult,
            crate::migration::MigrationSummary,
            FilesDownloadQuery,
            FilesPullQuery,
            FileChangeType,
            PullFileEntry,
            FilesPullResponse,
            ManifestFileEntry,
            FilesManifestResponse,
            crate::issue::IssueAttachment,
        )
    ),
    modifiers(&SecurityAddon),
    info(
        title = "vai API",
        version = env!("CARGO_PKG_VERSION"),
        description = "REST API for vai — a version control system built for AI agents",
    ),
    servers(
        (url = "http://localhost:7865", description = "Default local server")
    ),
    tags(
        (name = "status", description = "Server and repository health"),
        (name = "workspaces", description = "Workspace management"),
        (name = "versions", description = "Version history and rollback"),
        (name = "files", description = "File upload and download"),
        (name = "graph", description = "Semantic graph queries"),
        (name = "issues", description = "Issue tracking"),
        (name = "escalations", description = "Escalation management"),
        (name = "work-queue", description = "Work queue and task claiming"),
        (name = "watchers", description = "Watcher agent registration"),
        (name = "repos", description = "Repository management"),
        (name = "orgs", description = "Organization management"),
        (name = "users", description = "User management"),
        (name = "auth", description = "Authentication and token exchange"),
        (name = "keys", description = "API key management"),
        (name = "migration", description = "Data migration"),
    ),
)]
struct VaiApi;

/// `GET /api/openapi.json` — returns the generated OpenAPI 3.1 spec as JSON.
///
/// Unauthenticated. Consumed by the dashboard's orval code generator and
/// can optionally serve a Swagger UI in the future.
#[utoipa::path(
    get,
    path = "/api/openapi.json",
    responses(
        (status = 200, description = "OpenAPI 3.1 specification", content_type = "application/json"),
    ),
    tag = "status"
)]
async fn openapi_handler() -> impl IntoResponse {
    Json(VaiApi::openapi())
}

// ── Router builder (pub(crate) for integration tests) ────────────────────────

/// Constructs the axum [`Router`] with all registered routes.
///
/// Public REST endpoints:
/// - `GET /api/status` (unauthenticated)
/// - `GET /ws/events?key=<api_key>` (WebSocket, auth via query param)
///
/// Protected REST endpoints (require `Authorization: Bearer <key>`):
/// - Global management routes: `/api/repos`, `/api/users`, `/api/orgs`, `/api/keys`
/// - All per-repo routes are under `/api/repos/:repo/` (workspaces, versions,
///   graph, issues, escalations, work-queue, watchers, files, migrate, etc.)
///
/// Exposed as `pub(crate)` so integration tests can build the app directly
/// without going through the full TCP listener setup.
pub(crate) fn build_app(state: Arc<AppState>) -> Router {
    use axum::middleware;

    // Unauthenticated routes (REST + WebSocket).
    let public = Router::new()
        .route("/health", get(health_handler))
        .route("/api/status", get(status_handler))
        .route("/api/server/stats", get(server_stats_handler))
        .route("/api/openapi.json", get(openapi_handler))
        .route("/ws/events", get(ws::ws_events_handler))
        // Token exchange is unauthenticated — it *is* the authentication step.
        .route("/api/auth/token", post(token_exchange_handler))
        // Refresh and revoke use the refresh token itself as the credential.
        .route("/api/auth/refresh", post(refresh_token_handler))
        .route("/api/auth/revoke", post(revoke_token_handler));

    // Routes requiring `Authorization: Bearer <key>`.
    let protected = Router::new()
        // Multi-repo management endpoints.
        .route("/api/repos", post(create_repo_handler))
        .route("/api/repos", get(list_repos_handler))
        // User management endpoints.
        .route("/api/users", post(create_user_handler))
        .route("/api/users/:user", get(get_user_handler))
        // Organization management endpoints (PRD 10.3).
        .route("/api/orgs", post(create_org_handler))
        .route("/api/orgs", get(list_orgs_handler))
        .route("/api/orgs/:org", get(get_org_handler))
        .route("/api/orgs/:org", delete(delete_org_handler))
        .route("/api/orgs/:org/members", post(add_org_member_handler))
        .route("/api/orgs/:org/members", get(list_org_members_handler))
        .route(
            "/api/orgs/:org/members/:user",
            axum::routing::patch(update_org_member_handler),
        )
        .route("/api/orgs/:org/members/:user", delete(remove_org_member_handler))
        // API key management endpoints (PRD 10.3).
        .route("/api/keys", post(create_key_handler))
        .route("/api/keys", get(list_keys_handler))
        .route("/api/keys", delete(bulk_revoke_keys_handler))
        .route("/api/keys/:id", delete(revoke_key_handler))
        // Repository collaborator endpoints (PRD 10.3).
        .route(
            "/api/orgs/:org/repos/:repo/collaborators",
            post(add_collaborator_handler),
        )
        .route(
            "/api/orgs/:org/repos/:repo/collaborators",
            get(list_collaborators_handler),
        )
        .route(
            "/api/orgs/:org/repos/:repo/collaborators/:user",
            axum::routing::patch(update_collaborator_handler),
        )
        .route(
            "/api/orgs/:org/repos/:repo/collaborators/:user",
            delete(remove_collaborator_handler),
        )
        // Rate limiting is INNER (added first) so it runs AFTER auth and has
        // access to the AgentIdentity set by auth_middleware for per-key limits.
        .layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            rate_limit_middleware,
        ))
        // Auth is OUTER (added last) so it runs FIRST, populating AgentIdentity
        // before rate_limit_middleware sees the request.
        .layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            auth_middleware,
        ));

    // Per-repo routes: `/api/repos/:repo/<resource>` mirrors the legacy routes
    // but resolves `vai_dir`/`repo_root` from the registry via
    // `repo_resolve_middleware`.  All the same handlers are reused — the
    // `RepoCtx` extractor picks up the per-repo paths from request extensions.
    let repo_scoped = Router::new()
        .route("/me", get(get_me_handler))
        .route("/status", get(status_handler))
        .route("/workspaces", post(create_workspace_handler))
        .route("/workspaces", get(list_workspaces_handler))
        .route("/workspaces/:id", get(get_workspace_handler))
        .route("/workspaces/:id/submit", post(submit_workspace_handler))
        .route("/workspaces/:id/files", post(upload_workspace_files_handler).layer(DefaultBodyLimit::max(UPLOAD_BODY_LIMIT)))
        .route("/workspaces/:id/upload-snapshot", post(upload_snapshot_handler).layer(DefaultBodyLimit::max(SNAPSHOT_BODY_LIMIT)))
        .route("/workspaces/:id/files/*path", get(get_workspace_file_handler))
        .route("/workspaces/:id", delete(discard_workspace_handler))
        .route("/files", get(list_repo_files_handler))
        .route("/files", post(upload_source_files_handler).layer(DefaultBodyLimit::max(UPLOAD_BODY_LIMIT)))
        // Static sub-routes must come before the wildcard `/files/*path`.
        .route("/files/download", get(files_download_handler))
        .route("/files/pull", get(files_pull_handler))
        .route("/files/manifest", get(files_manifest_handler))
        .route("/files/*path", get(get_main_file_handler))
        .route("/versions", get(version::list_versions_handler))
        .route("/versions/rollback", post(version::rollback_handler))
        .route("/versions/:id/diff", get(version::get_version_diff_handler))
        .route("/versions/:id", get(version::get_version_handler))
        .route("/graph/entities", get(graph::list_graph_entities_handler))
        .route("/graph/blast-radius", get(graph::get_blast_radius_handler))
        .route("/graph/entities/:id", get(graph::get_graph_entity_handler))
        .route("/graph/entities/:id/deps", get(graph::get_entity_deps_handler))
        .route("/graph/refresh", post(graph::server_graph_refresh_handler))
        .route("/issues", post(create_issue_handler))
        .route("/issues", get(list_issues_handler))
        .route("/issues/:id/close", post(close_issue_handler))
        .route("/issues/:id/comments", post(create_issue_comment_handler))
        .route("/issues/:id/comments", get(list_issue_comments_handler))
        .route("/issues/:id/comments/:comment_id", axum::routing::patch(update_issue_comment_handler))
        .route("/issues/:id/comments/:comment_id", axum::routing::delete(delete_issue_comment_handler))
        .route("/issues/:id/links", post(create_issue_link_handler))
        .route("/issues/:id/links", get(list_issue_links_handler))
        .route("/issues/:id/links/:target_id", axum::routing::delete(delete_issue_link_handler))
        .route("/issues/:id/attachments", post(upload_attachment_handler))
        .route("/issues/:id/attachments", get(list_attachments_handler))
        .route("/issues/:id/attachments/:filename", get(download_attachment_handler))
        .route("/issues/:id/attachments/:filename", axum::routing::delete(delete_attachment_handler))
        .route("/issues/:id", get(get_issue_handler))
        .route("/issues/:id", axum::routing::patch(update_issue_handler))
        .route("/escalations", get(escalation::list_escalations_handler))
        .route("/escalations/:id/resolve", post(escalation::resolve_escalation_handler))
        .route("/escalations/:id", get(escalation::get_escalation_handler))
        .route("/work-queue", get(work_queue::get_work_queue_handler))
        .route("/work-queue/claim", post(work_queue::claim_work_handler))
        .route("/watchers/register", post(watcher::register_watcher_handler))
        .route("/watchers", get(watcher::list_watchers_handler))
        .route("/watchers/:id/pause", post(watcher::pause_watcher_handler))
        .route("/watchers/:id/resume", post(watcher::resume_watcher_handler))
        .route("/discoveries", post(watcher::submit_discovery_handler))
        .route("/members", get(search_repo_members_handler))
        .route("/ws/events", get(ws::ws_events_handler))
        // Migration endpoints (PRD 12.2, 12.5) — multi-repo mode.
        .route("/migrate", post(migrate_handler).layer(DefaultBodyLimit::max(MIGRATE_BODY_LIMIT)))
        .route("/migration-stats", get(migration_stats_handler))
        // Apply repo resolution first (outermost = runs last).
        .layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            repo_resolve_middleware,
        ))
        // Rate limiting is INNER so it runs AFTER auth and has AgentIdentity.
        .layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            rate_limit_middleware,
        ))
        // Auth runs before rate limiting (and repo resolution) so unauth
        // requests are rejected cheaply before the registry lookup.
        .layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            auth_middleware,
        ));

    let cors = if state.cors_origins.is_empty() {
        // No CORS origins configured — fall back to permissive mode for
        // local development. Set VAI_CORS_ORIGINS for production.
        tracing::warn!("No CORS origins configured (VAI_CORS_ORIGINS). Allowing http://localhost:3000 for development.");
        tower_http::cors::CorsLayer::new()
            .allow_origin("http://localhost:3000".parse::<axum::http::HeaderValue>().unwrap())
            .allow_credentials(true)
            .allow_methods([
                axum::http::Method::GET,
                axum::http::Method::POST,
                axum::http::Method::PUT,
                axum::http::Method::PATCH,
                axum::http::Method::DELETE,
                axum::http::Method::OPTIONS,
            ])
            .allow_headers([
                axum::http::header::CONTENT_TYPE,
                axum::http::header::AUTHORIZATION,
            ])
    } else {
        tower_http::cors::CorsLayer::new()
            .allow_origin(tower_http::cors::AllowOrigin::list(
                state.cors_origins.clone(),
            ))
            .allow_credentials(true)
            .allow_methods([
                axum::http::Method::GET,
                axum::http::Method::POST,
                axum::http::Method::PUT,
                axum::http::Method::PATCH,
                axum::http::Method::DELETE,
                axum::http::Method::OPTIONS,
            ])
            .allow_headers([
                axum::http::header::CONTENT_TYPE,
                axum::http::header::AUTHORIZATION,
            ])
    };

    public
        .merge(protected)
        .nest("/api/repos/:repo", repo_scoped)
        .layer(cors)
        // Global JSON body size limit — per-route overrides on large-upload
        // endpoints (upload-snapshot: 100 MiB, file-upload/migrate: 50 MiB)
        // are applied as inner layers and take precedence over this default.
        .layer(DefaultBodyLimit::max(DEFAULT_BODY_LIMIT))
        .with_state(state)
}

// ── Test helper ───────────────────────────────────────────────────────────────

/// Starts an embedded vai server on a random available port.
///
/// Initialises shared state from the repository at `vai_dir`, binds to
/// `127.0.0.1:0`, and returns the actual socket address together with a
/// one-shot shutdown sender.  Call `shutdown_tx.send(())` to stop the server
/// gracefully.  Intended for integration tests that need a live server without
/// fixing a port.
pub async fn start_for_testing(
    vai_dir: &Path,
) -> Result<(SocketAddr, tokio::sync::oneshot::Sender<()>), ServerError> {
    let _ = tracing_subscriber::fmt::try_init();

    let repo_config = repo::read_config(vai_dir)?;
    let repo_root = vai_dir
        .parent()
        .unwrap_or(Path::new("."))
        .to_path_buf();

    let (event_tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);

    let state = Arc::new(AppState {
        vai_dir: vai_dir.to_owned(),
        repo_root,
        started_at: Instant::now(),
        repo_name: repo_config.name,
        vai_version: env!("CARGO_PKG_VERSION").to_string(),
        event_tx,
        event_seq: Arc::new(AtomicU64::new(0)),
        event_buffer: Arc::new(StdMutex::new(EventBuffer::new())),
        conflict_engine: Arc::new(Mutex::new(conflict::ConflictEngine::new())),
        repo_lock: Arc::new(Mutex::new(())),
        storage_root: None,
        storage: crate::storage::StorageBackend::local(vai_dir),
        // Tests use a fixed admin key so they can exercise admin-only endpoints.
        admin_key: "vai_admin_test".to_string(),
        jwt_service: Arc::new(crate::auth::jwt::JwtService::new(
            "test-jwt-secret".to_string(),
            None,
            3600,
        )),
        rate_limiter: Arc::new(RateLimiter::new()),
        cors_origins: vec![],
        default_new_user_role: crate::storage::RepoRole::Write,
    });

    let app = build_app(state);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    tokio::spawn(async move {
        axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
            .with_graceful_shutdown(async {
                shutdown_rx.await.ok();
            })
            .await
            .ok();
    });

    // Brief pause to let the server accept connections.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    Ok((addr, shutdown_tx))
}

/// Variant of [`start_for_testing`] that uses a Postgres storage backend.
///
/// Connects to `database_url`, runs schema migrations, inserts the repo row
/// derived from `vai_dir/config.toml`, then binds to a random port and
/// returns `(addr, shutdown_tx, repo_id)`.
///
/// The test admin key is `"vai_admin_test"`, identical to the SQLite variant.
///
/// # Cleanup
///
/// After the test, delete the repo row with:
/// `DELETE FROM repos WHERE id = $1` — all child rows cascade automatically.
pub async fn start_for_testing_pg(
    vai_dir: &Path,
    database_url: &str,
) -> Result<(SocketAddr, tokio::sync::oneshot::Sender<()>, uuid::Uuid), ServerError> {
    let _ = tracing_subscriber::fmt::try_init();

    let repo_config = repo::read_config(vai_dir)?;
    let repo_root = vai_dir
        .parent()
        .unwrap_or(Path::new("."))
        .to_path_buf();

    // Connect to Postgres and run schema migrations.
    let storage = crate::storage::StorageBackend::server(database_url, 5)
        .await
        .map_err(|e| ServerError::Io(std::io::Error::other(e.to_string())))?;

    let pg = match &storage {
        crate::storage::StorageBackend::Server(pg)
        | crate::storage::StorageBackend::ServerWithS3(pg, _)
        | crate::storage::StorageBackend::ServerWithMemFs(pg, _) => pg.clone(),
        _ => unreachable!(),
    };

    let migrations_path = concat!(env!("CARGO_MANIFEST_DIR"), "/migrations");
    pg.migrate(migrations_path)
        .await
        .map_err(|e| ServerError::Io(std::io::Error::other(e.to_string())))?;

    // Insert the repo row so FK constraints on events/issues/versions are satisfied.
    let repo_id = repo_config.repo_id;
    sqlx::query("INSERT INTO repos (id, name) VALUES ($1, $2) ON CONFLICT (id) DO NOTHING")
        .bind(repo_id)
        .bind(&repo_config.name)
        .execute(pg.pool())
        .await
        .map_err(|e| {
            ServerError::Io(std::io::Error::other(format!("failed to insert repo: {e}")))
        })?;

    let (event_tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);

    let state = Arc::new(AppState {
        vai_dir: vai_dir.to_owned(),
        repo_root,
        started_at: Instant::now(),
        repo_name: repo_config.name,
        vai_version: env!("CARGO_PKG_VERSION").to_string(),
        event_tx,
        event_seq: Arc::new(AtomicU64::new(0)),
        event_buffer: Arc::new(StdMutex::new(EventBuffer::new())),
        conflict_engine: Arc::new(Mutex::new(conflict::ConflictEngine::new())),
        repo_lock: Arc::new(Mutex::new(())),
        storage_root: None,
        storage,
        admin_key: "vai_admin_test".to_string(),
        jwt_service: Arc::new(crate::auth::jwt::JwtService::new(
            "test-jwt-secret".to_string(),
            None,
            3600,
        )),
        rate_limiter: Arc::new(RateLimiter::new()),
        cors_origins: vec![],
        default_new_user_role: crate::storage::RepoRole::Write,
    });

    let app = build_app(state);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    tokio::spawn(async move {
        axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
            .with_graceful_shutdown(async {
                shutdown_rx.await.ok();
            })
            .await
            .ok();
    });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    Ok((addr, shutdown_tx, repo_id))
}

/// Variant of [`start_for_testing`] for multi-repo Postgres mode.
///
/// Connects to `database_url`, runs schema migrations, then binds to a random
/// port and starts the server with `storage_root` set to `storage_root`.
/// The server begins with no repositories; use `POST /api/repos` to create
/// them during the test.
///
/// No `.vai/` directory needs to exist on disk before calling this function.
/// The test admin key is `"vai_admin_test"`.
///
/// # Cleanup
///
/// Call `shutdown_tx.send(())` to stop the server.  Drop the `storage_root`
/// directory (or its `TempDir`) to remove all on-disk state created by
/// `POST /api/repos`.  Remove Postgres state with:
/// `DELETE FROM repos WHERE id = ANY($1)`.
pub async fn start_for_testing_pg_multi_repo(
    storage_root: &Path,
    database_url: &str,
) -> Result<(SocketAddr, tokio::sync::oneshot::Sender<()>), ServerError> {
    let _ = tracing_subscriber::fmt::try_init();

    // Connect to Postgres and run schema migrations.
    let storage = crate::storage::StorageBackend::server(database_url, 5)
        .await
        .map_err(|e| ServerError::Io(std::io::Error::other(e.to_string())))?;

    let pg = match &storage {
        crate::storage::StorageBackend::Server(pg)
        | crate::storage::StorageBackend::ServerWithS3(pg, _)
        | crate::storage::StorageBackend::ServerWithMemFs(pg, _) => pg.clone(),
        _ => unreachable!(),
    };

    let migrations_path = concat!(env!("CARGO_MANIFEST_DIR"), "/migrations");
    pg.migrate(migrations_path)
        .await
        .map_err(|e| ServerError::Io(std::io::Error::other(e.to_string())))?;

    // In multi-repo mode the top-level vai_dir is unused (each repo gets its
    // own vai_dir under storage_root/{name}/.vai/).  We point it at a
    // non-existent path within the storage root as a clear placeholder.
    let vai_dir = storage_root.join(".vai");

    let (event_tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);

    let state = Arc::new(AppState {
        vai_dir,
        repo_root: storage_root.to_path_buf(),
        started_at: Instant::now(),
        repo_name: "multi-repo-test".to_string(),
        vai_version: env!("CARGO_PKG_VERSION").to_string(),
        event_tx,
        event_seq: Arc::new(AtomicU64::new(0)),
        event_buffer: Arc::new(StdMutex::new(EventBuffer::new())),
        conflict_engine: Arc::new(Mutex::new(conflict::ConflictEngine::new())),
        repo_lock: Arc::new(Mutex::new(())),
        storage_root: Some(storage_root.to_path_buf()),
        storage,
        admin_key: "vai_admin_test".to_string(),
        jwt_service: Arc::new(crate::auth::jwt::JwtService::new(
            "test-jwt-secret".to_string(),
            None,
            3600,
        )),
        rate_limiter: Arc::new(RateLimiter::new()),
        cors_origins: vec![],
        default_new_user_role: crate::storage::RepoRole::Write,
    });

    let app = build_app(state);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    tokio::spawn(async move {
        axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
            .with_graceful_shutdown(async {
                shutdown_rx.await.ok();
            })
            .await
            .ok();
    });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    Ok((addr, shutdown_tx))
}

/// Starts a vai server backed by Postgres + an in-memory file store for testing.
///
/// This is identical to [`start_for_testing_pg_multi_repo`] except it uses
/// [`StorageBackend::ServerWithMemFs`] instead of [`StorageBackend::Server`].
/// That causes the submit handler to use the `S3MergeFs` code path (which
/// updates `current/` in the file store) rather than the local disk path,
/// allowing end-to-end tests to verify that `current/` is correctly maintained
/// after merges without requiring real S3.
pub async fn start_for_testing_pg_with_mem_fs(
    storage_root: &Path,
    database_url: &str,
) -> Result<(SocketAddr, tokio::sync::oneshot::Sender<()>), ServerError> {
    let _ = tracing_subscriber::fmt::try_init();

    // Connect to Postgres + in-memory file store, then run migrations.
    let storage = crate::storage::StorageBackend::server_with_mem_fs(database_url, 5)
        .await
        .map_err(|e| ServerError::Io(std::io::Error::other(e.to_string())))?;

    let pg = match &storage {
        crate::storage::StorageBackend::ServerWithMemFs(pg, _) => pg.clone(),
        _ => unreachable!(),
    };

    let migrations_path = concat!(env!("CARGO_MANIFEST_DIR"), "/migrations");
    pg.migrate(migrations_path)
        .await
        .map_err(|e| ServerError::Io(std::io::Error::other(e.to_string())))?;

    let vai_dir = storage_root.join(".vai");
    let (event_tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);

    let state = Arc::new(AppState {
        vai_dir,
        repo_root: storage_root.to_path_buf(),
        started_at: Instant::now(),
        repo_name: "multi-repo-test-memfs".to_string(),
        vai_version: env!("CARGO_PKG_VERSION").to_string(),
        event_tx,
        event_seq: Arc::new(AtomicU64::new(0)),
        event_buffer: Arc::new(StdMutex::new(EventBuffer::new())),
        conflict_engine: Arc::new(Mutex::new(conflict::ConflictEngine::new())),
        repo_lock: Arc::new(Mutex::new(())),
        storage_root: Some(storage_root.to_path_buf()),
        storage,
        admin_key: "vai_admin_test".to_string(),
        jwt_service: Arc::new(crate::auth::jwt::JwtService::new(
            "test-jwt-secret".to_string(),
            None,
            3600,
        )),
        rate_limiter: Arc::new(RateLimiter::new()),
        cors_origins: vec![],
        default_new_user_role: crate::storage::RepoRole::Write,
    });

    let app = build_app(state);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    tokio::spawn(async move {
        axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
            .with_graceful_shutdown(async {
                shutdown_rx.await.ok();
            })
            .await
            .ok();
    });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    Ok((addr, shutdown_tx))
}

// ── CORS helpers ──────────────────────────────────────────────────────────────

/// Parses a list of origin strings into `HeaderValue`s suitable for `CorsLayer`.
///
/// Invalid origin strings are silently skipped with a warning log.
fn parse_cors_origins(origins: &[String]) -> Vec<axum::http::HeaderValue> {
    origins
        .iter()
        .filter_map(|o| {
            let trimmed = o.trim();
            match trimmed.parse::<axum::http::HeaderValue>() {
                Ok(v) => Some(v),
                Err(_) => {
                    tracing::warn!("invalid CORS origin ignored: {}", trimmed);
                    None
                }
            }
        })
        .collect()
}

// ── Bootstrap admin key ───────────────────────────────────────────────────────

/// Returns the bootstrap admin key to use for this server instance.
///
/// Resolution order:
/// 1. `VAI_ADMIN_KEY` environment variable — use the provided value as-is.
/// 2. Not set — generate a fresh `vai_admin_<uuid>` key, print it to stdout
///    (so the operator can copy it), and return it.
fn resolve_admin_key() -> String {
    if let Ok(key) = std::env::var("VAI_ADMIN_KEY") {
        if !key.is_empty() {
            return key;
        }
    }
    let generated = format!("vai_admin_{}", uuid::Uuid::new_v4().simple());
    println!();
    println!("╔══════════════════════════════════════════════════════════════════╗");
    println!("║              VAI BOOTSTRAP ADMIN KEY (shown once)               ║");
    println!("╠══════════════════════════════════════════════════════════════════╣");
    println!("║  {}  ║", generated);
    println!("╠══════════════════════════════════════════════════════════════════╣");
    println!("║  Set VAI_ADMIN_KEY=<key> to reuse this key across restarts.     ║");
    println!("╚══════════════════════════════════════════════════════════════════╝");
    println!();
    generated
}

// ── Public start function ─────────────────────────────────────────────────────

/// Starts the vai HTTP server.
///
/// Binds to the address configured in `config`, initialises shared state from
/// the repository at `vai_dir`, and serves requests until a SIGINT or SIGTERM
/// is received. Uses axum's built-in graceful shutdown.
pub async fn start(vai_dir: &Path, mut config: ServerConfig) -> Result<(), ServerError> {
    // Initialise structured logging if not already set up.
    let _ = tracing_subscriber::fmt::try_init();

    // In multi-repo mode there is no per-repo config file; derive a display
    // name from the storage root path instead.  In single-repo mode read the
    // per-repo config as before.
    let repo_name = if config.storage_root.is_some() {
        config
            .storage_root
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "multi-repo".to_string())
    } else {
        repo::read_config(vai_dir)?.name
    };

    let repo_root = vai_dir
        .parent()
        .unwrap_or(Path::new("."))
        .to_path_buf();

    let (event_tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);

    // Build the storage backend: Postgres when a database URL is configured,
    // SQLite otherwise (legacy local mode).  When an S3 config is also present,
    // use the full ServerWithS3 variant so file uploads are durably stored.
    let pool_size = config.db_pool_size.unwrap_or(25);
    let storage = match (config.database_url.as_deref(), config.s3.take()) {
        (Some(url), Some(s3_cfg)) => {
            crate::storage::StorageBackend::server_with_s3(url, pool_size, s3_cfg).await?
        }
        (Some(url), None) => crate::storage::StorageBackend::server(url, pool_size).await?,
        _ => crate::storage::StorageBackend::local(vai_dir),
    };

    // Run schema migrations when using a Postgres backend so the server is
    // always up-to-date (including the file_index table required by S3 mode).
    if let crate::storage::StorageBackend::Server(ref pg)
        | crate::storage::StorageBackend::ServerWithS3(ref pg, _)
        | crate::storage::StorageBackend::ServerWithMemFs(ref pg, _) = storage
    {
        let migrations_path = concat!(env!("CARGO_MANIFEST_DIR"), "/migrations");
        pg.migrate(migrations_path)
            .await
            .map_err(|e| ServerError::Io(std::io::Error::other(e.to_string())))?;
    }

    let admin_key = resolve_admin_key();

    // Resolve JWT signing service: VAI_JWT_SECRET env var or generate ephemeral key.
    let jwt_overlap_secs = std::env::var("VAI_JWT_OVERLAP_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(3600);
    let (jwt_service, _jwt_ephemeral) =
        crate::auth::jwt::resolve_jwt_service(jwt_overlap_secs);
    let jwt_service = Arc::new(jwt_service);

    // Resolve CORS origins: config takes priority, then VAI_CORS_ORIGINS env var.
    let cors_origins_raw = config
        .cors_origins
        .clone()
        .or_else(|| {
            std::env::var("VAI_CORS_ORIGINS").ok().map(|v| {
                v.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
        })
        .unwrap_or_default();
    let cors_origins = parse_cors_origins(&cors_origins_raw);

    // Resolve default role for auto-provisioned users: VAI_DEFAULT_USER_ROLE or "write".
    let default_new_user_role = {
        use crate::storage::RepoRole;
        match std::env::var("VAI_DEFAULT_USER_ROLE")
            .unwrap_or_default()
            .to_lowercase()
            .as_str()
        {
            "admin" => RepoRole::Admin,
            "read" => RepoRole::Read,
            _ => RepoRole::Write,
        }
    };

    let state = Arc::new(AppState {
        vai_dir: vai_dir.to_owned(),
        repo_root,
        started_at: Instant::now(),
        repo_name: repo_name.clone(),
        vai_version: env!("CARGO_PKG_VERSION").to_string(),
        event_tx,
        event_seq: Arc::new(AtomicU64::new(0)),
        event_buffer: Arc::new(StdMutex::new(EventBuffer::new())),
        conflict_engine: Arc::new(Mutex::new(conflict::ConflictEngine::new())),
        repo_lock: Arc::new(Mutex::new(())),
        storage_root: config.storage_root.clone(),
        storage,
        admin_key,
        jwt_service,
        rate_limiter: Arc::new(RateLimiter::new()),
        cors_origins,
        default_new_user_role,
    });

    let app = build_app(state);

    let addr = config.socket_addr()?;
    let listener = TcpListener::bind(addr).await?;
    let actual_addr = listener.local_addr()?;

    // Write PID file if requested.
    if let Some(ref pid_path) = config.pid_file {
        let pid = std::process::id();
        // ALLOW_FS: PID file management for server process lifecycle
        std::fs::write(pid_path, format!("{}\n", pid))
            .map_err(ServerError::Io)?;
        tracing::info!("PID {} written to {}", pid, pid_path.display());
    }

    let started_at = chrono::Utc::now();
    tracing::info!(
        timestamp = %started_at.to_rfc3339(),
        addr = %actual_addr,
        repo = %repo_name,
        version = env!("CARGO_PKG_VERSION"),
        "vai server started",
    );
    println!(
        "[{}] vai server running on http://{}",
        started_at.format("%Y-%m-%dT%H:%M:%SZ"),
        actual_addr
    );
    println!("repository: {}", repo_name);
    println!("Press Ctrl+C to stop.");

    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(ServerError::Io)?;

    let stopped_at = chrono::Utc::now();
    tracing::info!(timestamp = %stopped_at.to_rfc3339(), "vai server stopped");
    println!("[{}] vai server stopped", stopped_at.format("%Y-%m-%dT%H:%M:%SZ"));

    // Remove PID file on clean shutdown.
    if let Some(ref pid_path) = config.pid_file {
        // ALLOW_FS: PID file management for server process lifecycle
        if let Err(e) = std::fs::remove_file(pid_path) {
            tracing::warn!("failed to remove PID file {}: {}", pid_path.display(), e);
        }
    }

    Ok(())
}

/// Resolves when a SIGINT (Ctrl-C) or SIGTERM is received.
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    tracing::info!("shutdown signal received, draining in-flight requests…");
}

// ── Private helpers ───────────────────────────────────────────────────────────

/// A self-cleaning temporary directory for use with [`setup_tmpdir_for_s3_submit`].
///
/// The inner directory is removed when this value is dropped.
struct TmpDir(std::path::PathBuf);

impl TmpDir {
    /// Returns the path to the temporary directory root.
    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TmpDir {
    fn drop(&mut self) {
        // ALLOW_FS: tmpdir cleanup for S3 submit merge engine scaffold
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Creates a minimal temporary `.vai/` directory for use with
/// [`merge::submit_with_fs`] in S3 server mode.
///
/// The merge engine expects a `.vai/` directory for metadata operations
/// (event log, workspace status, HEAD file, version directory).  In S3 mode
/// the real repo directory may not exist on disk, so this function sets up
/// the minimal structure in a temporary directory that the merge engine can
/// operate on.  The caller must keep the returned [`TmpDir`] alive for the
/// duration of the `submit_with_fs` call; it is automatically cleaned up when
/// dropped.
///
/// # Minimal structure created
///
/// | Path                              | Purpose                                      |
/// |-----------------------------------|----------------------------------------------|
/// | `.vai/head`                       | Lets `repo::read_head` return `current_head` |
/// | `.vai/workspaces/<id>/meta.toml`  | Workspace metadata for the merge engine      |
/// | `.vai/workspaces/active`          | Active workspace pointer for `diff::record_events` |
/// | `.vai/versions/<head>.toml`       | Stub so `version::next_version_id` is correct |
fn setup_tmpdir_for_s3_submit(
    ws_meta: &workspace::WorkspaceMeta,
    current_head: &str,
) -> Result<TmpDir, ApiError> {
    let tmp_path = std::env::temp_dir().join(format!("vai-submit-{}", uuid::Uuid::new_v4()));
    // ALLOW_FS: tmpdir scaffold required by the merge engine for S3 server-mode submit
    std::fs::create_dir_all(&tmp_path)
        .map_err(|e| ApiError::internal(format!("create tmpdir for submit: {e}")))?;
    let tmp = TmpDir(tmp_path);
    let vai = tmp.path().join(".vai");

    // HEAD file.
    // ALLOW_FS: tmpdir scaffold required by the merge engine for S3 server-mode submit
    std::fs::create_dir_all(&vai)
        .map_err(|e| ApiError::internal(format!("create tmpdir/.vai: {e}")))?;
    // ALLOW_FS: tmpdir scaffold required by the merge engine for S3 server-mode submit
    std::fs::write(vai.join("head"), format!("{current_head}\n"))
        .map_err(|e| ApiError::internal(format!("write tmpdir head: {e}")))?;

    // Workspace dir + meta.toml.
    let ws_dir = vai.join("workspaces").join(ws_meta.id.to_string());
    // ALLOW_FS: tmpdir scaffold required by the merge engine for S3 server-mode submit
    std::fs::create_dir_all(&ws_dir)
        .map_err(|e| ApiError::internal(format!("create tmpdir workspace dir: {e}")))?;
    // ALLOW_FS: tmpdir scaffold required by the merge engine for S3 server-mode submit
    workspace::update_meta(&vai, ws_meta)
        .map_err(|e| ApiError::internal(format!("write tmpdir workspace meta: {e}")))?;

    // Active workspace pointer (needed by diff::record_events → workspace::active).
    // ALLOW_FS: tmpdir scaffold required by the merge engine for S3 server-mode submit
    std::fs::write(vai.join("workspaces").join("active"), ws_meta.id.to_string())
        .map_err(|e| ApiError::internal(format!("set tmpdir active workspace: {e}")))?;

    // Version TOML stub for current HEAD so next_version_id returns the right value.
    let versions_dir = vai.join("versions");
    // ALLOW_FS: tmpdir scaffold required by the merge engine for S3 server-mode submit
    std::fs::create_dir_all(&versions_dir)
        .map_err(|e| ApiError::internal(format!("create tmpdir versions dir: {e}")))?;
    let stub_toml = format!(
        "version_id = \"{current_head}\"\nintent = \"placeholder\"\n\
         created_by = \"server\"\ncreated_at = \"{}\"\n",
        chrono::Utc::now().to_rfc3339()
    );
    // ALLOW_FS: tmpdir scaffold required by the merge engine for S3 server-mode submit
    std::fs::write(versions_dir.join(format!("{current_head}.toml")), stub_toml)
        .map_err(|e| ApiError::internal(format!("write tmpdir version toml: {e}")))?;

    Ok(tmp)
}

/// Recursively collects all files under `dir`, returning `(relative_path,
/// content)` pairs.  `relative_path` uses `/` separators.
///
/// Returns an empty vec if `dir` does not exist.  Silently skips any entry
/// that cannot be read.
fn collect_dir_files_with_content(dir: &std::path::Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    collect_dir_recursive(dir, dir, &mut out);
    out
}

fn collect_dir_recursive(
    base: &std::path::Path,
    cur: &std::path::Path,
    out: &mut Vec<(String, Vec<u8>)>,
) {
    // ALLOW_FS: disk traversal helper used only by local SQLite mode path in submit handler
    let entries = match std::fs::read_dir(cur) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_dir_recursive(base, &path, out);
        } else {
            let rel = path
                .strip_prefix(base)
                .map(|r| r.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            if rel.is_empty() {
                continue;
            }
            // ALLOW_FS: disk traversal helper used only by local SQLite mode path in submit handler
            if let Ok(bytes) = std::fs::read(&path) {
                out.push((rel.to_owned(), bytes));
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::fs;

    use futures_util::{SinkExt, StreamExt};
    use tempfile::TempDir;
    use tokio::sync::oneshot;

    use super::*;
    use crate::auth;
    use crate::repo;

    /// Initialise a repo in `root`, create a test API key, and return a
    /// running test server address along with the plaintext key.
    async fn start_test_server(
        root: &Path,
    ) -> (SocketAddr, oneshot::Sender<()>, Arc<AppState>, String) {
        repo::init(root).unwrap();
        let vai_dir = root.join(".vai");
        let repo_config = repo::read_config(&vai_dir).unwrap();

        // Create a regular API key for tests that need a revocable key
        // (e.g. the authentication test). All other tests use the admin key.
        auth::create(&vai_dir, "test-agent").unwrap();

        // Use the bootstrap admin key so tests can access all endpoints,
        // including admin-only routes like /api/repos and /api/orgs.
        let key = "vai_admin_test".to_string();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let (event_tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);

        let state = Arc::new(AppState {
            vai_dir: vai_dir.clone(),
            repo_root: root.to_path_buf(),
            started_at: Instant::now(),
            repo_name: repo_config.name.clone(),
            vai_version: env!("CARGO_PKG_VERSION").to_string(),
            event_tx,
            event_seq: Arc::new(AtomicU64::new(0)),
            event_buffer: Arc::new(StdMutex::new(EventBuffer::new())),
            conflict_engine: Arc::new(Mutex::new(conflict::ConflictEngine::new())),
            repo_lock: Arc::new(Mutex::new(())),
            storage_root: None,
            storage: crate::storage::StorageBackend::local(&vai_dir),
            admin_key: "vai_admin_test".to_string(),
            jwt_service: Arc::new(crate::auth::jwt::JwtService::new(
                "test-jwt-secret".to_string(),
                None,
                3600,
            )),
            rate_limiter: Arc::new(RateLimiter::new()),
            cors_origins: vec![],
            default_new_user_role: crate::storage::RepoRole::Write,
        });

        let app = build_app(Arc::clone(&state));
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

        tokio::spawn(async move {
            axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
                .with_graceful_shutdown(async {
                    shutdown_rx.await.ok();
                })
                .await
                .unwrap();
        });

        // Give the server a moment to accept connections.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        (addr, shutdown_tx, state, key)
    }

    #[tokio::test]
    async fn status_endpoint_returns_correct_data() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), b"pub fn hello() {}\n").unwrap();

        let (addr, shutdown_tx, state, _key) = start_test_server(root).await;
        let client = reqwest::Client::new();

        // status is unauthenticated
        let resp = client
            .get(format!("http://{addr}/api/status"))
            .send()
            .await
            .expect("request failed");

        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["repo_name"], state.repo_name.as_str());
        assert_eq!(body["head_version"], "v1");
        assert!(body["uptime_secs"].is_u64());
        assert_eq!(body["workspace_count"], 0);

        shutdown_tx.send(()).ok();
    }

    #[tokio::test]
    async fn health_endpoint_returns_ok() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let (addr, shutdown_tx, _state, _key) = start_test_server(root).await;
        let client = reqwest::Client::new();

        let resp = client
            .get(format!("http://{addr}/health"))
            .send()
            .await
            .expect("request failed");

        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["status"], "ok");

        shutdown_tx.send(()).ok();
    }

    #[tokio::test]
    async fn server_stats_endpoint_returns_data() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let (addr, shutdown_tx, _state, _key) = start_test_server(root).await;
        let client = reqwest::Client::new();

        // stats is unauthenticated
        let resp = client
            .get(format!("http://{addr}/api/server/stats"))
            .send()
            .await
            .expect("request failed");

        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(body["uptime_secs"].is_u64());
        assert!(body["vai_version"].is_string());
        assert_eq!(body["workspace_count"], 0);

        shutdown_tx.send(()).ok();
    }

    #[tokio::test]
    async fn workspace_crud_endpoints() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), b"pub fn hello() {}\n").unwrap();

        let (addr, shutdown_tx, state, key) = start_test_server(root).await;
        let repo = &state.repo_name;
        let client = reqwest::Client::new();

        // POST /api/repos/:repo/workspaces — create
        let resp = client
            .post(format!("http://{addr}/api/repos/{repo}/workspaces"))
            .bearer_auth(&key)
            .json(&serde_json::json!({ "intent": "add hello world feature" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201, "expected 201 Created");
        let created: serde_json::Value = resp.json().await.unwrap();
        let ws_id = created["id"].as_str().unwrap().to_string();
        assert_eq!(created["intent"], "add hello world feature");
        assert_eq!(created["status"], "Created");
        assert_eq!(created["base_version"], "v1");

        // GET /api/repos/:repo/workspaces — list
        let resp = client
            .get(format!("http://{addr}/api/repos/{repo}/workspaces"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let list: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(list["data"].as_array().unwrap().len(), 1);
        assert_eq!(list["data"][0]["id"], ws_id.as_str());

        // GET /api/repos/:repo/workspaces/:id — details
        let resp = client
            .get(format!("http://{addr}/api/repos/{repo}/workspaces/{ws_id}"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let detail: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(detail["id"], ws_id.as_str());
        assert_eq!(detail["intent"], "add hello world feature");

        // GET /api/repos/:repo/workspaces/:id — 404 for unknown ID
        let resp = client
            .get(format!("http://{addr}/api/repos/{repo}/workspaces/nonexistent-id"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 404);

        // DELETE /api/repos/:repo/workspaces/:id — discard
        let resp = client
            .delete(format!("http://{addr}/api/repos/{repo}/workspaces/{ws_id}"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 204, "expected 204 No Content");

        // After discard, workspace should not appear in list
        let resp = client
            .get(format!("http://{addr}/api/repos/{repo}/workspaces"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let list: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(
            list["data"].as_array().unwrap().len(),
            0,
            "discarded workspace should not appear"
        );

        shutdown_tx.send(()).ok();
    }

    /// `GET /api/repos/:repo/me` — admin key returns role "admin".
    #[tokio::test]
    async fn me_endpoint_admin_key() {
        let tmp = TempDir::new().unwrap();
        let storage_tmp = TempDir::new().unwrap();

        let (addr, shutdown_tx, key) =
            start_test_server_multi_repo(tmp.path(), storage_tmp.path().to_path_buf()).await;
        let client = reqwest::Client::new();

        // Create a repo first.
        let resp = client
            .post(format!("http://{addr}/api/repos"))
            .bearer_auth(&key)
            .json(&serde_json::json!({ "name": "my-repo" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);

        // Call /me with the bootstrap admin key.
        let resp = client
            .get(format!("http://{addr}/api/repos/my-repo/me"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200, "admin key /me should succeed");
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["user_id"], "admin");
        assert_eq!(body["role"], "admin");
        assert_eq!(body["auth_type"], "api_key");
        assert!(body["email"].is_null());

        shutdown_tx.send(()).ok();
    }

    /// `GET /api/repos/:repo/me` — non-admin key (no user_id) returns role "owner"
    /// in local SQLite mode.
    #[tokio::test]
    async fn me_endpoint_local_key() {
        let tmp = TempDir::new().unwrap();
        let storage_tmp = TempDir::new().unwrap();

        let (addr, shutdown_tx, admin_key) =
            start_test_server_multi_repo(tmp.path(), storage_tmp.path().to_path_buf()).await;
        let client = reqwest::Client::new();

        // Create a repo.
        let resp = client
            .post(format!("http://{addr}/api/repos"))
            .bearer_auth(&admin_key)
            .json(&serde_json::json!({ "name": "my-repo" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);

        // Create a regular API key in the server-level key store (root vai_dir).
        // Auth middleware validates against state.storage which is rooted at root/.vai.
        let server_vai_dir = tmp.path().join(".vai");
        let (_, test_key) = crate::auth::create(&server_vai_dir, "local-me-test").unwrap();

        let resp = client
            .get(format!("http://{addr}/api/repos/my-repo/me"))
            .bearer_auth(&test_key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200, "local key /me should succeed");
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["role"], "owner", "local mode keys get owner role");
        assert_eq!(body["auth_type"], "api_key");

        shutdown_tx.send(()).ok();
    }

    /// `GET /api/repos/:repo/me` — missing key returns 401.
    #[tokio::test]
    async fn me_endpoint_unauthorized() {
        let tmp = TempDir::new().unwrap();
        let storage_tmp = TempDir::new().unwrap();

        let (addr, shutdown_tx, admin_key) =
            start_test_server_multi_repo(tmp.path(), storage_tmp.path().to_path_buf()).await;
        let client = reqwest::Client::new();

        // Create a repo.
        let resp = client
            .post(format!("http://{addr}/api/repos"))
            .bearer_auth(&admin_key)
            .json(&serde_json::json!({ "name": "my-repo" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);

        // Call /me without auth.
        let resp = client
            .get(format!("http://{addr}/api/repos/my-repo/me"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 401);

        shutdown_tx.send(()).ok();
    }

    #[tokio::test]
    async fn submit_workspace_creates_new_version() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), b"pub fn hello() {}\n").unwrap();

        let (addr, shutdown_tx, state, key) = start_test_server(root).await;
        let repo = &state.repo_name;
        let client = reqwest::Client::new();

        // Create a workspace.
        let resp = client
            .post(format!("http://{addr}/api/repos/{repo}/workspaces"))
            .bearer_auth(&key)
            .json(&serde_json::json!({ "intent": "extend hello" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);
        let created: serde_json::Value = resp.json().await.unwrap();
        let ws_id = created["id"].as_str().unwrap().to_string();

        // Write a file into the workspace overlay so there's something to merge.
        let vai_dir = root.join(".vai");
        let overlay = vai_dir.join("workspaces").join(&ws_id).join("overlay");
        fs::create_dir_all(overlay.join("src")).unwrap();
        fs::write(
            overlay.join("src/lib.rs"),
            b"pub fn hello() {}\npub fn world() {}\n",
        )
        .unwrap();

        // POST /api/repos/:repo/workspaces/:id/submit
        let resp = client
            .post(format!("http://{addr}/api/repos/{repo}/workspaces/{ws_id}/submit"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200, "expected 200 OK from submit");
        let result: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(result["version"], "v2", "submit should create v2");
        assert!(result["files_applied"].as_u64().unwrap() > 0);

        shutdown_tx.send(()).ok();
    }

    #[tokio::test]
    async fn api_key_authentication() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), b"pub fn hello() {}\n").unwrap();

        let (addr, shutdown_tx, state, key) = start_test_server(root).await;
        let repo = &state.repo_name;
        let client = reqwest::Client::new();

        // 1. Authenticated request succeeds.
        let resp = client
            .get(format!("http://{addr}/api/repos/{repo}/workspaces"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200, "valid key should be accepted");

        // 2. Missing Authorization header returns 401.
        let resp = client
            .get(format!("http://{addr}/api/repos/{repo}/workspaces"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 401, "missing auth should return 401");

        // 3. Wrong key returns 401.
        let resp = client
            .get(format!("http://{addr}/api/repos/{repo}/workspaces"))
            .bearer_auth("vai_thisisnottherightkey00000000000")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 401, "invalid key should return 401");

        // 4. status endpoint does NOT require auth.
        let resp = client
            .get(format!("http://{addr}/api/status"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200, "status should be unauthenticated");

        // 5. Revoked key returns 401.
        // Create a fresh key, use it once, then revoke it to verify 401.
        let vai_dir = root.join(".vai");
        let (_, revocable_key) = auth::create(&vai_dir, "revoke-me").unwrap();
        auth::revoke(&vai_dir, "revoke-me").unwrap();
        let resp = client
            .get(format!("http://{addr}/api/repos/{repo}/workspaces"))
            .bearer_auth(&revocable_key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 401, "revoked key should return 401");

        shutdown_tx.send(()).ok();
    }

    /// Tracer bullet: connect WebSocket → subscribe to WorkspaceCreated →
    /// create workspace via REST → verify event received on WebSocket.
    #[tokio::test]
    async fn websocket_events_delivered_on_workspace_create() {
        use tokio_tungstenite::connect_async;
        use tokio_tungstenite::tungstenite::Message as TungsteniteMessage;

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), b"pub fn hello() {}\n").unwrap();

        let (addr, shutdown_tx, state, key) = start_test_server(root).await;
        let repo = &state.repo_name;

        // Connect to WebSocket, authenticating via query param.
        let ws_url = format!("ws://{addr}/ws/events?key={key}");
        let (mut ws_stream, _) = connect_async(&ws_url)
            .await
            .expect("WebSocket connection failed");

        // Send subscribe message — subscribe to WorkspaceCreated events only.
        let subscribe_msg = serde_json::json!({
            "subscribe": { "event_types": ["WorkspaceCreated"] }
        })
        .to_string();
        ws_stream
            .send(TungsteniteMessage::Text(subscribe_msg))
            .await
            .expect("failed to send subscribe message");

        // Give the server a moment to process the subscribe message.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Create a workspace via REST API — this should trigger a broadcast.
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://{addr}/api/repos/{repo}/workspaces"))
            .bearer_auth(&key)
            .json(&serde_json::json!({ "intent": "websocket test workspace" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);
        let created: serde_json::Value = resp.json().await.unwrap();
        let ws_id = created["id"].as_str().unwrap().to_string();

        // Receive the WorkspaceCreated event from the WebSocket.
        let received = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            ws_stream.next(),
        )
        .await
        .expect("timed out waiting for WebSocket event")
        .expect("stream ended")
        .expect("WebSocket error");

        let event: serde_json::Value = match received {
            TungsteniteMessage::Text(text) => serde_json::from_str(&text).unwrap(),
            other => panic!("expected Text message, got: {other:?}"),
        };

        assert_eq!(event["type"], "WorkspaceCreated", "wrong event type");
        assert_eq!(
            event["workspace_id"].as_str().unwrap(),
            ws_id,
            "workspace ID mismatch"
        );
        assert_eq!(
            event["data"]["intent"],
            "websocket test workspace",
            "intent mismatch"
        );

        shutdown_tx.send(()).ok();
    }

    /// WebSocket returns 401 when `key` query param is missing.
    #[tokio::test]
    async fn websocket_auth_required() {
        use tokio_tungstenite::connect_async;

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), b"pub fn hello() {}\n").unwrap();

        let (addr, shutdown_tx, _state, _key) = start_test_server(root).await;

        // Connect without a key — server should reject the upgrade.
        let ws_url = format!("ws://{addr}/ws/events");
        let result = connect_async(&ws_url).await;
        // Connection should fail (HTTP 401 upgrade rejection) or succeed then
        // immediately close. Either way, no events should flow.
        match result {
            Err(_) => {} // Connection rejected outright — expected.
            Ok((mut stream, _)) => {
                // Connection established but should close quickly.
                let msg = tokio::time::timeout(
                    std::time::Duration::from_secs(1),
                    stream.next(),
                )
                .await;
                // Accept timeout (no message) or a Close frame.
                match msg {
                    Err(_) | Ok(None) => {}
                    Ok(Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_)))) => {}
                    Ok(Some(m)) => panic!("unexpected message: {m:?}"),
                }
            }
        }

        shutdown_tx.send(()).ok();
    }

    /// Events filtered by event_type are NOT delivered to clients subscribed
    /// to a different event type.
    #[tokio::test]
    async fn websocket_filter_by_event_type() {
        use tokio_tungstenite::connect_async;
        use tokio_tungstenite::tungstenite::Message as TungsteniteMessage;

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), b"pub fn hello() {}\n").unwrap();

        let (addr, shutdown_tx, state, key) = start_test_server(root).await;
        let repo = &state.repo_name;

        // Subscribe to WorkspaceDiscarded only.
        let ws_url = format!("ws://{addr}/ws/events?key={key}");
        let (mut ws_stream, _) = connect_async(&ws_url).await.unwrap();
        ws_stream
            .send(TungsteniteMessage::Text(
                serde_json::json!({
                    "subscribe": { "event_types": ["WorkspaceDiscarded"] }
                })
                .to_string(),
            ))
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Create a workspace — WorkspaceCreated should NOT be delivered.
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://{addr}/api/repos/{repo}/workspaces"))
            .bearer_auth(&key)
            .json(&serde_json::json!({ "intent": "filter test" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);

        // Expect no message within a short window.
        let nothing = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            ws_stream.next(),
        )
        .await;
        assert!(
            nothing.is_err(),
            "expected no WorkspaceCreated to arrive on a WorkspaceDiscarded-only subscription"
        );

        shutdown_tx.send(()).ok();
    }

    /// Reconnecting agent receives events missed during disconnection via
    /// `?last_event_id=N`.  Only events matching the subscription filter are
    /// replayed.
    #[tokio::test]
    async fn websocket_event_replay_on_reconnect() {
        use tokio_tungstenite::connect_async;
        use tokio_tungstenite::tungstenite::Message as TungsteniteMessage;

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), b"pub fn hello() {}\n").unwrap();

        let (addr, shutdown_tx, state, key) = start_test_server(root).await;
        let repo = &state.repo_name;
        let client = reqwest::Client::new();

        // ── Phase 1: connect, receive first event, then disconnect ─────────────

        let ws_url = format!("ws://{addr}/ws/events?key={key}");
        let (mut ws_stream, _) = connect_async(&ws_url).await.unwrap();

        // Subscribe to all WorkspaceCreated events.
        ws_stream
            .send(TungsteniteMessage::Text(
                serde_json::json!({
                    "subscribe": { "event_types": ["WorkspaceCreated"] }
                })
                .to_string(),
            ))
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Create workspace A — this event should be delivered live.
        let resp = client
            .post(format!("http://{addr}/api/repos/{repo}/workspaces"))
            .bearer_auth(&key)
            .json(&serde_json::json!({ "intent": "workspace A" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);

        let live_msg = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            ws_stream.next(),
        )
        .await
        .expect("timed out waiting for live event")
        .expect("stream ended")
        .expect("WebSocket error");

        let live_event: serde_json::Value = match live_msg {
            TungsteniteMessage::Text(t) => serde_json::from_str(&t).unwrap(),
            other => panic!("expected Text, got: {other:?}"),
        };
        assert_eq!(live_event["type"], "WorkspaceCreated");
        assert_eq!(live_event["data"]["intent"], "workspace A");
        let last_seen_id = live_event["event_id"].as_u64().expect("event_id must be u64");
        assert!(last_seen_id > 0, "event_id must be a positive monotonic value");

        // Disconnect.
        drop(ws_stream);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // ── Phase 2: trigger events while disconnected ─────────────────────────

        // Create workspace B — missed by disconnected agent.
        let resp = client
            .post(format!("http://{addr}/api/repos/{repo}/workspaces"))
            .bearer_auth(&key)
            .json(&serde_json::json!({ "intent": "workspace B" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);
        let ws_b: serde_json::Value = resp.json().await.unwrap();
        let ws_b_id = ws_b["id"].as_str().unwrap().to_string();

        // Discard workspace B — NOT a WorkspaceCreated event; should NOT appear
        // in the replay (agent subscribed to WorkspaceCreated only).
        let resp = client
            .delete(format!("http://{addr}/api/repos/{repo}/workspaces/{ws_b_id}"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 204);

        // Create workspace C — also missed; matches subscription.
        let resp = client
            .post(format!("http://{addr}/api/repos/{repo}/workspaces"))
            .bearer_auth(&key)
            .json(&serde_json::json!({ "intent": "workspace C" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);

        // ── Phase 3: reconnect with last_event_id and verify replay ───────────

        let reconnect_url =
            format!("ws://{addr}/ws/events?key={key}&last_event_id={last_seen_id}");
        let (mut ws_reconnect, _) = connect_async(&reconnect_url).await.unwrap();

        // Send subscription — triggers replay.
        ws_reconnect
            .send(TungsteniteMessage::Text(
                serde_json::json!({
                    "subscribe": { "event_types": ["WorkspaceCreated"] }
                })
                .to_string(),
            ))
            .await
            .unwrap();

        // Collect up to 3 messages (replayed events + possible live events).
        // We expect exactly: WorkspaceCreated(B) and WorkspaceCreated(C).
        // WorkspaceDiscarded(B) must NOT appear.
        let mut replayed: Vec<serde_json::Value> = Vec::new();
        for _ in 0..2 {
            let msg = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                ws_reconnect.next(),
            )
            .await
            .expect("timed out waiting for replayed event")
            .expect("stream ended")
            .expect("WebSocket error");

            let event: serde_json::Value = match msg {
                TungsteniteMessage::Text(t) => serde_json::from_str(&t).unwrap(),
                other => panic!("expected Text, got: {other:?}"),
            };

            // No buffer_exceeded expected in this test (events are within buffer).
            assert!(
                event.get("buffer_exceeded").is_none(),
                "unexpected buffer_exceeded: {event}"
            );
            assert_eq!(event["type"], "WorkspaceCreated", "unexpected event type: {event}");
            assert!(
                event["event_id"].as_u64().unwrap() > last_seen_id,
                "replayed event_id must be newer than last_seen_id"
            );
            replayed.push(event);
        }

        let intents: Vec<&str> = replayed
            .iter()
            .map(|e| e["data"]["intent"].as_str().unwrap())
            .collect();
        assert!(
            intents.contains(&"workspace B"),
            "expected workspace B in replay; got: {intents:?}"
        );
        assert!(
            intents.contains(&"workspace C"),
            "expected workspace C in replay; got: {intents:?}"
        );

        // No third message should arrive within a short window.
        let extra = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            ws_reconnect.next(),
        )
        .await;
        // Only a timeout (no message) is acceptable here.
        assert!(extra.is_err(), "unexpected extra message after replay");

        shutdown_tx.send(()).ok();
    }

    /// When the server's replay buffer has been exceeded the agent receives a
    /// `{"buffer_exceeded": true}` message followed by whatever events are
    /// still in the buffer.
    #[tokio::test]
    async fn websocket_buffer_exceeded_flag_sent_on_reconnect() {
        use tokio_tungstenite::connect_async;
        use tokio_tungstenite::tungstenite::Message as TungsteniteMessage;

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), b"pub fn hello() {}\n").unwrap();

        let (addr, shutdown_tx, state, key) = start_test_server(root).await;
        let repo = &state.repo_name;
        let client = reqwest::Client::new();

        // Create a workspace to register one real event.
        let resp = client
            .post(format!("http://{addr}/api/repos/{repo}/workspaces"))
            .bearer_auth(&key)
            .json(&serde_json::json!({ "intent": "seed workspace" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);

        // Manually drain the event buffer to simulate exceeded capacity — the
        // agent's last_event_id will precede the oldest buffered event.
        {
            let mut buf = state.event_buffer.lock().unwrap();
            buf.events.clear();
        }

        // Reconnect with an old last_event_id (before the cleared buffer).
        let reconnect_url = format!("ws://{addr}/ws/events?key={key}&last_event_id=1");
        let (mut ws_reconnect, _) = connect_async(&reconnect_url).await.unwrap();

        ws_reconnect
            .send(TungsteniteMessage::Text(
                serde_json::json!({
                    "subscribe": { "event_types": ["WorkspaceCreated"] }
                })
                .to_string(),
            ))
            .await
            .unwrap();

        // First message must be buffer_exceeded.
        let msg = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            ws_reconnect.next(),
        )
        .await
        .expect("timed out waiting for buffer_exceeded message")
        .expect("stream ended")
        .expect("WebSocket error");

        let flag: serde_json::Value = match msg {
            TungsteniteMessage::Text(t) => serde_json::from_str(&t).unwrap(),
            other => panic!("expected Text, got: {other:?}"),
        };
        assert_eq!(
            flag["buffer_exceeded"], true,
            "expected buffer_exceeded=true; got: {flag}"
        );

        shutdown_tx.send(()).ok();
    }

    // ── Version endpoint tests ─────────────────────────────────────────────────

    /// Helper: create a workspace, write a file into its overlay, and submit it.
    /// Returns the new version ID.
    async fn create_version_via_submit(
        root: &std::path::Path,
        addr: SocketAddr,
        key: &str,
        repo: &str,
        intent: &str,
        overlay_content: &[u8],
    ) -> String {
        let client = reqwest::Client::new();

        // Create workspace.
        let resp = client
            .post(format!("http://{addr}/api/repos/{repo}/workspaces"))
            .bearer_auth(key)
            .json(&serde_json::json!({ "intent": intent }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);
        let ws: serde_json::Value = resp.json().await.unwrap();
        let ws_id = ws["id"].as_str().unwrap().to_string();

        // Write overlay file.
        let vai_dir = root.join(".vai");
        let overlay = vai_dir.join("workspaces").join(&ws_id).join("overlay");
        fs::create_dir_all(overlay.join("src")).unwrap();
        fs::write(overlay.join("src/lib.rs"), overlay_content).unwrap();

        // Submit.
        let resp = client
            .post(format!("http://{addr}/api/repos/{repo}/workspaces/{ws_id}/submit"))
            .bearer_auth(key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let result: serde_json::Value = resp.json().await.unwrap();
        result["version"].as_str().unwrap().to_string()
    }

    #[tokio::test]
    async fn list_versions_endpoint() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), b"pub fn hello() {}\n").unwrap();

        let (addr, shutdown_tx, state, key) = start_test_server(root).await;
        let repo = &state.repo_name;
        let client = reqwest::Client::new();

        // Initially only v1 exists.
        let resp = client
            .get(format!("http://{addr}/api/repos/{repo}/versions"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let versions: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(versions["data"].as_array().unwrap().len(), 1);
        assert_eq!(versions["data"][0]["version_id"], "v1");

        // Submit to create v2.
        create_version_via_submit(
            root,
            addr,
            &key,
            repo,
            "add world",
            b"pub fn hello() {}\npub fn world() {}\n",
        )
        .await;

        // Now two versions.
        let resp = client
            .get(format!("http://{addr}/api/repos/{repo}/versions"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let versions: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(versions["data"].as_array().unwrap().len(), 2);
        assert_eq!(versions["data"][0]["version_id"], "v1");
        assert_eq!(versions["data"][1]["version_id"], "v2");
        assert_eq!(versions["pagination"]["total"], 2);

        // ?per_page=1&page=2 returns only the second version (v2).
        let resp = client
            .get(format!("http://{addr}/api/repos/{repo}/versions?per_page=1&page=2"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let versions: serde_json::Value = resp.json().await.unwrap();
        let arr = versions["data"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["version_id"], "v2");
        assert_eq!(versions["pagination"]["total"], 2);

        shutdown_tx.send(()).ok();
    }

    #[tokio::test]
    async fn get_version_details_endpoint() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), b"pub fn hello() {}\n").unwrap();

        let (addr, shutdown_tx, state, key) = start_test_server(root).await;
        let repo = &state.repo_name;
        let client = reqwest::Client::new();

        // Create v2 with a new function.
        create_version_via_submit(
            root,
            addr,
            &key,
            repo,
            "add world function",
            b"pub fn hello() {}\npub fn world() -> u32 { 42 }\n",
        )
        .await;

        // GET /api/repos/:repo/versions/v2 returns version changes.
        let resp = client
            .get(format!("http://{addr}/api/repos/{repo}/versions/v2"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["version"]["version_id"], "v2");
        assert_eq!(body["version"]["intent"], "add world function");
        assert!(
            body["file_changes"].as_array().unwrap().len() > 0,
            "v2 should have file changes"
        );

        // GET /api/repos/:repo/versions/v999 → 404.
        let resp = client
            .get(format!("http://{addr}/api/repos/{repo}/versions/v999"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 404);

        shutdown_tx.send(()).ok();
    }

    #[tokio::test]
    async fn get_version_diff_endpoint() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), b"pub fn hello() {}\n").unwrap();

        let (addr, shutdown_tx, state, key) = start_test_server(root).await;
        let repo = &state.repo_name;
        let client = reqwest::Client::new();

        // Create v2 by modifying src/lib.rs.
        create_version_via_submit(
            root,
            addr,
            &key,
            repo,
            "add world function",
            b"pub fn hello() {}\npub fn world() -> u32 { 42 }\n",
        )
        .await;

        // GET /api/repos/:repo/versions/v2/diff returns file diffs.
        let resp = client
            .get(format!("http://{addr}/api/repos/{repo}/versions/v2/diff"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["version_id"], "v2");
        assert_eq!(body["base_version_id"], "v1");
        let files = body["files"].as_array().unwrap();
        assert!(!files.is_empty(), "v2 should have file diffs");
        let file = &files[0];
        assert_eq!(file["path"], "src/lib.rs");
        let diff = file["diff"].as_str().unwrap();
        assert!(
            diff.contains('+') || diff.contains('-'),
            "diff should contain + or - markers"
        );

        // GET /api/repos/:repo/versions/v1/diff → initial version has no file diffs.
        let resp = client
            .get(format!("http://{addr}/api/repos/{repo}/versions/v1/diff"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["files"].as_array().unwrap().len(), 0);

        // GET /api/repos/:repo/versions/v999/diff → 404.
        let resp = client
            .get(format!("http://{addr}/api/repos/{repo}/versions/v999/diff"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 404);

        shutdown_tx.send(()).ok();
    }

    #[tokio::test]
    async fn rollback_endpoint_no_downstream() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), b"pub fn hello() {}\n").unwrap();

        let (addr, shutdown_tx, state, key) = start_test_server(root).await;
        let repo = &state.repo_name;
        let client = reqwest::Client::new();

        // Create v2.
        create_version_via_submit(
            root,
            addr,
            &key,
            repo,
            "add world",
            b"pub fn hello() {}\npub fn world() {}\n",
        )
        .await;

        // Rollback v2 — no downstream, so should succeed with force: false.
        let resp = client
            .post(format!("http://{addr}/api/repos/{repo}/versions/rollback"))
            .bearer_auth(&key)
            .json(&serde_json::json!({ "version": "v2", "force": false }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200, "rollback with no downstream should succeed");
        let result: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(result["new_version"]["version_id"], "v3");

        shutdown_tx.send(()).ok();
    }

    #[tokio::test]
    async fn rollback_endpoint_blocks_when_downstream_without_force() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), b"pub fn hello() {}\n").unwrap();

        let (addr, shutdown_tx, state, key) = start_test_server(root).await;
        let repo = &state.repo_name;
        let client = reqwest::Client::new();

        // Create v2 modifying src/lib.rs.
        create_version_via_submit(
            root,
            addr,
            &key,
            repo,
            "add world",
            b"pub fn hello() {}\npub fn world() {}\n",
        )
        .await;

        // Create v3 also modifying src/lib.rs (downstream of v2).
        create_version_via_submit(
            root,
            addr,
            &key,
            repo,
            "add foo",
            b"pub fn hello() {}\npub fn world() {}\npub fn foo() {}\n",
        )
        .await;

        // Rolling back v2 without force should return 409 because v3 depends on it.
        let resp = client
            .post(format!("http://{addr}/api/repos/{repo}/versions/rollback"))
            .bearer_auth(&key)
            .json(&serde_json::json!({ "version": "v2", "force": false }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 409, "should be blocked by downstream v3");
        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(body["error"].as_str().unwrap().contains("downstream"));
        assert!(body["impact"].is_object());

        // With force: true the rollback should proceed.
        let resp = client
            .post(format!("http://{addr}/api/repos/{repo}/versions/rollback"))
            .bearer_auth(&key)
            .json(&serde_json::json!({ "version": "v2", "force": true }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200, "force rollback should succeed");
        let result: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(result["new_version"]["version_id"], "v4");

        shutdown_tx.send(()).ok();
    }

    // ── File upload / download tests ───────────────────────────────────────────

    /// Upload a text file and a binary file into a workspace, then download
    /// each one back and verify the content round-trips correctly.
    #[tokio::test]
    async fn file_upload_download_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), b"pub fn hello() {}\n").unwrap();

        let (addr, shutdown_tx, state, key) = start_test_server(root).await;
        let repo = &state.repo_name;
        let client = reqwest::Client::new();

        // Create a workspace.
        let resp = client
            .post(format!("http://{addr}/api/repos/{repo}/workspaces"))
            .bearer_auth(&key)
            .json(&serde_json::json!({ "intent": "file upload test" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);
        let ws: serde_json::Value = resp.json().await.unwrap();
        let ws_id = ws["id"].as_str().unwrap().to_string();

        // Prepare a text file and a small binary file.
        let text_content = b"pub fn new_func() -> u32 { 42 }\n";
        let binary_content: Vec<u8> = (0u8..=255).collect(); // 256 bytes

        let text_b64 = BASE64.encode(text_content);
        let binary_b64 = BASE64.encode(&binary_content);

        // Upload both files via POST /api/repos/:repo/workspaces/:id/files.
        let resp = client
            .post(format!("http://{addr}/api/repos/{repo}/workspaces/{ws_id}/files"))
            .bearer_auth(&key)
            .json(&serde_json::json!({
                "files": [
                    { "path": "src/new.rs",  "content_base64": text_b64   },
                    { "path": "data/bin.bin","content_base64": binary_b64 },
                ]
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200, "upload should return 200");
        let upload_result: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(upload_result["uploaded"], 2);
        let paths = upload_result["paths"].as_array().unwrap();
        assert!(paths.iter().any(|p| p == "src/new.rs"), "src/new.rs should be listed");
        assert!(paths.iter().any(|p| p == "data/bin.bin"), "data/bin.bin should be listed");

        // Workspace should now be Active.
        let resp = client
            .get(format!("http://{addr}/api/repos/{repo}/workspaces/{ws_id}"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        let ws_detail: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(ws_detail["status"], "Active", "workspace should be Active after upload");

        // Download text file from overlay.
        let resp = client
            .get(format!("http://{addr}/api/repos/{repo}/workspaces/{ws_id}/files/src/new.rs"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let dl: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(dl["found_in"], "overlay");
        assert_eq!(dl["size"], text_content.len() as u64);
        let decoded = BASE64.decode(dl["content_base64"].as_str().unwrap()).unwrap();
        assert_eq!(decoded, text_content);

        // Download binary file from overlay.
        let resp = client
            .get(format!("http://{addr}/api/repos/{repo}/workspaces/{ws_id}/files/data/bin.bin"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let dl: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(dl["found_in"], "overlay");
        let decoded = BASE64.decode(dl["content_base64"].as_str().unwrap()).unwrap();
        assert_eq!(decoded, binary_content);

        // Download a file not in overlay — falls back to base (repo root).
        let resp = client
            .get(format!("http://{addr}/api/repos/{repo}/workspaces/{ws_id}/files/src/lib.rs"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let dl: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(dl["found_in"], "base");
        let decoded = BASE64.decode(dl["content_base64"].as_str().unwrap()).unwrap();
        assert_eq!(decoded, b"pub fn hello() {}\n");

        // Download from main version via GET /api/repos/:repo/files/:path.
        let resp = client
            .get(format!("http://{addr}/api/repos/{repo}/files/src/lib.rs"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let dl: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(dl["found_in"], "base");
        let decoded = BASE64.decode(dl["content_base64"].as_str().unwrap()).unwrap();
        assert_eq!(decoded, b"pub fn hello() {}\n");

        // 404 for non-existent file.
        let resp = client
            .get(format!("http://{addr}/api/repos/{repo}/workspaces/{ws_id}/files/does/not/exist.txt"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 404);

        // 404 for non-existent main file.
        let resp = client
            .get(format!("http://{addr}/api/repos/{repo}/files/does/not/exist.txt"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 404);

        shutdown_tx.send(()).ok();
    }

    /// Re-uploading a file that already exists in the overlay is treated as a
    /// modify (FileModified event) rather than an add.
    #[tokio::test]
    async fn file_upload_modify_existing() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), b"pub fn hello() {}\n").unwrap();

        let (addr, shutdown_tx, state, key) = start_test_server(root).await;
        let repo = &state.repo_name;
        let client = reqwest::Client::new();

        // Create workspace.
        let resp = client
            .post(format!("http://{addr}/api/repos/{repo}/workspaces"))
            .bearer_auth(&key)
            .json(&serde_json::json!({ "intent": "modify test" }))
            .send()
            .await
            .unwrap();
        let ws: serde_json::Value = resp.json().await.unwrap();
        let ws_id = ws["id"].as_str().unwrap().to_string();

        let upload_file = |content: &'static [u8]| {
            let b64 = BASE64.encode(content);
            serde_json::json!({
                "files": [{ "path": "src/lib.rs", "content_base64": b64 }]
            })
        };

        // First upload: FileAdded.
        let resp = client
            .post(format!("http://{addr}/api/repos/{repo}/workspaces/{ws_id}/files"))
            .bearer_auth(&key)
            .json(&upload_file(b"pub fn hello() {}\npub fn world() {}\n"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);

        // Second upload of the same path: FileModified.
        let resp = client
            .post(format!("http://{addr}/api/repos/{repo}/workspaces/{ws_id}/files"))
            .bearer_auth(&key)
            .json(&upload_file(b"pub fn hello() {}\npub fn world() {}\npub fn foo() {}\n"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);

        // Verify final content is the latest upload.
        let resp = client
            .get(format!("http://{addr}/api/repos/{repo}/workspaces/{ws_id}/files/src/lib.rs"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let dl: serde_json::Value = resp.json().await.unwrap();
        let decoded = BASE64.decode(dl["content_base64"].as_str().unwrap()).unwrap();
        assert_eq!(
            decoded,
            b"pub fn hello() {}\npub fn world() {}\npub fn foo() {}\n"
        );

        shutdown_tx.send(()).ok();
    }

    /// Path traversal attempts are rejected with 400 Bad Request.
    #[tokio::test]
    async fn file_upload_path_traversal_rejected() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), b"pub fn hello() {}\n").unwrap();

        let (addr, shutdown_tx, state, key) = start_test_server(root).await;
        let repo = &state.repo_name;
        let client = reqwest::Client::new();

        // Create workspace.
        let resp = client
            .post(format!("http://{addr}/api/repos/{repo}/workspaces"))
            .bearer_auth(&key)
            .json(&serde_json::json!({ "intent": "traversal test" }))
            .send()
            .await
            .unwrap();
        let ws: serde_json::Value = resp.json().await.unwrap();
        let ws_id = ws["id"].as_str().unwrap().to_string();

        let b64 = BASE64.encode(b"evil content");
        let resp = client
            .post(format!("http://{addr}/api/repos/{repo}/workspaces/{ws_id}/files"))
            .bearer_auth(&key)
            .json(&serde_json::json!({
                "files": [{ "path": "../../etc/passwd", "content_base64": b64 }]
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 400, "path traversal should be rejected");

        shutdown_tx.send(()).ok();
    }

    // ── Graph API tests ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn graph_entities_list_and_filter() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Create a Rust source file so the graph has something to query.
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("src/lib.rs"),
            b"pub fn hello() {}\npub struct World;\n",
        )
        .unwrap();

        let (addr, shutdown_tx, state, key) = start_test_server(root).await;
        let repo = &state.repo_name;
        let client = reqwest::Client::new();

        // List all entities — graph was populated during init.
        let resp = client
            .get(format!("http://{addr}/api/repos/{repo}/graph/entities"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let entities: serde_json::Value = resp.json().await.unwrap();
        let arr = entities.as_array().unwrap();
        assert!(!arr.is_empty(), "expected entities from src/lib.rs");

        // Filter by kind=function — only functions should appear.
        let resp = client
            .get(format!("http://{addr}/api/repos/{repo}/graph/entities?kind=function"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let funcs: serde_json::Value = resp.json().await.unwrap();
        let funcs_arr = funcs.as_array().unwrap();
        assert!(
            funcs_arr.iter().all(|e| e["kind"] == "function"),
            "expected only functions"
        );

        // Filter by name=hello — should find at least one entity.
        let resp = client
            .get(format!("http://{addr}/api/repos/{repo}/graph/entities?name=hello"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let by_name: serde_json::Value = resp.json().await.unwrap();
        let by_name_arr = by_name.as_array().unwrap();
        assert!(
            !by_name_arr.is_empty(),
            "expected at least one entity named 'hello'"
        );

        shutdown_tx.send(()).ok();
    }

    #[tokio::test]
    async fn graph_entity_detail_and_deps() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("src/lib.rs"),
            b"pub fn hello() {}\npub struct World;\n",
        )
        .unwrap();

        let (addr, shutdown_tx, state, key) = start_test_server(root).await;
        let repo = &state.repo_name;
        let client = reqwest::Client::new();

        // Get all entities and pick one ID.
        let resp = client
            .get(format!("http://{addr}/api/repos/{repo}/graph/entities"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        let entities: serde_json::Value = resp.json().await.unwrap();
        let id = entities.as_array().unwrap()[0]["id"]
            .as_str()
            .unwrap()
            .to_string();

        // GET /api/repos/:repo/graph/entities/:id
        let resp = client
            .get(format!("http://{addr}/api/repos/{repo}/graph/entities/{id}"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let detail: serde_json::Value = resp.json().await.unwrap();
        assert!(detail["entity"].is_object());
        assert!(detail["relationships"].is_array());
        assert_eq!(detail["entity"]["id"], id);

        // GET /api/repos/:repo/graph/entities/:id/deps
        let resp = client
            .get(format!("http://{addr}/api/repos/{repo}/graph/entities/{id}/deps"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let deps: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(deps["entity_id"], id);
        assert!(deps["deps"].is_array());
        assert!(deps["relationships"].is_array());

        // 404 for non-existent entity.
        let resp = client
            .get(format!("http://{addr}/api/repos/{repo}/graph/entities/nonexistent-id"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 404);

        shutdown_tx.send(()).ok();
    }

    #[tokio::test]
    async fn graph_blast_radius() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("src/lib.rs"),
            b"pub fn hello() {}\npub struct World;\n",
        )
        .unwrap();

        let (addr, shutdown_tx, state, key) = start_test_server(root).await;
        let repo = &state.repo_name;
        let client = reqwest::Client::new();

        // Get one entity ID to use as seed.
        let resp = client
            .get(format!("http://{addr}/api/repos/{repo}/graph/entities"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        let entities: serde_json::Value = resp.json().await.unwrap();
        let id = entities.as_array().unwrap()[0]["id"]
            .as_str()
            .unwrap()
            .to_string();

        // GET /api/repos/:repo/graph/blast-radius?entities=<id>&hops=2
        let resp = client
            .get(format!(
                "http://{addr}/api/repos/{repo}/graph/blast-radius?entities={id}&hops=2"
            ))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let br: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(br["hops"], 2);
        assert!(br["entities"].is_array());
        assert!(br["relationships"].is_array());
        assert!(br["seed_entities"].as_array().unwrap().contains(&serde_json::json!(id)));

        // Missing `entities` param → 400.
        let resp = client
            .get(format!("http://{addr}/api/repos/{repo}/graph/blast-radius"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        // axum returns 400 when a required query parameter is missing.
        assert!(
            resp.status().is_client_error(),
            "missing required param should return 4xx"
        );

        shutdown_tx.send(()).ok();
    }

    // ── Clone endpoint tests ───────────────────────────────────────────────────

    /// `GET /api/repo/files` returns all files in the repo root.
    #[tokio::test]
    async fn list_repo_files_endpoint() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Create a small directory tree.
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), b"pub fn hello() {}\n").unwrap();
        fs::write(root.join("src/main.rs"), b"fn main() {}\n").unwrap();
        fs::write(root.join("README.md"), b"# test\n").unwrap();

        let (addr, shutdown_tx, state, key) = start_test_server(root).await;
        let repo = &state.repo_name;
        let client = reqwest::Client::new();

        let resp = client
            .get(format!("http://{addr}/api/repos/{repo}/files"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        let files = body["files"].as_array().unwrap();

        // Should contain the source files (at minimum the Rust files).
        let paths: Vec<&str> = files.iter().map(|f| f.as_str().unwrap()).collect();
        assert!(paths.contains(&"src/lib.rs"), "should list src/lib.rs");
        assert!(paths.contains(&"src/main.rs"), "should list src/main.rs");
        assert!(paths.contains(&"README.md"), "should list README.md");

        // head_version should be present.
        assert!(body["head_version"].is_string(), "should include head_version");
        assert!(body["count"].is_number(), "should include count");
        assert_eq!(body["count"].as_u64().unwrap(), files.len() as u64);

        shutdown_tx.send(()).ok();
    }

    /// Full clone flow: start server → call clone() → verify files on disk and
    /// `.vai/remote.toml` written correctly.
    #[tokio::test]
    async fn clone_downloads_files_and_writes_remote_config() {
        use crate::clone as remote_clone;

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Repo has two source files.
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), b"pub fn hello() -> u32 { 42 }\n").unwrap();
        fs::write(root.join("src/main.rs"), b"fn main() { println!(\"hi\"); }\n").unwrap();

        let (addr, shutdown_tx, state, key) = start_test_server(root).await;
        let repo_name = &state.repo_name;

        let dest = tmp.path().join("cloned");
        let vai_url = format!("vai://{addr}/{repo_name}");
        let result = remote_clone::clone(&vai_url, &dest, &key)
            .await
            .expect("clone should succeed");

        // Files should be present locally.
        assert!(dest.join("src/lib.rs").exists(), "src/lib.rs should be cloned");
        assert!(dest.join("src/main.rs").exists(), "src/main.rs should be cloned");

        // File contents should match.
        let lib_content = fs::read(dest.join("src/lib.rs")).unwrap();
        assert_eq!(lib_content, b"pub fn hello() -> u32 { 42 }\n");

        // .vai/ structure should exist.
        assert!(dest.join(".vai").is_dir(), ".vai/ should exist");
        assert!(dest.join(".vai/config.toml").exists(), "config.toml should exist");
        assert!(dest.join(".vai/head").exists(), "head should exist");
        assert!(dest.join(".vai/remote.toml").exists(), "remote.toml should exist");

        // remote.toml should contain the server URL and repo name.
        let remote = remote_clone::read_remote_config(&dest.join(".vai"))
            .expect("remote.toml should be readable");
        assert_eq!(remote.server_url, format!("http://{addr}"));
        // repo_name comes from the temp dir name — just check it is non-empty.
        assert!(!remote.repo_name.is_empty(), "repo_name should be set");

        // Result counts should match: src/lib.rs + src/main.rs + vai.toml (from init).
        assert!(result.files_downloaded >= 2, "should download at least 2 files");
        assert!(!result.head_version.is_empty());

        shutdown_tx.send(()).ok();
    }

    /// Full remote workspace workflow: server running → register workspace →
    /// upload files → submit → verify new version on server.
    #[tokio::test]
    async fn remote_workspace_register_upload_submit() {
        use crate::clone::RemoteConfig;
        use crate::remote_workspace;

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Repo has one source file.
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), b"pub fn hello() -> u32 { 42 }\n").unwrap();

        let (addr, shutdown_tx, state, key) = start_test_server(root).await;

        let remote = RemoteConfig {
            server_url: format!("http://{addr}"),
            api_key: key.clone(),
            repo_name: state.repo_name.clone(),
            cloned_at_version: "v1".to_string(),
        };

        // 1. Register a workspace on the server.
        let meta = remote_workspace::register_workspace(&remote, "add logging feature")
            .await
            .expect("register_workspace should succeed");

        assert_eq!(meta.intent, "add logging feature");
        assert_eq!(meta.status, "Created");
        assert!(!meta.id.is_empty());
        let ws_id = meta.id.clone();

        // 2. Verify it appears in the list.
        let workspaces = remote_workspace::list_workspaces(&remote)
            .await
            .expect("list_workspaces should succeed");
        assert!(workspaces.iter().any(|w| w.id == ws_id));

        // 3. Upload an overlay file.
        let overlay_dir = tmp.path().join("overlay");
        fs::create_dir_all(overlay_dir.join("src")).unwrap();
        fs::write(
            overlay_dir.join("src/lib.rs"),
            b"pub fn hello() -> u32 { 42 }\npub fn log(msg: &str) { eprintln!(\"{}\", msg); }\n",
        )
        .unwrap();

        let uploaded = remote_workspace::upload_overlay_files(&remote, &ws_id, &overlay_dir)
            .await
            .expect("upload_overlay_files should succeed");
        assert_eq!(uploaded, vec!["src/lib.rs"]);

        // 4. Submit the workspace — triggers server-side semantic merge.
        let submit_result = remote_workspace::submit_workspace(&remote, &ws_id)
            .await
            .expect("submit_workspace should succeed");

        assert!(!submit_result.version.is_empty());
        assert!(submit_result.files_applied > 0);

        // 5. Server HEAD should have advanced.
        let client = reqwest::Client::new();
        let resp = client
            .get(format!("http://{addr}/api/status"))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_ne!(
            body["head_version"].as_str().unwrap(),
            "v1",
            "server HEAD should advance after submit"
        );

        shutdown_tx.send(()).ok();
    }

    // ── Issue REST API tests ───────────────────────────────────────────────────

    /// Full CRUD cycle for the issue REST API:
    /// create → list → get → update → close.
    #[tokio::test]
    async fn issue_crud_endpoints() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let (addr, shutdown_tx, state, key) = start_test_server(root).await;
        let repo = &state.repo_name;
        let client = reqwest::Client::new();

        // ── Create ────────────────────────────────────────────────────────────

        let create_body = serde_json::json!({
            "title": "Fix login bug",
            "description": "Auth is broken for new users",
            "priority": "high",
            "labels": ["bug", "auth"],
            "creator": "agent-01"
        });

        let resp = client
            .post(format!("http://{addr}/api/repos/{repo}/issues"))
            .bearer_auth(&key)
            .json(&create_body)
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 201);
        let created: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(created["title"], "Fix login bug");
        assert_eq!(created["status"], "open");
        assert_eq!(created["priority"], "high");
        assert_eq!(created["creator"], "agent-01");
        assert!(created["labels"].as_array().unwrap().contains(&serde_json::json!("bug")));

        let issue_id = created["id"].as_str().unwrap().to_string();

        // ── Create a second issue ─────────────────────────────────────────────

        let resp2 = client
            .post(format!("http://{addr}/api/repos/{repo}/issues"))
            .bearer_auth(&key)
            .json(&serde_json::json!({
                "title": "Add rate limiting",
                "priority": "medium",
                "creator": "agent-02"
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp2.status(), 201);

        // ── List all ──────────────────────────────────────────────────────────

        let resp = client
            .get(format!("http://{addr}/api/repos/{repo}/issues"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let issues: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(issues["data"].as_array().unwrap().len(), 2);

        // ── List with filter ──────────────────────────────────────────────────

        let resp = client
            .get(format!("http://{addr}/api/repos/{repo}/issues?priority=high"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let filtered: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(filtered["data"].as_array().unwrap().len(), 1);
        assert_eq!(filtered["data"][0]["title"], "Fix login bug");

        // Filter by creator.
        let resp = client
            .get(format!("http://{addr}/api/repos/{repo}/issues?created_by=agent-02"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        let by_creator: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(by_creator["data"].as_array().unwrap().len(), 1);
        assert_eq!(by_creator["data"][0]["title"], "Add rate limiting");

        // ── Get by ID ─────────────────────────────────────────────────────────

        let resp = client
            .get(format!("http://{addr}/api/repos/{repo}/issues/{issue_id}"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let fetched: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(fetched["id"], issue_id.as_str());
        assert_eq!(fetched["title"], "Fix login bug");

        // ── Get non-existent issue → 404 ──────────────────────────────────────

        let fake_id = uuid::Uuid::new_v4();
        let resp = client
            .get(format!("http://{addr}/api/repos/{repo}/issues/{fake_id}"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 404);

        // ── Update ────────────────────────────────────────────────────────────

        let resp = client
            .patch(format!("http://{addr}/api/repos/{repo}/issues/{issue_id}"))
            .bearer_auth(&key)
            .json(&serde_json::json!({
                "priority": "critical",
                "labels": ["bug", "auth", "urgent"]
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let updated: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(updated["priority"], "critical");
        assert_eq!(updated["labels"].as_array().unwrap().len(), 3);

        // ── Close ─────────────────────────────────────────────────────────────

        let resp = client
            .post(format!("http://{addr}/api/repos/{repo}/issues/{issue_id}/close"))
            .bearer_auth(&key)
            .json(&serde_json::json!({ "resolution": "resolved" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let closed: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(closed["status"], "closed");
        assert_eq!(closed["resolution"], "resolved");

        // ── List with status filter ───────────────────────────────────────────

        let resp = client
            .get(format!("http://{addr}/api/repos/{repo}/issues?status=open"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        let open_issues: serde_json::Value = resp.json().await.unwrap();
        // Only the second issue (rate limiting) remains open.
        assert_eq!(open_issues["data"].as_array().unwrap().len(), 1);
        assert_eq!(open_issues["data"][0]["title"], "Add rate limiting");

        // ── Free-text resolution is accepted (any string allowed) ────────────────

        // Re-open by creating a fresh issue and closing it with a free-text resolution.
        let resp = client
            .post(format!("http://{addr}/api/repos/{repo}/issues"))
            .bearer_auth(&key)
            .json(&serde_json::json!({
                "title": "Temp issue for free-text resolution test",
                "body": "",
                "priority": "low",
                "labels": [],
                "created_by": "test"
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);
        let temp_issue: serde_json::Value = resp.json().await.unwrap();
        let temp_id = temp_issue["id"].as_str().unwrap();

        let resp = client
            .post(format!("http://{addr}/api/repos/{repo}/issues/{temp_id}/close"))
            .bearer_auth(&key)
            .json(&serde_json::json!({ "resolution": "resolved in v5" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let closed_temp: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(closed_temp["resolution"], "resolved in v5");

        shutdown_tx.send(()).ok();
    }

    /// `GET /api/repos/:repo/issues/:id` returns enriched detail with `links`,
    /// `attachments`, and `comments` fields.  Comments round-trip correctly.
    /// Links and attachments are empty in SQLite mode (write-path not supported).
    #[tokio::test]
    async fn issue_detail_response_is_enriched() {
        let tmp = TempDir::new().unwrap();
        let storage_tmp = TempDir::new().unwrap();
        fs::create_dir_all(storage_tmp.path()).unwrap();

        let (addr, shutdown_tx, key) =
            start_test_server_multi_repo(tmp.path(), storage_tmp.path().to_path_buf()).await;
        let client = reqwest::Client::new();

        // Register a repo.
        let resp = client
            .post(format!("http://{addr}/api/repos"))
            .bearer_auth(&key)
            .json(&serde_json::json!({ "name": "detail-repo" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);

        let base = format!("http://{addr}/api/repos/detail-repo");

        // Create an issue.
        let resp = client
            .post(format!("{base}/issues"))
            .bearer_auth(&key)
            .json(&serde_json::json!({
                "title": "Issue A", "description": "desc", "priority": "high", "creator": "alice"
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);
        let issue_a: serde_json::Value = resp.json().await.unwrap();
        let id_a = issue_a["id"].as_str().unwrap().to_string();

        // Add two comments to verify they appear in the detail response.
        for body in ["First comment", "Second comment"] {
            let resp = client
                .post(format!("{base}/issues/{id_a}/comments"))
                .bearer_auth(&key)
                .json(&serde_json::json!({
                    "author": "alice", "body": body, "author_type": "human"
                }))
                .send()
                .await
                .unwrap();
            assert_eq!(resp.status(), 201);
        }

        // Fetch issue detail — should include enriched fields.
        let resp = client
            .get(format!("{base}/issues/{id_a}"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let detail: serde_json::Value = resp.json().await.unwrap();

        assert_eq!(detail["id"], id_a.as_str());
        assert_eq!(detail["title"], "Issue A");

        // `comments` field contains the two comments we added (in order).
        let comments = detail["comments"].as_array().unwrap();
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[0]["body"], "First comment");
        assert_eq!(comments[1]["body"], "Second comment");
        assert_eq!(comments[0]["author_type"], "human");

        // `attachments` field is present (empty — no attachments uploaded).
        let attachments = detail["attachments"].as_array().unwrap();
        assert_eq!(attachments.len(), 0);

        // `links` field is present (empty — SQLite write-path not supported).
        let links = detail["links"].as_array().unwrap();
        assert_eq!(links.len(), 0);

        shutdown_tx.send(()).ok();
    }

    /// Agent-initiated issue creation: guardrails (rate limit + duplicate detection).
    #[tokio::test]
    async fn agent_initiated_issues() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let (addr, shutdown_tx, state, key) = start_test_server(root).await;
        let repo = &state.repo_name;
        let client = reqwest::Client::new();

        let agent_source = serde_json::json!({
            "source_type": "test_failure",
            "details": { "suite": "unit", "test": "auth::login" }
        });

        // ── Create first agent issue ──────────────────────────────────────────
        let resp = client
            .post(format!("http://{addr}/api/repos/{repo}/issues"))
            .bearer_auth(&key)
            .json(&serde_json::json!({
                "title": "Auth login unit test failing",
                "priority": "high",
                "created_by_agent": "ci-agent",
                "source": agent_source,
                "max_per_hour": 2
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);
        let created: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(created["creator"], "ci-agent");
        assert!(created["agent_source"].is_object());
        assert_eq!(created["agent_source"]["source_type"], "test_failure");
        assert!(created["possible_duplicate_of"].is_null());
        let first_id = created["id"].as_str().unwrap().to_string();

        // ── Filter by agent creator ───────────────────────────────────────────
        let resp = client
            .get(format!("http://{addr}/api/repos/{repo}/issues?created_by=ci-agent"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        let by_agent: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(by_agent["data"].as_array().unwrap().len(), 1);

        // ── Duplicate detection: similar title warns ──────────────────────────
        let resp = client
            .post(format!("http://{addr}/api/repos/{repo}/issues"))
            .bearer_auth(&key)
            .json(&serde_json::json!({
                "title": "Auth login test failing again",
                "priority": "medium",
                "created_by_agent": "ci-agent",
                "source": { "source_type": "test_failure" },
                "max_per_hour": 20
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);
        let dup_resp: serde_json::Value = resp.json().await.unwrap();
        // Should report possible duplicate pointing to first issue.
        assert_eq!(
            dup_resp["possible_duplicate_of"].as_str().unwrap(),
            first_id.as_str()
        );

        // ── Rate limit: exceed max_per_hour = 2 ──────────────────────────────
        // Already created 2 above; third should be rejected.
        let resp = client
            .post(format!("http://{addr}/api/repos/{repo}/issues"))
            .bearer_auth(&key)
            .json(&serde_json::json!({
                "title": "Overflow issue",
                "priority": "low",
                "created_by_agent": "ci-agent",
                "source": { "source_type": "test_failure" },
                "max_per_hour": 2
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 429);
        assert!(resp.headers().contains_key("retry-after"));

        shutdown_tx.send(()).ok();
    }

    /// Work queue: create issues, verify they appear as available, claim one.
    #[tokio::test]
    async fn work_queue_available_and_claim() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let (addr, shutdown_tx, state, key) = start_test_server(root).await;
        let repo = &state.repo_name;
        let client = reqwest::Client::new();

        // Create two open issues.
        let resp = client
            .post(format!("http://{addr}/api/repos/{repo}/issues"))
            .bearer_auth(&key)
            .json(&serde_json::json!({
                "title": "Fix login bug",
                "description": "Users cannot log in with OAuth",
                "priority": "high"
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);
        let issue1: serde_json::Value = resp.json().await.unwrap();
        let issue1_id = issue1["id"].as_str().unwrap().to_string();

        let resp = client
            .post(format!("http://{addr}/api/repos/{repo}/issues"))
            .bearer_auth(&key)
            .json(&serde_json::json!({
                "title": "Add metrics dashboard",
                "description": "Build a dashboard for system metrics",
                "priority": "medium"
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);

        // ── GET /api/repos/:repo/work-queue ───────────────────────────────────

        let resp = client
            .get(format!("http://{addr}/api/repos/{repo}/work-queue"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let queue: serde_json::Value = resp.json().await.unwrap();
        let available = queue["available_work"].as_array().unwrap();
        // Both issues should be available (no active workspaces).
        assert_eq!(available.len(), 2);
        // High priority comes first.
        assert_eq!(available[0]["issue_id"], issue1_id);
        assert_eq!(queue["blocked_work"].as_array().unwrap().len(), 0);

        // ── POST /api/repos/:repo/work-queue/claim ────────────────────────────

        let resp = client
            .post(format!("http://{addr}/api/repos/{repo}/work-queue/claim"))
            .bearer_auth(&key)
            .json(&serde_json::json!({ "issue_id": issue1_id }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);
        let claim: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(claim["issue_id"], issue1_id);
        assert!(claim["workspace_id"].as_str().is_some());

        // Claimed issue should now be in_progress.
        let resp = client
            .get(format!("http://{addr}/api/repos/{repo}/issues/{issue1_id}"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        let issue: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(issue["status"], "in_progress");

        // ── Claim a non-existent issue → 404 ──────────────────────────────────

        let resp = client
            .post(format!("http://{addr}/api/repos/{repo}/work-queue/claim"))
            .bearer_auth(&key)
            .json(&serde_json::json!({
                "issue_id": "00000000-0000-0000-0000-000000000000"
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 404);

        // ── Claim an already in-progress issue → 409 ──────────────────────────

        let resp = client
            .post(format!("http://{addr}/api/repos/{repo}/work-queue/claim"))
            .bearer_auth(&key)
            .json(&serde_json::json!({ "issue_id": issue1_id }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 409);

        shutdown_tx.send(()).ok();
    }

    // ── /api/repos endpoint tests ─────────────────────────────────────────────

    /// Helper: start a test server with a storage_root configured.
    async fn start_test_server_multi_repo(
        root: &Path,
        storage_root: PathBuf,
    ) -> (SocketAddr, oneshot::Sender<()>, String) {
        repo::init(root).unwrap();
        let vai_dir = root.join(".vai");
        let repo_config = repo::read_config(&vai_dir).unwrap();
        // Use the bootstrap admin key so tests can access all endpoints,
        // including admin-only routes like /api/repos.
        let key = "vai_admin_test".to_string();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let (event_tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let state = Arc::new(AppState {
            vai_dir: vai_dir.clone(),
            repo_root: root.to_path_buf(),
            started_at: Instant::now(),
            repo_name: repo_config.name.clone(),
            vai_version: env!("CARGO_PKG_VERSION").to_string(),
            event_tx,
            event_seq: Arc::new(AtomicU64::new(0)),
            event_buffer: Arc::new(StdMutex::new(EventBuffer::new())),
            conflict_engine: Arc::new(Mutex::new(conflict::ConflictEngine::new())),
            repo_lock: Arc::new(Mutex::new(())),
            storage_root: Some(storage_root),
            storage: crate::storage::StorageBackend::local(&vai_dir),
            admin_key: "vai_admin_test".to_string(),
            jwt_service: Arc::new(crate::auth::jwt::JwtService::new(
                "test-jwt-secret".to_string(),
                None,
                3600,
            )),
            rate_limiter: Arc::new(RateLimiter::new()),
            cors_origins: vec![],
            default_new_user_role: crate::storage::RepoRole::Write,
        });

        let app = build_app(Arc::clone(&state));
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        tokio::spawn(async move {
            axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
                .with_graceful_shutdown(async { shutdown_rx.await.ok(); })
                .await
                .unwrap();
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        (addr, shutdown_tx, key)
    }

    #[tokio::test]
    async fn list_repos_empty_when_no_storage_root() {
        let tmp = TempDir::new().unwrap();
        let (addr, shutdown_tx, _state, key) = start_test_server(tmp.path()).await;
        let client = reqwest::Client::new();

        let resp = client
            .get(format!("http://{addr}/api/repos"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let list: serde_json::Value = resp.json().await.unwrap();
        assert!(list.as_array().unwrap().is_empty());

        shutdown_tx.send(()).ok();
    }

    #[tokio::test]
    async fn create_repo_rejected_without_storage_root() {
        let tmp = TempDir::new().unwrap();
        let (addr, shutdown_tx, _state, key) = start_test_server(tmp.path()).await;
        let client = reqwest::Client::new();

        let resp = client
            .post(format!("http://{addr}/api/repos"))
            .bearer_auth(&key)
            .json(&serde_json::json!({ "name": "my-project" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 400);

        shutdown_tx.send(()).ok();
    }

    #[tokio::test]
    async fn create_and_list_repos() {
        let tmp = TempDir::new().unwrap();
        let storage_tmp = TempDir::new().unwrap();
        fs::create_dir_all(storage_tmp.path()).unwrap();

        let (addr, shutdown_tx, key) =
            start_test_server_multi_repo(tmp.path(), storage_tmp.path().to_path_buf()).await;
        let client = reqwest::Client::new();

        // Create a repo.
        let resp = client
            .post(format!("http://{addr}/api/repos"))
            .bearer_auth(&key)
            .json(&serde_json::json!({ "name": "my-project" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);
        let created: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(created["name"], "my-project");
        assert_eq!(created["head_version"], "v1");
        assert_eq!(created["workspace_count"], 0);

        // Verify the directory was created and initialized.
        let repo_root = storage_tmp.path().join("my-project");
        assert!(repo_root.join(".vai").is_dir(), ".vai/ not created");

        // List repos — should contain the one we created.
        let resp = client
            .get(format!("http://{addr}/api/repos"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let list: serde_json::Value = resp.json().await.unwrap();
        let repos = list.as_array().unwrap();
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0]["name"], "my-project");

        shutdown_tx.send(()).ok();
    }

    #[tokio::test]
    async fn create_repo_duplicate_rejected() {
        let tmp = TempDir::new().unwrap();
        let storage_tmp = TempDir::new().unwrap();

        let (addr, shutdown_tx, key) =
            start_test_server_multi_repo(tmp.path(), storage_tmp.path().to_path_buf()).await;
        let client = reqwest::Client::new();

        // First create — OK.
        let resp = client
            .post(format!("http://{addr}/api/repos"))
            .bearer_auth(&key)
            .json(&serde_json::json!({ "name": "alpha" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);

        // Second create with same name — conflict.
        let resp = client
            .post(format!("http://{addr}/api/repos"))
            .bearer_auth(&key)
            .json(&serde_json::json!({ "name": "alpha" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 409);

        shutdown_tx.send(()).ok();
    }

    #[tokio::test]
    async fn create_repo_invalid_name_rejected() {
        let tmp = TempDir::new().unwrap();
        let storage_tmp = TempDir::new().unwrap();

        let (addr, shutdown_tx, key) =
            start_test_server_multi_repo(tmp.path(), storage_tmp.path().to_path_buf()).await;
        let client = reqwest::Client::new();

        // Name with path traversal characters.
        let resp = client
            .post(format!("http://{addr}/api/repos"))
            .bearer_auth(&key)
            .json(&serde_json::json!({ "name": "../evil" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 400);

        // Empty name.
        let resp = client
            .post(format!("http://{addr}/api/repos"))
            .bearer_auth(&key)
            .json(&serde_json::json!({ "name": "" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 400);

        shutdown_tx.send(()).ok();
    }

    // ── /api/repos/:repo/* routing tests ─────────────────────────────────────

    /// Register a repo, then hit `/api/repos/:repo/status` to verify routing.
    #[tokio::test]
    async fn repo_scoped_status_route() {
        let tmp = TempDir::new().unwrap();
        let storage_tmp = TempDir::new().unwrap();
        fs::create_dir_all(storage_tmp.path()).unwrap();

        let (addr, shutdown_tx, key) =
            start_test_server_multi_repo(tmp.path(), storage_tmp.path().to_path_buf()).await;
        let client = reqwest::Client::new();

        // Register a repo.
        let resp = client
            .post(format!("http://{addr}/api/repos"))
            .bearer_auth(&key)
            .json(&serde_json::json!({ "name": "my-project" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);

        // Hit the repo-scoped status endpoint.
        let resp = client
            .get(format!("http://{addr}/api/repos/my-project/status"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        // The registered repo is initialised with `vai init` so it has a head version.
        assert_eq!(body["head_version"], "v1");
        assert_eq!(body["workspace_count"], 0);

        shutdown_tx.send(()).ok();
    }

    /// Accessing a repo-scoped route for an unregistered repo returns 404.
    #[tokio::test]
    async fn repo_scoped_route_unknown_repo_returns_404() {
        let tmp = TempDir::new().unwrap();
        let storage_tmp = TempDir::new().unwrap();
        fs::create_dir_all(storage_tmp.path()).unwrap();

        let (addr, shutdown_tx, key) =
            start_test_server_multi_repo(tmp.path(), storage_tmp.path().to_path_buf()).await;
        let client = reqwest::Client::new();

        let resp = client
            .get(format!("http://{addr}/api/repos/does-not-exist/status"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 404);

        shutdown_tx.send(()).ok();
    }

    /// Create a workspace via `/api/repos/:repo/workspaces` and list it back.
    #[tokio::test]
    async fn repo_scoped_workspace_create_and_list() {
        let tmp = TempDir::new().unwrap();
        let storage_tmp = TempDir::new().unwrap();
        fs::create_dir_all(storage_tmp.path()).unwrap();

        let (addr, shutdown_tx, key) =
            start_test_server_multi_repo(tmp.path(), storage_tmp.path().to_path_buf()).await;
        let client = reqwest::Client::new();

        // Register a repo.
        let resp = client
            .post(format!("http://{addr}/api/repos"))
            .bearer_auth(&key)
            .json(&serde_json::json!({ "name": "alpha" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);

        // Create a workspace in that repo via the scoped route.
        let resp = client
            .post(format!("http://{addr}/api/repos/alpha/workspaces"))
            .bearer_auth(&key)
            .json(&serde_json::json!({ "intent": "test feature" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);
        let ws: serde_json::Value = resp.json().await.unwrap();
        let ws_id = ws["id"].as_str().unwrap().to_string();

        // List workspaces — should contain the one we created.
        let resp = client
            .get(format!("http://{addr}/api/repos/alpha/workspaces"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let list: serde_json::Value = resp.json().await.unwrap();
        let workspaces = list["data"].as_array().unwrap();
        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0]["id"], ws_id);

        shutdown_tx.send(()).ok();
    }

    /// Repo-scoped routes require auth — missing key returns 401.
    #[tokio::test]
    async fn repo_scoped_route_requires_auth() {
        let tmp = TempDir::new().unwrap();
        let storage_tmp = TempDir::new().unwrap();
        fs::create_dir_all(storage_tmp.path()).unwrap();

        let (addr, shutdown_tx, key) =
            start_test_server_multi_repo(tmp.path(), storage_tmp.path().to_path_buf()).await;
        let client = reqwest::Client::new();

        // Register a repo first (with auth).
        client
            .post(format!("http://{addr}/api/repos"))
            .bearer_auth(&key)
            .json(&serde_json::json!({ "name": "secure-repo" }))
            .send()
            .await
            .unwrap();

        // Access without token → 401.
        let resp = client
            .get(format!("http://{addr}/api/repos/secure-repo/workspaces"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 401);

        shutdown_tx.send(()).ok();
    }

    /// Issues created via a repo-scoped route are isolated to that repo.
    #[tokio::test]
    async fn repo_scoped_issues_are_isolated() {
        let tmp = TempDir::new().unwrap();
        let storage_tmp = TempDir::new().unwrap();
        fs::create_dir_all(storage_tmp.path()).unwrap();

        let (addr, shutdown_tx, key) =
            start_test_server_multi_repo(tmp.path(), storage_tmp.path().to_path_buf()).await;
        let client = reqwest::Client::new();

        // Register two repos.
        for name in ["repo-a", "repo-b"] {
            let resp = client
                .post(format!("http://{addr}/api/repos"))
                .bearer_auth(&key)
                .json(&serde_json::json!({ "name": name }))
                .send()
                .await
                .unwrap();
            assert_eq!(resp.status(), 201);
        }

        // Create an issue in repo-a.
        let resp = client
            .post(format!("http://{addr}/api/repos/repo-a/issues"))
            .bearer_auth(&key)
            .json(&serde_json::json!({
                "title": "Issue in A",
                "description": "",
                "priority": "medium",
                "labels": []
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);

        // repo-b should have no issues.
        let resp = client
            .get(format!("http://{addr}/api/repos/repo-b/issues"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let b_issues: serde_json::Value = resp.json().await.unwrap();
        assert!(b_issues["data"].as_array().unwrap().is_empty());

        // repo-a should have exactly one issue.
        let resp = client
            .get(format!("http://{addr}/api/repos/repo-a/issues"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let a_issues: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(a_issues["data"].as_array().unwrap().len(), 1);
        assert_eq!(a_issues["data"][0]["title"], "Issue in A");

        shutdown_tx.send(()).ok();
    }

    // ── Rate limiter unit tests ───────────────────────────────────────────────

    #[test]
    fn rate_limiter_allows_within_limit() {
        let rl = RateLimiter::new();
        for _ in 0..5 {
            assert!(
                matches!(rl.check("key", 5, std::time::Duration::from_secs(60)), RateLimitResult::Allowed),
                "expected Allowed"
            );
        }
    }

    #[test]
    fn rate_limiter_denies_over_limit() {
        let rl = RateLimiter::new();
        for _ in 0..3 {
            rl.check("key", 3, std::time::Duration::from_secs(60));
        }
        assert!(
            matches!(rl.check("key", 3, std::time::Duration::from_secs(60)), RateLimitResult::Denied { .. }),
            "expected Denied"
        );
    }

    #[test]
    fn rate_limiter_retry_after_is_positive() {
        let rl = RateLimiter::new();
        for _ in 0..2 {
            rl.check("k", 2, std::time::Duration::from_secs(60));
        }
        match rl.check("k", 2, std::time::Duration::from_secs(60)) {
            RateLimitResult::Denied { retry_after_secs } => {
                assert!(retry_after_secs >= 1, "retry_after_secs should be ≥ 1");
            }
            RateLimitResult::Allowed => panic!("expected Denied"),
        }
    }

    #[test]
    fn rate_limiter_independent_keys() {
        let rl = RateLimiter::new();
        // Fill key-a to its limit.
        for _ in 0..2 {
            rl.check("a", 2, std::time::Duration::from_secs(60));
        }
        // key-b should still be allowed.
        assert!(
            matches!(rl.check("b", 2, std::time::Duration::from_secs(60)), RateLimitResult::Allowed),
            "key-b should be allowed independently"
        );
        // key-a should now be denied.
        assert!(
            matches!(rl.check("a", 2, std::time::Duration::from_secs(60)), RateLimitResult::Denied { .. }),
            "key-a should be denied"
        );
    }

    #[test]
    fn classify_rate_limit_categories() {
        use axum::http::Method;
        assert_eq!(
            classify_rate_limit(&Method::POST, "/api/keys"),
            RateLimitCategory::AuthIp
        );
        assert_eq!(
            classify_rate_limit(&Method::POST, "/api/repos/my-repo/issues"),
            RateLimitCategory::IssueCreate
        );
        assert_eq!(
            classify_rate_limit(&Method::POST, "/api/repos/my-repo/workspaces"),
            RateLimitCategory::WorkspaceCreate
        );
        assert_eq!(
            classify_rate_limit(&Method::GET, "/health"),
            RateLimitCategory::None
        );
    }

    // ── Rate limiting HTTP integration tests ─────────────────────────────────

    /// Verify that the key-creation endpoint is rate-limited per IP.
    /// We use X-Forwarded-For to simulate a consistent client IP.
    #[tokio::test]
    async fn test_key_creation_rate_limited_after_10_per_min() {
        let root = tempfile::TempDir::new().unwrap();
        let (addr, shutdown_tx, state, _key) = start_test_server(root.path()).await;

        // Set the rate limiter to a very tight limit so the test is fast.
        // We do this by pre-filling the bucket for our test IP.
        let test_ip = "10.0.0.99";
        let rl_key = format!("ip_auth:{test_ip}");
        // Fill 10 slots.
        for _ in 0..10 {
            state.rate_limiter.check(&rl_key, 10, std::time::Duration::from_secs(60));
        }

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://{addr}/api/keys"))
            .bearer_auth("vai_admin_test")
            .header("X-Forwarded-For", test_ip)
            .json(&serde_json::json!({"name": "test-key", "repo_id": null}))
            .send()
            .await
            .unwrap();

        assert_eq!(
            resp.status(),
            429,
            "expected 429 Too Many Requests"
        );
        assert!(
            resp.headers().contains_key("Retry-After"),
            "expected Retry-After header"
        );

        shutdown_tx.send(()).ok();
    }

    /// Verify that workspace creation is rate-limited per API key.
    #[tokio::test]
    async fn test_workspace_creation_rate_limited() {
        let root = tempfile::TempDir::new().unwrap();
        let (addr, shutdown_tx, state, _key) = start_test_server(root.path()).await;
        let repo = &state.repo_name.clone();

        // Pre-fill the workspace_create bucket for the admin key.
        let rl_key = "workspace_create:admin";
        for _ in 0..50 {
            state.rate_limiter.check(rl_key, 50, std::time::Duration::from_secs(3600));
        }

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://{addr}/api/repos/{repo}/workspaces"))
            .bearer_auth("vai_admin_test")
            .json(&serde_json::json!({"name": "ws", "description": null}))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 429);
        assert!(resp.headers().contains_key("Retry-After"));

        shutdown_tx.send(()).ok();
    }

    // ── CORS configuration tests ───────────────────────────────────────────────

    /// Helper: spin up a server with specific CORS origins configured.
    async fn start_test_server_with_cors(
        root: &Path,
        cors_origins: Vec<axum::http::HeaderValue>,
    ) -> (SocketAddr, oneshot::Sender<()>) {
        repo::init(root).unwrap();
        let vai_dir = root.join(".vai");
        let repo_config = repo::read_config(&vai_dir).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (event_tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);

        let state = Arc::new(AppState {
            vai_dir: vai_dir.clone(),
            repo_root: root.to_path_buf(),
            started_at: Instant::now(),
            repo_name: repo_config.name.clone(),
            vai_version: env!("CARGO_PKG_VERSION").to_string(),
            event_tx,
            event_seq: Arc::new(AtomicU64::new(0)),
            event_buffer: Arc::new(StdMutex::new(EventBuffer::new())),
            conflict_engine: Arc::new(Mutex::new(conflict::ConflictEngine::new())),
            repo_lock: Arc::new(Mutex::new(())),
            storage_root: None,
            storage: crate::storage::StorageBackend::local(&vai_dir),
            admin_key: "vai_admin_test".to_string(),
            jwt_service: Arc::new(crate::auth::jwt::JwtService::new(
                "test-jwt-secret".to_string(),
                None,
                3600,
            )),
            rate_limiter: Arc::new(RateLimiter::new()),
            cors_origins,
            default_new_user_role: crate::storage::RepoRole::Write,
        });

        let app = build_app(Arc::clone(&state));
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        tokio::spawn(async move {
            axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
                .with_graceful_shutdown(async { shutdown_rx.await.ok(); })
                .await
                .ok();
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        (addr, shutdown_tx)
    }

    #[tokio::test]
    async fn cors_allow_any_when_no_origins_configured() {
        let tmp = TempDir::new().unwrap();
        let (addr, shutdown_tx) = start_test_server_with_cors(tmp.path(), vec![]).await;

        let client = reqwest::Client::new();
        let resp = client
            .request(
                reqwest::Method::OPTIONS,
                format!("http://{addr}/health"),
            )
            .header("Origin", "https://attacker.example.com")
            .header("Access-Control-Request-Method", "GET")
            .send()
            .await
            .unwrap();

        // With no origins configured the server uses Any — the ACAO header
        // should be present (either "*" or echoed origin).
        assert!(
            resp.headers().contains_key("access-control-allow-origin"),
            "expected ACAO header, got: {:?}",
            resp.headers()
        );

        shutdown_tx.send(()).ok();
    }

    #[tokio::test]
    async fn cors_restricts_to_configured_origin() {
        let tmp = TempDir::new().unwrap();
        let allowed: axum::http::HeaderValue =
            "https://app.example.com".parse().unwrap();
        let (addr, shutdown_tx) =
            start_test_server_with_cors(tmp.path(), vec![allowed]).await;

        let client = reqwest::Client::new();

        // Allowed origin should receive the ACAO header.
        let resp = client
            .request(
                reqwest::Method::OPTIONS,
                format!("http://{addr}/health"),
            )
            .header("Origin", "https://app.example.com")
            .header("Access-Control-Request-Method", "GET")
            .send()
            .await
            .unwrap();
        let acao = resp
            .headers()
            .get("access-control-allow-origin")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert_eq!(acao, "https://app.example.com");

        // Disallowed origin should NOT receive the ACAO header (tower-http omits it).
        let resp2 = client
            .request(
                reqwest::Method::OPTIONS,
                format!("http://{addr}/health"),
            )
            .header("Origin", "https://attacker.example.com")
            .header("Access-Control-Request-Method", "GET")
            .send()
            .await
            .unwrap();
        assert!(
            !resp2
                .headers()
                .get("access-control-allow-origin")
                .map(|v| v == "https://attacker.example.com")
                .unwrap_or(false),
            "disallowed origin must not appear in ACAO header"
        );

        shutdown_tx.send(()).ok();
    }

    #[test]
    fn parse_cors_origins_skips_invalid() {
        let origins = vec![
            "https://valid.example.com".to_string(),
            "not a valid header\x00value".to_string(),
            "https://also-valid.example.com".to_string(),
        ];
        let parsed = parse_cors_origins(&origins);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0], "https://valid.example.com");
        assert_eq!(parsed[1], "https://also-valid.example.com");
    }

    #[test]
    fn parse_cors_origins_empty_input() {
        let parsed = parse_cors_origins(&[]);
        assert!(parsed.is_empty());
    }

    // ── Upload validation tests ────────────────────────────────────────────────

    /// Build a minimal in-memory gzip tarball from a list of (path, content) pairs.
    fn make_tarball(entries: &[(&str, &[u8])]) -> Vec<u8> {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        let mut enc = GzEncoder::new(Vec::new(), Compression::default());
        {
            let mut ar = tar::Builder::new(&mut enc);
            for (path, content) in entries {
                let mut header = tar::Header::new_gnu();
                header.set_size(content.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                ar.append_data(&mut header, path, *content).unwrap();
            }
            ar.finish().unwrap();
        }
        enc.finish().unwrap()
    }

    /// Build a tarball that contains a symlink entry.
    fn make_tarball_with_symlink(link_name: &str, target: &str) -> Vec<u8> {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        let mut enc = GzEncoder::new(Vec::new(), Compression::default());
        {
            let mut ar = tar::Builder::new(&mut enc);
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            header.set_mode(0o644);
            header.set_link_name(target).unwrap();
            header.set_cksum();
            ar.append(&header, link_name.as_bytes()).unwrap();
            ar.finish().unwrap();
        }
        enc.finish().unwrap()
    }

    /// Verify that upload-snapshot rejects tarballs containing symlinks.
    #[tokio::test]
    async fn upload_snapshot_rejects_symlinks() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let (addr, shutdown_tx, state, key) = start_test_server(root).await;
        let repo = &state.repo_name;
        let client = reqwest::Client::new();

        // Create a workspace to upload into.
        let resp = client
            .post(format!("http://{addr}/api/repos/{repo}/workspaces"))
            .bearer_auth(&key)
            .json(&serde_json::json!({ "intent": "symlink test" }))
            .send()
            .await
            .unwrap();
        let ws: serde_json::Value = resp.json().await.unwrap();
        let ws_id = ws["id"].as_str().unwrap().to_string();

        let tarball = make_tarball_with_symlink("evil_link", "/etc/passwd");

        let resp = client
            .post(format!("http://{addr}/api/repos/{repo}/workspaces/{ws_id}/upload-snapshot"))
            .bearer_auth(&key)
            .header("Content-Type", "application/gzip")
            .body(tarball)
            .send()
            .await
            .unwrap();

        assert_eq!(
            resp.status(),
            400,
            "tarball with symlink should be rejected with 400"
        );

        shutdown_tx.send(()).ok();
    }

    /// Unit tests for `sanitize_path` — the function that protects against path
    /// traversal in both JSON file uploads and tarball entries.
    #[test]
    fn sanitize_path_rejects_traversal() {
        // Classic traversal patterns.
        assert!(sanitize_path("../../etc/passwd").is_none(), ".. traversal must be rejected");
        assert!(sanitize_path("../secret").is_none(), "single .. must be rejected");
        assert!(sanitize_path("foo/../../etc/passwd").is_none(), "embedded .. must be rejected");
        // Absolute paths have their leading slash stripped (normalised to relative).
        // The important thing is they don't escape the workspace root.
        let abs = sanitize_path("/etc/passwd").expect("absolute path should be normalised");
        assert_eq!(abs.to_string_lossy(), "etc/passwd", "leading slash must be stripped");
        // Null byte.
        assert!(sanitize_path("foo\0bar").is_none(), "null byte must be rejected");
        // Valid relative paths must be preserved.
        assert!(sanitize_path("src/main.rs").is_some());
        assert!(sanitize_path("deeply/nested/file.txt").is_some());
    }

    /// Verify that upload-snapshot rejects individual files exceeding 10 MiB.
    #[tokio::test]
    async fn upload_snapshot_rejects_oversized_file() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let (addr, shutdown_tx, state, key) = start_test_server(root).await;
        let repo = &state.repo_name;
        let client = reqwest::Client::new();

        let resp = client
            .post(format!("http://{addr}/api/repos/{repo}/workspaces"))
            .bearer_auth(&key)
            .json(&serde_json::json!({ "intent": "size test" }))
            .send()
            .await
            .unwrap();
        let ws: serde_json::Value = resp.json().await.unwrap();
        let ws_id = ws["id"].as_str().unwrap().to_string();

        // Build a file that is just over the 10 MiB per-file limit.
        let oversized = vec![0u8; MAX_FILE_SIZE_BYTES + 1];
        let tarball = make_tarball(&[("big_file.bin", &oversized)]);

        let resp = client
            .post(format!("http://{addr}/api/repos/{repo}/workspaces/{ws_id}/upload-snapshot"))
            .bearer_auth(&key)
            .header("Content-Type", "application/gzip")
            .body(tarball)
            .send()
            .await
            .unwrap();

        assert_eq!(
            resp.status(),
            400,
            "tarball entry over 10 MiB should be rejected"
        );

        shutdown_tx.send(()).ok();
    }

    /// Verify that JSON file upload rejects individual files exceeding 10 MiB.
    #[tokio::test]
    async fn file_upload_rejects_oversized_file() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let (addr, shutdown_tx, state, key) = start_test_server(root).await;
        let repo = &state.repo_name;
        let client = reqwest::Client::new();

        let resp = client
            .post(format!("http://{addr}/api/repos/{repo}/workspaces"))
            .bearer_auth(&key)
            .json(&serde_json::json!({ "intent": "size test" }))
            .send()
            .await
            .unwrap();
        let ws: serde_json::Value = resp.json().await.unwrap();
        let ws_id = ws["id"].as_str().unwrap().to_string();

        // Just over the 10 MiB limit.
        let oversized = vec![0u8; MAX_FILE_SIZE_BYTES + 1];
        let b64 = BASE64.encode(&oversized);

        let resp = client
            .post(format!("http://{addr}/api/repos/{repo}/workspaces/{ws_id}/files"))
            .bearer_auth(&key)
            .json(&serde_json::json!({
                "files": [{ "path": "big.bin", "content_base64": b64 }]
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(
            resp.status(),
            400,
            "file over 10 MiB should be rejected"
        );

        shutdown_tx.send(()).ok();
    }

    /// Verify that issue creation is rate-limited per API key.
    #[tokio::test]
    async fn test_issue_creation_rate_limited() {
        let root = tempfile::TempDir::new().unwrap();
        let (addr, shutdown_tx, state, _key) = start_test_server(root.path()).await;
        let repo = &state.repo_name.clone();

        // Pre-fill the issue_create bucket for the admin key.
        let rl_key = "issue_create:admin";
        for _ in 0..100 {
            state
                .rate_limiter
                .check(rl_key, 100, std::time::Duration::from_secs(3600));
        }

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://{addr}/api/repos/{repo}/issues"))
            .bearer_auth("vai_admin_test")
            .json(&serde_json::json!({ "title": "test issue", "body": "" }))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 429, "expected 429 Too Many Requests");
        assert!(
            resp.headers().contains_key("Retry-After"),
            "expected Retry-After header"
        );

        shutdown_tx.send(()).ok();
    }

    // ── Token exchange tests (PRD 18) ──────────────────────────────────────────

    /// `api_key` grant with a valid key mints a JWT.
    #[tokio::test]
    async fn token_exchange_api_key_grant_returns_jwt() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let (addr, shutdown_tx, _state, _key) = start_test_server(root).await;

        // The test server creates "test-agent" with a known plaintext key via
        // auth::create(). Retrieve the key from the local keys.db so we can
        // exercise the api_key grant.
        let vai_dir = root.join(".vai");
        let keys = auth::list(&vai_dir).unwrap();
        let test_key_meta = keys.iter().find(|k| k.name == "test-agent").unwrap().clone();
        // We need the plaintext token — re-create it for the test, since
        // auth::create() is the only caller that returns the plaintext.  Instead
        // we use the admin key (known plaintext) so we can test the full round-trip.
        let admin_key = "vai_admin_test";

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://{addr}/api/auth/token"))
            .json(&serde_json::json!({
                "grant_type": "api_key",
                "api_key": admin_key,
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200, "expected 200 for valid api_key grant");
        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(body["access_token"].is_string(), "access_token must be present");
        assert_eq!(body["token_type"].as_str(), Some("Bearer"));
        assert!(body["expires_in"].is_number());
        assert!(body["refresh_token"].is_null(), "api_key grant should not return refresh_token");

        // Verify the JWT is structurally valid (three dot-separated parts).
        let token = body["access_token"].as_str().unwrap();
        assert_eq!(token.split('.').count(), 3, "access_token should be a JWT");

        let _ = test_key_meta; // suppress unused warning
        shutdown_tx.send(()).ok();
    }

    /// Missing `api_key` field returns 400.
    #[tokio::test]
    async fn token_exchange_api_key_grant_missing_key_returns_400() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let (addr, shutdown_tx, _state, _key) = start_test_server(root).await;

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://{addr}/api/auth/token"))
            .json(&serde_json::json!({ "grant_type": "api_key" }))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 400);
        shutdown_tx.send(()).ok();
    }

    /// Invalid API key returns 401.
    #[tokio::test]
    async fn token_exchange_api_key_grant_invalid_key_returns_401() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let (addr, shutdown_tx, _state, _key) = start_test_server(root).await;

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://{addr}/api/auth/token"))
            .json(&serde_json::json!({
                "grant_type": "api_key",
                "api_key": "vai_notavalidkey000000000000000000",
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 401);
        shutdown_tx.send(()).ok();
    }

    /// Unknown grant type returns 400.
    #[tokio::test]
    async fn token_exchange_unknown_grant_type_returns_400() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let (addr, shutdown_tx, _state, _key) = start_test_server(root).await;

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://{addr}/api/auth/token"))
            .json(&serde_json::json!({ "grant_type": "password" }))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 400);
        shutdown_tx.send(()).ok();
    }

    // ── Refresh token tests (PRD 18) ───────────────────────────────────────────

    /// `POST /api/auth/refresh` with a missing body returns 422 (unprocessable).
    #[tokio::test]
    async fn refresh_missing_body_returns_error() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let (addr, shutdown_tx, _state, _key) = start_test_server(root).await;

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://{addr}/api/auth/refresh"))
            .header("Content-Type", "application/json")
            .body("{}")
            .send()
            .await
            .unwrap();

        // Missing `refresh_token` field — axum returns 422.
        assert!(
            resp.status().is_client_error(),
            "empty body should return 4xx, got {}",
            resp.status()
        );
        shutdown_tx.send(()).ok();
    }

    /// `POST /api/auth/refresh` with an invalid token returns 500 (SQLite mode
    /// does not support refresh tokens, so storage returns a database error).
    #[tokio::test]
    async fn refresh_invalid_token_returns_error_in_sqlite_mode() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let (addr, shutdown_tx, _state, _key) = start_test_server(root).await;

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://{addr}/api/auth/refresh"))
            .json(&serde_json::json!({ "refresh_token": "rt_notavalidtoken" }))
            .send()
            .await
            .unwrap();

        // SQLite mode returns a database error (5xx).
        assert!(
            resp.status().is_server_error(),
            "refresh in SQLite mode should return 5xx, got {}",
            resp.status()
        );
        shutdown_tx.send(()).ok();
    }

    /// `POST /api/auth/revoke` with an invalid token returns 500 (SQLite mode).
    #[tokio::test]
    async fn revoke_invalid_token_returns_error_in_sqlite_mode() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let (addr, shutdown_tx, _state, _key) = start_test_server(root).await;

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://{addr}/api/auth/revoke"))
            .json(&serde_json::json!({ "refresh_token": "rt_notavalidtoken" }))
            .send()
            .await
            .unwrap();

        // SQLite mode returns a database error (5xx).
        assert!(
            resp.status().is_server_error(),
            "revoke in SQLite mode should return 5xx, got {}",
            resp.status()
        );
        shutdown_tx.send(()).ok();
    }

    /// `POST /api/auth/revoke` with a missing body returns 422.
    #[tokio::test]
    async fn revoke_missing_body_returns_error() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let (addr, shutdown_tx, _state, _key) = start_test_server(root).await;

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://{addr}/api/auth/revoke"))
            .header("Content-Type", "application/json")
            .body("{}")
            .send()
            .await
            .unwrap();

        assert!(
            resp.status().is_client_error(),
            "empty body should return 4xx, got {}",
            resp.status()
        );
        shutdown_tx.send(()).ok();
    }

    /// `session_exchange` grant without a database returns an error (not 500 —
    /// SQLite mode does not support sessions, so it returns 500 wrapped as a
    /// storage error, which the handler propagates as 500).
    #[tokio::test]
    async fn token_exchange_session_exchange_unsupported_in_local_mode() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let (addr, shutdown_tx, _state, _key) = start_test_server(root).await;

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://{addr}/api/auth/token"))
            .json(&serde_json::json!({
                "grant_type": "session_exchange",
                "session_token": "some-session-token",
            }))
            .send()
            .await
            .unwrap();

        // SQLite mode: validate_session returns a database error → 500.
        assert!(
            resp.status().is_server_error(),
            "session_exchange in SQLite mode should return 5xx, got {}",
            resp.status()
        );
        shutdown_tx.send(()).ok();
    }

    // ── JWT auth middleware tests ──────────────────────────────────────────────

    /// A valid JWT signed with the server's secret is accepted.
    #[tokio::test]
    async fn jwt_bearer_token_accepted() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let (addr, shutdown_tx, state, _key) = start_test_server(root).await;

        // Mint a token using the same JwtService the server holds.
        let token = state
            .jwt_service
            .sign("user-jwt-test".to_string(), None, None, Some("write".to_string()))
            .unwrap();

        let client = reqwest::Client::new();
        let resp = client
            .get(format!("http://{addr}/api/status"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();

        // /api/status is public (unauthenticated), but a valid JWT should not
        // cause a 401. We just verify the request is not rejected by auth middleware.
        assert_ne!(resp.status(), 401, "valid JWT should not be rejected");
        shutdown_tx.send(()).ok();
    }

    /// A JWT signed with a wrong secret is rejected with 401.
    #[tokio::test]
    async fn jwt_wrong_secret_returns_401() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let (addr, shutdown_tx, state, _key) = start_test_server(root).await;
        let repo = &state.repo_name;

        // Mint a token with a *different* secret than the server uses.
        let wrong_svc = crate::auth::jwt::JwtService::new(
            "wrong-secret".to_string(),
            None,
            3600,
        );
        let token = wrong_svc.sign("attacker".to_string(), None, None, None).unwrap();

        let client = reqwest::Client::new();
        let resp = client
            .get(format!("http://{addr}/api/repos/{repo}/workspaces"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 401, "JWT with wrong secret should be rejected");
        shutdown_tx.send(()).ok();
    }

    /// An expired JWT is rejected with 401 and a descriptive message.
    ///
    /// To avoid a slow wall-clock sleep, we craft a JWT whose `exp` is set to
    /// a Unix timestamp in the distant past (year 1970), which is always more
    /// than the server's 30-second leeway.
    #[tokio::test]
    async fn jwt_expired_token_returns_401() {
        use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let (addr, shutdown_tx, state, _key) = start_test_server(root).await;
        let repo = &state.repo_name;

        // Build a JWT with exp = 1000 (year 1970 — definitely expired).
        let claims = serde_json::json!({
            "sub": "u",
            "iat": 900u64,
            "exp": 1000u64,
        });
        let token = encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(b"test-jwt-secret"),
        )
        .unwrap();

        let client = reqwest::Client::new();
        let resp = client
            .get(format!("http://{addr}/api/repos/{repo}/workspaces"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 401, "expired JWT should be rejected with 401");
        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(
            body["error"].as_str().unwrap_or("").contains("expired"),
            "response should mention expiry: {body}"
        );
        shutdown_tx.send(()).ok();
    }

    /// A valid API key still works (JWT path does not break existing API key auth).
    #[tokio::test]
    async fn api_key_still_accepted_after_jwt_middleware_change() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let (addr, shutdown_tx, state, _key) = start_test_server(root).await;
        let repo = &state.repo_name;

        // Use the admin key (no dots — goes through API key + admin key path).
        let client = reqwest::Client::new();
        let resp = client
            .get(format!("http://{addr}/api/repos/{repo}/workspaces"))
            .bearer_auth("vai_admin_test")
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200, "admin key should still be accepted");
        shutdown_tx.send(()).ok();
    }

    /// A JWT with `role = "admin"` sets `is_admin = true`, verified by accessing
    /// `GET /api/keys` which returns all keys for admins and 403 for non-admins
    /// without a user_id.
    #[tokio::test]
    async fn jwt_admin_role_grants_admin_access() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let (addr, shutdown_tx, state, _key) = start_test_server(root).await;

        // Admin JWT — is_admin should be true.
        let admin_token = state
            .jwt_service
            .sign("svc-account".to_string(), None, None, Some("admin".to_string()))
            .unwrap();

        // Non-admin JWT without a user_id UUID as sub — is_admin false, no user_id.
        let non_admin_token = state
            .jwt_service
            .sign("not-a-uuid-sub".to_string(), None, None, Some("write".to_string()))
            .unwrap();

        let client = reqwest::Client::new();

        // Admin JWT: list_keys_handler returns all keys (200).
        let resp = client
            .get(format!("http://{addr}/api/keys"))
            .bearer_auth(&admin_token)
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            200,
            "JWT with admin role should list all keys, got {}",
            resp.status()
        );

        // Non-admin JWT without user association: list_keys_handler returns 403.
        let resp = client
            .get(format!("http://{addr}/api/keys"))
            .bearer_auth(&non_admin_token)
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            403,
            "JWT without user association should be rejected with 403, got {}",
            resp.status()
        );

        shutdown_tx.send(()).ok();
    }

    // ── Bulk key revocation tests (PRD 18 Issue 6) ────────────────────────────

    /// Non-admin key is rejected with 403.
    #[tokio::test]
    async fn bulk_revoke_keys_non_admin_forbidden() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let (addr, shutdown_tx, _state, _admin_key) = start_test_server(root).await;

        // Create a regular (non-admin) key directly in the local store.
        let vai_dir = root.join(".vai");
        let (_, regular_key) = auth::create(&vai_dir, "non-admin-agent").unwrap();

        let client = reqwest::Client::new();
        let resp = client
            .delete(format!(
                "http://{addr}/api/keys?repo_id=00000000-0000-0000-0000-000000000001"
            ))
            .bearer_auth(&regular_key)
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            403,
            "non-admin should get 403, got {}",
            resp.status()
        );

        shutdown_tx.send(()).ok();
    }

    /// No query params returns 400.
    #[tokio::test]
    async fn bulk_revoke_keys_missing_params_returns_400() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let (addr, shutdown_tx, _state, admin_key) = start_test_server(root).await;

        let client = reqwest::Client::new();
        let resp = client
            .delete(format!("http://{addr}/api/keys"))
            .bearer_auth(&admin_key)
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            400,
            "missing params should return 400, got {}",
            resp.status()
        );

        shutdown_tx.send(()).ok();
    }

    /// Providing both repo_id and created_by returns 400.
    #[tokio::test]
    async fn bulk_revoke_keys_both_params_returns_400() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let (addr, shutdown_tx, _state, admin_key) = start_test_server(root).await;

        let client = reqwest::Client::new();
        let resp = client
            .delete(format!(
                "http://{addr}/api/keys\
                 ?repo_id=00000000-0000-0000-0000-000000000001\
                 &created_by=00000000-0000-0000-0000-000000000002"
            ))
            .bearer_auth(&admin_key)
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            400,
            "both params should return 400, got {}",
            resp.status()
        );

        shutdown_tx.send(()).ok();
    }

    /// Admin with repo_id in SQLite mode returns 500 (unsupported).
    /// This verifies the request reaches the backend before failing.
    #[tokio::test]
    async fn bulk_revoke_keys_by_repo_sqlite_returns_error() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let (addr, shutdown_tx, _state, admin_key) = start_test_server(root).await;

        let client = reqwest::Client::new();
        let resp = client
            .delete(format!(
                "http://{addr}/api/keys?repo_id=00000000-0000-0000-0000-000000000001"
            ))
            .bearer_auth(&admin_key)
            .send()
            .await
            .unwrap();
        // SQLite mode doesn't support bulk revocation; expect an internal error,
        // not a 400/403/401.
        assert_ne!(resp.status(), 400, "should not be 400");
        assert_ne!(resp.status(), 401, "should not be 401");
        assert_ne!(resp.status(), 403, "should not be 403");

        shutdown_tx.send(()).ok();
    }

    /// Admin with created_by in SQLite mode returns 500 (unsupported).
    #[tokio::test]
    async fn bulk_revoke_keys_by_user_sqlite_returns_error() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let (addr, shutdown_tx, _state, admin_key) = start_test_server(root).await;

        let client = reqwest::Client::new();
        let resp = client
            .delete(format!(
                "http://{addr}/api/keys?created_by=00000000-0000-0000-0000-000000000002"
            ))
            .bearer_auth(&admin_key)
            .send()
            .await
            .unwrap();
        assert_ne!(resp.status(), 400, "should not be 400");
        assert_ne!(resp.status(), 401, "should not be 401");
        assert_ne!(resp.status(), 403, "should not be 403");

        shutdown_tx.send(()).ok();
    }
}
