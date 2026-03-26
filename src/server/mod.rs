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
//!
//! ## Multi-Repo Endpoints (`/api/repos/:repo/`)
//!   - `GET /api/repos/:repo/status` — per-repo health (same fields as `/api/status`)
//!   - `POST /api/repos/:repo/workspaces` — create workspace in the named repo
//!   - `GET /api/repos/:repo/workspaces` — list workspaces in the named repo
//!   - `GET /api/repos/:repo/workspaces/:id` — workspace details
//!   - `POST /api/repos/:repo/workspaces/:id/submit` — submit workspace for merge
//!   - `DELETE /api/repos/:repo/workspaces/:id` — discard workspace
//!   - `POST /api/repos/:repo/workspaces/:id/files` — upload files into workspace overlay
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

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Extension, Path as AxumPath, Query as AxumQuery, Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use utoipa::{OpenApi, ToSchema};
use tokio::net::TcpListener;
use tokio::sync::broadcast;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};

use crate::auth;
use crate::conflict;
use crate::storage::{EventFilter, EventStore as _};
use crate::escalation::EscalationStatus;
use crate::event_log::{EventKind, EventLog};
use crate::graph::GraphSnapshot;
use crate::merge;
use crate::repo;
use crate::version;
use crate::watcher::{DiscoveryEventKind, IssueCreationPolicy, Watcher, WatcherStatus, WatcherStore, WatchType};
use crate::work_queue;
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

/// Subscription filter sent by the client after connecting.
///
/// An empty list for any field means "match all" for that dimension.
#[derive(Debug, Default, Clone, Deserialize, Serialize, ToSchema)]
pub struct SubscriptionFilter {
    /// Match events touching any of these entity IDs.
    #[serde(default)]
    pub entities: Vec<String>,
    /// Match events touching any of these file paths (glob patterns not yet supported).
    #[serde(default)]
    pub paths: Vec<String>,
    /// Match only these event type names (e.g. `"WorkspaceCreated"`).
    #[serde(default)]
    pub event_types: Vec<String>,
    /// Match only events from these workspace IDs.
    #[serde(default)]
    pub workspaces: Vec<String>,
}

/// Incoming WebSocket message from a client.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ClientMessage {
    Subscribe { subscribe: SubscriptionFilter },
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

/// The authenticated agent making the current request.
///
/// Injected into request extensions by [`auth_middleware`] and available to
/// handlers via `Extension<AgentIdentity>`.
#[derive(Debug, Clone)]
pub struct AgentIdentity {
    /// Key record ID.
    pub key_id: String,
    /// Human-readable key name.
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
        StorageBackend::Server(_) | StorageBackend::ServerWithS3(_, _) => state_storage.clone(),
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

/// Axum middleware that enforces `Authorization: Bearer <key>` on every request.
///
/// Valid keys result in an [`AgentIdentity`] being inserted into request
/// extensions. Invalid or missing keys return 401 Unauthorized.
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
            return ApiError::unauthorized(
                "missing or invalid Authorization header; expected `Bearer <key>`",
            )
            .into_response();
        }
    };

    // Bootstrap admin key check — takes priority over per-repo API keys.
    if key_str == state.admin_key {
        tracing::debug!("authenticated request via bootstrap admin key");
        request.extensions_mut().insert(AgentIdentity {
            key_id: "admin".to_string(),
            name: "admin".to_string(),
            is_admin: true,
            user_id: None,
            role_override: None,
        });
        return next.run(request).await;
    }

    // Validate via the storage backend so that both SQLite (local) and
    // Postgres (server) key stores are supported.
    match state.storage.auth().validate_key(&key_str).await {
        Ok(api_key) => {
            tracing::debug!(agent = %api_key.name, "authenticated request");
            request.extensions_mut().insert(AgentIdentity {
                key_id: api_key.id,
                name: api_key.name,
                is_admin: false,
                user_id: api_key.user_id,
                role_override: api_key.role_override,
            });
            next.run(request).await
        }
        Err(crate::storage::StorageError::NotFound(_)) => {
            ApiError::unauthorized("invalid or revoked API key").into_response()
        }
        Err(e) => ApiError::internal(format!("auth error: {e}")).into_response(),
    }
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

    let storage_root = match state.storage_root.as_ref() {
        Some(sr) => sr.clone(),
        None => {
            return ApiError::bad_request(
                "server is not in multi-repo mode; set storage_root in ~/.vai/server.toml",
            )
            .into_response()
        }
    };

    let registry = match RepoRegistry::load(&storage_root) {
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

    let vai_dir = entry.path.join(".vai");
    let repo_id = repo_id_from_vai_dir(&vai_dir);
    let storage = repo_storage(&state.storage, &vai_dir);
    let ctx = RepoCtx {
        vai_dir,
        repo_root: entry.path.clone(),
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
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: msg.into(),
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

impl From<version::VersionError> for ApiError {
    fn from(e: version::VersionError) -> Self {
        match &e {
            version::VersionError::NotFound(_) => ApiError::not_found(e.to_string()),
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
        None => return Err(ApiError::forbidden("access denied")),
        Some(r) => r,
    };

    // Apply key-level role cap if present.
    let effective = if let Some(cap_str) = &identity.role_override {
        let cap = RepoRole::from_str(cap_str);
        if effective.rank() > cap.rank() { cap } else { effective }
    } else {
        effective
    };

    if effective.rank() < required.rank() {
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

/// Response body for `GET /health`.
#[derive(Debug, Serialize, ToSchema)]
pub struct HealthResponse {
    /// Always `"ok"` when the server is healthy.
    #[schema(value_type = String)]
    pub status: &'static str,
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

/// Query parameters for `GET /api/versions`.
#[derive(Debug, Deserialize)]
struct ListVersionsQuery {
    /// Maximum number of versions to return (default: unlimited).
    limit: Option<usize>,
}

/// Query parameters for `GET /api/versions/:id/diff`.
#[derive(Debug, Default, Deserialize)]
struct VersionDiffQuery {
    /// Version to diff against instead of the parent. Must be an ancestor of `:id`.
    base: Option<String>,
}

/// File-level diff entry returned by `GET /api/versions/:id/diff`.
#[derive(Debug, Serialize, ToSchema)]
struct VersionDiffFile {
    /// File path relative to the repository root.
    path: String,
    /// How the file was changed: `"added"`, `"modified"`, or `"removed"`.
    change_type: String,
    /// Unified diff string for this file.
    diff: String,
}

/// Response body for `GET /api/versions/:id/diff`.
#[derive(Debug, Serialize, ToSchema)]
struct VersionDiffResponse {
    /// The version whose changes are shown.
    version_id: String,
    /// The version used as the diff base (the parent, or the explicit `?base`).
    base_version_id: String,
    /// Per-file diffs.
    files: Vec<VersionDiffFile>,
}

/// Request body for `POST /api/versions/rollback`.
#[derive(Debug, Deserialize, ToSchema)]
struct RollbackRequest {
    /// Version identifier to roll back (e.g., `"v3"`).
    version: String,
    /// If `true`, proceed even when downstream versions depend on the changes.
    /// If `false` (default) and downstream impacts exist, returns 409.
    #[serde(default)]
    force: bool,
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
    /// Issue IDs that must be closed before this issue becomes available.
    #[serde(default)]
    depends_on: Vec<String>,
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
    /// Issue IDs that must be closed before this issue becomes available.
    depends_on: Vec<String>,
    /// IDs of issues that depend on this issue (reverse deps).
    blocked_by_issues: Vec<String>,
    created_at: String,
    updated_at: String,
}

impl IssueResponse {
    fn from_issue(issue: crate::issue::Issue, linked: Vec<uuid::Uuid>, blocked_by_issues: Vec<uuid::Uuid>) -> Self {
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
            depends_on: issue.depends_on.iter().map(|id| id.to_string()).collect(),
            blocked_by_issues: blocked_by_issues.iter().map(|id| id.to_string()).collect(),
            created_at: issue.created_at.to_rfc3339(),
            updated_at: issue.updated_at.to_rfc3339(),
        }
    }
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
        .list_workspaces(&ctx.repo_id, false)
        .await
        .map(|w| w.len())
        .unwrap_or(0);

    let issue_count = ctx.storage.issues()
        .list_issues(&ctx.repo_id, &IssueFilter {
            status: Some(IssueStatus::Open),
            ..Default::default()
        })
        .await
        .map(|v| v.len())
        .unwrap_or(0);

    let escalation_count = ctx.storage.escalations()
        .list_escalations(&ctx.repo_id, true)
        .await
        .map(|v| v.len())
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
        StorageBackend::Server(ref pg) | StorageBackend::ServerWithS3(ref pg, _) => {
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
        (status = 200, description = "Success", body = HealthResponse),
    ),
    tag = "status"
)]
/// `GET /health` — simple liveness probe for load balancers.
///
/// Returns `200 OK` with `{ "status": "ok" }`. No authentication required.
async fn health_handler() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
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
        .list_workspaces(&ctx.repo_id, false)
        .await
        .map(|w| w.len())
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
    path = "/api/workspaces",
    request_body = CreateWorkspaceRequest,
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

    Ok((StatusCode::CREATED, Json(WorkspaceResponse::from(ws))))
}

/// `GET /api/workspaces` — lists all active (non-discarded, non-merged) workspaces.
#[utoipa::path(
    get,
    path = "/api/workspaces",
    responses(
        (status = 200, description = "List of workspaces", body = Vec<WorkspaceResponse>),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = [])),
    tag = "workspaces"
)]
async fn list_workspaces_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
) -> Result<Json<Vec<WorkspaceResponse>>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Read).await?;
    let workspaces = ctx.storage.workspaces()
        .list_workspaces(&ctx.repo_id, false)
        .await
        .map_err(ApiError::from)?;
    let response: Vec<WorkspaceResponse> = workspaces.into_iter().map(Into::into).collect();
    Ok(Json(response))
}

/// `GET /api/workspaces/:id` — returns details for a single workspace.
///
/// Returns 404 if the workspace does not exist.
#[utoipa::path(
    get,
    path = "/api/workspaces/{id}",
    params(
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
    path = "/api/workspaces/{id}/submit",
    params(
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

    // Ensure the workspace overlay and filesystem state are ready for merge::submit.
    // In Postgres server mode workspaces live in the database + FileStore; this
    // step downloads them to the local .vai/ tree that the synchronous merge
    // engine expects.
    prepare_workspace_for_submit(&ctx, &meta).await?;

    // Make it the active workspace so merge::submit can find it.
    workspace::switch(&ctx.vai_dir, &id).map_err(ApiError::from)?;

    match merge::submit(&ctx.vai_dir, &ctx.repo_root) {
        Ok(result) => {
            // Remove from conflict engine — workspace is no longer active.
            state.conflict_engine.lock().await.remove_workspace(&workspace_uuid);

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
                    merge_event_id: result.version.merge_event_id,
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

            // Persist pre-change snapshot to FileStore so diffs survive container
            // restarts and cross-server migrations.
            {
                let snap_dir = ctx.vai_dir
                    .join("versions")
                    .join(&result.version.version_id)
                    .join("snapshot");
                let file_store = ctx.storage.files();
                for (rel, bytes) in collect_dir_files_with_content(&snap_dir) {
                    let key = format!("versions/{}/snapshot/{rel}", result.version.version_id);
                    let _ = file_store.put(&ctx.repo_id, &key, &bytes).await;
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

            let summary = format!(
                "Workspace \"{workspace_intent}\" has {count} unresolvable \
                 semantic conflict(s) requiring manual resolution"
            );

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
    path = "/api/workspaces/{id}",
    params(
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

    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    get,
    path = "/api/versions",
    params(
        ("limit" = Option<usize>, Query, description = "Maximum number of versions to return"),
    ),
    responses(
        (status = 200, description = "List of versions", body = Vec<version::VersionMeta>),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = [])),
    tag = "versions"
)]
/// `GET /api/versions` — lists all versions in chronological order.
///
/// Optional `?limit=N` query parameter truncates the result to the N most
/// recent versions (the list is already oldest-first, so we truncate from
/// the end after reversing).
async fn list_versions_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    AxumQuery(params): AxumQuery<ListVersionsQuery>,
) -> Result<Json<Vec<version::VersionMeta>>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Read).await?;
    let mut versions = ctx
        .storage
        .versions()
        .list_versions(&ctx.repo_id)
        .await
        .map_err(ApiError::from)?;
    if let Some(limit) = params.limit {
        // Keep the N most-recent: the list is oldest-first, so drop from the front.
        let len = versions.len();
        if limit < len {
            versions.drain(..len - limit);
        }
    }
    Ok(Json(versions))
}

#[utoipa::path(
    get,
    path = "/api/versions/{id}",
    params(
        ("id" = String, Path, description = "Version ID"),
    ),
    responses(
        (status = 200, description = "Version details", body = version::VersionChanges),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Version not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "versions"
)]
/// `GET /api/versions/:id` — returns details for a single version, including
/// entity-level and file-level changes derived from the event log.
///
/// Returns 404 if the version does not exist.
async fn get_version_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    PathId(id): PathId,
) -> Result<Json<version::VersionChanges>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Read).await?;
    let meta = ctx
        .storage
        .versions()
        .get_version(&ctx.repo_id, &id)
        .await
        .map_err(ApiError::from)?;

    let Some(merge_event_id) = meta.merge_event_id else {
        return Ok(Json(version::VersionChanges {
            version: meta,
            entity_changes: vec![],
            file_changes: vec![],
        }));
    };

    // Find the MergeCompleted event to get the workspace_id, then replay
    // all workspace events to reconstruct entity and file changes.
    let merge_events = ctx
        .storage
        .events()
        .query_by_type(&ctx.repo_id, "MergeCompleted")
        .await
        .map_err(ApiError::from)?;
    let workspace_id = merge_events
        .into_iter()
        .find(|e| e.id == merge_event_id)
        .and_then(|e| e.kind.workspace_id());
    let Some(workspace_id) = workspace_id else {
        return Ok(Json(version::VersionChanges {
            version: meta,
            entity_changes: vec![],
            file_changes: vec![],
        }));
    };

    let events = ctx
        .storage
        .events()
        .query_by_workspace(&ctx.repo_id, &workspace_id)
        .await
        .map_err(ApiError::from)?;

    let mut entity_changes = Vec::new();
    let mut file_changes = Vec::new();
    for event in events {
        match event.kind {
            EventKind::EntityAdded { entity, .. } => {
                entity_changes.push(version::VersionEntityChange {
                    entity_id: entity.id,
                    change_type: version::VersionChangeType::Added,
                    kind: Some(entity.kind),
                    qualified_name: Some(entity.qualified_name),
                    file_path: Some(entity.file_path),
                    change_description: None,
                });
            }
            EventKind::EntityModified {
                entity_id,
                change_description,
                ..
            } => {
                entity_changes.push(version::VersionEntityChange {
                    entity_id,
                    change_type: version::VersionChangeType::Modified,
                    kind: None,
                    qualified_name: None,
                    file_path: None,
                    change_description: Some(change_description),
                });
            }
            EventKind::EntityRemoved { entity_id, .. } => {
                entity_changes.push(version::VersionEntityChange {
                    entity_id,
                    change_type: version::VersionChangeType::Removed,
                    kind: None,
                    qualified_name: None,
                    file_path: None,
                    change_description: None,
                });
            }
            EventKind::FileAdded { path, hash, .. } => {
                file_changes.push(version::VersionFileChange {
                    path,
                    change_type: version::VersionFileChangeType::Added,
                    hash: Some(hash),
                });
            }
            EventKind::FileModified { path, new_hash, .. } => {
                file_changes.push(version::VersionFileChange {
                    path,
                    change_type: version::VersionFileChangeType::Modified,
                    hash: Some(new_hash),
                });
            }
            EventKind::FileRemoved { path, .. } => {
                file_changes.push(version::VersionFileChange {
                    path,
                    change_type: version::VersionFileChangeType::Removed,
                    hash: None,
                });
            }
            _ => {}
        }
    }

    Ok(Json(version::VersionChanges {
        version: meta,
        entity_changes,
        file_changes,
    }))
}

#[utoipa::path(
    get,
    path = "/api/versions/{id}/diff",
    params(
        ("id" = String, Path, description = "Version ID"),
        ("base" = Option<String>, Query, description = "Base version ID to diff against"),
    ),
    responses(
        (status = 200, description = "Version diff", body = VersionDiffResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Version not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "versions"
)]
/// `GET /api/versions/:id/diff` — returns unified diffs for all files changed
/// in this version compared to its parent (or a specific `?base=<version_id>`).
///
/// Response includes a per-file diff string in unified diff format.
/// Returns 404 if the version does not exist.
async fn get_version_diff_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    PathId(id): PathId,
    AxumQuery(query): AxumQuery<VersionDiffQuery>,
) -> Result<Json<VersionDiffResponse>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Read).await?;

    // Fetch version metadata.
    let meta = ctx
        .storage
        .versions()
        .get_version(&ctx.repo_id, &id)
        .await
        .map_err(ApiError::from)?;

    let base_version_id = query
        .base
        .or_else(|| meta.parent_version_id.clone())
        .unwrap_or_default();

    let Some(merge_event_id) = meta.merge_event_id else {
        // Initial version — no files changed.
        return Ok(Json(VersionDiffResponse {
            version_id: id,
            base_version_id,
            files: vec![],
        }));
    };

    // Find the workspace that produced this version via MergeCompleted event.
    let merge_events = ctx
        .storage
        .events()
        .query_by_type(&ctx.repo_id, "MergeCompleted")
        .await
        .map_err(ApiError::from)?;

    let workspace_id = merge_events
        .into_iter()
        .find(|e| e.id == merge_event_id)
        .and_then(|e| e.kind.workspace_id());

    let Some(workspace_id) = workspace_id else {
        return Ok(Json(VersionDiffResponse {
            version_id: id,
            base_version_id,
            files: vec![],
        }));
    };

    // Replay workspace events to collect file-level changes.
    let events = ctx
        .storage
        .events()
        .query_by_workspace(&ctx.repo_id, &workspace_id)
        .await
        .map_err(ApiError::from)?;

    // Collect file changes with both old and new hashes directly from workspace
    // events.  `FileModified` carries both `old_hash` and `new_hash`; `FileAdded`
    // carries only `new_hash`; `FileRemoved` carries no hash (resolved below).
    struct FileChangeHashes {
        path: String,
        change_type: version::VersionFileChangeType,
        new_hash: Option<String>,
        old_hash: Option<String>,
    }

    let mut file_changes: Vec<FileChangeHashes> = Vec::new();
    let mut has_removed_files = false;

    for event in events {
        match event.kind {
            EventKind::FileAdded { path, hash, .. } => {
                file_changes.push(FileChangeHashes {
                    path,
                    change_type: version::VersionFileChangeType::Added,
                    new_hash: Some(hash),
                    old_hash: None,
                });
            }
            EventKind::FileModified { path, old_hash, new_hash, .. } => {
                file_changes.push(FileChangeHashes {
                    path,
                    change_type: version::VersionFileChangeType::Modified,
                    new_hash: Some(new_hash),
                    old_hash: Some(old_hash),
                });
            }
            EventKind::FileRemoved { path, .. } => {
                has_removed_files = true;
                file_changes.push(FileChangeHashes {
                    path,
                    change_type: version::VersionFileChangeType::Removed,
                    new_hash: None,
                    old_hash: None,
                });
            }
            _ => {}
        }
    }

    // For removed files there is no hash in the event.  Scan the full event log
    // (excluding the current workspace's events) to reconstruct the last known
    // hash for each path, i.e. the content that existed in the parent version.
    if has_removed_files {
        let all_events = ctx
            .storage
            .events()
            .query_since_id(&ctx.repo_id, 0)
            .await
            .map_err(ApiError::from)?;

        let mut path_to_hash: HashMap<String, String> = HashMap::new();
        for e in &all_events {
            if e.kind.workspace_id() == Some(workspace_id) {
                // Skip events from the current workspace — we want the state
                // *before* this version's changes were applied.
                continue;
            }
            match &e.kind {
                EventKind::FileAdded { path, hash, .. } => {
                    path_to_hash.insert(path.clone(), hash.clone());
                }
                EventKind::FileModified { path, new_hash, .. } => {
                    path_to_hash.insert(path.clone(), new_hash.clone());
                }
                EventKind::FileRemoved { path, .. } => {
                    path_to_hash.remove(path);
                }
                _ => {}
            }
        }

        for fc in &mut file_changes {
            if fc.change_type == version::VersionFileChangeType::Removed && fc.old_hash.is_none() {
                fc.old_hash = path_to_hash.get(&fc.path).cloned();
            }
        }
    }

    // Fallback paths for pre-blob-storage versions (local/SQLite mode or versions
    // created before content-addressable storage was introduced).
    let snapshot_dir = ctx.vai_dir.join("versions").join(&id).join("snapshot");
    let overlay_dir = workspace::overlay_dir(&ctx.vai_dir, &workspace_id.to_string());
    let file_store = ctx.storage.files();

    let mut diff_files = Vec::new();
    for fc in file_changes {
        // Fetch old content.
        // Primary: content-addressable lookup by hash (works for all versions).
        // Fallback: snapshot directory written at merge time (pre-blob versions).
        let old_text = match fc.old_hash.as_deref() {
            Some(hash) => file_store
                .get(&ctx.repo_id, &format!("blobs/{hash}"))
                .await
                .ok()
                .and_then(|b| String::from_utf8(b).ok()),
            None => None,
        };
        let old_text = if old_text.is_none() {
            file_store
                .get(&ctx.repo_id, &format!("versions/{}/snapshot/{}", id, fc.path))
                .await
                .ok()
                .and_then(|b| String::from_utf8(b).ok())
                .or_else(|| {
                    let p = snapshot_dir.join(&fc.path);
                    if p.exists() { std::fs::read_to_string(&p).ok() } else { None }
                })
        } else {
            old_text
        };

        // Fetch new content.
        // Primary: content-addressable lookup by hash.
        // Fallback: workspace overlay path or local filesystem overlay.
        let new_text = match fc.new_hash.as_deref() {
            Some(hash) => file_store
                .get(&ctx.repo_id, &format!("blobs/{hash}"))
                .await
                .ok()
                .and_then(|b| String::from_utf8(b).ok()),
            None => None,
        };
        let new_text = if new_text.is_none() && fc.new_hash.is_some() {
            file_store
                .get(&ctx.repo_id, &format!("workspaces/{}/{}", workspace_id, fc.path))
                .await
                .ok()
                .and_then(|b| String::from_utf8(b).ok())
                .or_else(|| {
                    let p = overlay_dir.join(&fc.path);
                    if p.exists() { std::fs::read_to_string(&p).ok() } else { None }
                })
        } else {
            new_text
        };

        let change_type = match fc.change_type {
            version::VersionFileChangeType::Added => "added",
            version::VersionFileChangeType::Modified => "modified",
            version::VersionFileChangeType::Removed => "removed",
        };

        let diff = match (&old_text, &new_text) {
            (None, Some(new)) => {
                // Added: show entire file as additions.
                let patch = diffy::create_patch("", new);
                format!("{patch}")
            }
            (Some(old), None) => {
                // Removed: show entire old file as deletions.
                let patch = diffy::create_patch(old, "");
                format!("{patch}")
            }
            (Some(old), Some(new)) => {
                let patch = diffy::create_patch(old, new);
                format!("{patch}")
            }
            (None, None) => String::new(),
        };

        diff_files.push(VersionDiffFile {
            path: fc.path,
            change_type: change_type.to_string(),
            diff,
        });
    }

    Ok(Json(VersionDiffResponse {
        version_id: id,
        base_version_id,
        files: diff_files,
    }))
}

#[utoipa::path(
    post,
    path = "/api/versions/rollback",
    request_body = RollbackRequest,
    responses(
        (status = 200, description = "Rollback successful"),
        (status = 400, description = "Bad request", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Version not found", body = ErrorBody),
        (status = 409, description = "Downstream versions conflict", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "versions"
)]
/// `POST /api/versions/rollback` — rolls back the changes introduced by a
/// specific version by creating a new append-only version that restores the
/// prior state.
///
/// If `force` is `false` (the default) and downstream versions depend on the
/// target version's changes, returns **409 Conflict** with a JSON body
/// containing both an error message and the full `ImpactAnalysis`.
///
/// If `force` is `true`, the rollback proceeds regardless of downstream impact.
async fn rollback_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    Json(body): Json<RollbackRequest>,
) -> Response {
    if let Err(e) = require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Write).await {
        return e.into_response();
    }
    // Compute impact analysis before attempting the rollback.
    let impact = match version::analyze_rollback_impact(&ctx.vai_dir, &body.version) {
        Ok(i) => i,
        Err(e) => return ApiError::from(e).into_response(),
    };

    if !body.force && !impact.downstream_impacts.is_empty() {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "downstream versions depend on these changes; use \"force\": true to override",
                "impact": impact,
            })),
        )
            .into_response();
    }

    match version::rollback(&ctx.vai_dir, &ctx.repo_root, &body.version, None) {
        Ok(result) => Json(result).into_response(),
        Err(e) => ApiError::from(e).into_response(),
    }
}

// ── WebSocket handler ─────────────────────────────────────────────────────────

/// Query parameters for the WebSocket upgrade request.
#[derive(Debug, Deserialize)]
struct WsQueryParams {
    key: Option<String>,
    /// The `event_id` of the last event the agent received before disconnecting.
    ///
    /// When present, the server replays all buffered events that occurred after
    /// this ID (filtered by the agent's subscription).  If the buffer has been
    /// exceeded a `{"buffer_exceeded": true}` message is sent first so the
    /// agent knows to perform a full sync.
    last_event_id: Option<u64>,
}

/// `GET /ws/events?key=<api_key>` — upgrades the connection to WebSocket and
/// begins streaming events matching the client's subscription filter.
///
/// Authentication is via the `key` query parameter (plain API key string).
/// After connecting, the client must send a subscribe message:
///
/// ```json
/// { "subscribe": { "event_types": ["WorkspaceCreated"], "workspaces": [] } }
/// ```
///
/// An empty list for any field means "match all". Events are delivered as JSON
/// matching [`BroadcastEvent`]. The client can send updated subscribe messages
/// at any time to change the filter.
#[utoipa::path(
    get,
    path = "/ws/events",
    params(
        ("key" = String, Query, description = "API key for authentication"),
        ("last_event_id" = Option<u64>, Query, description = "Last received event ID for replay on reconnect"),
    ),
    responses(
        (status = 101, description = "WebSocket connection upgraded — events stream as JSON BroadcastEvent messages"),
        (status = 401, description = "Unauthorized — missing or invalid key"),
    ),
    tag = "status"
)]
async fn ws_events_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
    ctx: RepoCtx,
    AxumQuery(params): AxumQuery<WsQueryParams>,
) -> Response {
    let key_str = match params.key {
        Some(k) => k,
        None => {
            return ApiError::unauthorized(
                "missing `key` query parameter; use `?key=<api_key>`",
            )
            .into_response();
        }
    };

    // Admin key takes priority — allow it as a WebSocket credential.
    let agent_name = if key_str == state.admin_key {
        "admin".to_string()
    } else {
        // Validate via storage backend (handles both SQLite and Postgres).
        match state.storage.auth().validate_key(&key_str).await {
            Ok(api_key) => {
                tracing::debug!(agent = %api_key.name, "WebSocket connection authenticated");
                api_key.name
            }
            Err(crate::storage::StorageError::NotFound(_)) => {
                return ApiError::unauthorized("invalid or revoked API key").into_response();
            }
            Err(e) => {
                return ApiError::internal(format!("auth error: {e}")).into_response();
            }
        }
    };

    let last_event_id = params.last_event_id;

    // In server mode (Postgres), use LISTEN/NOTIFY-driven delivery.
    // In local mode (SQLite), fall back to the in-memory broadcast channel.
    match ctx.storage {
        crate::storage::StorageBackend::Server(ref pg)
        | crate::storage::StorageBackend::ServerWithS3(ref pg, _) => {
            let pg = Arc::clone(pg);
            let repo_id = ctx.repo_id;
            ws.on_upgrade(move |socket| {
                handle_ws_connection_pg(socket, pg, repo_id, agent_name, last_event_id)
            })
        }
        crate::storage::StorageBackend::Local(_) => {
            let event_rx = state.event_tx.subscribe();
            let event_buffer = Arc::clone(&state.event_buffer);
            ws.on_upgrade(move |socket| {
                handle_ws_connection(socket, event_rx, agent_name, event_buffer, last_event_id)
            })
        }
    }
}

/// Converts a WebSocket [`SubscriptionFilter`] into a storage [`EventFilter`]
/// so filter conditions can be pushed to the database layer.
///
/// Workspace IDs that cannot be parsed as UUIDs are silently dropped — they
/// cannot match any stored row.
fn subscription_to_event_filter(sub: &SubscriptionFilter) -> EventFilter {
    let workspace_ids = sub
        .workspaces
        .iter()
        .filter_map(|s| s.parse::<uuid::Uuid>().ok())
        .collect();
    EventFilter {
        event_types: sub.event_types.clone(),
        workspace_ids,
        entity_ids: sub.entities.clone(),
        paths: sub.paths.clone(),
    }
}

/// Returns `true` if `event` passes all non-empty dimensions of `filter`.
fn filter_matches(filter: &SubscriptionFilter, event: &BroadcastEvent) -> bool {
    // Event-type filter.
    if !filter.event_types.is_empty()
        && !filter.event_types.iter().any(|t| t == &event.event_type)
    {
        return false;
    }

    // Workspace filter.
    if !filter.workspaces.is_empty() {
        match &event.workspace_id {
            Some(ws) if filter.workspaces.contains(ws) => {}
            _ => return false,
        }
    }

    // Entity filter: check if any entity ID appears in event.data.
    if !filter.entities.is_empty() {
        let data_str = event.data.to_string();
        if !filter.entities.iter().any(|eid| data_str.contains(eid.as_str())) {
            return false;
        }
    }

    // Path filter: check if any path appears in event.data.
    if !filter.paths.is_empty() {
        let data_str = event.data.to_string();
        if !filter.paths.iter().any(|p| data_str.contains(p.as_str())) {
            return false;
        }
    }

    true
}

/// Manages a single WebSocket client connection.
///
/// Spawns a receiver task to handle incoming subscription messages while the
/// main task forwards matching events from the broadcast channel.
///
/// If `last_event_id` is `Some`, the server replays buffered events that the
/// agent missed since that ID (filtered by the agent's subscription filter).
/// The replay happens immediately after the first subscribe message arrives.
/// If the replay buffer has been exceeded a `{"buffer_exceeded": true}` JSON
/// message is sent before the replayed events so the agent knows to sync.
async fn handle_ws_connection(
    socket: WebSocket,
    mut event_rx: broadcast::Receiver<BroadcastEvent>,
    agent_name: String,
    event_buffer: Arc<StdMutex<EventBuffer>>,
    last_event_id: Option<u64>,
) {
    let (ws_tx, ws_rx) = socket.split();

    // Wrap the sender in Arc<Mutex> so it can be shared across tasks.
    let ws_tx = Arc::new(Mutex::new(ws_tx));

    // The current subscription filter, shared between the receiver task
    // (which updates it) and the event-forwarding loop (which reads it).
    // `None` means the client has not yet sent a subscribe message.
    let filter: Arc<Mutex<Option<SubscriptionFilter>>> = Arc::new(Mutex::new(None));
    let filter_for_recv = Arc::clone(&filter);
    let ws_tx_for_recv = Arc::clone(&ws_tx);

    // Whether the missed-event replay has already been performed for this
    // connection.  Reset to false on fresh connects (last_event_id == None).
    let replay_done = Arc::new(std::sync::atomic::AtomicBool::new(last_event_id.is_none()));
    let replay_done_for_recv = Arc::clone(&replay_done);

    // Spawn a task to handle incoming client messages (subscription updates).
    let recv_task = tokio::spawn(async move {
        let mut ws_rx = ws_rx;
        while let Some(msg) = ws_rx.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    match serde_json::from_str::<ClientMessage>(&text) {
                        Ok(ClientMessage::Subscribe { subscribe }) => {
                            tracing::debug!(
                                agent = %agent_name,
                                event_types = ?subscribe.event_types,
                                "WebSocket subscription updated"
                            );

                            // On the first subscribe message of a reconnection,
                            // replay any buffered events the agent missed.
                            if !replay_done_for_recv.swap(true, Ordering::Relaxed) {
                                if let Some(last_id) = last_event_id {
                                    let (buffer_exceeded, missed) = {
                                        match event_buffer.lock() {
                                            Ok(buf) => buf.events_since(last_id),
                                            Err(_) => (true, vec![]),
                                        }
                                    };

                                    let mut sender = ws_tx_for_recv.lock().await;

                                    if buffer_exceeded {
                                        let flag = serde_json::json!({ "buffer_exceeded": true })
                                            .to_string();
                                        if sender
                                            .send(Message::Text(flag))
                                            .await
                                            .is_err()
                                        {
                                            return;
                                        }
                                        tracing::info!(
                                            agent = %agent_name,
                                            last_event_id,
                                            "replay buffer exceeded; agent should sync"
                                        );
                                    }

                                    let count = missed.len();
                                    for event in missed {
                                        if filter_matches(&subscribe, &event) {
                                            match serde_json::to_string(&event) {
                                                Ok(json) => {
                                                    if sender
                                                        .send(Message::Text(json))
                                                        .await
                                                        .is_err()
                                                    {
                                                        return;
                                                    }
                                                }
                                                Err(e) => {
                                                    tracing::error!(
                                                        "failed to serialize replayed event: {e}"
                                                    );
                                                }
                                            }
                                        }
                                    }

                                    tracing::debug!(
                                        agent = %agent_name,
                                        replayed = count,
                                        "replayed missed events after reconnect"
                                    );
                                }
                            }

                            *filter_for_recv.lock().await = Some(subscribe);
                        }
                        Err(e) => {
                            tracing::debug!(error = %e, "invalid WebSocket message");
                            // Send an error back to the client.
                            let err_msg = serde_json::json!({ "error": format!("{e}") })
                                .to_string();
                            let _ = ws_tx_for_recv
                                .lock()
                                .await
                                .send(Message::Text(err_msg))
                                .await;
                        }
                    }
                }
                Ok(Message::Close(_)) | Err(_) => break,
                _ => {} // Ping/Pong/Binary — ignore.
            }
        }
    });

    // Forward matching events to the client until the channel closes or the
    // client disconnects.
    loop {
        match event_rx.recv().await {
            Ok(event) => {
                // Check filter (None = not yet subscribed, drop all events).
                let should_send = {
                    let guard = filter.lock().await;
                    guard
                        .as_ref()
                        .is_some_and(|f| filter_matches(f, &event))
                };

                if should_send {
                    let json = match serde_json::to_string(&event) {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::error!("failed to serialize broadcast event: {e}");
                            continue;
                        }
                    };
                    let send_result = ws_tx.lock().await.send(Message::Text(json)).await;
                    if send_result.is_err() {
                        break; // Client disconnected.
                    }
                }
            }
            Err(broadcast::error::RecvError::Closed) => break,
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!(skipped = n, "WebSocket client lagged behind event stream");
            }
        }
    }

    recv_task.abort();
}

/// Converts a storage [`crate::event_log::Event`] into the [`BroadcastEvent`]
/// wire format delivered to WebSocket clients.
fn event_to_broadcast(event: crate::event_log::Event) -> BroadcastEvent {
    let workspace_id = event.kind.workspace_id().map(|id| id.to_string());
    let event_type = event.kind.event_type().to_string();
    let data = serde_json::to_value(&event.kind).unwrap_or(serde_json::Value::Null);
    BroadcastEvent {
        event_type,
        event_id: event.id,
        workspace_id,
        timestamp: event.timestamp.to_rfc3339(),
        data,
    }
}

/// Manages a WebSocket connection backed by Postgres LISTEN/NOTIFY.
///
/// This is the server-mode counterpart to [`handle_ws_connection`]. Instead of
/// reading from an in-memory broadcast channel it:
///
/// 1. Creates a [`sqlx::postgres::PgListener`] on the `vai_events` channel.
/// 2. When the client sends a `subscribe` message and a `last_event_id` was
///    provided on connect, queries the database for missed events and delivers
///    them before switching to live delivery.
/// 3. On each `NOTIFY vai_events, '<repo_id>:<event_id>'`, queries all events
///    since the last delivered ID for the subscribed repo, applies the client's
///    subscription filter, and sends matching events.
async fn handle_ws_connection_pg(
    socket: WebSocket,
    pg: Arc<crate::storage::postgres::PostgresStorage>,
    repo_id: uuid::Uuid,
    agent_name: String,
    last_event_id: Option<u64>,
) {
    let (ws_tx, ws_rx) = socket.split();
    let ws_tx = Arc::new(Mutex::new(ws_tx));

    // Shared subscription filter — `None` until the client sends Subscribe.
    let filter: Arc<Mutex<Option<SubscriptionFilter>>> = Arc::new(Mutex::new(None));
    let filter_for_recv = Arc::clone(&filter);
    let ws_tx_for_recv = Arc::clone(&ws_tx);

    // Tracks the highest event ID we have delivered to this client.
    // Initialised to `last_event_id` so live NOTIFY delivery picks up from
    // where the client left off even if it sends no last_event_id (value 0
    // means "from the beginning").
    let last_delivered_id = Arc::new(AtomicU64::new(last_event_id.unwrap_or(0)));
    let last_delivered_for_recv = Arc::clone(&last_delivered_id);

    // Whether the missed-event replay has already been triggered.
    let replay_done = Arc::new(std::sync::atomic::AtomicBool::new(last_event_id.is_none()));
    let replay_done_for_recv = Arc::clone(&replay_done);

    let pg_for_recv = Arc::clone(&pg);

    // Spawn a task that reads incoming client messages (subscription updates).
    let recv_task = tokio::spawn(async move {
        let mut ws_rx = ws_rx;
        while let Some(msg) = ws_rx.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    match serde_json::from_str::<ClientMessage>(&text) {
                        Ok(ClientMessage::Subscribe { subscribe }) => {
                            tracing::debug!(
                                agent = %agent_name,
                                event_types = ?subscribe.event_types,
                                "WebSocket subscription updated (Postgres mode)"
                            );

                            // On the first subscribe message of a reconnection,
                            // replay any events the client missed — applying the
                            // subscription filter in the database query.
                            if !replay_done_for_recv.swap(true, Ordering::Relaxed) {
                                if let Some(last_id) = last_event_id {
                                    let ev_filter = subscription_to_event_filter(&subscribe);
                                    match pg_for_recv
                                        .query_since_id_filtered(
                                            &repo_id,
                                            last_id as i64,
                                            &ev_filter,
                                        )
                                        .await
                                    {
                                        Ok(events) => {
                                            let mut sender = ws_tx_for_recv.lock().await;
                                            let mut max_id = last_id;
                                            for event in events {
                                                let bc = event_to_broadcast(event);
                                                if bc.event_id > max_id {
                                                    max_id = bc.event_id;
                                                }
                                                match serde_json::to_string(&bc) {
                                                    Ok(json) => {
                                                        if sender
                                                            .send(Message::Text(json))
                                                            .await
                                                            .is_err()
                                                        {
                                                            return;
                                                        }
                                                    }
                                                    Err(e) => tracing::error!(
                                                        "replay serialize error: {e}"
                                                    ),
                                                }
                                            }
                                            // Advance the cursor past replayed events.
                                            last_delivered_for_recv
                                                .fetch_max(max_id, Ordering::Relaxed);
                                        }
                                        Err(e) => {
                                            tracing::error!("replay query failed: {e}");
                                        }
                                    }
                                }
                            }

                            *filter_for_recv.lock().await = Some(subscribe);
                        }
                        Err(e) => {
                            let err_msg =
                                serde_json::json!({ "error": format!("{e}") }).to_string();
                            let _ = ws_tx_for_recv
                                .lock()
                                .await
                                .send(Message::Text(err_msg))
                                .await;
                        }
                    }
                }
                Ok(Message::Close(_)) | Err(_) => break,
                _ => {}
            }
        }
    });

    // Create a Postgres listener on the `vai_events` channel.
    let mut listener = match pg.create_listener().await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("failed to create PgListener: {e}");
            recv_task.abort();
            return;
        }
    };
    if let Err(e) = listener.listen("vai_events").await {
        tracing::error!("failed to listen on vai_events: {e}");
        recv_task.abort();
        return;
    }

    // Forward events to the client whenever a NOTIFY arrives for this repo.
    loop {
        let notification = match listener.recv().await {
            Ok(n) => n,
            Err(e) => {
                tracing::error!("PgListener recv error: {e}");
                break;
            }
        };

        // Payload format: "<repo_id>:<event_id>" — lightweight pointer only.
        let payload = notification.payload();
        let Some((repo_str, event_id_str)) = payload.split_once(':') else {
            continue;
        };
        let Ok(notif_repo) = repo_str.parse::<uuid::Uuid>() else {
            continue;
        };
        let Ok(notif_event_id) = event_id_str.parse::<i64>() else {
            continue;
        };
        // Only handle NOTIFYs for the repo this client is subscribed to.
        if notif_repo != repo_id {
            continue;
        }

        // Gate delivery: client must have sent a subscribe message first.
        let current_filter = {
            let guard = filter.lock().await;
            guard.clone()
        };
        let Some(ref sub) = current_filter else {
            continue;
        };

        // Build a storage-level filter from the subscription so Postgres can
        // apply it in the query rather than loading all events into memory.
        let ev_filter = subscription_to_event_filter(sub);

        // Query only matching events since the last delivered ID.
        let since = last_delivered_id.load(Ordering::Relaxed) as i64;
        let events = match pg.query_since_id_filtered(&repo_id, since, &ev_filter).await {
            Ok(e) => e,
            Err(e) => {
                tracing::error!("query_since_id_filtered failed: {e}");
                // Still advance cursor so a transient error doesn't replay
                // non-matching events on the next NOTIFY.
                last_delivered_id.fetch_max(notif_event_id as u64, Ordering::Relaxed);
                continue;
            }
        };

        for event in events {
            let bc = event_to_broadcast(event);
            let json = match serde_json::to_string(&bc) {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!("serialize event failed: {e}");
                    continue;
                }
            };
            if ws_tx.lock().await.send(Message::Text(json)).await.is_err() {
                recv_task.abort();
                return;
            }
        }

        // Advance cursor to the notified event ID so subsequent NOTIFYs don't
        // re-scan events that were already considered (even if they didn't match
        // the filter).
        last_delivered_id.fetch_max(notif_event_id as u64, Ordering::Relaxed);
    }

    recv_task.abort();
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
}

/// Response body for a successful file upload.
#[derive(Debug, Serialize, ToSchema)]
struct UploadFilesResponse {
    /// Number of files successfully written.
    uploaded: usize,
    /// Repository-relative paths of all written files.
    paths: Vec<String>,
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

#[utoipa::path(
    post,
    path = "/api/workspaces/{id}/files",
    params(
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
    let _lock = state.repo_lock.lock().await;
    let mut meta = workspace::get(&ctx.vai_dir, &id).map_err(ApiError::from)?;
    let overlay = workspace::overlay_dir(&ctx.vai_dir, &id);
    let log_dir = ctx.vai_dir.join("event_log");
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

        let dest = overlay.join(&rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ApiError::internal(format!("create dirs: {e}")))?;
        }

        // Determine whether this is an add or a modify.
        let new_hash = sha256_hex(&content);
        let is_new = !dest.exists();
        let (old_hash, old_content) = if !is_new {
            let bytes = std::fs::read(&dest)
                .map_err(|e| ApiError::internal(format!("read existing overlay file: {e}")))?;
            let hash = sha256_hex(&bytes);
            (hash, Some(bytes))
        } else {
            (String::new(), None)
        };

        // Clone hashes before they are moved into the event log record.
        let new_hash_blob = new_hash.clone();
        let old_hash_blob = old_hash.clone();

        std::fs::write(&dest, &content)
            .map_err(|e| ApiError::internal(format!("write overlay file: {e}")))?;

        // Append to event log.
        let path_str = rel.to_string_lossy().replace('\\', "/");
        let mut log = EventLog::open(&log_dir)
            .map_err(|e| ApiError::internal(format!("event log: {e}")))?;
        if is_new {
            log.append(EventKind::FileAdded {
                workspace_id: workspace_uuid,
                path: path_str.clone(),
                hash: new_hash,
            })
            .map_err(|e| ApiError::internal(format!("event log append: {e}")))?;
        } else {
            log.append(EventKind::FileModified {
                workspace_id: workspace_uuid,
                path: path_str.clone(),
                old_hash,
                new_hash,
            })
            .map_err(|e| ApiError::internal(format!("event log append: {e}")))?;
        }

        // Persist to FileStore for durable diff generation (server/S3 deployments).
        {
            let file_store = ctx.storage.files();
            let store_key = format!("workspaces/{}/{}", id, path_str);
            let _ = file_store.put(&ctx.repo_id, &store_key, &content).await;
            // Also store content-addressably by hash so diffs can be computed for
            // all versions (including migrated ones) without relying on snapshots.
            let _ = file_store.put(&ctx.repo_id, &format!("blobs/{new_hash_blob}"), &content).await;
            if let Some(old_bytes) = old_content {
                let _ = file_store.put(&ctx.repo_id, &format!("blobs/{old_hash_blob}"), &old_bytes).await;
            }
        }

        uploaded_paths.push(path_str);
    }

    // Transition workspace to Active on first file upload.
    if meta.status == workspace::WorkspaceStatus::Created && !uploaded_paths.is_empty() {
        meta.status = workspace::WorkspaceStatus::Active;
        meta.updated_at = chrono::Utc::now();
        workspace::update_meta(&ctx.vai_dir, &meta)
            .map_err(|e| ApiError::internal(format!("update workspace meta: {e}")))?;
    }

    // Broadcast a WebSocket notification.
    state.broadcast(BroadcastEvent {
        event_type: "FilesUploaded".to_string(),
        event_id: 0,
        workspace_id: Some(id.clone()),
        timestamp: chrono::Utc::now().to_rfc3339(),
        data: serde_json::json!({ "paths": uploaded_paths }),
    });

    // Run conflict overlap detection and notify affected workspaces.
    {
        let mut engine = state.conflict_engine.lock().await;
        match engine.update_scope(workspace_uuid, &meta.intent, &uploaded_paths, &ctx.vai_dir) {
            Ok(overlaps) => {
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
            Err(e) => {
                tracing::warn!("conflict engine error: {e}");
            }
        }
    }

    let count = uploaded_paths.len();
    Ok((
        StatusCode::OK,
        Json(UploadFilesResponse {
            uploaded: count,
            paths: uploaded_paths,
        }),
    ))
}

#[utoipa::path(
    get,
    path = "/api/workspaces/{id}/files/{path}",
    params(
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
    // Verify workspace exists.
    workspace::get(&ctx.vai_dir, &id).map_err(ApiError::from)?;

    let rel = sanitize_path(&path)
        .ok_or_else(|| ApiError::bad_request(format!("invalid path: '{path}'")))?;

    // Try overlay first.
    let overlay_path = workspace::overlay_dir(&ctx.vai_dir, &id).join(&rel);
    let (content, found_in) = if overlay_path.exists() {
        let bytes = std::fs::read(&overlay_path)
            .map_err(|e| ApiError::internal(format!("read overlay file: {e}")))?;
        (bytes, "overlay".to_string())
    } else {
        let base_path = ctx.repo_root.join(&rel);
        if !base_path.exists() {
            return Err(ApiError::not_found(format!("file not found: '{path}'")));
        }
        let bytes = std::fs::read(&base_path)
            .map_err(|e| ApiError::internal(format!("read base file: {e}")))?;
        (bytes, "base".to_string())
    };

    let size = content.len();
    Ok(Json(FileDownloadResponse {
        path: rel.to_string_lossy().replace('\\', "/"),
        content_base64: BASE64.encode(&content),
        size,
        found_in,
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
    path = "/api/repo/files",
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
    let head_version = std::fs::read_to_string(ctx.vai_dir.join("head"))
        .map_err(|e| ApiError::internal(format!("read head: {e}")))?
        .trim()
        .to_string();

    let vai_toml_ignore = read_vai_toml_ignore(&ctx.repo_root);
    let mut files =
        crate::ignore_rules::collect_all_files_relative(&ctx.repo_root, &vai_toml_ignore);
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
    path = "/api/files",
    request_body = UploadFilesRequest,
    responses(
        (status = 200, description = "Files uploaded", body = UploadFilesResponse),
        (status = 400, description = "Bad request", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = [])),
    tag = "repos"
)]
/// `POST /api/files` — uploads source files into the repository root.
///
/// Used by `vai remote migrate` (PRD 12.3) to copy the local project directory
/// to the server after the metadata migration completes.  Files are written
/// directly to `repo_root`.  Call `POST /api/graph/refresh` afterwards to
/// rebuild the semantic graph from the uploaded files.
///
/// Accepts the same `{"files":[{"path":"…","content_base64":"…"}]}` format as
/// the workspace overlay upload endpoint.
async fn upload_source_files_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    ctx: RepoCtx,
    Json(body): Json<UploadFilesRequest>,
) -> Result<(StatusCode, Json<UploadFilesResponse>), ApiError> {
    require_repo_permission(
        &ctx.storage,
        &identity,
        &ctx.repo_id,
        crate::storage::RepoRole::Write,
    )
    .await?;
    let _lock = state.repo_lock.lock().await;

    let mut uploaded_paths: Vec<String> = Vec::new();

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

        let dest = ctx.repo_root.join(&rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ApiError::internal(format!("create dirs: {e}")))?;
        }
        std::fs::write(&dest, &content)
            .map_err(|e| ApiError::internal(format!("write source file: {e}")))?;

        // Store content-addressably so diffs can be computed for migrated versions.
        let hash = sha256_hex(&content);
        let _ = ctx.storage.files().put(&ctx.repo_id, &format!("blobs/{hash}"), &content).await;

        uploaded_paths.push(rel.to_string_lossy().replace('\\', "/"));
    }

    Ok((
        StatusCode::OK,
        Json(UploadFilesResponse {
            uploaded: uploaded_paths.len(),
            paths: uploaded_paths,
        }),
    ))
}

/// Response body for `POST /api/graph/refresh`.
#[derive(Debug, Serialize, ToSchema)]
struct ServerGraphRefreshResponse {
    /// Number of source files scanned during the rebuild.
    files_scanned: usize,
    /// Total entities in the graph after refresh.
    entities: usize,
    /// Total relationships in the graph after refresh.
    relationships: usize,
}

#[utoipa::path(
    post,
    path = "/api/graph/refresh",
    responses(
        (status = 200, description = "Graph refreshed", body = ServerGraphRefreshResponse),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = [])),
    tag = "graph"
)]
/// `POST /api/graph/refresh` — rebuilds the semantic graph from source files.
///
/// Should be called after `POST /api/files` completes to ensure the graph
/// reflects the uploaded source files (PRD 12.4).
///
/// Writes entity and relationship data via the configured [`GraphStore`] backend
/// (Postgres in server mode, SQLite in local mode) so the correct store is
/// always updated.
async fn server_graph_refresh_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    ctx: RepoCtx,
) -> Result<Json<ServerGraphRefreshResponse>, ApiError> {
    require_repo_permission(
        &ctx.storage,
        &identity,
        &ctx.repo_id,
        crate::storage::RepoRole::Write,
    )
    .await?;
    let _lock = state.repo_lock.lock().await;

    // Read ignore patterns from vai.toml (defaults if absent).
    let vai_toml_path = ctx.repo_root.join("vai.toml");
    let vai_toml: crate::repo::VaiToml = if vai_toml_path.exists() {
        let raw = std::fs::read_to_string(&vai_toml_path)
            .map_err(|e| ApiError::internal(format!("read vai.toml: {e}")))?;
        toml::from_str(&raw)
            .map_err(|e| ApiError::internal(format!("parse vai.toml: {e}")))?
    } else {
        crate::repo::VaiToml::default()
    };

    let source_files = crate::repo::collect_source_files(&ctx.repo_root, &vai_toml.ignore);
    let graph = ctx.storage.graph();
    let repo_id = ctx.repo_id;

    let mut files_scanned = 0usize;
    let mut total_entities = 0usize;
    let mut total_relationships = 0usize;

    for file_path in &source_files {
        let rel = file_path
            .strip_prefix(&ctx.repo_root)
            .unwrap_or(file_path)
            .to_string_lossy()
            .into_owned();

        let source = match std::fs::read(file_path) {
            Ok(b) => b,
            Err(_) => continue, // best-effort: skip unreadable files
        };

        let (entities, rels) = match crate::graph::parse_source_file(&rel, &source) {
            Ok(r) => r,
            Err(_) => continue, // best-effort: skip unparseable files
        };

        // Clear stale data for this file before upserting fresh entities.
        graph
            .clear_file(&repo_id, &rel)
            .await
            .map_err(|e| ApiError::internal(format!("clear graph for {rel}: {e}")))?;

        total_entities += entities.len();
        total_relationships += rels.len();

        if !entities.is_empty() {
            graph
                .upsert_entities(&repo_id, entities)
                .await
                .map_err(|e| ApiError::internal(format!("upsert entities for {rel}: {e}")))?;
        }
        if !rels.is_empty() {
            graph
                .upsert_relationships(&repo_id, rels)
                .await
                .map_err(|e| ApiError::internal(format!("upsert relationships for {rel}: {e}")))?;
        }

        files_scanned += 1;
    }

    Ok(Json(ServerGraphRefreshResponse {
        files_scanned,
        entities: total_entities,
        relationships: total_relationships,
    }))
}

#[utoipa::path(
    get,
    path = "/api/files/{path}",
    params(
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

    let file_path = ctx.repo_root.join(&rel);
    if !file_path.exists() {
        return Err(ApiError::not_found(format!("file not found: '{path}'")));
    }

    let content = std::fs::read(&file_path)
        .map_err(|e| ApiError::internal(format!("read file: {e}")))?;

    let size = content.len();
    Ok(Json(FileDownloadResponse {
        path: rel.to_string_lossy().replace('\\', "/"),
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
    /// Accepted for forward-compatibility; not yet used.
    #[allow(dead_code)]
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

#[utoipa::path(
    get,
    path = "/api/repos/{repo}/files/download",
    params(
        ("repo" = String, Path, description = "Repository name"),
        ("version" = Option<String>, Query, description = "Version to download (default: HEAD)"),
    ),
    responses(
        (status = 200, description = "Tar-gzip archive of all repo files", content_type = "application/gzip"),
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
/// The optional `?version=vN` query parameter is accepted for forward
/// compatibility but currently ignored; files are always read from the
/// current HEAD state on disk.
async fn files_download_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    AxumQuery(_query): AxumQuery<FilesDownloadQuery>,
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

    // Collect file paths to include in the archive (respects .gitignore, .vaignore, vai.toml).
    let vai_toml_ignore = read_vai_toml_ignore(&ctx.repo_root);
    let mut rel_paths =
        crate::ignore_rules::collect_all_files_relative(&ctx.repo_root, &vai_toml_ignore);
    rel_paths.sort();

    // Build an in-memory tar.gz archive.
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    {
        let mut archive = tar::Builder::new(&mut encoder);
        for rel in &rel_paths {
            let full = ctx.repo_root.join(rel);
            let content = std::fs::read(&full)
                .map_err(|e| ApiError::internal(format!("read file '{rel}': {e}")))?;
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
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
                let rel = sanitize_path(&path).ok_or_else(|| {
                    ApiError::bad_request(format!("invalid path: '{path}'"))
                })?;
                let full = ctx.repo_root.join(&rel);
                let content_base64 = if full.exists() {
                    let bytes = std::fs::read(&full)
                        .map_err(|e| ApiError::internal(format!("read '{path}': {e}")))?;
                    Some(BASE64.encode(&bytes))
                } else {
                    // File may have been merged but not yet written to repo_root
                    // (e.g. S3-only deployment). Omit content rather than error.
                    None
                };
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

// ── Graph API types ───────────────────────────────────────────────────────────

/// Query parameters for `GET /api/graph/entities`.
#[derive(Debug, Default, Deserialize, ToSchema)]
struct GraphEntityFilter {
    /// Filter by entity kind (e.g. `"function"`, `"struct"`).
    kind: Option<String>,
    /// Filter by exact file path (relative to repo root).
    file: Option<String>,
    /// Filter by entity name substring (case-insensitive).
    name: Option<String>,
}

/// Query parameters for `GET /api/graph/blast-radius`.
#[derive(Debug, Deserialize, ToSchema)]
struct BlastRadiusQuery {
    /// Comma-separated entity IDs to use as seeds.
    entities: String,
    /// Maximum traversal depth from each seed (default: 2).
    #[serde(default = "default_hops")]
    hops: usize,
}

fn default_hops() -> usize {
    2
}

/// Lightweight entity summary returned by graph list endpoints.
#[derive(Debug, Serialize, ToSchema)]
struct EntitySummary {
    id: String,
    kind: String,
    name: String,
    qualified_name: String,
    file: String,
    line_start: usize,
    line_end: usize,
    parent_entity: Option<String>,
}

impl From<crate::graph::Entity> for EntitySummary {
    fn from(e: crate::graph::Entity) -> Self {
        EntitySummary {
            id: e.id,
            kind: e.kind.to_string(),
            name: e.name,
            qualified_name: e.qualified_name,
            file: e.file_path,
            line_start: e.line_range.0,
            line_end: e.line_range.1,
            parent_entity: e.parent_entity,
        }
    }
}

/// Response body for `GET /api/graph/entities/:id`.
#[derive(Debug, Serialize, ToSchema)]
struct EntityDetailResponse {
    entity: EntitySummary,
    relationships: Vec<RelationshipSummary>,
}

/// Relationship summary used in graph API responses.
#[derive(Debug, Serialize, ToSchema)]
struct RelationshipSummary {
    id: String,
    kind: String,
    from_entity: String,
    to_entity: String,
}

impl From<crate::graph::Relationship> for RelationshipSummary {
    fn from(r: crate::graph::Relationship) -> Self {
        RelationshipSummary {
            id: r.id,
            kind: r.kind.as_str().to_string(),
            from_entity: r.from_entity,
            to_entity: r.to_entity,
        }
    }
}

/// Response body for `GET /api/graph/entities/:id/deps`.
#[derive(Debug, Serialize, ToSchema)]
struct EntityDepsResponse {
    entity_id: String,
    deps: Vec<EntitySummary>,
    relationships: Vec<RelationshipSummary>,
}

/// Response body for `GET /api/graph/blast-radius`.
#[derive(Debug, Serialize, ToSchema)]
struct BlastRadiusResponse {
    seed_entities: Vec<String>,
    hops: usize,
    entities: Vec<EntitySummary>,
    relationships: Vec<RelationshipSummary>,
}

// ── Graph API helpers ─────────────────────────────────────────────────────────

/// Opens the graph snapshot for the repository.
fn open_graph(vai_dir: &std::path::Path) -> Result<GraphSnapshot, ApiError> {
    let db_path = vai_dir.join("graph").join("snapshot.db");
    GraphSnapshot::open(&db_path).map_err(|e| ApiError::internal(format!("graph error: {e}")))
}

// ── Graph API handlers ────────────────────────────────────────────────────────

#[utoipa::path(
    get,
    path = "/api/graph/entities",
    params(
        ("kind" = Option<String>, Query, description = "Filter by entity kind (e.g. \"function\", \"struct\")"),
        ("file" = Option<String>, Query, description = "Filter by exact file path"),
        ("name" = Option<String>, Query, description = "Filter by entity name substring"),
    ),
    responses(
        (status = 200, description = "List of entities", body = Vec<EntitySummary>),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = [])),
    tag = "graph"
)]
/// `GET /api/graph/entities` — lists entities with optional filters.
///
/// Query params: `kind`, `file`, `name` (all optional, combined with AND).
async fn list_graph_entities_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    AxumQuery(filter): AxumQuery<GraphEntityFilter>,
) -> Result<Json<Vec<EntitySummary>>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Read).await?;
    let graph = open_graph(&ctx.vai_dir)?;
    let entities = graph
        .filter_entities(
            filter.kind.as_deref(),
            filter.file.as_deref(),
            filter.name.as_deref(),
        )
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(entities.into_iter().map(Into::into).collect()))
}

#[utoipa::path(
    get,
    path = "/api/graph/entities/{id}",
    params(
        ("id" = String, Path, description = "Entity ID"),
    ),
    responses(
        (status = 200, description = "Entity details and relationships", body = EntityDetailResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Entity not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "graph"
)]
/// `GET /api/graph/entities/:id` — entity details and its relationships.
async fn get_graph_entity_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    PathId(id): PathId,
) -> Result<Json<EntityDetailResponse>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Read).await?;
    let graph = open_graph(&ctx.vai_dir)?;
    let entity = graph
        .get_entity_by_id(&id)
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found(format!("entity '{id}' not found")))?;
    let relationships = graph
        .get_relationships_for_entity(&id)
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(EntityDetailResponse {
        entity: entity.into(),
        relationships: relationships.into_iter().map(Into::into).collect(),
    }))
}

#[utoipa::path(
    get,
    path = "/api/graph/entities/{id}/deps",
    params(
        ("id" = String, Path, description = "Entity ID"),
    ),
    responses(
        (status = 200, description = "Transitive dependencies of the entity", body = EntityDepsResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Entity not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "graph"
)]
/// `GET /api/graph/entities/:id/deps` — all entities transitively reachable
/// from this entity following any relationship direction.
async fn get_entity_deps_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    PathId(id): PathId,
) -> Result<Json<EntityDepsResponse>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Read).await?;
    let graph = open_graph(&ctx.vai_dir)?;
    // Verify the entity exists before traversal.
    graph
        .get_entity_by_id(&id)
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found(format!("entity '{id}' not found")))?;

    // Use a generous max-hops so we reach all transitive deps in practice.
    let (entities, relationships) = graph
        .reachable_entities(&[id.as_str()], 20)
        .map_err(|e| ApiError::internal(e.to_string()))?;

    // Exclude the seed entity itself from the deps list.
    let deps = entities
        .into_iter()
        .filter(|e| e.id != id)
        .map(Into::into)
        .collect();

    Ok(Json(EntityDepsResponse {
        entity_id: id,
        deps,
        relationships: relationships.into_iter().map(Into::into).collect(),
    }))
}

#[utoipa::path(
    get,
    path = "/api/graph/blast-radius",
    params(
        ("entities" = String, Query, description = "Comma-separated entity IDs"),
        ("hops" = Option<usize>, Query, description = "Maximum traversal depth (default: 2)"),
    ),
    responses(
        (status = 200, description = "Blast radius result", body = BlastRadiusResponse),
        (status = 400, description = "Bad request", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = [])),
    tag = "graph"
)]
/// `GET /api/graph/blast-radius` — entities reachable from a set of seeds within N hops.
///
/// Query params:
/// - `entities` — comma-separated entity IDs
/// - `hops` — max traversal depth (default: 2)
async fn get_blast_radius_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    AxumQuery(query): AxumQuery<BlastRadiusQuery>,
) -> Result<Json<BlastRadiusResponse>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Read).await?;
    let seed_ids: Vec<String> = query
        .entities
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    if seed_ids.is_empty() {
        return Err(ApiError::bad_request(
            "query param `entities` must contain at least one entity ID",
        ));
    }

    let hops = query.hops;
    let graph = open_graph(&ctx.vai_dir)?;

    let seed_refs: Vec<&str> = seed_ids.iter().map(String::as_str).collect();
    let (entities, relationships) = graph
        .reachable_entities(&seed_refs, hops)
        .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok(Json(BlastRadiusResponse {
        seed_entities: seed_ids,
        hops,
        entities: entities.into_iter().map(Into::into).collect(),
        relationships: relationships.into_iter().map(Into::into).collect(),
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
        .list_workspaces(&ctx.repo_id, true)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|ws| ws.issue_id == Some(issue_id))
        .map(|ws| ws.id)
        .collect()
}

#[utoipa::path(
    post,
    path = "/api/issues",
    request_body = CreateIssueRequest,
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
    use crate::issue::{AgentSource, IssueFilter, IssuePriority};
    use crate::storage::NewIssue;

    let _lock = state.repo_lock.lock().await;

    let priority = IssuePriority::from_str(&body.priority).ok_or_else(|| {
        ApiError::bad_request(format!("unknown priority `{}`", body.priority))
    })?;

    let issues = ctx.storage.issues();

    let (creator, agent_source, possible_duplicate_id) =
        if let Some(ref agent_id) = body.created_by_agent {
            // Agent-initiated path: apply rate-limiting and duplicate-detection.
            let all_issues = issues
                .list_issues(&ctx.repo_id, &IssueFilter::default())
                .await
                .map_err(ApiError::from)?;

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

    // Parse and validate dependency IDs.
    let mut dep_ids: Vec<uuid::Uuid> = Vec::new();
    for dep_str in &body.depends_on {
        let dep_id = uuid::Uuid::parse_str(dep_str)
            .map_err(|_| ApiError::bad_request(format!("invalid dependency ID `{dep_str}`")))?;
        // Verify the dependency exists.
        ctx.storage.issues()
            .get_issue(&ctx.repo_id, &dep_id)
            .await
            .map_err(|_| ApiError::bad_request(format!("dependency issue `{dep_id}` not found")))?;
        dep_ids.push(dep_id);
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
        depends_on: dep_ids,
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

    let mut resp = IssueResponse::from_issue(issue, vec![], vec![]);
    resp.possible_duplicate_of = possible_duplicate_id.map(|id| id.to_string());

    Ok((StatusCode::CREATED, Json(resp)))
}

#[utoipa::path(
    get,
    path = "/api/issues",
    params(
        ("status" = Option<String>, Query, description = "Filter by status (open, in_progress, closed)"),
        ("priority" = Option<String>, Query, description = "Filter by priority"),
        ("label" = Option<String>, Query, description = "Filter by label"),
        ("created_by" = Option<String>, Query, description = "Filter by creator"),
    ),
    responses(
        (status = 200, description = "List of issues", body = Vec<IssueResponse>),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = [])),
    tag = "issues"
)]
/// `GET /api/issues` — list issues with optional filters.
async fn list_issues_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    AxumQuery(query): AxumQuery<ListIssuesQuery>,
) -> Result<Json<Vec<IssueResponse>>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Read).await?;
    use crate::issue::{IssueFilter, IssueStatus, IssuePriority};

    let status = query.status.as_deref()
        .map(|s| IssueStatus::from_str(s).ok_or_else(|| ApiError::bad_request(format!("unknown status `{s}`"))))
        .transpose()?;
    let priority = query.priority.as_deref()
        .map(|p| IssuePriority::from_str(p).ok_or_else(|| ApiError::bad_request(format!("unknown priority `{p}`"))))
        .transpose()?;

    let filter = IssueFilter {
        status,
        priority,
        label: query.label,
        creator: query.created_by,
    };

    let issues = ctx.storage.issues()
        .list_issues(&ctx.repo_id, &filter)
        .await
        .map_err(ApiError::from)?;

    // Fetch all workspaces once to compute linked workspace IDs per issue.
    let all_workspaces = ctx.storage.workspaces()
        .list_workspaces(&ctx.repo_id, true)
        .await
        .unwrap_or_default();

    // Build reverse-dep map: issue_id → list of issue_ids that depend on it.
    let mut reverse_deps: std::collections::HashMap<uuid::Uuid, Vec<uuid::Uuid>> = std::collections::HashMap::new();
    for issue in &issues {
        for dep_id in &issue.depends_on {
            reverse_deps.entry(*dep_id).or_default().push(issue.id);
        }
    }

    let response = issues
        .into_iter()
        .map(|issue| {
            let linked: Vec<uuid::Uuid> = all_workspaces
                .iter()
                .filter(|ws| ws.issue_id == Some(issue.id))
                .map(|ws| ws.id)
                .collect();
            let blocked_by = reverse_deps.get(&issue.id).cloned().unwrap_or_default();
            IssueResponse::from_issue(issue, linked, blocked_by)
        })
        .collect();

    Ok(Json(response))
}

#[utoipa::path(
    get,
    path = "/api/issues/{id}",
    params(
        ("id" = String, Path, description = "Issue ID"),
    ),
    responses(
        (status = 200, description = "Issue details", body = IssueResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Issue not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "issues"
)]
/// `GET /api/issues/:id` — issue details with linked workspaces.
///
/// Returns 404 if the issue does not exist.
async fn get_issue_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    PathId(id): PathId,
) -> Result<Json<IssueResponse>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Read).await?;
    let issue_id = uuid::Uuid::parse_str(&id)
        .map_err(|_| ApiError::bad_request(format!("invalid issue ID `{id}`")))?;

    let issue = ctx.storage.issues()
        .get_issue(&ctx.repo_id, &issue_id)
        .await
        .map_err(ApiError::from)?;

    let all_issues = ctx.storage.issues()
        .list_issues(&ctx.repo_id, &crate::issue::IssueFilter::default())
        .await
        .unwrap_or_default();
    let blocked_by_issues: Vec<uuid::Uuid> = all_issues
        .iter()
        .filter(|i| i.depends_on.contains(&issue_id))
        .map(|i| i.id)
        .collect();

    let linked = linked_workspace_ids(&ctx, issue_id).await;

    Ok(Json(IssueResponse::from_issue(issue, linked, blocked_by_issues)))
}

#[utoipa::path(
    patch,
    path = "/api/issues/{id}",
    params(
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
    use crate::issue::IssuePriority;
    use crate::storage::IssueUpdate;

    let _lock = state.repo_lock.lock().await;

    let issue_id = uuid::Uuid::parse_str(&id)
        .map_err(|_| ApiError::bad_request(format!("invalid issue ID `{id}`")))?;

    let priority = body.priority.as_deref()
        .map(|p| IssuePriority::from_str(p).ok_or_else(|| ApiError::bad_request(format!("unknown priority `{p}`"))))
        .transpose()?;

    // Collect changed field names before moving body fields into update.
    let fields_changed: Vec<String> = [
        body.title.as_ref().map(|_| "title"),
        body.description.as_ref().map(|_| "description"),
        priority.as_ref().map(|_| "priority"),
        body.labels.as_ref().map(|_| "labels"),
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
    Ok(Json(IssueResponse::from_issue(issue, linked, vec![])))
}

#[utoipa::path(
    post,
    path = "/api/issues/{id}/close",
    params(
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

    let linked = linked_workspace_ids(&ctx, issue_id).await;
    Ok(Json(IssueResponse::from_issue(issue, linked, vec![])))
}

// ── Escalation handlers ───────────────────────────────────────────────────────

/// Response body for a single escalation.
#[derive(Debug, Serialize, ToSchema)]
struct EscalationResponse {
    id: String,
    escalation_type: String,
    severity: String,
    status: String,
    summary: String,
    intents: Vec<String>,
    agents: Vec<String>,
    workspace_ids: Vec<String>,
    affected_entities: Vec<String>,
    resolution_options: Vec<String>,
    resolution: Option<String>,
    resolved_by: Option<String>,
    created_at: String,
    resolved_at: Option<String>,
}

impl From<crate::escalation::Escalation> for EscalationResponse {
    fn from(e: crate::escalation::Escalation) -> Self {
        EscalationResponse {
            id: e.id.to_string(),
            escalation_type: e.escalation_type.as_str().to_string(),
            severity: e.severity.as_str().to_string(),
            status: e.status.as_str().to_string(),
            summary: e.summary,
            intents: e.intents,
            agents: e.agents,
            workspace_ids: e.workspace_ids.iter().map(|u| u.to_string()).collect(),
            affected_entities: e.affected_entities,
            resolution_options: e.resolution_options.iter().map(|o| o.as_str().to_string()).collect(),
            resolution: e.resolution.as_ref().map(|r| r.as_str().to_string()),
            resolved_by: e.resolved_by,
            created_at: e.created_at.to_rfc3339(),
            resolved_at: e.resolved_at.map(|t| t.to_rfc3339()),
        }
    }
}

/// Request body for `POST /api/escalations/:id/resolve`.
#[derive(Debug, Deserialize, ToSchema)]
struct ResolveEscalationRequest {
    /// Resolution option: keep_agent_a, keep_agent_b,
    /// send_back_to_agent_a, send_back_to_agent_b, pause_both.
    option: String,
    /// Identifier of the human or agent resolving this escalation.
    #[serde(default = "default_resolved_by")]
    resolved_by: String,
}

fn default_resolved_by() -> String {
    "api".to_string()
}

#[utoipa::path(
    get,
    path = "/api/escalations",
    params(
        ("status" = Option<String>, Query, description = "Filter by status (pending, resolved)"),
    ),
    responses(
        (status = 200, description = "List of escalations", body = Vec<EscalationResponse>),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = [])),
    tag = "escalations"
)]
/// `GET /api/escalations` — list escalations.
///
/// Optional `?status=pending|resolved` filter.
async fn list_escalations_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    AxumQuery(params): AxumQuery<ListEscalationsQuery>,
) -> Result<Json<Vec<EscalationResponse>>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Read).await?;
    let status_filter = params
        .status
        .as_deref()
        .map(|s| {
            EscalationStatus::from_str(s)
                .ok_or_else(|| ApiError::bad_request(format!("unknown status `{s}`")))
        })
        .transpose()?;

    let pending_only = matches!(status_filter, Some(EscalationStatus::Pending));
    let escalations = ctx.storage.escalations()
        .list_escalations(&ctx.repo_id, pending_only)
        .await
        .map_err(ApiError::from)?;

    // If a specific status other than Pending was requested (e.g. Resolved),
    // filter client-side since the trait only supports pending_only.
    let escalations = if let Some(ref sf) = status_filter {
        escalations.into_iter().filter(|e| &e.status == sf).collect()
    } else {
        escalations
    };

    Ok(Json(
        escalations.into_iter().map(EscalationResponse::from).collect(),
    ))
}

/// Query parameters for `GET /api/escalations`.
#[derive(Debug, Deserialize)]
struct ListEscalationsQuery {
    status: Option<String>,
}

#[utoipa::path(
    get,
    path = "/api/escalations/{id}",
    params(
        ("id" = String, Path, description = "Escalation ID"),
    ),
    responses(
        (status = 200, description = "Escalation details", body = EscalationResponse),
        (status = 404, description = "Escalation not found", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = [])),
    tag = "escalations"
)]
/// `GET /api/escalations/:id` — details for a single escalation.
///
/// Returns 404 if the escalation does not exist.
async fn get_escalation_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    PathId(id): PathId,
) -> Result<Json<EscalationResponse>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Read).await?;
    let esc_id = uuid::Uuid::parse_str(&id)
        .map_err(|_| ApiError::bad_request(format!("invalid escalation ID `{id}`")))?;

    let escalation = ctx.storage.escalations()
        .get_escalation(&ctx.repo_id, &esc_id)
        .await
        .map_err(ApiError::from)?;

    Ok(Json(EscalationResponse::from(escalation)))
}

#[utoipa::path(
    post,
    path = "/api/escalations/{id}/resolve",
    params(
        ("id" = String, Path, description = "Escalation ID"),
    ),
    request_body = ResolveEscalationRequest,
    responses(
        (status = 200, description = "Escalation resolved", body = EscalationResponse),
        (status = 400, description = "Bad request", body = ErrorBody),
        (status = 404, description = "Escalation not found", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = [])),
    tag = "escalations"
)]
/// `POST /api/escalations/:id/resolve` — resolve an escalation.
///
/// Returns 404 if the escalation does not exist.
/// Returns 400 if the escalation is already resolved or the option is invalid.
async fn resolve_escalation_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    ctx: RepoCtx,
    PathId(id): PathId,
    Json(body): Json<ResolveEscalationRequest>,
) -> Result<Json<EscalationResponse>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Write).await?;
    use crate::escalation::ResolutionOption;

    let esc_id = uuid::Uuid::parse_str(&id)
        .map_err(|_| ApiError::bad_request(format!("invalid escalation ID `{id}`")))?;

    let option = ResolutionOption::from_str(&body.option).ok_or_else(|| {
        ApiError::bad_request(format!(
            "unknown resolution option `{}`; valid options: keep_agent_a, keep_agent_b, \
             send_back_to_agent_a, send_back_to_agent_b, pause_both",
            body.option
        ))
    })?;

    let escalation = ctx.storage.escalations()
        .resolve_escalation(&ctx.repo_id, &esc_id, option, &body.resolved_by)
        .await
        .map_err(ApiError::from)?;

    // Broadcast the resolution.
    state.broadcast(BroadcastEvent {
        event_type: "EscalationResolved".to_string(),
        event_id: 0,
        workspace_id: escalation
            .workspace_ids
            .first()
            .map(|u| u.to_string()),
        timestamp: chrono::Utc::now().to_rfc3339(),
        data: serde_json::json!({
            "escalation_id": escalation.id,
            "resolution": escalation.resolution.as_ref().map(|r| r.as_str()),
            "resolved_by": escalation.resolved_by,
        }),
    });

    Ok(Json(EscalationResponse::from(escalation)))
}

// ── Work queue API types ──────────────────────────────────────────────────────

/// Request body for `POST /api/work-queue/claim`.
#[derive(Debug, Deserialize, ToSchema)]
struct ClaimWorkRequest {
    /// Issue ID to claim.
    issue_id: String,
}

// ── Work queue route handlers ─────────────────────────────────────────────────

#[utoipa::path(
    get,
    path = "/api/work-queue",
    responses(
        (status = 200, description = "Work queue", body = work_queue::WorkQueue),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = [])),
    tag = "work-queue"
)]
/// `GET /api/work-queue` — returns available and blocked work.
///
/// Predicts the scope of every open issue via keyword matching against the
/// semantic graph and checks each against active workspace scopes.
/// Results are ranked by priority (critical → high → medium → low).
async fn get_work_queue_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    ctx: RepoCtx,
) -> Result<Json<work_queue::WorkQueue>, ApiError> {
    use crate::issue::{IssueFilter, IssueStatus};

    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Read).await?;

    // Fetch open issues from storage (works for both SQLite and Postgres backends).
    let open_issues = ctx.storage.issues()
        .list_issues(&ctx.repo_id, &IssueFilter {
            status: Some(IssueStatus::Open),
            ..Default::default()
        })
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    // Fetch all issues for dependency status lookups.
    let all_issues_for_deps = ctx.storage.issues()
        .list_issues(&ctx.repo_id, &IssueFilter::default())
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    let issue_status_map: std::collections::HashMap<uuid::Uuid, &crate::issue::IssueStatus> =
        all_issues_for_deps.iter().map(|i| (i.id, &i.status)).collect();
    let issue_title_map: std::collections::HashMap<uuid::Uuid, &str> =
        all_issues_for_deps.iter().map(|i| (i.id, i.title.as_str())).collect();

    let engine = state.conflict_engine.lock().await;
    let active_scopes: Vec<_> = engine.all_scopes().cloned().collect();

    let mut available: Vec<work_queue::AvailableWork> = Vec::new();
    let mut blocked: Vec<work_queue::BlockedWork> = Vec::new();

    for issue in open_issues {
        // Check dependency blocking.
        let open_dep_titles: Vec<String> = issue.depends_on.iter()
            .filter_map(|dep_id| {
                issue_status_map.get(dep_id).map(|status| (dep_id, *status))
            })
            .filter(|(_, status)| {
                **status != crate::issue::IssueStatus::Closed
                    && **status != crate::issue::IssueStatus::Resolved
            })
            .filter_map(|(dep_id, _)| issue_title_map.get(dep_id).map(|t| t.to_string()))
            .collect();

        if !open_dep_titles.is_empty() {
            blocked.push(work_queue::BlockedWork {
                issue_id: issue.id.to_string(),
                title: issue.title.clone(),
                priority: issue.priority.as_str().to_string(),
                blocked_by: issue.depends_on.iter().map(|id| id.to_string()).collect(),
                reason: format!("Blocked by: {}", open_dep_titles.join(", ")),
            });
            continue;
        }

        let text = format!("{} {}", issue.title, issue.description);
        let prediction = work_queue::predict_scope(&text, &ctx.vai_dir)
            .map_err(|e| ApiError::internal(e.to_string()))?;

        let pred_ids = prediction.entity_ids();
        let pred_files = prediction.file_set();

        let mut conflicting_ws: Vec<String> = Vec::new();
        let mut reasons: Vec<String> = Vec::new();

        for scope in &active_scopes {
            let file_conflict = pred_files.iter().any(|f| scope.write_files.contains(f));
            let entity_conflict = pred_ids.iter().any(|id| scope.blast_radius.contains(id));
            if file_conflict || entity_conflict {
                conflicting_ws.push(scope.workspace_id.to_string());
                reasons.push(format!(
                    "workspace {} is modifying related code (intent: \"{}\")",
                    scope.workspace_id, scope.intent
                ));
            }
        }

        if conflicting_ws.is_empty() {
            available.push(work_queue::AvailableWork {
                issue_id: issue.id.to_string(),
                title: issue.title,
                priority: issue.priority.as_str().to_string(),
                predicted_scope: prediction,
            });
        } else {
            blocked.push(work_queue::BlockedWork {
                issue_id: issue.id.to_string(),
                title: issue.title,
                priority: issue.priority.as_str().to_string(),
                blocked_by: conflicting_ws,
                reason: reasons.join("; "),
            });
        }
    }

    // Secondary sort: within same priority, issues that unblock the most work come first.
    let dep_count_map: std::collections::HashMap<uuid::Uuid, usize> = {
        let mut map: std::collections::HashMap<uuid::Uuid, usize> = std::collections::HashMap::new();
        for issue in &all_issues_for_deps {
            for dep_id in &issue.depends_on {
                *map.entry(*dep_id).or_insert(0) += 1;
            }
        }
        map
    };
    available.sort_by_key(|w| {
        let issue_id = uuid::Uuid::parse_str(&w.issue_id).unwrap_or_default();
        let dep_count = dep_count_map.get(&issue_id).copied().unwrap_or(0);
        (work_queue::priority_rank(&w.priority), std::cmp::Reverse(dep_count))
    });
    blocked.sort_by_key(|w| work_queue::priority_rank(&w.priority));

    Ok(Json(work_queue::WorkQueue { available_work: available, blocked_work: blocked }))
}

#[utoipa::path(
    post,
    path = "/api/work-queue/claim",
    request_body = ClaimWorkRequest,
    responses(
        (status = 201, description = "Work claimed", body = work_queue::ClaimResult),
        (status = 404, description = "Issue not found", body = ErrorBody),
        (status = 409, description = "Issue no longer claimable", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = [])),
    tag = "work-queue"
)]
/// `POST /api/work-queue/claim` — atomically claim an issue and create a workspace.
///
/// Verifies the issue is still `Open` and uncontested, then creates a workspace
/// and transitions the issue to `InProgress`.  Returns 409 if the issue is no
/// longer open or if a conflict has appeared since the queue was last fetched
/// (caller should refresh the queue and retry with a different issue).
async fn claim_work_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    ctx: RepoCtx,
    Json(body): Json<ClaimWorkRequest>,
) -> Result<(StatusCode, Json<work_queue::ClaimResult>), ApiError> {
    use crate::issue::IssueStatus;
    use crate::storage::{IssueUpdate, NewWorkspace};

    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Write).await?;
    let _lock = state.repo_lock.lock().await;
    let issue_id = body.issue_id.parse::<uuid::Uuid>().map_err(|_| {
        ApiError::bad_request(format!("invalid issue_id: {}", body.issue_id))
    })?;

    // Fetch issue from storage (works for both SQLite and Postgres backends).
    let issue = ctx.storage.issues()
        .get_issue(&ctx.repo_id, &issue_id)
        .await
        .map_err(|e| match &e {
            crate::storage::StorageError::NotFound(_) => ApiError::not_found(e.to_string()),
            _ => ApiError::internal(e.to_string()),
        })?;

    // Guard: issue must still be Open.
    if issue.status != IssueStatus::Open {
        return Err(ApiError::conflict(format!(
            "Issue {issue_id} is no longer open — refresh the work queue and try again"
        )));
    }

    // Guard: re-check for conflicts against current active scopes.
    let engine = state.conflict_engine.lock().await;
    let text = format!("{} {}", issue.title, issue.description);
    let prediction = work_queue::predict_scope(&text, &ctx.vai_dir)
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let pred_ids = prediction.entity_ids();
    let pred_files = prediction.file_set();

    for scope in engine.all_scopes() {
        let file_conflict = pred_files.iter().any(|f| scope.write_files.contains(f));
        let entity_conflict = pred_ids.iter().any(|id| scope.blast_radius.contains(id));
        if file_conflict || entity_conflict {
            return Err(ApiError::conflict(format!(
                "Issue {issue_id} conflicts with active workspaces — refresh the work queue and try again"
            )));
        }
    }
    drop(engine);

    // Read HEAD from storage.
    let head = ctx.storage.versions()
        .read_head(&ctx.repo_id)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| "unknown".to_string());

    // Create workspace linked to this issue.
    let ws = ctx.storage.workspaces()
        .create_workspace(&ctx.repo_id, NewWorkspace {
            id: None,
            intent: issue.title.clone(),
            base_version: head,
            issue_id: Some(issue_id),
        })
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    // Transition issue to InProgress, linking the new workspace.
    ctx.storage.issues()
        .update_issue(&ctx.repo_id, &issue_id, IssueUpdate {
            status: Some(IssueStatus::InProgress),
            workspace_id: Some(ws.id),
            ..Default::default()
        })
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    let result = work_queue::ClaimResult {
        issue_id: issue_id.to_string(),
        workspace_id: ws.id.to_string(),
        intent: issue.title.clone(),
        predicted_scope: prediction,
    };

    // Append events to event store — triggers pg_notify in Postgres mode.
    let _ = ctx.storage.events()
        .append(&ctx.repo_id, EventKind::WorkspaceCreated {
            workspace_id: ws.id,
            intent: ws.intent.clone(),
            base_version: ws.base_version.clone(),
        })
        .await;
    let _ = ctx.storage.events()
        .append(&ctx.repo_id, EventKind::IssueLinkedToWorkspace {
            issue_id,
            workspace_id: ws.id,
        })
        .await;

    // Broadcast workspace creation event.
    state.broadcast(BroadcastEvent {
        event_type: "WorkspaceCreated".to_string(),
        event_id: 0,
        workspace_id: Some(result.workspace_id.clone()),
        timestamp: chrono::Utc::now().to_rfc3339(),
        data: serde_json::json!({
            "workspace_id": result.workspace_id,
            "intent": result.intent,
            "issue_id": result.issue_id,
            "claimed_via": "work_queue",
        }),
    });

    Ok((StatusCode::CREATED, Json(result)))
}

// ── Watcher handlers ──────────────────────────────────────────────────────────

/// Request body for `POST /api/watchers/register`.
#[derive(Debug, Deserialize, ToSchema)]
struct RegisterWatcherRequest {
    agent_id: String,
    watch_type: String,
    description: String,
    #[serde(default)]
    issue_creation_policy: IssueCreationPolicy,
}

/// Response body for watcher endpoints.
#[derive(Debug, Serialize, ToSchema)]
struct WatcherResponse {
    agent_id: String,
    watch_type: String,
    description: String,
    issue_creation_policy: IssueCreationPolicy,
    status: String,
    registered_at: String,
    last_discovery_at: Option<String>,
    discovery_count: u32,
}

impl From<Watcher> for WatcherResponse {
    fn from(w: Watcher) -> Self {
        WatcherResponse {
            agent_id: w.agent_id,
            watch_type: w.watch_type.as_str().to_string(),
            description: w.description,
            issue_creation_policy: w.issue_creation_policy,
            status: w.status.as_str().to_string(),
            registered_at: w.registered_at.to_rfc3339(),
            last_discovery_at: w.last_discovery_at.map(|d| d.to_rfc3339()),
            discovery_count: w.discovery_count,
        }
    }
}

#[utoipa::path(
    post,
    path = "/api/watchers/register",
    request_body = RegisterWatcherRequest,
    responses(
        (status = 201, description = "Watcher registered", body = WatcherResponse),
        (status = 400, description = "Bad request", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 409, description = "Watcher already registered", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "watchers"
)]
/// `POST /api/watchers/register` — register a new watcher agent.
async fn register_watcher_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    ctx: RepoCtx,
    Json(body): Json<RegisterWatcherRequest>,
) -> Result<(StatusCode, Json<WatcherResponse>), ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Write).await?;
    let _lock = state.repo_lock.lock().await;
    let store = WatcherStore::open(&ctx.vai_dir).map_err(|e| ApiError::internal(e.to_string()))?;
    let now = chrono::Utc::now();
    let watcher = Watcher {
        agent_id: body.agent_id,
        watch_type: WatchType::from_str(&body.watch_type),
        description: body.description,
        issue_creation_policy: body.issue_creation_policy,
        status: WatcherStatus::Active,
        registered_at: now,
        last_discovery_at: None,
        discovery_count: 0,
    };
    store.register(&watcher).map_err(|e| {
        use crate::watcher::WatcherError;
        match &e {
            WatcherError::AlreadyExists(_) => ApiError::conflict(e.to_string()),
            _ => ApiError::internal(e.to_string()),
        }
    })?;
    state.broadcast(BroadcastEvent {
        event_type: "WatcherRegistered".to_string(),
        event_id: 0,
        workspace_id: None,
        timestamp: now.to_rfc3339(),
        data: serde_json::json!({ "agent_id": watcher.agent_id }),
    });
    Ok((StatusCode::CREATED, Json(watcher.into())))
}

#[utoipa::path(
    get,
    path = "/api/watchers",
    responses(
        (status = 200, description = "List of registered watchers", body = Vec<WatcherResponse>),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = [])),
    tag = "watchers"
)]
/// `GET /api/watchers` — list all registered watchers.
async fn list_watchers_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
) -> Result<Json<Vec<WatcherResponse>>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Read).await?;
    let store = WatcherStore::open(&ctx.vai_dir).map_err(|e| ApiError::internal(e.to_string()))?;
    let watchers = store.list().map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(watchers.into_iter().map(Into::into).collect()))
}

#[utoipa::path(
    post,
    path = "/api/watchers/{id}/pause",
    params(
        ("id" = String, Path, description = "Watcher agent ID"),
    ),
    responses(
        (status = 200, description = "Watcher paused", body = WatcherResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Watcher not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "watchers"
)]
/// `POST /api/watchers/:id/pause` — pause a watcher.
async fn pause_watcher_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    ctx: RepoCtx,
    PathId(agent_id): PathId,
) -> Result<Json<WatcherResponse>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Write).await?;
    let _lock = state.repo_lock.lock().await;
    let store = WatcherStore::open(&ctx.vai_dir).map_err(|e| ApiError::internal(e.to_string()))?;
    store.pause(&agent_id).map_err(|e| {
        use crate::watcher::WatcherError;
        match &e {
            WatcherError::NotFound(_) => ApiError::not_found(e.to_string()),
            _ => ApiError::internal(e.to_string()),
        }
    })?;
    let watcher = store.get(&agent_id).map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(watcher.into()))
}

#[utoipa::path(
    post,
    path = "/api/watchers/{id}/resume",
    params(
        ("id" = String, Path, description = "Watcher agent ID"),
    ),
    responses(
        (status = 200, description = "Watcher resumed", body = WatcherResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Watcher not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "watchers"
)]
/// `POST /api/watchers/:id/resume` — resume a paused watcher.
async fn resume_watcher_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    ctx: RepoCtx,
    PathId(agent_id): PathId,
) -> Result<Json<WatcherResponse>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Write).await?;
    let _lock = state.repo_lock.lock().await;
    let store = WatcherStore::open(&ctx.vai_dir).map_err(|e| ApiError::internal(e.to_string()))?;
    store.resume(&agent_id).map_err(|e| {
        use crate::watcher::WatcherError;
        match &e {
            WatcherError::NotFound(_) => ApiError::not_found(e.to_string()),
            _ => ApiError::internal(e.to_string()),
        }
    })?;
    let watcher = store.get(&agent_id).map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(watcher.into()))
}

/// Request body for `POST /api/discoveries`.
#[derive(Debug, Deserialize, ToSchema)]
struct SubmitDiscoveryRequest {
    /// The watcher agent submitting this event.
    agent_id: String,
    /// The discovery event payload.
    event: DiscoveryEventKind,
}

/// Response body for `POST /api/discoveries`.
#[derive(Debug, Serialize, ToSchema)]
struct DiscoveryOutcomeResponse {
    record_id: String,
    agent_id: String,
    event_type: String,
    received_at: String,
    created_issue_id: Option<String>,
    suppressed: bool,
    message: String,
}

#[utoipa::path(
    post,
    path = "/api/discoveries",
    request_body = SubmitDiscoveryRequest,
    responses(
        (status = 201, description = "Discovery submitted", body = DiscoveryOutcomeResponse),
        (status = 400, description = "Bad request", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = [])),
    tag = "watchers"
)]
/// `POST /api/discoveries` — submit a discovery event from a watcher.
async fn submit_discovery_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    ctx: RepoCtx,
    Json(body): Json<SubmitDiscoveryRequest>,
) -> Result<(StatusCode, Json<DiscoveryOutcomeResponse>), ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Write).await?;
    let _lock = state.repo_lock.lock().await;
    let watcher_store = WatcherStore::open(&ctx.vai_dir)
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let issue_store = crate::issue::IssueStore::open(&ctx.vai_dir)
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let mut event_log = EventLog::open(&ctx.vai_dir)
        .map_err(|e| ApiError::internal(e.to_string()))?;

    let event_type = body.event.event_type().to_string();
    let outcome = watcher_store
        .submit_discovery(&body.agent_id, body.event, &issue_store, &mut event_log)
        .map_err(|e| {
            use crate::watcher::WatcherError;
            match &e {
                WatcherError::NotFound(_) => ApiError::not_found(e.to_string()),
                WatcherError::RateLimitExceeded { .. } => ApiError::rate_limited(e.to_string()),
                _ => ApiError::internal(e.to_string()),
            }
        })?;

    if let Some(issue_id) = outcome.issue_id {
        state.broadcast(BroadcastEvent {
            event_type: "IssueCreated".to_string(),
            event_id: 0,
            workspace_id: None,
            timestamp: outcome.record.received_at.to_rfc3339(),
            data: serde_json::json!({
                "issue_id": issue_id.to_string(),
                "source": "watcher_discovery",
                "watcher_agent_id": &body.agent_id,
            }),
        });
    }

    let status = if outcome.suppressed {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };

    Ok((
        status,
        Json(DiscoveryOutcomeResponse {
            record_id: outcome.record.id.to_string(),
            agent_id: outcome.record.agent_id,
            event_type,
            received_at: outcome.record.received_at.to_rfc3339(),
            created_issue_id: outcome.record.created_issue_id.map(|id| id.to_string()),
            suppressed: outcome.suppressed,
            message: outcome.message,
        }),
    ))
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
        let raw = std::fs::read_to_string(&path)?;
        serde_json::from_str(&raw).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    /// Saves the registry to `{storage_root}/registry.json`.
    fn save(&self, storage_root: &Path) -> Result<(), std::io::Error> {
        let path = storage_root.join("registry.json");
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
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

    // Create the directory and initialise the vai repo.
    let repo_root = storage_root.join(&body.name);
    std::fs::create_dir_all(&repo_root).map_err(|e| ApiError::internal(e.to_string()))?;

    // repo::init is synchronous and may do significant I/O; run on the
    // blocking thread pool so we don't stall the async executor.
    let repo_root_clone = repo_root.clone();
    let init_result = tokio::task::spawn_blocking(move || repo::init(&repo_root_clone))
        .await
        .map_err(|e| ApiError::internal(format!("task join error: {e}")))?
        .map_err(|e| ApiError::internal(format!("vai init failed: {e}")))?;

    let repo_id = init_result.config.repo_id;

    // If a Postgres backend is configured, insert the repo row so FK
    // constraints on events, issues, versions, and workspaces are satisfied.
    if let crate::storage::StorageBackend::Server(ref pg)
    | crate::storage::StorageBackend::ServerWithS3(ref pg, _) = state.storage
    {
        sqlx::query("INSERT INTO repos (id, name) VALUES ($1, $2) ON CONFLICT (id) DO NOTHING")
            .bind(repo_id)
            .bind(&body.name)
            .execute(pg.pool())
            .await
            .map_err(|e| ApiError::internal(format!("failed to insert repo into Postgres: {e}")))?;
        tracing::debug!(repo_id = %repo_id, name = %body.name, "repo inserted into Postgres");
    }

    let entry = RepoRegistryEntry {
        name: body.name.clone(),
        path: repo_root,
        created_at: chrono::Utc::now(),
    };

    // Persist the updated registry.
    registry.repos.push(entry.clone());
    registry.save(storage_root).map_err(|e| ApiError::internal(e.to_string()))?;

    tracing::info!(repo_id = %repo_id, name = %entry.name, path = %entry.path.display(), "repo registered");

    Ok((StatusCode::CREATED, Json(RepoResponse::from_entry(&entry))))
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
/// Returns an empty array if no repos are registered or if storage_root is not
/// set.
async fn list_repos_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<RepoResponse>>, ApiError> {
    require_server_admin(&identity)?;
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
        .create_user(NewUser { email: body.email, name: body.name })
        .await
        .map_err(ApiError::from)?;

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
    Ok(StatusCode::NO_CONTENT)
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
            let parsed = crate::storage::RepoRole::from_str(r);
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
        )
        .await
        .map_err(ApiError::from)?;

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
    Ok(StatusCode::NO_CONTENT)
}

// ── Migration handler (PRD 12.2) ──────────────────────────────────────────────

#[utoipa::path(
    post,
    path = "/api/migrate",
    request_body(content = inline(serde_json::Value), description = "Migration payload (events, issues, versions, escalations)"),
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
        StorageBackend::Server(pg) | StorageBackend::ServerWithS3(pg, _) => pg.clone(),
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
        .bind(&event_type)
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
                    creator, agent_source, resolution, created_at, updated_at)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
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
    path = "/api/migration-stats",
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
        StorageBackend::Server(pg) | StorageBackend::ServerWithS3(pg, _) => pg.clone(),
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
        get_workspace_file_handler,
        list_versions_handler,
        get_version_handler,
        get_version_diff_handler,
        rollback_handler,
        ws_events_handler,
        list_repo_files_handler,
        upload_source_files_handler,
        get_main_file_handler,
        server_graph_refresh_handler,
        list_graph_entities_handler,
        get_graph_entity_handler,
        get_entity_deps_handler,
        get_blast_radius_handler,
        create_issue_handler,
        list_issues_handler,
        get_issue_handler,
        update_issue_handler,
        close_issue_handler,
        list_escalations_handler,
        get_escalation_handler,
        resolve_escalation_handler,
        get_work_queue_handler,
        claim_work_handler,
        register_watcher_handler,
        list_watchers_handler,
        pause_watcher_handler,
        resume_watcher_handler,
        submit_discovery_handler,
        create_repo_handler,
        list_repos_handler,
        create_org_handler,
        list_orgs_handler,
        get_org_handler,
        delete_org_handler,
        create_user_handler,
        get_user_handler,
        add_org_member_handler,
        list_org_members_handler,
        update_org_member_handler,
        remove_org_member_handler,
        add_collaborator_handler,
        list_collaborators_handler,
        update_collaborator_handler,
        remove_collaborator_handler,
        create_key_handler,
        list_keys_handler,
        revoke_key_handler,
        migrate_handler,
        migration_stats_handler,
        openapi_handler,
        files_download_handler,
        files_pull_handler,
    ),
    components(
        schemas(
            BroadcastEvent,
            SubscriptionFilter,
            ErrorBody,
            StatusResponse,
            HealthResponse,
            ServerStatsResponse,
            CreateWorkspaceRequest,
            WorkspaceResponse,
            SubmitResponse,
            VersionDiffFile,
            VersionDiffResponse,
            RollbackRequest,
            CreateIssueRequest,
            AgentSourceRequest,
            UpdateIssueRequest,
            CloseIssueRequest,
            IssueResponse,
            FileUploadEntry,
            UploadFilesRequest,
            UploadFilesResponse,
            FileDownloadResponse,
            RepoFileListResponse,
            ServerGraphRefreshResponse,
            GraphEntityFilter,
            BlastRadiusQuery,
            EntitySummary,
            EntityDetailResponse,
            RelationshipSummary,
            EntityDepsResponse,
            BlastRadiusResponse,
            EscalationResponse,
            ResolveEscalationRequest,
            ClaimWorkRequest,
            RegisterWatcherRequest,
            WatcherResponse,
            SubmitDiscoveryRequest,
            DiscoveryOutcomeResponse,
            CreateRepoRequest,
            RepoResponse,
            CreateOrgRequest,
            CreateUserRequest,
            AddMemberRequest,
            UpdateMemberRequest,
            OrgResponse,
            UserResponse,
            OrgMemberResponse,
            AddCollaboratorRequest,
            UpdateCollaboratorRequest,
            CollaboratorResponse,
            CreateKeyRequest,
            CreateKeyResponse,
            ApiKeyResponse,
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
/// - All `/api/workspaces` routes
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
        .route("/ws/events", get(ws_events_handler));

    // Routes requiring `Authorization: Bearer <key>`.
    let protected = Router::new()
        .route("/api/workspaces", post(create_workspace_handler))
        .route("/api/workspaces", get(list_workspaces_handler))
        .route("/api/workspaces/:id", get(get_workspace_handler))
        .route("/api/workspaces/:id/submit", post(submit_workspace_handler))
        .route("/api/workspaces/:id/files", post(upload_workspace_files_handler))
        .route("/api/workspaces/:id/files/*path", get(get_workspace_file_handler))
        .route("/api/workspaces/:id", delete(discard_workspace_handler))
        .route("/api/repo/files", get(list_repo_files_handler))
        .route("/api/files", post(upload_source_files_handler))
        .route("/api/files/*path", get(get_main_file_handler))
        .route("/api/graph/refresh", post(server_graph_refresh_handler))
        .route("/api/versions", get(list_versions_handler))
        // Static route registered before the dynamic one so that
        // POST /api/versions/rollback is never captured by :id.
        .route("/api/versions/rollback", post(rollback_handler))
        .route("/api/versions/:id/diff", get(get_version_diff_handler))
        .route("/api/versions/:id", get(get_version_handler))
        // Graph query endpoints.
        .route("/api/graph/entities", get(list_graph_entities_handler))
        // Static sub-routes must come before the dynamic :id route.
        .route("/api/graph/blast-radius", get(get_blast_radius_handler))
        .route("/api/graph/entities/:id", get(get_graph_entity_handler))
        .route(
            "/api/graph/entities/:id/deps",
            get(get_entity_deps_handler),
        )
        // Issue management endpoints.
        .route("/api/issues", post(create_issue_handler))
        .route("/api/issues", get(list_issues_handler))
        // Static sub-routes must come before :id.
        .route("/api/issues/:id/close", post(close_issue_handler))
        .route("/api/issues/:id", get(get_issue_handler))
        .route("/api/issues/:id", axum::routing::patch(update_issue_handler))
        // Escalation endpoints.
        .route("/api/escalations", get(list_escalations_handler))
        // Static sub-routes must come before :id.
        .route("/api/escalations/:id/resolve", post(resolve_escalation_handler))
        .route("/api/escalations/:id", get(get_escalation_handler))
        // Work queue endpoints.
        .route("/api/work-queue", get(get_work_queue_handler))
        .route("/api/work-queue/claim", post(claim_work_handler))
        // Watcher registration and discovery endpoints.
        .route("/api/watchers/register", post(register_watcher_handler))
        .route("/api/watchers", get(list_watchers_handler))
        .route("/api/watchers/:id/pause", post(pause_watcher_handler))
        .route("/api/watchers/:id/resume", post(resume_watcher_handler))
        .route("/api/discoveries", post(submit_discovery_handler))
        // Migration endpoints (PRD 12.2, 12.5) — single-repo mode.
        .route("/api/migrate", post(migrate_handler))
        .route("/api/migration-stats", get(migration_stats_handler))
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
        .layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            auth_middleware,
        ));

    // Per-repo routes: `/api/repos/:repo/<resource>` mirrors the legacy routes
    // but resolves `vai_dir`/`repo_root` from the registry via
    // `repo_resolve_middleware`.  All the same handlers are reused — the
    // `RepoCtx` extractor picks up the per-repo paths from request extensions.
    let repo_scoped = Router::new()
        .route("/status", get(status_handler))
        .route("/workspaces", post(create_workspace_handler))
        .route("/workspaces", get(list_workspaces_handler))
        .route("/workspaces/:id", get(get_workspace_handler))
        .route("/workspaces/:id/submit", post(submit_workspace_handler))
        .route("/workspaces/:id/files", post(upload_workspace_files_handler))
        .route("/workspaces/:id/files/*path", get(get_workspace_file_handler))
        .route("/workspaces/:id", delete(discard_workspace_handler))
        .route("/files", get(list_repo_files_handler))
        .route("/files", post(upload_source_files_handler))
        // Static sub-routes must come before the wildcard `/files/*path`.
        .route("/files/download", get(files_download_handler))
        .route("/files/pull", get(files_pull_handler))
        .route("/files/*path", get(get_main_file_handler))
        .route("/versions", get(list_versions_handler))
        .route("/versions/rollback", post(rollback_handler))
        .route("/versions/:id/diff", get(get_version_diff_handler))
        .route("/versions/:id", get(get_version_handler))
        .route("/graph/entities", get(list_graph_entities_handler))
        .route("/graph/blast-radius", get(get_blast_radius_handler))
        .route("/graph/entities/:id", get(get_graph_entity_handler))
        .route("/graph/entities/:id/deps", get(get_entity_deps_handler))
        .route("/graph/refresh", post(server_graph_refresh_handler))
        .route("/issues", post(create_issue_handler))
        .route("/issues", get(list_issues_handler))
        .route("/issues/:id/close", post(close_issue_handler))
        .route("/issues/:id", get(get_issue_handler))
        .route("/issues/:id", axum::routing::patch(update_issue_handler))
        .route("/escalations", get(list_escalations_handler))
        .route("/escalations/:id/resolve", post(resolve_escalation_handler))
        .route("/escalations/:id", get(get_escalation_handler))
        .route("/work-queue", get(get_work_queue_handler))
        .route("/work-queue/claim", post(claim_work_handler))
        .route("/watchers/register", post(register_watcher_handler))
        .route("/watchers", get(list_watchers_handler))
        .route("/watchers/:id/pause", post(pause_watcher_handler))
        .route("/watchers/:id/resume", post(resume_watcher_handler))
        .route("/discoveries", post(submit_discovery_handler))
        .route("/ws/events", get(ws_events_handler))
        // Migration endpoints (PRD 12.2, 12.5) — multi-repo mode.
        .route("/migrate", post(migrate_handler))
        .route("/migration-stats", get(migration_stats_handler))
        // Apply repo resolution first (outermost = runs last).
        .layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            repo_resolve_middleware,
        ))
        // Auth runs before repo resolution so unauthenticated requests are
        // rejected cheaply before the registry lookup.
        .layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            auth_middleware,
        ));

    let cors = tower_http::cors::CorsLayer::new()
        .allow_origin(tower_http::cors::Any)
        .allow_methods(tower_http::cors::Any)
        .allow_headers([
            axum::http::header::CONTENT_TYPE,
            axum::http::header::AUTHORIZATION,
        ]);

    public
        .merge(protected)
        .nest("/api/repos/:repo", repo_scoped)
        .layer(cors)
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
    });

    let app = build_app(state);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    tokio::spawn(async move {
        axum::serve(listener, app)
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
        | crate::storage::StorageBackend::ServerWithS3(pg, _) => pg.clone(),
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
    });

    let app = build_app(state);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                shutdown_rx.await.ok();
            })
            .await
            .ok();
    });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    Ok((addr, shutdown_tx, repo_id))
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
        | crate::storage::StorageBackend::ServerWithS3(ref pg, _) = storage
    {
        let migrations_path = concat!(env!("CARGO_MANIFEST_DIR"), "/migrations");
        pg.migrate(migrations_path)
            .await
            .map_err(|e| ServerError::Io(std::io::Error::other(e.to_string())))?;
    }

    let admin_key = resolve_admin_key();

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
    });

    let app = build_app(state);

    let addr = config.socket_addr()?;
    let listener = TcpListener::bind(addr).await?;
    let actual_addr = listener.local_addr()?;

    // Write PID file if requested.
    if let Some(ref pid_path) = config.pid_file {
        let pid = std::process::id();
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

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(ServerError::Io)?;

    let stopped_at = chrono::Utc::now();
    tracing::info!(timestamp = %stopped_at.to_rfc3339(), "vai server stopped");
    println!("[{}] vai server stopped", stopped_at.format("%Y-%m-%dT%H:%M:%SZ"));

    // Remove PID file on clean shutdown.
    if let Some(ref pid_path) = config.pid_file {
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

/// Ensures the workspace is present on the local filesystem so `merge::submit` can run.
///
/// In Postgres server mode, workspaces are created in the database only; their
/// overlay files live in FileStore (S3). This function:
///
/// 1. Creates the workspace directory and `meta.toml` from storage if missing.
/// 2. Downloads overlay files from FileStore into the local overlay directory
///    if it is empty.
/// 3. Syncs the current HEAD from storage into the local `.vai/head` file.
/// 4. Creates a version TOML stub for the current HEAD so that
///    `version::next_version_id` returns the correct next ID.
/// 5. Downloads version snapshot files needed for the semantic merge path.
async fn prepare_workspace_for_submit(
    ctx: &RepoCtx,
    ws_meta: &workspace::WorkspaceMeta,
) -> Result<(), ApiError> {
    let ws_id = ws_meta.id.to_string();
    let ws_dir = ctx.vai_dir.join("workspaces").join(&ws_id);
    let overlay_dir = ws_dir.join("overlay");

    // ── 1. Create workspace directory and meta.toml if missing ────────────────
    if !ws_dir.exists() {
        std::fs::create_dir_all(&overlay_dir)
            .map_err(|e| ApiError::internal(format!("create workspace dir: {e}")))?;
        workspace::update_meta(&ctx.vai_dir, ws_meta)
            .map_err(|e| ApiError::internal(format!("write workspace meta: {e}")))?;
    }

    // ── 2. Download overlay files from FileStore if the overlay is empty ──────
    let overlay_is_empty = std::fs::read_dir(&overlay_dir)
        .map(|mut d| d.next().is_none())
        .unwrap_or(true);

    if overlay_is_empty {
        let file_store = ctx.storage.files();
        let prefix = format!("workspaces/{ws_id}/");
        // Best-effort — if FileStore is not configured (local SQLite mode) the
        // list will return an empty vec and we let merge::submit fail naturally.
        let files = file_store.list(&ctx.repo_id, &prefix).await.unwrap_or_default();

        for file_meta in files {
            let rel = file_meta
                .path
                .strip_prefix(&prefix)
                .unwrap_or(&file_meta.path)
                .to_string();
            if rel.is_empty() {
                continue;
            }
            let content = file_store
                .get(&ctx.repo_id, &file_meta.path)
                .await
                .map_err(|e| ApiError::internal(format!("download overlay file `{rel}`: {e}")))?;
            let dest = overlay_dir.join(&rel);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| ApiError::internal(format!("create overlay parent dir: {e}")))?;
            }
            std::fs::write(&dest, &content)
                .map_err(|e| ApiError::internal(format!("write overlay file `{rel}`: {e}")))?;
        }

        // Verify that we have something to merge.
        let still_empty = std::fs::read_dir(&overlay_dir)
            .map(|mut d| d.next().is_none())
            .unwrap_or(true);
        if still_empty {
            return Err(ApiError::bad_request("workspace has no overlay files to submit"));
        }
    }

    // ── 3. Sync HEAD from storage to local filesystem ─────────────────────────
    let current_head = ctx
        .storage
        .versions()
        .read_head(&ctx.repo_id)
        .await
        .map_err(|e| ApiError::internal(format!("read HEAD from storage: {e}")))?
        .unwrap_or_else(|| ws_meta.base_version.clone());

    std::fs::write(ctx.vai_dir.join("head"), format!("{current_head}\n"))
        .map_err(|e| ApiError::internal(format!("write local head file: {e}")))?;

    // ── 4. Ensure a version TOML exists for current HEAD ─────────────────────
    // `version::next_version_id` scans `.vai/versions/` to find the highest
    // version number.  In server mode that directory may be empty, so we write
    // at least a stub for the current HEAD version so the scan returns the
    // correct next ID.
    let versions_dir = ctx.vai_dir.join("versions");
    std::fs::create_dir_all(&versions_dir)
        .map_err(|e| ApiError::internal(format!("create versions dir: {e}")))?;

    let head_toml_path = versions_dir.join(format!("{current_head}.toml"));
    if !head_toml_path.exists() {
        let toml_str = ctx
            .storage
            .versions()
            .get_version(&ctx.repo_id, &current_head)
            .await
            .ok()
            .and_then(|vm| toml::to_string_pretty(&vm).ok())
            .unwrap_or_else(|| {
                format!(
                    "version_id = \"{current_head}\"\nintent = \"placeholder\"\n\
                     created_by = \"server\"\ncreated_at = \"{}\"\n",
                    chrono::Utc::now().to_rfc3339()
                )
            });
        std::fs::write(&head_toml_path, toml_str)
            .map_err(|e| ApiError::internal(format!("write version toml for {current_head}: {e}")))?;
    }

    // ── 5. Download snapshot files for semantic merge ─────────────────────────
    // The three-level merge reads pre-change snapshots from
    // `.vai/versions/<id>/snapshot/` for each version between the workspace
    // base and the current HEAD.  These were uploaded to FileStore by earlier
    // submit operations.
    let base_n = server_parse_version_num(&ws_meta.base_version);
    let head_n = server_parse_version_num(&current_head);

    if head_n > base_n {
        let file_store = ctx.storage.files();
        for n in (base_n + 1)..=head_n {
            let ver_id = format!("v{n}");

            // Also ensure a version TOML exists for every intermediate version
            // so the merge engine can scan the versions directory correctly.
            let ver_toml = versions_dir.join(format!("{ver_id}.toml"));
            if !ver_toml.exists() {
                if let Ok(vm) = ctx.storage.versions().get_version(&ctx.repo_id, &ver_id).await {
                    if let Ok(s) = toml::to_string_pretty(&vm) {
                        let _ = std::fs::write(&ver_toml, s);
                    }
                }
            }

            let snap_dir = versions_dir.join(&ver_id).join("snapshot");

            // Skip if snapshots are already on disk.
            if snap_dir.exists()
                && std::fs::read_dir(&snap_dir)
                    .map(|mut d| d.next().is_some())
                    .unwrap_or(false)
            {
                continue;
            }

            let prefix = format!("versions/{ver_id}/snapshot/");
            if let Ok(snap_files) = file_store.list(&ctx.repo_id, &prefix).await {
                for sf in snap_files {
                    let rel = sf
                        .path
                        .strip_prefix(&prefix)
                        .unwrap_or(&sf.path)
                        .to_string();
                    if rel.is_empty() {
                        continue;
                    }
                    if let Ok(content) = file_store.get(&ctx.repo_id, &sf.path).await {
                        let dest = snap_dir.join(&rel);
                        if let Some(parent) = dest.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        let _ = std::fs::write(&dest, content);
                    }
                }
            }
        }
    }

    Ok(())
}

/// Parses a version string like `"v3"` into the integer `3`.
/// Returns `0` for unrecognised formats.
fn server_parse_version_num(version: &str) -> u64 {
    version.trim_start_matches('v').parse::<u64>().unwrap_or(0)
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
        });

        let app = build_app(Arc::clone(&state));
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

        tokio::spawn(async move {
            axum::serve(listener, app)
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

        let (addr, shutdown_tx, _state, key) = start_test_server(root).await;
        let client = reqwest::Client::new();

        // POST /api/workspaces — create
        let resp = client
            .post(format!("http://{addr}/api/workspaces"))
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

        // GET /api/workspaces — list
        let resp = client
            .get(format!("http://{addr}/api/workspaces"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let list: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(list.as_array().unwrap().len(), 1);
        assert_eq!(list[0]["id"], ws_id.as_str());

        // GET /api/workspaces/:id — details
        let resp = client
            .get(format!("http://{addr}/api/workspaces/{ws_id}"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let detail: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(detail["id"], ws_id.as_str());
        assert_eq!(detail["intent"], "add hello world feature");

        // GET /api/workspaces/:id — 404 for unknown ID
        let resp = client
            .get(format!("http://{addr}/api/workspaces/nonexistent-id"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 404);

        // DELETE /api/workspaces/:id — discard
        let resp = client
            .delete(format!("http://{addr}/api/workspaces/{ws_id}"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 204, "expected 204 No Content");

        // After discard, workspace should not appear in list
        let resp = client
            .get(format!("http://{addr}/api/workspaces"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let list: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(
            list.as_array().unwrap().len(),
            0,
            "discarded workspace should not appear"
        );

        shutdown_tx.send(()).ok();
    }

    #[tokio::test]
    async fn submit_workspace_creates_new_version() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), b"pub fn hello() {}\n").unwrap();

        let (addr, shutdown_tx, _state, key) = start_test_server(root).await;
        let client = reqwest::Client::new();

        // Create a workspace.
        let resp = client
            .post(format!("http://{addr}/api/workspaces"))
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

        // POST /api/workspaces/:id/submit
        let resp = client
            .post(format!("http://{addr}/api/workspaces/{ws_id}/submit"))
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

        let (addr, shutdown_tx, _state, key) = start_test_server(root).await;
        let client = reqwest::Client::new();

        // 1. Authenticated request succeeds.
        let resp = client
            .get(format!("http://{addr}/api/workspaces"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200, "valid key should be accepted");

        // 2. Missing Authorization header returns 401.
        let resp = client
            .get(format!("http://{addr}/api/workspaces"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 401, "missing auth should return 401");

        // 3. Wrong key returns 401.
        let resp = client
            .get(format!("http://{addr}/api/workspaces"))
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
            .get(format!("http://{addr}/api/workspaces"))
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

        let (addr, shutdown_tx, _state, key) = start_test_server(root).await;

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
            .post(format!("http://{addr}/api/workspaces"))
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

        let (addr, shutdown_tx, _state, key) = start_test_server(root).await;

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
            .post(format!("http://{addr}/api/workspaces"))
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

        let (addr, shutdown_tx, _state, key) = start_test_server(root).await;
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
            .post(format!("http://{addr}/api/workspaces"))
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
            .post(format!("http://{addr}/api/workspaces"))
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
            .delete(format!("http://{addr}/api/workspaces/{ws_b_id}"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 204);

        // Create workspace C — also missed; matches subscription.
        let resp = client
            .post(format!("http://{addr}/api/workspaces"))
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
        let client = reqwest::Client::new();

        // Create a workspace to register one real event.
        let resp = client
            .post(format!("http://{addr}/api/workspaces"))
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
        intent: &str,
        overlay_content: &[u8],
    ) -> String {
        let client = reqwest::Client::new();

        // Create workspace.
        let resp = client
            .post(format!("http://{addr}/api/workspaces"))
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
            .post(format!("http://{addr}/api/workspaces/{ws_id}/submit"))
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

        let (addr, shutdown_tx, _state, key) = start_test_server(root).await;
        let client = reqwest::Client::new();

        // Initially only v1 exists.
        let resp = client
            .get(format!("http://{addr}/api/versions"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let versions: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(versions.as_array().unwrap().len(), 1);
        assert_eq!(versions[0]["version_id"], "v1");

        // Submit to create v2.
        create_version_via_submit(
            root,
            addr,
            &key,
            "add world",
            b"pub fn hello() {}\npub fn world() {}\n",
        )
        .await;

        // Now two versions.
        let resp = client
            .get(format!("http://{addr}/api/versions"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let versions: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(versions.as_array().unwrap().len(), 2);
        assert_eq!(versions[0]["version_id"], "v1");
        assert_eq!(versions[1]["version_id"], "v2");

        // ?limit=1 returns only v2 (most recent).
        let resp = client
            .get(format!("http://{addr}/api/versions?limit=1"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let versions: serde_json::Value = resp.json().await.unwrap();
        let arr = versions.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["version_id"], "v2");

        shutdown_tx.send(()).ok();
    }

    #[tokio::test]
    async fn get_version_details_endpoint() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), b"pub fn hello() {}\n").unwrap();

        let (addr, shutdown_tx, _state, key) = start_test_server(root).await;
        let client = reqwest::Client::new();

        // Create v2 with a new function.
        create_version_via_submit(
            root,
            addr,
            &key,
            "add world function",
            b"pub fn hello() {}\npub fn world() -> u32 { 42 }\n",
        )
        .await;

        // GET /api/versions/v2 returns version changes.
        let resp = client
            .get(format!("http://{addr}/api/versions/v2"))
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

        // GET /api/versions/v999 → 404.
        let resp = client
            .get(format!("http://{addr}/api/versions/v999"))
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

        let (addr, shutdown_tx, _state, key) = start_test_server(root).await;
        let client = reqwest::Client::new();

        // Create v2 by modifying src/lib.rs.
        create_version_via_submit(
            root,
            addr,
            &key,
            "add world function",
            b"pub fn hello() {}\npub fn world() -> u32 { 42 }\n",
        )
        .await;

        // GET /api/versions/v2/diff returns file diffs.
        let resp = client
            .get(format!("http://{addr}/api/versions/v2/diff"))
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

        // GET /api/versions/v1/diff → initial version has no file diffs.
        let resp = client
            .get(format!("http://{addr}/api/versions/v1/diff"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["files"].as_array().unwrap().len(), 0);

        // GET /api/versions/v999/diff → 404.
        let resp = client
            .get(format!("http://{addr}/api/versions/v999/diff"))
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

        let (addr, shutdown_tx, _state, key) = start_test_server(root).await;
        let client = reqwest::Client::new();

        // Create v2.
        create_version_via_submit(
            root,
            addr,
            &key,
            "add world",
            b"pub fn hello() {}\npub fn world() {}\n",
        )
        .await;

        // Rollback v2 — no downstream, so should succeed with force: false.
        let resp = client
            .post(format!("http://{addr}/api/versions/rollback"))
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

        let (addr, shutdown_tx, _state, key) = start_test_server(root).await;
        let client = reqwest::Client::new();

        // Create v2 modifying src/lib.rs.
        create_version_via_submit(
            root,
            addr,
            &key,
            "add world",
            b"pub fn hello() {}\npub fn world() {}\n",
        )
        .await;

        // Create v3 also modifying src/lib.rs (downstream of v2).
        create_version_via_submit(
            root,
            addr,
            &key,
            "add foo",
            b"pub fn hello() {}\npub fn world() {}\npub fn foo() {}\n",
        )
        .await;

        // Rolling back v2 without force should return 409 because v3 depends on it.
        let resp = client
            .post(format!("http://{addr}/api/versions/rollback"))
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
            .post(format!("http://{addr}/api/versions/rollback"))
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

        let (addr, shutdown_tx, _state, key) = start_test_server(root).await;
        let client = reqwest::Client::new();

        // Create a workspace.
        let resp = client
            .post(format!("http://{addr}/api/workspaces"))
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

        // Upload both files via POST /api/workspaces/:id/files.
        let resp = client
            .post(format!("http://{addr}/api/workspaces/{ws_id}/files"))
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
            .get(format!("http://{addr}/api/workspaces/{ws_id}"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        let ws_detail: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(ws_detail["status"], "Active", "workspace should be Active after upload");

        // Download text file from overlay.
        let resp = client
            .get(format!("http://{addr}/api/workspaces/{ws_id}/files/src/new.rs"))
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
            .get(format!("http://{addr}/api/workspaces/{ws_id}/files/data/bin.bin"))
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
            .get(format!("http://{addr}/api/workspaces/{ws_id}/files/src/lib.rs"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let dl: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(dl["found_in"], "base");
        let decoded = BASE64.decode(dl["content_base64"].as_str().unwrap()).unwrap();
        assert_eq!(decoded, b"pub fn hello() {}\n");

        // Download from main version via GET /api/files/:path.
        let resp = client
            .get(format!("http://{addr}/api/files/src/lib.rs"))
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
            .get(format!("http://{addr}/api/workspaces/{ws_id}/files/does/not/exist.txt"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 404);

        // 404 for non-existent main file.
        let resp = client
            .get(format!("http://{addr}/api/files/does/not/exist.txt"))
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

        let (addr, shutdown_tx, _state, key) = start_test_server(root).await;
        let client = reqwest::Client::new();

        // Create workspace.
        let resp = client
            .post(format!("http://{addr}/api/workspaces"))
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
            .post(format!("http://{addr}/api/workspaces/{ws_id}/files"))
            .bearer_auth(&key)
            .json(&upload_file(b"pub fn hello() {}\npub fn world() {}\n"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);

        // Second upload of the same path: FileModified.
        let resp = client
            .post(format!("http://{addr}/api/workspaces/{ws_id}/files"))
            .bearer_auth(&key)
            .json(&upload_file(b"pub fn hello() {}\npub fn world() {}\npub fn foo() {}\n"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);

        // Verify final content is the latest upload.
        let resp = client
            .get(format!("http://{addr}/api/workspaces/{ws_id}/files/src/lib.rs"))
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

        let (addr, shutdown_tx, _state, key) = start_test_server(root).await;
        let client = reqwest::Client::new();

        // Create workspace.
        let resp = client
            .post(format!("http://{addr}/api/workspaces"))
            .bearer_auth(&key)
            .json(&serde_json::json!({ "intent": "traversal test" }))
            .send()
            .await
            .unwrap();
        let ws: serde_json::Value = resp.json().await.unwrap();
        let ws_id = ws["id"].as_str().unwrap().to_string();

        let b64 = BASE64.encode(b"evil content");
        let resp = client
            .post(format!("http://{addr}/api/workspaces/{ws_id}/files"))
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

        let (addr, shutdown_tx, _state, key) = start_test_server(root).await;
        let client = reqwest::Client::new();

        // List all entities — graph was populated during init.
        let resp = client
            .get(format!("http://{addr}/api/graph/entities"))
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
            .get(format!("http://{addr}/api/graph/entities?kind=function"))
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
            .get(format!("http://{addr}/api/graph/entities?name=hello"))
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

        let (addr, shutdown_tx, _state, key) = start_test_server(root).await;
        let client = reqwest::Client::new();

        // Get all entities and pick one ID.
        let resp = client
            .get(format!("http://{addr}/api/graph/entities"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        let entities: serde_json::Value = resp.json().await.unwrap();
        let id = entities.as_array().unwrap()[0]["id"]
            .as_str()
            .unwrap()
            .to_string();

        // GET /api/graph/entities/:id
        let resp = client
            .get(format!("http://{addr}/api/graph/entities/{id}"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let detail: serde_json::Value = resp.json().await.unwrap();
        assert!(detail["entity"].is_object());
        assert!(detail["relationships"].is_array());
        assert_eq!(detail["entity"]["id"], id);

        // GET /api/graph/entities/:id/deps
        let resp = client
            .get(format!("http://{addr}/api/graph/entities/{id}/deps"))
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
            .get(format!("http://{addr}/api/graph/entities/nonexistent-id"))
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

        let (addr, shutdown_tx, _state, key) = start_test_server(root).await;
        let client = reqwest::Client::new();

        // Get one entity ID to use as seed.
        let resp = client
            .get(format!("http://{addr}/api/graph/entities"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        let entities: serde_json::Value = resp.json().await.unwrap();
        let id = entities.as_array().unwrap()[0]["id"]
            .as_str()
            .unwrap()
            .to_string();

        // GET /api/graph/blast-radius?entities=<id>&hops=2
        let resp = client
            .get(format!(
                "http://{addr}/api/graph/blast-radius?entities={id}&hops=2"
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
            .get(format!("http://{addr}/api/graph/blast-radius"))
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

        let (addr, shutdown_tx, _state, key) = start_test_server(root).await;
        let client = reqwest::Client::new();

        let resp = client
            .get(format!("http://{addr}/api/repo/files"))
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

        let (addr, shutdown_tx, _state, key) = start_test_server(root).await;

        let dest = tmp.path().join("cloned");
        let vai_url = format!("vai://{addr}/test-repo");
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

        let (addr, shutdown_tx, _state, key) = start_test_server(root).await;

        let remote = RemoteConfig {
            server_url: format!("http://{addr}"),
            api_key: key.clone(),
            repo_name: "test-repo".to_string(),
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

        let (addr, shutdown_tx, _state, key) = start_test_server(root).await;
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
            .post(format!("http://{addr}/api/issues"))
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
            .post(format!("http://{addr}/api/issues"))
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
            .get(format!("http://{addr}/api/issues"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let issues: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(issues.as_array().unwrap().len(), 2);

        // ── List with filter ──────────────────────────────────────────────────

        let resp = client
            .get(format!("http://{addr}/api/issues?priority=high"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let filtered: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(filtered.as_array().unwrap().len(), 1);
        assert_eq!(filtered[0]["title"], "Fix login bug");

        // Filter by creator.
        let resp = client
            .get(format!("http://{addr}/api/issues?created_by=agent-02"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        let by_creator: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(by_creator.as_array().unwrap().len(), 1);
        assert_eq!(by_creator[0]["title"], "Add rate limiting");

        // ── Get by ID ─────────────────────────────────────────────────────────

        let resp = client
            .get(format!("http://{addr}/api/issues/{issue_id}"))
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
            .get(format!("http://{addr}/api/issues/{fake_id}"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 404);

        // ── Update ────────────────────────────────────────────────────────────

        let resp = client
            .patch(format!("http://{addr}/api/issues/{issue_id}"))
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
            .post(format!("http://{addr}/api/issues/{issue_id}/close"))
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
            .get(format!("http://{addr}/api/issues?status=open"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        let open_issues: serde_json::Value = resp.json().await.unwrap();
        // Only the second issue (rate limiting) remains open.
        assert_eq!(open_issues.as_array().unwrap().len(), 1);
        assert_eq!(open_issues[0]["title"], "Add rate limiting");

        // ── Free-text resolution is accepted (any string allowed) ────────────────

        // Re-open by creating a fresh issue and closing it with a free-text resolution.
        let resp = client
            .post(format!("http://{addr}/api/issues"))
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
            .post(format!("http://{addr}/api/issues/{temp_id}/close"))
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

    /// Agent-initiated issue creation: guardrails (rate limit + duplicate detection).
    #[tokio::test]
    async fn agent_initiated_issues() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let (addr, shutdown_tx, _state, key) = start_test_server(root).await;
        let client = reqwest::Client::new();

        let agent_source = serde_json::json!({
            "source_type": "test_failure",
            "details": { "suite": "unit", "test": "auth::login" }
        });

        // ── Create first agent issue ──────────────────────────────────────────
        let resp = client
            .post(format!("http://{addr}/api/issues"))
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
            .get(format!("http://{addr}/api/issues?created_by=ci-agent"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        let by_agent: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(by_agent.as_array().unwrap().len(), 1);

        // ── Duplicate detection: similar title warns ──────────────────────────
        let resp = client
            .post(format!("http://{addr}/api/issues"))
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
            .post(format!("http://{addr}/api/issues"))
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
        let (addr, shutdown_tx, _state, key) = start_test_server(root).await;
        let client = reqwest::Client::new();

        // Create two open issues.
        let resp = client
            .post(format!("http://{addr}/api/issues"))
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
            .post(format!("http://{addr}/api/issues"))
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

        // ── GET /api/work-queue ────────────────────────────────────────────────

        let resp = client
            .get(format!("http://{addr}/api/work-queue"))
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

        // ── POST /api/work-queue/claim ─────────────────────────────────────────

        let resp = client
            .post(format!("http://{addr}/api/work-queue/claim"))
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
            .get(format!("http://{addr}/api/issues/{issue1_id}"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        let issue: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(issue["status"], "in_progress");

        // ── Claim a non-existent issue → 404 ──────────────────────────────────

        let resp = client
            .post(format!("http://{addr}/api/work-queue/claim"))
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
            .post(format!("http://{addr}/api/work-queue/claim"))
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
        });

        let app = build_app(Arc::clone(&state));
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        tokio::spawn(async move {
            axum::serve(listener, app)
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
        let workspaces = list.as_array().unwrap();
        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0]["id"], ws_id);

        // The legacy single-repo routes should NOT see this workspace
        // (it lives under the storage_root repo, not the server's own .vai/).
        let resp = client
            .get(format!("http://{addr}/api/workspaces"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let legacy: serde_json::Value = resp.json().await.unwrap();
        assert!(legacy.as_array().unwrap().is_empty());

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
        assert!(b_issues.as_array().unwrap().is_empty());

        // repo-a should have exactly one issue.
        let resp = client
            .get(format!("http://{addr}/api/repos/repo-a/issues"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let a_issues: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(a_issues.as_array().unwrap().len(), 1);
        assert_eq!(a_issues[0]["title"], "Issue in A");

        shutdown_tx.send(()).ok();
    }
}
