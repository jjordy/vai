//! HTTP server for vai — exposes REST API and WebSocket endpoints for
//! multi-agent coordination.
//!
//! Entry point: [`start`] — binds a TCP socket and serves the application.
//! For Phase 2 tracer bullet this module implements:
//!   - `GET /api/status` — server and repository health
//!
//! Further endpoints (workspaces, graph, versions, WebSocket) will be added
//! in subsequent issues.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use axum::{extract::State, routing::get, Json, Router};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::net::TcpListener;

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
    /// Monotonic timestamp recorded when the server started.
    started_at: Instant,
    /// Human-readable repository name from `.vai/config.toml`.
    repo_name: String,
    /// vai crate version string.
    vai_version: String,
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

// ── Router builder (pub(crate) for integration tests) ────────────────────────

/// Constructs the axum [`Router`] with all registered routes.
///
/// Exposed as `pub(crate)` so integration tests can build the app directly
/// without going through the full TCP listener setup.
pub(crate) fn build_app(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/api/status", get(status_handler))
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

    let state = Arc::new(AppState {
        vai_dir: vai_dir.to_owned(),
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

    /// Start a server on a random port, hit `/api/status`, verify the response.
    #[tokio::test]
    async fn status_endpoint_returns_correct_data() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Create a small Rust file so the repo has content.
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), b"pub fn hello() {}\n").unwrap();

        repo::init(root).unwrap();
        let vai_dir = root.join(".vai");

        // Bind to port 0 so the OS assigns a free port.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let repo_config = repo::read_config(&vai_dir).unwrap();
        let state = Arc::new(AppState {
            vai_dir: vai_dir.clone(),
            started_at: Instant::now(),
            repo_name: repo_config.name.clone(),
            vai_version: env!("CARGO_PKG_VERSION").to_string(),
        });
        let app = build_app(state);

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

        let client = reqwest::Client::new();
        let resp = client
            .get(format!("http://{addr}/api/status"))
            .send()
            .await
            .expect("request failed");

        assert_eq!(resp.status(), 200, "expected HTTP 200");

        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(
            body["repo_name"],
            repo_config.name.as_str(),
            "repo_name mismatch"
        );
        assert_eq!(body["head_version"], "v1", "expected HEAD at v1 after init");
        assert!(body["uptime_secs"].is_u64(), "uptime_secs should be a number");
        assert_eq!(body["workspace_count"], 0, "no workspaces expected");
        assert_eq!(
            body["vai_version"],
            env!("CARGO_PKG_VERSION"),
            "vai_version mismatch"
        );

        shutdown_tx.send(()).ok();
    }
}
