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

use crate::auth;
use crate::merge;
use crate::repo;
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
    workspace::get(&state.vai_dir, &id).map_err(ApiError::from)?;
    // Make it the active workspace so merge::submit can find it.
    workspace::switch(&state.vai_dir, &id).map_err(ApiError::from)?;
    let result = merge::submit(&state.vai_dir, &state.repo_root).map_err(ApiError::from)?;

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
    workspace::discard(&state.vai_dir, &id, None).map_err(ApiError::from)?;

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
        .route("/api/workspaces/:id", delete(discard_workspace_handler))
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
}
