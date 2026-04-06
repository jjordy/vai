//! Watcher API handlers — watcher agent registration and discovery events.
//!
//! Endpoints:
//!   - `POST /api/repos/:repo/watchers/register` — register a new watcher agent
//!   - `GET /api/repos/:repo/watchers` — list all registered watchers
//!   - `POST /api/repos/:repo/watchers/:id/pause` — pause a watcher
//!   - `POST /api/repos/:repo/watchers/:id/resume` — resume a paused watcher
//!   - `POST /api/repos/:repo/discoveries` — submit a discovery event from a watcher

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Extension;
use axum::Json;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::event_log::EventKind;
use crate::watcher::{DiscoveryEventKind, IssueCreationPolicy, Watcher, WatcherStatus, WatchType};

use super::{AgentIdentity, ApiError, AppState, BroadcastEvent, ErrorBody, PathId, RepoCtx};
use super::require_repo_permission;

// ── Request / response types ──────────────────────────────────────────────────

/// Request body for `POST /api/watchers/register`.
#[derive(Debug, Deserialize, ToSchema)]
pub(super) struct RegisterWatcherRequest {
    agent_id: String,
    watch_type: String,
    description: String,
    #[serde(default)]
    issue_creation_policy: IssueCreationPolicy,
}

/// Response body for watcher endpoints.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct WatcherResponse {
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

/// Request body for `POST /api/discoveries`.
#[derive(Debug, Deserialize, ToSchema)]
pub(super) struct SubmitDiscoveryRequest {
    /// The watcher agent submitting this event.
    agent_id: String,
    /// The discovery event payload.
    event: DiscoveryEventKind,
}

/// Response body for `POST /api/discoveries`.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct DiscoveryOutcomeResponse {
    record_id: String,
    agent_id: String,
    event_type: String,
    received_at: String,
    created_issue_id: Option<String>,
    suppressed: bool,
    message: String,
}

// ── Handlers ──────────────────────────────────────────────────────────────────

#[utoipa::path(
    post,
    path = "/api/repos/{repo}/watchers/register",
    request_body = RegisterWatcherRequest,
    params(
        ("repo" = String, Path, description = "Repository name"),
    ),
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
pub(super) async fn register_watcher_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    ctx: RepoCtx,
    Json(body): Json<RegisterWatcherRequest>,
) -> Result<(StatusCode, Json<WatcherResponse>), ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Write).await?;
    let _lock = state.repo_lock.lock().await;
    let now = chrono::Utc::now();
    let watcher = Watcher {
        agent_id: body.agent_id,
        watch_type: WatchType::from_db_str(&body.watch_type),
        description: body.description,
        issue_creation_policy: body.issue_creation_policy,
        status: WatcherStatus::Active,
        registered_at: now,
        last_discovery_at: None,
        discovery_count: 0,
    };
    let watcher = ctx
        .storage
        .watchers()
        .register_watcher(&ctx.repo_id, watcher)
        .await
        .map_err(|e| match &e {
            crate::storage::StorageError::Conflict(_) => ApiError::conflict(e.to_string()),
            _ => ApiError::internal(e.to_string()),
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
    path = "/api/repos/{repo}/watchers",
    params(
        ("repo" = String, Path, description = "Repository name"),
    ),
    responses(
        (status = 200, description = "List of registered watchers", body = Vec<WatcherResponse>),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = [])),
    tag = "watchers"
)]
/// `GET /api/watchers` — list all registered watchers.
pub(super) async fn list_watchers_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
) -> Result<Json<Vec<WatcherResponse>>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Read).await?;
    let watchers = ctx
        .storage
        .watchers()
        .list_watchers(&ctx.repo_id)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(watchers.into_iter().map(Into::into).collect()))
}

#[utoipa::path(
    post,
    path = "/api/repos/{repo}/watchers/{id}/pause",
    params(
        ("repo" = String, Path, description = "Repository name"),
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
pub(super) async fn pause_watcher_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    ctx: RepoCtx,
    PathId(agent_id): PathId,
) -> Result<Json<WatcherResponse>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Write).await?;
    let _lock = state.repo_lock.lock().await;
    let watcher = ctx
        .storage
        .watchers()
        .pause_watcher(&ctx.repo_id, &agent_id)
        .await
        .map_err(|e| match &e {
            crate::storage::StorageError::NotFound(_) => ApiError::not_found(e.to_string()),
            _ => ApiError::internal(e.to_string()),
        })?;
    Ok(Json(watcher.into()))
}

#[utoipa::path(
    post,
    path = "/api/repos/{repo}/watchers/{id}/resume",
    params(
        ("repo" = String, Path, description = "Repository name"),
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
pub(super) async fn resume_watcher_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    ctx: RepoCtx,
    PathId(agent_id): PathId,
) -> Result<Json<WatcherResponse>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Write).await?;
    let _lock = state.repo_lock.lock().await;
    let watcher = ctx
        .storage
        .watchers()
        .resume_watcher(&ctx.repo_id, &agent_id)
        .await
        .map_err(|e| match &e {
            crate::storage::StorageError::NotFound(_) => ApiError::not_found(e.to_string()),
            _ => ApiError::internal(e.to_string()),
        })?;
    Ok(Json(watcher.into()))
}

#[utoipa::path(
    post,
    path = "/api/repos/{repo}/discoveries",
    request_body = SubmitDiscoveryRequest,
    params(
        ("repo" = String, Path, description = "Repository name"),
    ),
    responses(
        (status = 201, description = "Discovery submitted", body = DiscoveryOutcomeResponse),
        (status = 400, description = "Bad request", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = [])),
    tag = "watchers"
)]
/// `POST /api/discoveries` — submit a discovery event from a watcher.
pub(super) async fn submit_discovery_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    ctx: RepoCtx,
    Json(body): Json<SubmitDiscoveryRequest>,
) -> Result<(StatusCode, Json<DiscoveryOutcomeResponse>), ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Write).await?;
    let _lock = state.repo_lock.lock().await;

    let event_type = body.event.event_type().to_string();

    // Phase 1: validate watcher, apply rate-limit, check for duplicates via
    // storage trait — works in both SQLite and Postgres modes.
    let prep = ctx
        .storage
        .watchers()
        .prepare_discovery(&ctx.repo_id, &body.agent_id, &body.event)
        .await
        .map_err(|e| match &e {
            crate::storage::StorageError::NotFound(_) => ApiError::not_found(e.to_string()),
            crate::storage::StorageError::RateLimitExceeded(_) => {
                ApiError::rate_limited(e.to_string())
            }
            _ => ApiError::internal(e.to_string()),
        })?;

    // Suppressed duplicate: just record and return 200.
    if let Some(existing_issue_id) = prep.suppressed_with_issue_id {
        let record = ctx
            .storage
            .watchers()
            .record_discovery(
                &ctx.repo_id,
                &body.agent_id,
                &body.event,
                prep.record_id,
                &prep.dedup_key,
                prep.received_at,
                Some(existing_issue_id),
                true,
            )
            .await
            .map_err(|e| ApiError::internal(e.to_string()))?;
        return Ok((
            StatusCode::OK,
            Json(DiscoveryOutcomeResponse {
                record_id: record.id.to_string(),
                agent_id: record.agent_id,
                event_type,
                received_at: record.received_at.to_rfc3339(),
                created_issue_id: record.created_issue_id.map(|id| id.to_string()),
                suppressed: true,
                message: format!(
                    "Suppressed duplicate: issue {existing_issue_id} already tracks this problem"
                ),
            }),
        ));
    }

    // Phase 2: create issue via storage trait — works in both SQLite and Postgres
    // modes without touching the local filesystem.
    let mut created_issue_id: Option<uuid::Uuid> = None;
    if prep.should_create_issue {
        let title = body.event.default_title();
        let description = format!(
            "Automatically created by watcher `{}`.\n\n**Event type:** {}\n\n**Details:**\n```json\n{}\n```",
            &body.agent_id,
            body.event.event_type(),
            serde_json::to_string_pretty(&body.event).unwrap_or_default(),
        );
        let agent_source_val = serde_json::to_value(serde_json::json!({
            "source_type": body.event.event_type(),
            "details": &body.event,
        }))
        .ok();

        let create_result = ctx
            .storage
            .issues()
            .create_issue(
                &ctx.repo_id,
                crate::storage::NewIssue {
                    title: title.clone(),
                    description,
                    priority: prep.priority.clone(),
                    labels: vec![event_type.clone(), "watcher".to_string()],
                    creator: body.agent_id.clone(),
                    agent_source: agent_source_val,
                    acceptance_criteria: vec![],
                },
            )
            .await;

        if let Ok(issue) = create_result {
            // Append IssueCreated event via storage trait.
            let _ = ctx
                .storage
                .events()
                .append(
                    &ctx.repo_id,
                    EventKind::IssueCreated {
                        issue_id: issue.id,
                        title: issue.title.clone(),
                        creator: body.agent_id.clone(),
                        priority: issue.priority.as_str().to_string(),
                    },
                )
                .await;
            created_issue_id = Some(issue.id);
        }
    }

    // Phase 3: persist discovery record via storage trait.
    let message = if let Some(id) = created_issue_id {
        format!("Discovery recorded; issue {id} created")
    } else if prep.should_create_issue {
        "Discovery recorded; issue creation failed or rate-limited".to_string()
    } else {
        "Discovery recorded; auto-create disabled by policy".to_string()
    };

    let record = ctx
        .storage
        .watchers()
        .record_discovery(
            &ctx.repo_id,
            &body.agent_id,
            &body.event,
            prep.record_id,
            &prep.dedup_key,
            prep.received_at,
            created_issue_id,
            false,
        )
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    if let Some(issue_id) = created_issue_id {
        state.broadcast(BroadcastEvent {
            event_type: "IssueCreated".to_string(),
            event_id: 0,
            workspace_id: None,
            timestamp: record.received_at.to_rfc3339(),
            data: serde_json::json!({
                "issue_id": issue_id.to_string(),
                "source": "watcher_discovery",
                "watcher_agent_id": &body.agent_id,
            }),
        });
    }

    Ok((
        StatusCode::CREATED,
        Json(DiscoveryOutcomeResponse {
            record_id: record.id.to_string(),
            agent_id: record.agent_id,
            event_type,
            received_at: record.received_at.to_rfc3339(),
            created_issue_id: record.created_issue_id.map(|id| id.to_string()),
            suppressed: false,
            message,
        }),
    ))
}
