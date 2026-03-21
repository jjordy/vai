//! HTTP server for vai — exposes REST API and WebSocket endpoints for
//! multi-agent coordination.
//!
//! Entry point: [`start`] — binds a TCP socket and serves the application.
//! Current endpoints:
//!   - `GET /api/status` — server and repository health
//!   - `POST /api/workspaces` — create a new workspace
//!   - `GET /api/workspaces` — list active workspaces
//!   - `GET /api/workspaces/:id` — workspace details
//!   - `POST /api/workspaces/:id/submit` — submit workspace for merge
//!   - `DELETE /api/workspaces/:id` — discard a workspace
//!
//! Further endpoints (graph, versions, WebSocket) will be added in subsequent
//! issues.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::net::TcpListener;

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
async fn create_workspace_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateWorkspaceRequest>,
) -> Result<(StatusCode, Json<WorkspaceResponse>), ApiError> {
    let head = repo::read_head(&state.vai_dir).map_err(|e| ApiError::internal(e.to_string()))?;
    let result = workspace::create(&state.vai_dir, &body.intent, &head)
        .map_err(ApiError::from)?;
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
async fn submit_workspace_handler(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<SubmitResponse>, ApiError> {
    // Verify the workspace exists before switching.
    workspace::get(&state.vai_dir, &id).map_err(ApiError::from)?;
    // Make it the active workspace so merge::submit can find it.
    workspace::switch(&state.vai_dir, &id).map_err(ApiError::from)?;
    let result = merge::submit(&state.vai_dir, &state.repo_root).map_err(ApiError::from)?;
    Ok(Json(SubmitResponse::from(result)))
}

/// `DELETE /api/workspaces/:id` — discards a workspace.
///
/// Returns 404 if the workspace does not exist.
/// Returns 204 No Content on success.
async fn discard_workspace_handler(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<StatusCode, ApiError> {
    workspace::discard(&state.vai_dir, &id, None).map_err(ApiError::from)?;
    Ok(StatusCode::NO_CONTENT)
}

// ── Router builder (pub(crate) for integration tests) ────────────────────────

/// Constructs the axum [`Router`] with all registered routes.
///
/// Exposed as `pub(crate)` so integration tests can build the app directly
/// without going through the full TCP listener setup.
pub(crate) fn build_app(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/api/status", get(status_handler))
        .route("/api/workspaces", post(create_workspace_handler))
        .route("/api/workspaces", get(list_workspaces_handler))
        .route("/api/workspaces/:id", get(get_workspace_handler))
        .route("/api/workspaces/:id/submit", post(submit_workspace_handler))
        .route("/api/workspaces/:id", delete(discard_workspace_handler))
        .with_state(state)
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

    let state = Arc::new(AppState {
        vai_dir: vai_dir.to_owned(),
        repo_root,
        started_at: Instant::now(),
        repo_name: repo_config.name.clone(),
        vai_version: env!("CARGO_PKG_VERSION").to_string(),
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
    use crate::repo;

    /// Initialise a repo in `root` and return a running test server address.
    async fn start_test_server(
        root: &Path,
    ) -> (SocketAddr, oneshot::Sender<()>, Arc<AppState>) {
        repo::init(root).unwrap();
        let vai_dir = root.join(".vai");
        let repo_config = repo::read_config(&vai_dir).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let state = Arc::new(AppState {
            vai_dir: vai_dir.clone(),
            repo_root: root.to_path_buf(),
            started_at: Instant::now(),
            repo_name: repo_config.name.clone(),
            vai_version: env!("CARGO_PKG_VERSION").to_string(),
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

        (addr, shutdown_tx, state)
    }

    #[tokio::test]
    async fn status_endpoint_returns_correct_data() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), b"pub fn hello() {}\n").unwrap();

        let (addr, shutdown_tx, state) = start_test_server(root).await;
        let client = reqwest::Client::new();

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

        let (addr, shutdown_tx, _state) = start_test_server(root).await;
        let client = reqwest::Client::new();

        // POST /api/workspaces — create
        let resp = client
            .post(format!("http://{addr}/api/workspaces"))
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
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 404);

        // DELETE /api/workspaces/:id — discard
        let resp = client
            .delete(format!("http://{addr}/api/workspaces/{ws_id}"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 204, "expected 204 No Content");

        // After discard, workspace should not appear in list
        let resp = client
            .get(format!("http://{addr}/api/workspaces"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let list: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(list.as_array().unwrap().len(), 0, "discarded workspace should not appear");

        shutdown_tx.send(()).ok();
    }

    #[tokio::test]
    async fn submit_workspace_creates_new_version() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), b"pub fn hello() {}\n").unwrap();

        let (addr, shutdown_tx, _state) = start_test_server(root).await;
        let client = reqwest::Client::new();

        // Create a workspace.
        let resp = client
            .post(format!("http://{addr}/api/workspaces"))
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
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200, "expected 200 OK from submit");
        let result: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(result["version"], "v2", "submit should create v2");
        assert!(result["files_applied"].as_u64().unwrap() > 0);

        shutdown_tx.send(()).ok();
    }
}
