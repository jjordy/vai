//! Agent worker lifecycle API — heartbeat, log ingestion, and terminal-state
//! transitions for cloud workers spawned by the vai server (PRD 28).
//!
//! Endpoints:
//!   - `POST /api/agent-workers/:id/heartbeat` — keep a running worker alive
//!   - `POST /api/agent-workers/:id/logs`      — ingest a batch of log chunks
//!   - `POST /api/agent-workers/:id/done`      — mark a worker terminal

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::storage::{LogStream, WorkerDoneReason};

use super::{ApiError, AppState, ErrorBody};

// ── Request / response types ──────────────────────────────────────────────────

/// Request body for `POST /api/agent-workers/:id/logs`.
#[derive(Debug, Deserialize, ToSchema)]
pub(super) struct AppendLogsRequest {
    /// Which I/O stream these chunks came from.
    pub stream: LogStream,
    /// Ordered list of log lines / chunks to append.
    pub chunks: Vec<String>,
}

/// Request body for `POST /api/agent-workers/:id/done`.
#[derive(Debug, Deserialize, ToSchema)]
pub(super) struct MarkDoneRequest {
    /// Why the worker is terminating.
    pub reason: WorkerDoneReason,
}

/// Response body shared by worker state-change endpoints.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct WorkerAckResponse {
    pub ok: bool,
}

// ── Handlers ──────────────────────────────────────────────────────────────────

#[utoipa::path(
    post,
    path = "/api/agent-workers/{id}/heartbeat",
    params(
        ("id" = Uuid, Path, description = "Agent worker UUID"),
    ),
    responses(
        (status = 204, description = "Heartbeat recorded"),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Worker not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "agent-workers"
)]
/// `POST /api/agent-workers/:id/heartbeat` — record a liveness heartbeat.
pub(super) async fn heartbeat_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    state
        .storage
        .workers()
        .update_heartbeat(&id)
        .await
        .map_err(|e| match &e {
            crate::storage::StorageError::NotFound(_) => ApiError::not_found(e.to_string()),
            _ => ApiError::internal(e.to_string()),
        })?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/api/agent-workers/{id}/logs",
    request_body = AppendLogsRequest,
    params(
        ("id" = Uuid, Path, description = "Agent worker UUID"),
    ),
    responses(
        (status = 204, description = "Logs appended"),
        (status = 400, description = "Bad request", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Worker not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "agent-workers"
)]
/// `POST /api/agent-workers/:id/logs` — append a batch of log chunks.
pub(super) async fn append_logs_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Json(body): Json<AppendLogsRequest>,
) -> Result<StatusCode, ApiError> {
    state
        .storage
        .workers()
        .append_logs(&id, body.stream, &body.chunks)
        .await
        .map_err(|e| match &e {
            crate::storage::StorageError::NotFound(_) => ApiError::not_found(e.to_string()),
            _ => ApiError::internal(e.to_string()),
        })?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/api/agent-workers/{id}/done",
    request_body = MarkDoneRequest,
    params(
        ("id" = Uuid, Path, description = "Agent worker UUID"),
    ),
    responses(
        (status = 204, description = "Worker marked terminal"),
        (status = 400, description = "Bad request", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Worker not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "agent-workers"
)]
/// `POST /api/agent-workers/:id/done` — mark a worker as terminal.
pub(super) async fn mark_done_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Json(body): Json<MarkDoneRequest>,
) -> Result<StatusCode, ApiError> {
    state
        .storage
        .workers()
        .mark_done(&id, body.reason)
        .await
        .map_err(|e| match &e {
            crate::storage::StorageError::NotFound(_) => ApiError::not_found(e.to_string()),
            _ => ApiError::internal(e.to_string()),
        })?;
    Ok(StatusCode::NO_CONTENT)
}
