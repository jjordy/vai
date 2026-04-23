//! Agent worker lifecycle API — heartbeat, log ingestion, terminal-state
//! transitions, and read endpoints for cloud workers spawned by the vai server
//! (PRD 28).
//!
//! Endpoints:
//!   - `GET  /api/agent-workers/:id`           — fetch worker state
//!   - `GET  /api/agent-workers/:id/logs`      — fetch log chunks
//!   - `POST /api/agent-workers/:id/heartbeat` — keep a running worker alive
//!   - `POST /api/agent-workers/:id/logs`      — ingest a batch of log chunks
//!   - `POST /api/agent-workers/:id/done`      — mark a worker terminal

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::storage::{AgentWorker, LogStream, WorkerDoneReason, WorkerLog};

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

/// Query parameters for `GET /api/agent-workers/:id/logs`.
#[derive(Debug, Deserialize, ToSchema)]
pub(super) struct LogsQuery {
    /// Return only chunks with `id` greater than this value (pagination cursor).
    pub since_id: Option<i64>,
}

/// Response body for `GET /api/agent-workers/:id/logs`.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct LogsResponse {
    pub logs: Vec<WorkerLog>,
    /// `id` of the last returned chunk, usable as the next `since_id`.
    pub last_id: Option<i64>,
}

// ── Handlers ──────────────────────────────────────────────────────────────────

#[utoipa::path(
    get,
    path = "/api/agent-workers/{id}",
    params(
        ("id" = Uuid, Path, description = "Agent worker UUID"),
    ),
    responses(
        (status = 200, description = "Worker state", body = AgentWorker),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Worker not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "agent-workers"
)]
/// `GET /api/agent-workers/:id` — fetch the current state of a cloud worker.
pub(super) async fn get_worker_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<AgentWorker>, ApiError> {
    let worker = state
        .storage
        .workers()
        .get_worker(&id)
        .await
        .map_err(|e| match &e {
            crate::storage::StorageError::NotFound(_) => ApiError::not_found(e.to_string()),
            _ => ApiError::internal(e.to_string()),
        })?;
    Ok(Json(worker))
}

#[utoipa::path(
    get,
    path = "/api/agent-workers/{id}/logs",
    params(
        ("id" = Uuid, Path, description = "Agent worker UUID"),
        ("since_id" = Option<i64>, Query, description = "Pagination cursor — return chunks with id > since_id"),
    ),
    responses(
        (status = 200, description = "Log chunks", body = LogsResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Worker not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "agent-workers"
)]
/// `GET /api/agent-workers/:id/logs` — fetch buffered log chunks for a worker.
pub(super) async fn get_logs_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Query(q): Query<LogsQuery>,
) -> Result<Json<LogsResponse>, ApiError> {
    // Verify the worker exists first so we return 404 rather than an empty list.
    state
        .storage
        .workers()
        .get_worker(&id)
        .await
        .map_err(|e| match &e {
            crate::storage::StorageError::NotFound(_) => ApiError::not_found(e.to_string()),
            _ => ApiError::internal(e.to_string()),
        })?;

    let logs = state
        .storage
        .workers()
        .list_logs(&id, q.since_id)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    let last_id = logs.last().map(|l| l.id);
    Ok(Json(LogsResponse { logs, last_id }))
}

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
