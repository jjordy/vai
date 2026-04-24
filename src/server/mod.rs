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

use axum::extract::{DefaultBodyLimit, Extension, Path as AxumPath, Query as AxumQuery, Request, State};
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

pub mod compute;
pub mod pagination;
pub use pagination::{PaginatedResponse, PaginationMeta, PaginationParams};

use workspace::{FileDownloadResponse, UploadFilesRequest, UploadFilesResponse};

mod admin;
mod auth;
mod escalation;
mod graph;
mod issue;
mod me;
mod version;
mod watcher;
mod worker;
mod worker_registry;
mod work_queue;
mod workspace;
mod ws;
#[cfg(feature = "postgres")]
pub(crate) mod secrets;
#[cfg(feature = "postgres")]
mod agent_secrets;

use crate::auth as crate_auth;
use crate::conflict;
use crate::storage::ListQuery;
use crate::event_log::EventKind;
use crate::merge;
use crate::repo;
use crate::version as vai_version;
use crate::workspace as vai_workspace;

// ── Error type ────────────────────────────────────────────────────────────────

/// Errors that can occur during server operations.
#[derive(Debug, Error)]
pub enum ServerError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Repository error: {0}")]
    Repo(#[from] repo::RepoError),

    #[error("Workspace error: {0}")]
    Workspace(#[from] vai_workspace::WorkspaceError),

    #[error("Auth error: {0}")]
    Auth(#[from] crate_auth::AuthError),

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

/// Maximum length for workspace intent (characters).
pub(super) const MAX_INTENT_LEN: usize = 1000;
/// Maximum length for a file path (characters).
pub(super) const MAX_PATH_LEN: usize = 1000;
/// Maximum number of files per upload request.
pub(super) const MAX_FILES_PER_REQUEST: usize = 100;
/// Default JSON body size limit (10 MiB) — applies to all endpoints.
const DEFAULT_BODY_LIMIT: usize = 10 * 1024 * 1024;
/// Body size limit for file-upload endpoints (50 MiB).
pub(super) const UPLOAD_BODY_LIMIT: usize = 50 * 1024 * 1024;
/// Body size limit for the migration endpoint (50 MiB).
const MIGRATE_BODY_LIMIT: usize = 50 * 1024 * 1024;
/// Body size limit for tarball snapshot uploads (100 MiB).
pub(super) const SNAPSHOT_BODY_LIMIT: usize = 100 * 1024 * 1024;
/// Maximum allowed size for a single uploaded file (10 MiB).
pub(super) const MAX_FILE_SIZE_BYTES: usize = 10 * 1024 * 1024;

// ── Input validation helpers ───────────────────────────────────────────────────

/// Returns `Err(ApiError::bad_request(...))` when `value` exceeds `max` bytes.
pub(super) fn validate_str_len(value: &str, max: usize, field: &str) -> Result<(), ApiError> {
    if value.len() > max {
        return Err(ApiError::bad_request(format!(
            "`{field}` exceeds maximum length of {max} bytes (got {} bytes)",
            value.len()
        )));
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
    /// Human-readable repository name. In local mode, from `.vai/config.toml`; in server mode, from the `repos` table.
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
    /// Maximum number of repos a non-admin user may own (have `admin` collaborator role on).
    ///
    /// Set via `VAI_MAX_REPOS_PER_USER` env var. Defaults to 100.
    /// Admin keys bypass this quota entirely.
    max_repos_per_user: u64,
    /// In-memory sliding-window rate limiter shared across all requests.
    rate_limiter: Arc<RateLimiter>,
    /// Parsed CORS allowed origins.
    ///
    /// Empty means "allow any origin" (`*`).  Non-empty restricts to the listed
    /// origins.  Set from `ServerConfig::cors_origins` or `VAI_CORS_ORIGINS`.
    cors_origins: Vec<axum::http::HeaderValue>,
    /// Cloud compute provider for spawning per-issue agent workers (PRD 28).
    ///
    /// `None` means cloud agent spawning is disabled (local/test mode or no
    /// `VAI_COMPUTE_FLY_TOKEN` set).  When `Some`, each new issue creation
    /// triggers [`worker_registry::spawn_if_capacity`] if the repo has
    /// `cloud_agent_enabled = true`.
    pub(crate) compute: Option<std::sync::Arc<dyn compute::ComputeProvider>>,
    /// Public URL of this server, injected as `VAI_SERVER_URL` into spawned workers.
    ///
    /// Read from the `VAI_SERVER_URL` environment variable at startup.
    /// Empty string means "not configured" — spawn will be skipped.
    pub(crate) worker_server_url: String,
    /// Anthropic API key injected into spawned workers as `ANTHROPIC_API_KEY`.
    ///
    /// Read from `ANTHROPIC_API_KEY` at startup.
    /// Empty string means "not configured" — spawn will be skipped.
    pub(crate) worker_anthropic_key: String,
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
    /// Repository the JWT is scoped to, if the token carried a `repo_id` claim.
    ///
    /// Used for user-less service tokens (e.g. cloud-worker JWTs minted by
    /// `spawn_if_capacity`) that grant per-repo access without being attached
    /// to a user in the orgs table. `require_repo_permission` accepts such
    /// tokens when `role_override == Some("worker")` and the scope matches
    /// the target repo.
    pub jwt_repo_scope: Option<uuid::Uuid>,
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
    /// In Postgres (server) mode this is populated by `repo_resolve_middleware`
    /// from the `repos` table — no filesystem read required.  In SQLite (local)
    /// mode the value is read from `.vai/config.toml` and is otherwise ignored
    /// by all trait implementations.
    repo_id: uuid::Uuid,
    /// Human-readable repository name (the `:repo` path segment).
    repo_name: String,
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
///
/// **Only called in local (SQLite) mode.** In server (Postgres) mode, `repo_id`
/// is resolved from the `repos` table by `repo_resolve_middleware`.
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
            repo_name: state.repo_name.clone(),
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
                let jwt_repo_scope = claims
                    .repo_id
                    .as_deref()
                    .and_then(|s| uuid::Uuid::parse_str(s).ok());
                request.extensions_mut().insert(AgentIdentity {
                    key_id: format!("jwt:{}", claims.sub),
                    name,
                    is_admin,
                    user_id,
                    role_override: claims.role,
                    jwt_repo_scope,
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
                jwt_repo_scope: None,
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
            jwt_repo_scope: None,
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

    let (vai_dir, repo_root, repo_id) = if let Some(storage_root) = state.storage_root.as_ref() {
        // Multi-repo mode.
        //
        // In server (Postgres) mode, query the `repos` table directly — no filesystem
        // lookup needed.  In local (SQLite) mode, `get_repo_by_name` returns `None`
        // and we fall back to `registry.json` as before.
        match state.storage.get_repo_by_name(&repo_name).await {
            Err(e) => {
                return ApiError::internal(format!("failed to query repo: {e}"))
                    .into_response();
            }
            Ok(Some((id, _))) => {
                // Server (Postgres) mode: repo found — no filesystem path required.
                let repo_root = storage_root.join(&repo_name);
                let vai_dir = repo_root.join(".vai");
                (vai_dir, repo_root, id)
            }
            Ok(None) => {
                // Local (SQLite) mode: resolve via registry.json.
                let registry = match RepoRegistry::load(storage_root) {
                    Ok(r) => r,
                    Err(e) => {
                        return ApiError::internal(format!(
                            "failed to load repo registry: {e}"
                        ))
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
                let vai_dir = entry.path.join(".vai");
                let repo_id = repo_id_from_vai_dir(&vai_dir);
                (vai_dir, entry.path.clone(), repo_id)
            }
        }
    } else {
        // Single-repo mode: only the server's own repository is available.
        if repo_name != state.repo_name {
            return ApiError::not_found(format!(
                "repository `{repo_name}` is not registered on this server"
            ))
            .into_response();
        }
        let vai_dir = state.vai_dir.clone();
        let repo_id = repo_id_from_vai_dir(&vai_dir);
        (vai_dir, state.repo_root.clone(), repo_id)
    };

    let storage = repo_storage(&state.storage, &vai_dir);
    let ctx = RepoCtx {
        vai_dir,
        repo_root,
        repo_id,
        repo_name,
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
pub(crate) struct ApiError {
    status: StatusCode,
    message: String,
    /// Optional override body. When set, this JSON value is serialised as the
    /// response body instead of the default `{"error": "..."}` shape.
    custom_json: Option<serde_json::Value>,
}

impl ApiError {
    fn not_found(msg: impl Into<String>) -> Self {
        Self { status: StatusCode::NOT_FOUND, message: msg.into(), custom_json: None }
    }

    fn conflict(msg: impl Into<String>) -> Self {
        Self { status: StatusCode::CONFLICT, message: msg.into(), custom_json: None }
    }

    fn internal(msg: impl Into<String>) -> Self {
        // Log full details server-side; never return them to the client.
        tracing::error!("internal server error: {}", msg.into());
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: "Internal server error".to_string(),
            custom_json: None,
        }
    }

    fn unauthorized(msg: impl Into<String>) -> Self {
        Self { status: StatusCode::UNAUTHORIZED, message: msg.into(), custom_json: None }
    }

    fn bad_request(msg: impl Into<String>) -> Self {
        Self { status: StatusCode::BAD_REQUEST, message: msg.into(), custom_json: None }
    }

    fn rate_limited(msg: impl Into<String>) -> Self {
        Self { status: StatusCode::TOO_MANY_REQUESTS, message: msg.into(), custom_json: None }
    }

    /// Returns a 409 with the structured `workspace_empty` body indicating the
    /// submitted workspace had no file changes.  The client should close the
    /// issue permanently rather than resetting and re-claiming it.
    fn workspace_empty() -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: "workspace has no file changes to submit".to_string(),
            custom_json: Some(serde_json::json!({
                "error": "workspace_empty",
                "message": "workspace has no file changes to submit",
                "hint": "if the issue is already resolved, call POST /api/repos/:r/issues/:id/close instead"
            })),
        }
    }

    fn forbidden(msg: impl Into<String>) -> Self {
        Self { status: StatusCode::FORBIDDEN, message: msg.into(), custom_json: None }
    }

    fn payload_too_large(msg: impl Into<String>) -> Self {
        Self { status: StatusCode::PAYLOAD_TOO_LARGE, message: msg.into(), custom_json: None }
    }

    /// Returns a 403 with the structured quota-exceeded body
    /// `{"error":"repo quota exceeded","limit":<n>,"current":<n>}`.
    fn quota_exceeded(limit: u64, current: u64) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            message: "repo quota exceeded".to_string(),
            custom_json: Some(serde_json::json!({
                "error": "repo quota exceeded",
                "limit": limit,
                "current": current,
            })),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut resp = if let Some(json) = self.custom_json {
            (self.status, Json(json)).into_response()
        } else {
            (self.status, Json(ErrorBody { error: self.message })).into_response()
        };
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

impl From<vai_workspace::WorkspaceError> for ApiError {
    fn from(e: vai_workspace::WorkspaceError) -> Self {
        match &e {
            vai_workspace::WorkspaceError::NotFound(_) => ApiError::not_found(e.to_string()),
            vai_workspace::WorkspaceError::NoActiveWorkspace => ApiError::not_found(e.to_string()),
            _ => ApiError::internal(e.to_string()),
        }
    }
}

impl From<merge::MergeError> for ApiError {
    fn from(e: merge::MergeError) -> Self {
        match &e {
            merge::MergeError::SemanticConflicts { .. } => ApiError::conflict(e.to_string()),
            merge::MergeError::EmptyWorkspace => ApiError::workspace_empty(),
            merge::MergeError::Workspace(vai_workspace::WorkspaceError::NotFound(_)) => {
                ApiError::not_found(e.to_string())
            }
            merge::MergeError::Workspace(vai_workspace::WorkspaceError::NoActiveWorkspace) => {
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

    // Cloud-worker JWT path: tokens minted by `spawn_if_capacity` have
    // role_override="worker" and a repo_id claim, but no user association.
    // They grant Write access when the scope matches the target repo.
    if identity.role_override.as_deref() == Some("worker") {
        if identity.jwt_repo_scope == Some(*repo_id) {
            // Worker role is capped at Write; reject if caller needs
            // Admin / Owner.
            if matches!(required, RepoRole::Admin | RepoRole::Owner) {
                tracing::warn!(
                    event = "permission.denied",
                    actor = %identity.name,
                    repo = %repo_id,
                    required = %required.as_str(),
                    reason = "worker_role_insufficient",
                    "permission denied: worker role capped at Write"
                );
                return Err(ApiError::forbidden("worker role cannot perform this action"));
            }
            return Ok(RepoRole::Write);
        }
        tracing::warn!(
            event = "permission.denied",
            actor = %identity.name,
            repo = %repo_id,
            required = %required.as_str(),
            reason = "worker_scope_mismatch",
            "permission denied: worker JWT scoped to a different repo"
        );
        return Err(ApiError::forbidden(
            "worker token is not scoped to this repo",
        ));
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
            status: Some(vec![IssueStatus::Open]),
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

    // In server (Postgres) mode, look up the repo name from the `repos`
    // table — no filesystem reads.  In local (SQLite) mode, read from
    // `.vai/config.toml` or fall back to `state.repo_name`.
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
                .unwrap_or_else(|| state.repo_name.clone())
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

/// Validates and normalises a client-supplied file path.
///
/// Returns `None` if the path is absolute or contains any parent-directory
/// (`..`) components, preventing directory-traversal attacks.
pub(super) fn sanitize_path(raw: &str) -> Option<std::path::PathBuf> {
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
pub(super) fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(data);
    format!("{:x}", h.finalize())
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
        (status = 200, description = "Tar-gzip archive of all repo files. Response headers include `X-Vai-Head` (version) and `X-Vai-Expected-Files` (total file count for integrity verification).", content_type = "application/gzip"),
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

    let file_count = sorted_paths.len();
    let response = axum::response::Response::builder()
        .status(StatusCode::OK)
        .header(axum::http::header::CONTENT_TYPE, "application/gzip")
        .header(
            axum::http::header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{filename}\""),
        )
        .header("X-Vai-Head", head_version.clone())
        .header("X-Vai-Expected-Files", file_count.to_string())
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

    // Read head from storage trait — no filesystem read needed in server mode.
    let head_version = ctx.storage.versions()
        .read_head(&ctx.repo_id)
        .await
        .ok()
        .flatten();

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
        workspace::create_workspace_handler,
        workspace::list_workspaces_handler,
        workspace::get_workspace_handler,
        workspace::submit_workspace_handler,
        workspace::discard_workspace_handler,
        workspace::upload_workspace_files_handler,
        workspace::upload_snapshot_handler,
        workspace::get_workspace_file_handler,
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
        issue::create_issue_handler,
        issue::list_issues_handler,
        issue::get_issue_handler,
        issue::update_issue_handler,
        issue::close_issue_handler,
        issue::create_issue_comment_handler,
        issue::list_issue_comments_handler,
        issue::update_issue_comment_handler,
        issue::delete_issue_comment_handler,
        issue::create_issue_link_handler,
        issue::list_issue_links_handler,
        issue::delete_issue_link_handler,
        issue::upload_attachment_handler,
        issue::list_attachments_handler,
        issue::download_attachment_handler,
        issue::delete_attachment_handler,
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
        worker::get_worker_handler,
        worker::get_logs_handler,
        worker::heartbeat_handler,
        worker::append_logs_handler,
        worker::mark_done_handler,
        worker::destroy_worker_handler,
        admin::create_repo_handler,
        admin::list_repos_handler,
        admin::get_repo_handler,
        admin::update_repo_handler,
        admin::create_org_handler,
        admin::list_orgs_handler,
        admin::get_org_handler,
        admin::delete_org_handler,
        admin::create_user_handler,
        admin::get_user_handler,
        admin::get_me_handler,
        me::get_onboarding_handler,
        me::complete_onboarding_handler,
        admin::add_org_member_handler,
        admin::list_org_members_handler,
        admin::update_org_member_handler,
        admin::remove_org_member_handler,
        admin::add_collaborator_handler,
        admin::list_collaborators_handler,
        admin::update_collaborator_handler,
        admin::remove_collaborator_handler,
        admin::search_repo_members_handler,
        auth::token_exchange_handler,
        auth::refresh_token_handler,
        auth::revoke_token_handler,
        auth::create_device_code_handler,
        auth::poll_device_code_handler,
        auth::authorize_device_code_handler,
        admin::create_key_handler,
        admin::list_keys_handler,
        admin::revoke_key_handler,
        admin::bulk_revoke_keys_handler,
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
            workspace::CreateWorkspaceRequest,
            workspace::WorkspaceResponse,
            workspace::SubmitResponse,
            version::VersionDiffFile,
            version::VersionDiffResponse,
            version::RollbackRequest,
            issue::CreateIssueRequest,
            issue::AgentSourceRequest,
            issue::UpdateIssueRequest,
            issue::CloseIssueRequest,
            issue::IssueResponse,
            issue::IssueDetailResponse,
            issue::IssueLinkDetailResponse,
            issue::CreateCommentRequest,
            issue::UpdateCommentRequest,
            issue::CommentResponse,
            issue::MentionRef,
            issue::CreateIssueLinkRequest,
            issue::IssueLinkResponse,
            issue::UploadAttachmentRequest,
            issue::AttachmentResponse,
            workspace::FileUploadEntry,
            workspace::UploadFilesRequest,
            workspace::UploadFilesResponse,
            workspace::UploadSnapshotResponse,
            workspace::DeltaManifest,
            workspace::FileDownloadResponse,
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
            worker::AppendLogsRequest,
            worker::MarkDoneRequest,
            worker::WorkerAckResponse,
            worker::LogsQuery,
            worker::LogsResponse,
            crate::storage::AgentWorker,
            crate::storage::WorkerLog,
            crate::storage::LogStream,
            crate::storage::WorkerDoneReason,
            admin::CreateRepoRequest,
            admin::UpdateRepoRequest,
            admin::RepoResponse,
            admin::QuotaExceededBody,
            admin::CreateOrgRequest,
            admin::CreateUserRequest,
            admin::AddMemberRequest,
            admin::UpdateMemberRequest,
            admin::OrgResponse,
            admin::UserResponse,
            admin::MeResponse,
            me::OnboardingStatusResponse,
            me::OnboardingCompleteResponse,
            admin::OrgMemberResponse,
            admin::AddCollaboratorRequest,
            admin::UpdateCollaboratorRequest,
            admin::CollaboratorResponse,
            admin::RepoMemberResponse,
            auth::TokenRequest,
            auth::TokenResponse,
            auth::RefreshRequest,
            auth::RefreshResponse,
            auth::RevokeRequest,
            auth::DeviceCodeResponse,
            auth::DeviceCodeStatusResponse,
            auth::AuthorizeDeviceCodeRequest,
            admin::CreateKeyRequest,
            admin::CreateKeyResponse,
            admin::ApiKeyResponse,
            admin::BulkRevokeResponse,
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
        (name = "agent-workers", description = "Cloud agent worker lifecycle (PRD 28)"),
        (name = "agent-secrets", description = "Per-repo encrypted agent secrets (PRD 28)"),
        (name = "repos", description = "Repository management"),
        (name = "orgs", description = "Organization management"),
        (name = "users", description = "User management"),
        (name = "me", description = "Per-user state (onboarding, preferences)"),
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
    let mut doc = VaiApi::openapi();
    // Merge postgres-only endpoint specs so they appear in /api/openapi.json
    // even though they can't be included in the compile-time VaiApi derive.
    #[cfg(feature = "postgres")]
    {
        use utoipa::OpenApi as _;
        doc.merge(agent_secrets::AgentSecretsApiDoc::openapi());
    }
    Json(doc)
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
        .route("/api/auth/token", post(auth::token_exchange_handler))
        // Refresh and revoke use the refresh token itself as the credential.
        .route("/api/auth/refresh", post(auth::refresh_token_handler))
        .route("/api/auth/revoke", post(auth::revoke_token_handler))
        // CLI device code flow (PRD 26 V-3) — unauthenticated endpoints.
        .route("/api/auth/cli-device", post(auth::create_device_code_handler))
        .route("/api/auth/cli-device/:code", get(auth::poll_device_code_handler));

    // Routes requiring `Authorization: Bearer <key>`.
    let protected = Router::new()
        // Multi-repo management endpoints.
        .route("/api/repos", post(admin::create_repo_handler))
        .route("/api/repos", get(admin::list_repos_handler))
        .route("/api/repos/:name", get(admin::get_repo_handler).patch(admin::update_repo_handler))
        // User management endpoints.
        .route("/api/users", post(admin::create_user_handler))
        .route("/api/users/:user", get(admin::get_user_handler))
        // Organization management endpoints (PRD 10.3).
        .route("/api/orgs", post(admin::create_org_handler))
        .route("/api/orgs", get(admin::list_orgs_handler))
        .route("/api/orgs/:org", get(admin::get_org_handler))
        .route("/api/orgs/:org", delete(admin::delete_org_handler))
        .route("/api/orgs/:org/members", post(admin::add_org_member_handler))
        .route("/api/orgs/:org/members", get(admin::list_org_members_handler))
        .route(
            "/api/orgs/:org/members/:user",
            axum::routing::patch(admin::update_org_member_handler),
        )
        .route("/api/orgs/:org/members/:user", delete(admin::remove_org_member_handler))
        // API key management endpoints (PRD 10.3).
        .route("/api/keys", post(admin::create_key_handler))
        .route("/api/keys", get(admin::list_keys_handler))
        .route("/api/keys", delete(admin::bulk_revoke_keys_handler))
        .route("/api/keys/:id", delete(admin::revoke_key_handler))
        // Agent worker lifecycle — read, heartbeat, log ingest, terminal state (PRD 28).
        .route("/api/agent-workers/:id", get(worker::get_worker_handler))
        .route("/api/agent-workers/:id", delete(worker::destroy_worker_handler))
        .route("/api/agent-workers/:id/logs", get(worker::get_logs_handler))
        .route("/api/agent-workers/:id/heartbeat", post(worker::heartbeat_handler))
        .route("/api/agent-workers/:id/logs", post(worker::append_logs_handler))
        .route("/api/agent-workers/:id/done", post(worker::mark_done_handler))
        // Per-user onboarding state (PRD 26).
        .route("/api/me/onboarding", get(me::get_onboarding_handler))
        .route("/api/me/onboarding-complete", post(me::complete_onboarding_handler))
        // CLI device code authorization (authenticated — requires a user identity).
        .route("/api/auth/cli-device/authorize", post(auth::authorize_device_code_handler))
        // Repository collaborator endpoints (PRD 10.3).
        .route(
            "/api/orgs/:org/repos/:repo/collaborators",
            post(admin::add_collaborator_handler),
        )
        .route(
            "/api/orgs/:org/repos/:repo/collaborators",
            get(admin::list_collaborators_handler),
        )
        .route(
            "/api/orgs/:org/repos/:repo/collaborators/:user",
            axum::routing::patch(admin::update_collaborator_handler),
        )
        .route(
            "/api/orgs/:org/repos/:repo/collaborators/:user",
            delete(admin::remove_collaborator_handler),
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
        .route("/me", get(admin::get_me_handler))
        .route("/status", get(status_handler))
        .route("/workspaces", post(workspace::create_workspace_handler))
        .route("/workspaces", get(workspace::list_workspaces_handler))
        .route("/workspaces/:id", get(workspace::get_workspace_handler))
        .route("/workspaces/:id/submit", post(workspace::submit_workspace_handler))
        .route("/workspaces/:id/files", post(workspace::upload_workspace_files_handler).layer(DefaultBodyLimit::max(UPLOAD_BODY_LIMIT)))
        .route("/workspaces/:id/upload-snapshot", post(workspace::upload_snapshot_handler).layer(DefaultBodyLimit::max(SNAPSHOT_BODY_LIMIT)))
        .route("/workspaces/:id/files/*path", get(workspace::get_workspace_file_handler))
        .route("/workspaces/:id", delete(workspace::discard_workspace_handler))
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
        .route("/issues", post(issue::create_issue_handler))
        .route("/issues", get(issue::list_issues_handler))
        .route("/issues/:id/close", post(issue::close_issue_handler))
        .route("/issues/:id/comments", post(issue::create_issue_comment_handler))
        .route("/issues/:id/comments", get(issue::list_issue_comments_handler))
        .route("/issues/:id/comments/:comment_id", axum::routing::patch(issue::update_issue_comment_handler))
        .route("/issues/:id/comments/:comment_id", axum::routing::delete(issue::delete_issue_comment_handler))
        .route("/issues/:id/links", post(issue::create_issue_link_handler))
        .route("/issues/:id/links", get(issue::list_issue_links_handler))
        .route("/issues/:id/links/:target_id", axum::routing::delete(issue::delete_issue_link_handler))
        .route("/issues/:id/attachments", post(issue::upload_attachment_handler))
        .route("/issues/:id/attachments", get(issue::list_attachments_handler))
        .route("/issues/:id/attachments/:filename", get(issue::download_attachment_handler))
        .route("/issues/:id/attachments/:filename", axum::routing::delete(issue::delete_attachment_handler))
        .route("/issues/:id", get(issue::get_issue_handler))
        .route("/issues/:id", axum::routing::patch(issue::update_issue_handler))
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
        .route("/members", get(admin::search_repo_members_handler))
        .route("/ws/events", get(ws::ws_events_handler))
        // Migration endpoints (PRD 12.2, 12.5) — multi-repo mode.
        .route("/migrate", post(migrate_handler).layer(DefaultBodyLimit::max(MIGRATE_BODY_LIMIT)))
        .route("/migration-stats", get(migration_stats_handler));

    // Agent secrets endpoints require Postgres for AES-GCM encryption.
    #[cfg(feature = "postgres")]
    let repo_scoped = repo_scoped
        .route("/agent-secrets", post(agent_secrets::set_agent_secret_handler))
        .route("/agent-secrets", get(agent_secrets::list_agent_secrets_handler))
        .route(
            "/agent-secrets/:key",
            axum::routing::delete(agent_secrets::delete_agent_secret_handler),
        );

    let repo_scoped = repo_scoped
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
        max_repos_per_user: 100,
        compute: None,
        worker_server_url: String::new(),
        worker_anthropic_key: String::new(),
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

    // Connect to Postgres + in-memory file store and run schema migrations.
    // We use `server_with_mem_fs` rather than plain `server` so that file
    // upload/download handlers work without requiring real S3.
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
        max_repos_per_user: 100,
        compute: None,
        worker_server_url: String::new(),
        worker_anthropic_key: String::new(),
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

    // Connect to Postgres + in-memory file store and run schema migrations.
    // We use `server_with_mem_fs` rather than plain `server` so that file
    // upload/download handlers work without requiring real S3.
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
        max_repos_per_user: 100,
        compute: None,
        worker_server_url: String::new(),
        worker_anthropic_key: String::new(),
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

/// Same as [`start_for_testing_pg_multi_repo`] but with a custom per-user repo quota.
///
/// Used by integration tests that need to exercise the quota-exceeded code path
/// without needing to create 100+ repos.
pub async fn start_for_testing_pg_multi_repo_with_quota(
    storage_root: &Path,
    database_url: &str,
    max_repos_per_user: u64,
) -> Result<(SocketAddr, tokio::sync::oneshot::Sender<()>), ServerError> {
    let _ = tracing_subscriber::fmt::try_init();

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
        repo_name: "multi-repo-quota-test".to_string(),
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
        max_repos_per_user,
        compute: None,
        worker_server_url: String::new(),
        worker_anthropic_key: String::new(),
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
        max_repos_per_user: 100,
        compute: None,
        worker_server_url: String::new(),
        worker_anthropic_key: String::new(),
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

/// Same as [`start_for_testing_pg_with_mem_fs`] but with an injected
/// [`compute::ComputeProvider`] so tests can exercise spawn_if_capacity and
/// the agent-worker lifecycle endpoints.
///
/// Returns the server address, shutdown sender, and a shared handle to the
/// injected [`compute::in_memory::InMemoryProvider`] so callers can inspect
/// or advance worker state.
pub async fn start_for_testing_pg_with_compute(
    storage_root: &Path,
    database_url: &str,
) -> Result<
    (
        SocketAddr,
        tokio::sync::oneshot::Sender<()>,
        std::sync::Arc<compute::in_memory::InMemoryProvider>,
    ),
    ServerError,
> {
    let _ = tracing_subscriber::fmt::try_init();

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

    let provider = std::sync::Arc::new(compute::in_memory::InMemoryProvider::new());

    let state = Arc::new(AppState {
        vai_dir,
        repo_root: storage_root.to_path_buf(),
        started_at: Instant::now(),
        repo_name: "multi-repo-test-compute".to_string(),
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
        max_repos_per_user: 100,
        compute: Some(provider.clone() as std::sync::Arc<dyn compute::ComputeProvider>),
        worker_server_url: "http://test-server.local".to_string(),
        worker_anthropic_key: "test-anthropic-key".to_string(),
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

    Ok((addr, shutdown_tx, provider))
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
    // Initialise structured logging.
    //
    // Priority order: RUST_LOG > VAI_LOG_LEVEL > "info".
    // The fly.toml sets VAI_LOG_LEVEL = 'info'; without this fallback the
    // tracing subscriber silently drops INFO events when RUST_LOG is not set.
    let log_filter = std::env::var("RUST_LOG")
        .ok()
        .or_else(|| std::env::var("VAI_LOG_LEVEL").ok())
        .unwrap_or_else(|| "info".to_string());
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_new(&log_filter)
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();

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

    // Guard: multi-repo mode (storage_root set) requires a Postgres database.
    // If DATABASE_URL is missing the server falls back to local SQLite mode,
    // which silently skips the collaborator insert and returns empty repo lists
    // for non-admin users — a production foot-gun that is very hard to debug.
    if config.storage_root.is_some() && config.database_url.is_none() {
        return Err(ServerError::Io(std::io::Error::other(
            "VAI_STORAGE_ROOT is set but DATABASE_URL (or --database-url) is not configured. \
             Multi-repo mode requires a Postgres database. \
             Set DATABASE_URL in Fly secrets or ~/.vai/server.toml.",
        )));
    }

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

    // Log the storage backend so startup logs make the mode unambiguous.
    match &storage {
        crate::storage::StorageBackend::Local(_) => {
            tracing::warn!(
                event = "server.storage_mode",
                mode = "local_sqlite",
                "storage: local SQLite mode — RBAC and multi-repo features are disabled"
            );
        }
        #[cfg(feature = "postgres")]
        crate::storage::StorageBackend::Server(_) => {
            tracing::info!(event = "server.storage_mode", mode = "postgres", "storage: Postgres mode");
        }
        #[cfg(feature = "s3")]
        crate::storage::StorageBackend::ServerWithS3(_, _) => {
            tracing::info!(event = "server.storage_mode", mode = "postgres_s3", "storage: Postgres + S3 mode");
        }
        #[cfg(feature = "postgres")]
        crate::storage::StorageBackend::ServerWithMemFs(_, _) => {
            tracing::info!(event = "server.storage_mode", mode = "postgres_memfs", "storage: Postgres + in-memory FS mode");
        }
    }

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

    // Resolve CORS origins: config.cors_origins is populated by server_cmd.rs from
    // (in priority order) CLI --cors-origins flag, VAI_CORS_ORIGINS env var, or
    // ~/.vai/server.toml. Fall back to VAI_CORS_ORIGINS here only when start() is
    // called directly without going through server_cmd.rs (e.g. in tests).
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

    // Per-user repo quota: VAI_MAX_REPOS_PER_USER or 100.
    let max_repos_per_user: u64 = std::env::var("VAI_MAX_REPOS_PER_USER")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(100);

    // Cloud compute provider: wire FlyMachinesProvider when VAI_COMPUTE_FLY_TOKEN is set.
    let compute: Option<std::sync::Arc<dyn compute::ComputeProvider>> = {
        let app_name = std::env::var("VAI_COMPUTE_FLY_APP")
            .unwrap_or_else(|_| "vai-workers".to_string());
        let region = std::env::var("VAI_COMPUTE_FLY_REGION")
            .unwrap_or_else(|_| "iad".to_string());
        compute::fly::FlyMachinesProvider::from_env(app_name, region)
            .map(|p| std::sync::Arc::new(p) as std::sync::Arc<dyn compute::ComputeProvider>)
    };

    if compute.is_some() {
        tracing::info!(event = "server.compute", provider = "fly", "cloud compute provider enabled");
    } else {
        tracing::info!(event = "server.compute", provider = "none", "cloud compute provider disabled (VAI_COMPUTE_FLY_TOKEN not set)");
    }

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
        max_repos_per_user,
        compute,
        worker_server_url: std::env::var("VAI_SERVER_URL").unwrap_or_default(),
        worker_anthropic_key: std::env::var("ANTHROPIC_API_KEY").unwrap_or_default(),
    });

    // Start the dead-worker reconciliation background task when a compute
    // provider is configured (i.e., cloud mode).  The task is a no-op in
    // local SQLite mode — list_stale_workers returns [] there.
    if state.compute.is_some() {
        worker_registry::run_reconciliation_loop(state.storage.clone());
    }

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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::fs;

    use futures_util::{SinkExt, StreamExt};
    use tempfile::TempDir;
    use tokio::sync::oneshot;

    use super::*;
    use crate::auth as crate_auth;
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
        crate_auth::create(&vai_dir, "test-agent").unwrap();

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
            max_repos_per_user: 100,
            compute: None,
        worker_server_url: String::new(),
        worker_anthropic_key: String::new(),
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
        let (_, revocable_key) = crate_auth::create(&vai_dir, "revoke-me").unwrap();
        crate_auth::revoke(&vai_dir, "revoke-me").unwrap();
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
            !body["file_changes"].as_array().unwrap().is_empty(),
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
            max_repos_per_user: 100,
            compute: None,
        worker_server_url: String::new(),
        worker_anthropic_key: String::new(),
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
            max_repos_per_user: 100,
            compute: None,
        worker_server_url: String::new(),
        worker_anthropic_key: String::new(),
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
        let keys = crate_auth::list(&vai_dir).unwrap();
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
        let (_, regular_key) = crate_auth::create(&vai_dir, "non-admin-agent").unwrap();

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

    /// The startup guard must reject the config rather than silently falling
    /// back to SQLite when storage_root is set but database_url is absent.
    /// This is the root cause of the repeated issue #305 production failures.
    #[tokio::test]
    async fn start_rejects_storage_root_without_database_url() {
        let tmp = TempDir::new().unwrap();
        let vai_dir = tmp.path().join(".vai");
        // ALLOW_FS: test setup only — not a server handler
        std::fs::create_dir_all(&vai_dir).unwrap();

        let config = ServerConfig {
            storage_root: Some(tmp.path().to_path_buf()),
            database_url: None,
            ..ServerConfig::default()
        };

        let result = start(&vai_dir, config).await;
        assert!(result.is_err(), "start() must Err when storage_root set without database_url");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("DATABASE_URL") || msg.contains("database_url"),
            "error message should name DATABASE_URL, got: {msg}"
        );
    }
}
