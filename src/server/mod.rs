//! HTTP server for vai — exposes REST API and WebSocket endpoints for
//! multi-agent coordination.
//!
//! Entry point: [`start`] — binds a TCP socket and serves the application.
//!
//! ## REST Endpoints
//!   - `GET /api/status` — server and repository health (unauthenticated)
//!   - `POST /api/workspaces` — create a new workspace
//!   - `GET /api/workspaces` — list active workspaces
//!   - `GET /api/workspaces/:id` — workspace details
//!   - `POST /api/workspaces/:id/submit` — submit workspace for merge
//!   - `DELETE /api/workspaces/:id` — discard a workspace
//!   - `GET /api/versions` — list version history (optional `?limit=N`)
//!   - `GET /api/versions/:id` — version details with entity-level changes
//!   - `POST /api/versions/rollback` — rollback to a prior version
//!   - `POST /api/workspaces/:id/files` — upload files into a workspace overlay (base64 JSON)
//!   - `GET /api/workspaces/:id/files/*path` — download a file from workspace (overlay or base)
//!   - `GET /api/files/*path` — download a file from the current main version
//!   - `GET /api/graph/entities` — list entities with optional filters (`?kind=`, `?file=`, `?name=`)
//!   - `GET /api/graph/entities/:id` — entity details and relationships
//!   - `GET /api/graph/entities/:id/deps` — transitive dependencies (bidirectional)
//!   - `GET /api/graph/blast-radius` — entities reachable from seeds (`?entities=id1,id2&hops=2`)
//!
//! ## WebSocket Endpoints
//!   - `GET /ws/events?key=<api_key>` — real-time event stream
//!
//! All REST endpoints except `GET /api/status` require
//! `Authorization: Bearer <key>`. WebSocket auth uses the `key` query param.
//! Keys are managed with `vai server keys`.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;
use std::time::Instant;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path as AxumPath, Query as AxumQuery, Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::net::TcpListener;
use tokio::sync::broadcast;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};

use crate::auth;
use crate::conflict;
use crate::event_log::{EventKind, EventLog};
use crate::graph::GraphSnapshot;
use crate::merge;
use crate::repo;
use crate::version;
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
    pub bind: String,
    /// TCP port to listen on (default: `7832`).
    pub port: u16,
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            bind: "127.0.0.1".to_string(),
            port: 7832,
        }
    }
}

impl ServerConfig {
    /// Returns the socket address derived from bind + port.
    pub fn socket_addr(&self) -> Result<SocketAddr, ServerError> {
        let raw = format!("{}:{}", self.bind, self.port);
        raw.parse().map_err(|source| ServerError::BadAddress {
            addr: raw,
            source,
        })
    }
}

// ── Broadcast event types ─────────────────────────────────────────────────────

/// Capacity of the broadcast channel (number of events buffered).
const EVENT_CHANNEL_CAPACITY: usize = 1024;

/// An event broadcast to all connected WebSocket clients.
///
/// Clients receive this as a JSON message on their WebSocket connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    pub data: serde_json::Value,
}

/// Subscription filter sent by the client after connecting.
///
/// An empty list for any field means "match all" for that dimension.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
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
    /// Conflict engine — tracks workspace scopes and detects overlaps.
    conflict_engine: Arc<Mutex<conflict::ConflictEngine>>,
}

impl AppState {
    /// Broadcast an event to all connected WebSocket clients.
    ///
    /// Silently drops the event if no clients are connected.
    pub(crate) fn broadcast(&self, event: BroadcastEvent) {
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

    match auth::validate(&state.vai_dir, &key_str) {
        Ok(Some(api_key)) => {
            tracing::debug!(agent = %api_key.name, "authenticated request");
            request.extensions_mut().insert(AgentIdentity {
                key_id: api_key.id,
                name: api_key.name,
            });
            next.run(request).await
        }
        Ok(None) => ApiError::unauthorized("invalid or revoked API key").into_response(),
        Err(e) => ApiError::internal(format!("auth error: {e}")).into_response(),
    }
}

// ── API error helper ──────────────────────────────────────────────────────────

/// JSON body for error responses.
#[derive(Debug, Serialize)]
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
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = Json(ErrorBody {
            error: self.message,
        });
        (self.status, body).into_response()
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

// ── API response types ────────────────────────────────────────────────────────

/// Response body for `GET /api/status`.
#[derive(Debug, Serialize)]
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
}

/// Request body for `POST /api/workspaces`.
#[derive(Debug, Deserialize)]
struct CreateWorkspaceRequest {
    /// Stated agent intent for this workspace.
    intent: String,
}

/// Response body for workspace creation and detail endpoints.
#[derive(Debug, Serialize)]
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
#[derive(Debug, Serialize)]
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

/// Request body for `POST /api/versions/rollback`.
#[derive(Debug, Deserialize)]
struct RollbackRequest {
    /// Version identifier to roll back (e.g., `"v3"`).
    version: String,
    /// If `true`, proceed even when downstream versions depend on the changes.
    /// If `false` (default) and downstream impacts exist, returns 409.
    #[serde(default)]
    force: bool,
}

// ── Route handlers ────────────────────────────────────────────────────────────

/// `GET /api/status` — returns live repository and server health info.
///
/// This is the only unauthenticated REST endpoint.
async fn status_handler(State(state): State<Arc<AppState>>) -> Json<StatusResponse> {
    let head = repo::read_head(&state.vai_dir).unwrap_or_else(|_| "unknown".to_string());
    let workspace_count = workspace::list(&state.vai_dir)
        .map(|w| w.len())
        .unwrap_or(0);

    Json(StatusResponse {
        repo_name: state.repo_name.clone(),
        head_version: head,
        uptime_secs: state.started_at.elapsed().as_secs(),
        workspace_count,
        vai_version: state.vai_version.clone(),
    })
}

/// `POST /api/workspaces` — creates a new workspace at the current HEAD.
///
/// Returns 201 Created with the workspace metadata.
/// Broadcasts a `WorkspaceCreated` event to WebSocket subscribers.
async fn create_workspace_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateWorkspaceRequest>,
) -> Result<(StatusCode, Json<WorkspaceResponse>), ApiError> {
    let head = repo::read_head(&state.vai_dir).map_err(|e| ApiError::internal(e.to_string()))?;
    let result = workspace::create(&state.vai_dir, &body.intent, &head)
        .map_err(ApiError::from)?;

    // Broadcast the workspace creation event to all WebSocket subscribers.
    let ws_id = result.workspace.id.to_string();
    state.broadcast(BroadcastEvent {
        event_type: "WorkspaceCreated".to_string(),
        event_id: 0, // event ID not surfaced by workspace::CreateResult
        workspace_id: Some(ws_id.clone()),
        timestamp: result.workspace.created_at.to_rfc3339(),
        data: serde_json::json!({
            "workspace_id": ws_id,
            "intent": result.workspace.intent,
            "base_version": result.workspace.base_version,
        }),
    });

    Ok((StatusCode::CREATED, Json(WorkspaceResponse::from(result.workspace))))
}

/// `GET /api/workspaces` — lists all active (non-discarded, non-merged) workspaces.
async fn list_workspaces_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<WorkspaceResponse>>, ApiError> {
    let workspaces = workspace::list(&state.vai_dir).map_err(ApiError::from)?;
    let response: Vec<WorkspaceResponse> = workspaces.into_iter().map(Into::into).collect();
    Ok(Json(response))
}

/// `GET /api/workspaces/:id` — returns details for a single workspace.
///
/// Returns 404 if the workspace does not exist.
async fn get_workspace_handler(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<WorkspaceResponse>, ApiError> {
    let meta = workspace::get(&state.vai_dir, &id).map_err(ApiError::from)?;
    Ok(Json(WorkspaceResponse::from(meta)))
}

/// `POST /api/workspaces/:id/submit` — submits a workspace for merge.
///
/// Switches the active workspace to `id`, then runs the merge engine.
/// Returns 409 Conflict if the merge cannot be auto-resolved.
/// Returns 404 if the workspace does not exist.
/// Broadcasts a `WorkspaceSubmitted` event to WebSocket subscribers.
async fn submit_workspace_handler(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<SubmitResponse>, ApiError> {
    // Verify the workspace exists before switching.
    let meta = workspace::get(&state.vai_dir, &id).map_err(ApiError::from)?;
    let workspace_uuid = meta.id;
    // Make it the active workspace so merge::submit can find it.
    workspace::switch(&state.vai_dir, &id).map_err(ApiError::from)?;
    let result = merge::submit(&state.vai_dir, &state.repo_root).map_err(ApiError::from)?;

    // Remove from conflict engine — workspace is no longer active.
    state.conflict_engine.lock().await.remove_workspace(&workspace_uuid);

    // Broadcast the submit/merge event.
    state.broadcast(BroadcastEvent {
        event_type: "WorkspaceSubmitted".to_string(),
        event_id: 0, // event ID not surfaced by merge result; use 0 as sentinel
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

/// `DELETE /api/workspaces/:id` — discards a workspace.
///
/// Returns 404 if the workspace does not exist.
/// Returns 204 No Content on success.
/// Broadcasts a `WorkspaceDiscarded` event to WebSocket subscribers.
async fn discard_workspace_handler(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<StatusCode, ApiError> {
    // Resolve UUID before discarding so we can remove it from the conflict engine.
    let ws_uuid = workspace::get(&state.vai_dir, &id)
        .map(|m| m.id)
        .ok();
    workspace::discard(&state.vai_dir, &id, None).map_err(ApiError::from)?;

    // Remove from conflict engine — workspace is no longer active.
    if let Some(uuid) = ws_uuid {
        state.conflict_engine.lock().await.remove_workspace(&uuid);
    }

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

/// `GET /api/versions` — lists all versions in chronological order.
///
/// Optional `?limit=N` query parameter truncates the result to the N most
/// recent versions (the list is already oldest-first, so we truncate from
/// the end after reversing).
async fn list_versions_handler(
    State(state): State<Arc<AppState>>,
    AxumQuery(params): AxumQuery<ListVersionsQuery>,
) -> Result<Json<Vec<version::VersionMeta>>, ApiError> {
    let mut versions =
        version::list_versions(&state.vai_dir).map_err(ApiError::from)?;
    if let Some(limit) = params.limit {
        // Keep the N most-recent: the list is oldest-first, so drop from the front.
        let len = versions.len();
        if limit < len {
            versions.drain(..len - limit);
        }
    }
    Ok(Json(versions))
}

/// `GET /api/versions/:id` — returns details for a single version, including
/// entity-level and file-level changes derived from the event log.
///
/// Returns 404 if the version does not exist.
async fn get_version_handler(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<version::VersionChanges>, ApiError> {
    let changes =
        version::get_version_changes(&state.vai_dir, &id).map_err(ApiError::from)?;
    Ok(Json(changes))
}

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
    State(state): State<Arc<AppState>>,
    Json(body): Json<RollbackRequest>,
) -> Response {
    // Compute impact analysis before attempting the rollback.
    let impact = match version::analyze_rollback_impact(&state.vai_dir, &body.version) {
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

    match version::rollback(&state.vai_dir, &state.repo_root, &body.version, None) {
        Ok(result) => Json(result).into_response(),
        Err(e) => ApiError::from(e).into_response(),
    }
}

// ── WebSocket handler ─────────────────────────────────────────────────────────

/// Query parameters for the WebSocket upgrade request.
#[derive(Debug, Deserialize)]
struct WsQueryParams {
    key: Option<String>,
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
async fn ws_events_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
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

    match auth::validate(&state.vai_dir, &key_str) {
        Ok(Some(api_key)) => {
            tracing::debug!(agent = %api_key.name, "WebSocket connection authenticated");
            let event_rx = state.event_tx.subscribe();
            ws.on_upgrade(move |socket| handle_ws_connection(socket, event_rx, api_key.name))
        }
        Ok(None) => ApiError::unauthorized("invalid or revoked API key").into_response(),
        Err(e) => ApiError::internal(format!("auth error: {e}")).into_response(),
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
async fn handle_ws_connection(
    socket: WebSocket,
    mut event_rx: broadcast::Receiver<BroadcastEvent>,
    agent_name: String,
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

// ── File upload / download ────────────────────────────────────────────────────

/// Maximum allowed size for a single uploaded file (10 MiB).
const MAX_FILE_SIZE_BYTES: usize = 10 * 1024 * 1024;

/// A single file entry within an upload request.
#[derive(Debug, Deserialize)]
struct FileUploadEntry {
    /// Path relative to the repository root (e.g. `src/auth.rs`).
    path: String,
    /// File content encoded as standard (padded) base64.
    content_base64: String,
}

/// Request body for `POST /api/workspaces/:id/files`.
#[derive(Debug, Deserialize)]
struct UploadFilesRequest {
    /// One or more files to upload into the workspace overlay.
    files: Vec<FileUploadEntry>,
}

/// Response body for a successful file upload.
#[derive(Debug, Serialize)]
struct UploadFilesResponse {
    /// Number of files successfully written.
    uploaded: usize,
    /// Repository-relative paths of all written files.
    paths: Vec<String>,
}

/// Response body for file download endpoints.
#[derive(Debug, Serialize)]
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
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<UploadFilesRequest>,
) -> Result<(StatusCode, Json<UploadFilesResponse>), ApiError> {
    let mut meta = workspace::get(&state.vai_dir, &id).map_err(ApiError::from)?;
    let overlay = workspace::overlay_dir(&state.vai_dir, &id);
    let log_dir = state.vai_dir.join("event_log");
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
        let old_hash = if !is_new {
            sha256_hex(
                &std::fs::read(&dest)
                    .map_err(|e| ApiError::internal(format!("read existing overlay file: {e}")))?,
            )
        } else {
            String::new()
        };

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

        uploaded_paths.push(path_str);
    }

    // Transition workspace to Active on first file upload.
    if meta.status == workspace::WorkspaceStatus::Created && !uploaded_paths.is_empty() {
        meta.status = workspace::WorkspaceStatus::Active;
        meta.updated_at = chrono::Utc::now();
        workspace::update_meta(&state.vai_dir, &meta)
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
        match engine.update_scope(workspace_uuid, &meta.intent, &uploaded_paths, &state.vai_dir) {
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

/// `GET /api/workspaces/:id/files/*path` — downloads a file from a workspace.
///
/// The overlay is checked first; if the file is not present there the base
/// repository (repo root) is used as a fallback. Returns 404 if the file
/// exists in neither location. Response includes `found_in: "overlay"|"base"`.
async fn get_workspace_file_handler(
    State(state): State<Arc<AppState>>,
    AxumPath((id, path)): AxumPath<(String, String)>,
) -> Result<Json<FileDownloadResponse>, ApiError> {
    // Verify workspace exists.
    workspace::get(&state.vai_dir, &id).map_err(ApiError::from)?;

    let rel = sanitize_path(&path)
        .ok_or_else(|| ApiError::bad_request(format!("invalid path: '{path}'")))?;

    // Try overlay first.
    let overlay_path = workspace::overlay_dir(&state.vai_dir, &id).join(&rel);
    let (content, found_in) = if overlay_path.exists() {
        let bytes = std::fs::read(&overlay_path)
            .map_err(|e| ApiError::internal(format!("read overlay file: {e}")))?;
        (bytes, "overlay".to_string())
    } else {
        let base_path = state.repo_root.join(&rel);
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
#[derive(Debug, Serialize)]
struct RepoFileListResponse {
    /// Relative paths of all files in the repository root, sorted.
    files: Vec<String>,
    /// Total number of files.
    count: usize,
    /// Current HEAD version of the repository.
    head_version: String,
}

/// `GET /api/repo/files` — lists every file in the current main codebase.
///
/// Returns relative paths suitable for use with `GET /api/files/*path`.
/// Hidden directories (`.git`, `.vai`) and common build artefacts are excluded.
async fn list_repo_files_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Json<RepoFileListResponse>, ApiError> {
    let head_version = std::fs::read_to_string(state.vai_dir.join("head"))
        .map_err(|e| ApiError::internal(format!("read head: {e}")))?
        .trim()
        .to_string();

    let mut files = Vec::new();
    collect_repo_files(&state.repo_root, &state.repo_root, &mut files)
        .map_err(|e| ApiError::internal(format!("list files: {e}")))?;
    files.sort();

    let count = files.len();
    Ok(Json(RepoFileListResponse {
        files,
        count,
        head_version,
    }))
}

/// Recursively collects relative file paths under `dir`, skipping common
/// build artefacts and hidden directories (`.vai`, `.git`, `target`, etc.).
fn collect_repo_files(
    root: &std::path::Path,
    dir: &std::path::Path,
    out: &mut Vec<String>,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        // Skip hidden/build directories.
        if name.starts_with('.') || name == "target" || name == "node_modules" {
            continue;
        }
        if path.is_dir() {
            collect_repo_files(root, &path, out)?;
        } else {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            out.push(rel);
        }
    }
    Ok(())
}

/// `GET /api/files/*path` — downloads a file from the current main version.
///
/// Returns the file as base64-encoded content. Returns 404 if not found.
async fn get_main_file_handler(
    State(state): State<Arc<AppState>>,
    AxumPath(path): AxumPath<String>,
) -> Result<Json<FileDownloadResponse>, ApiError> {
    let rel = sanitize_path(&path)
        .ok_or_else(|| ApiError::bad_request(format!("invalid path: '{path}'")))?;

    let file_path = state.repo_root.join(&rel);
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

// ── Graph API types ───────────────────────────────────────────────────────────

/// Query parameters for `GET /api/graph/entities`.
#[derive(Debug, Default, Deserialize)]
struct GraphEntityFilter {
    /// Filter by entity kind (e.g. `"function"`, `"struct"`).
    kind: Option<String>,
    /// Filter by exact file path (relative to repo root).
    file: Option<String>,
    /// Filter by entity name substring (case-insensitive).
    name: Option<String>,
}

/// Query parameters for `GET /api/graph/blast-radius`.
#[derive(Debug, Deserialize)]
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
#[derive(Debug, Serialize)]
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
#[derive(Debug, Serialize)]
struct EntityDetailResponse {
    entity: EntitySummary,
    relationships: Vec<RelationshipSummary>,
}

/// Relationship summary used in graph API responses.
#[derive(Debug, Serialize)]
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
#[derive(Debug, Serialize)]
struct EntityDepsResponse {
    entity_id: String,
    deps: Vec<EntitySummary>,
    relationships: Vec<RelationshipSummary>,
}

/// Response body for `GET /api/graph/blast-radius`.
#[derive(Debug, Serialize)]
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

/// `GET /api/graph/entities` — lists entities with optional filters.
///
/// Query params: `kind`, `file`, `name` (all optional, combined with AND).
async fn list_graph_entities_handler(
    State(state): State<Arc<AppState>>,
    AxumQuery(filter): AxumQuery<GraphEntityFilter>,
) -> Result<Json<Vec<EntitySummary>>, ApiError> {
    let graph = open_graph(&state.vai_dir)?;
    let entities = graph
        .filter_entities(
            filter.kind.as_deref(),
            filter.file.as_deref(),
            filter.name.as_deref(),
        )
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(entities.into_iter().map(Into::into).collect()))
}

/// `GET /api/graph/entities/:id` — entity details and its relationships.
async fn get_graph_entity_handler(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<EntityDetailResponse>, ApiError> {
    let graph = open_graph(&state.vai_dir)?;
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

/// `GET /api/graph/entities/:id/deps` — all entities transitively reachable
/// from this entity following any relationship direction.
async fn get_entity_deps_handler(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<EntityDepsResponse>, ApiError> {
    let graph = open_graph(&state.vai_dir)?;
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

/// `GET /api/graph/blast-radius` — entities reachable from a set of seeds within N hops.
///
/// Query params:
/// - `entities` — comma-separated entity IDs
/// - `hops` — max traversal depth (default: 2)
async fn get_blast_radius_handler(
    State(state): State<Arc<AppState>>,
    AxumQuery(query): AxumQuery<BlastRadiusQuery>,
) -> Result<Json<BlastRadiusResponse>, ApiError> {
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
    let graph = open_graph(&state.vai_dir)?;

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
        .route("/api/status", get(status_handler))
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
        .route("/api/files/*path", get(get_main_file_handler))
        .route("/api/versions", get(list_versions_handler))
        // Static route registered before the dynamic one so that
        // POST /api/versions/rollback is never captured by :id.
        .route("/api/versions/rollback", post(rollback_handler))
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
        .layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            auth_middleware,
        ));

    public.merge(protected).with_state(state)
}

// ── Public start function ─────────────────────────────────────────────────────

/// Starts the vai HTTP server.
///
/// Binds to the address configured in `config`, initialises shared state from
/// the repository at `vai_dir`, and serves requests until a SIGINT or SIGTERM
/// is received. Uses axum's built-in graceful shutdown.
pub async fn start(vai_dir: &Path, config: ServerConfig) -> Result<(), ServerError> {
    // Initialise structured logging if not already set up.
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
        repo_name: repo_config.name.clone(),
        vai_version: env!("CARGO_PKG_VERSION").to_string(),
        event_tx,
        conflict_engine: Arc::new(Mutex::new(conflict::ConflictEngine::new())),
    });

    let app = build_app(state);

    let addr = config.socket_addr()?;
    let listener = TcpListener::bind(addr).await?;
    let actual_addr = listener.local_addr()?;

    tracing::info!(
        "vai server started on http://{} — repo: {}",
        actual_addr,
        repo_config.name
    );
    println!("vai server running on http://{actual_addr}");
    println!("repository: {}", repo_config.name);
    println!("Press Ctrl+C to stop.");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(ServerError::Io)?;

    tracing::info!("vai server stopped");
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

        // Create a test API key so authenticated requests can succeed.
        let (_, key) = auth::create(&vai_dir, "test-agent").unwrap();

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
            conflict_engine: Arc::new(Mutex::new(conflict::ConflictEngine::new())),
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
        let vai_dir = root.join(".vai");
        auth::revoke(&vai_dir, "test-agent").unwrap();
        let resp = client
            .get(format!("http://{addr}/api/workspaces"))
            .bearer_auth(&key)
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
}
