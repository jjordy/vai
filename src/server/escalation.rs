//! Escalation API handlers — list, get, and resolve escalations.
//!
//! Endpoints:
//!   - `GET /api/repos/:repo/escalations` — list escalations
//!   - `GET /api/repos/:repo/escalations/:id` — escalation details
//!   - `POST /api/repos/:repo/escalations/:id/resolve` — resolve an escalation

use std::sync::Arc;

use axum::extract::{Query as AxumQuery, State};
use axum::Extension;
use axum::Json;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::escalation::EscalationStatus;
use crate::storage::ListQuery;

use super::{AgentIdentity, ApiError, AppState, BroadcastEvent, ErrorBody, PathId, RepoCtx};
use super::pagination::PaginatedResponse;
use super::require_repo_permission;

// ── Response types ────────────────────────────────────────────────────────────

/// Per-conflict detail included in an escalation response.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct EscalationConflictResponse {
    /// Repository-relative file path.
    file: String,
    /// Merge level: 1=textual, 2=structural, 3=referential.
    merge_level: u8,
    /// Stable entity IDs involved in this conflict.
    entity_ids: Vec<String>,
    /// Human-readable description.
    description: String,
    /// HEAD version of the file at conflict time (may be absent for binary files).
    ours_content: Option<String>,
    /// Workspace (agent) version of the file at conflict time.
    theirs_content: Option<String>,
    /// Common base ancestor content.
    base_content: Option<String>,
}

/// A resolution option presented to the operator.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct ResolutionOptionResponse {
    /// Machine-readable identifier.
    id: String,
    /// Human-readable label.
    label: String,
}

/// Response body for a single escalation.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct EscalationResponse {
    id: String,
    escalation_type: String,
    severity: String,
    status: String,
    summary: String,
    intents: Vec<String>,
    agents: Vec<String>,
    workspace_ids: Vec<String>,
    affected_entities: Vec<String>,
    /// Detailed per-conflict records for merge conflict escalations.
    conflicts: Vec<EscalationConflictResponse>,
    resolution_options: Vec<ResolutionOptionResponse>,
    resolution: Option<String>,
    resolved_by: Option<String>,
    created_at: String,
    resolved_at: Option<String>,
}

impl From<crate::escalation::Escalation> for EscalationResponse {
    fn from(e: crate::escalation::Escalation) -> Self {
        let conflicts = e
            .conflicts
            .into_iter()
            .map(|c| EscalationConflictResponse {
                file: c.file,
                merge_level: c.merge_level,
                entity_ids: c.entity_ids,
                description: c.description,
                ours_content: c.ours_content,
                theirs_content: c.theirs_content,
                base_content: c.base_content,
            })
            .collect();

        let resolution_options = e
            .resolution_options
            .iter()
            .map(|o| ResolutionOptionResponse {
                id: o.as_str().to_string(),
                label: o.label().to_string(),
            })
            .collect();

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
            conflicts,
            resolution_options,
            resolution: e.resolution.as_ref().map(|r| r.as_str().to_string()),
            resolved_by: e.resolved_by,
            created_at: e.created_at.to_rfc3339(),
            resolved_at: e.resolved_at.map(|t| t.to_rfc3339()),
        }
    }
}

/// Request body for `POST /api/escalations/:id/resolve`.
#[derive(Debug, Deserialize, ToSchema)]
pub(super) struct ResolveEscalationRequest {
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

/// Query parameters for `GET /api/escalations`.
#[derive(Debug, Deserialize)]
pub(super) struct ListEscalationsQuery {
    status: Option<String>,
    page: Option<u32>,
    per_page: Option<u32>,
    sort: Option<String>,
}

// ── Handlers ──────────────────────────────────────────────────────────────────

/// `GET /api/escalations` — list escalations with optional filter and pagination.
///
/// Supports pagination via `?page=1&per_page=25` and sorting via
/// `?sort=created_at:desc`.  Sortable columns: `created_at`, `status`.
#[utoipa::path(
    get,
    path = "/api/repos/{repo}/escalations",
    params(
        ("repo" = String, Path, description = "Repository name"),
        ("status" = Option<String>, Query, description = "Filter by status (pending, resolved)"),
        ("page" = Option<u32>, Query, description = "Page number (1-indexed, default 1)"),
        ("per_page" = Option<u32>, Query, description = "Items per page (default 25, max 100)"),
        ("sort" = Option<String>, Query, description = "Sort fields, e.g. `created_at:desc`"),
    ),
    responses(
        (status = 200, description = "Paginated list of escalations", body = PaginatedResponse<EscalationResponse>),
        (status = 400, description = "Invalid pagination or sort params", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = [])),
    tag = "escalations"
)]
pub(super) async fn list_escalations_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    AxumQuery(params): AxumQuery<ListEscalationsQuery>,
) -> Result<Json<PaginatedResponse<EscalationResponse>>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Read).await?;
    let status_filter = params
        .status
        .as_deref()
        .map(|s| {
            EscalationStatus::from_db_str(s)
                .ok_or_else(|| ApiError::bad_request(format!("unknown status `{s}`")))
        })
        .transpose()?;

    const ALLOWED_SORT: &[&str] = &["created_at", "status", "id", "severity"];
    let list_query = ListQuery::from_params(
        params.page,
        params.per_page,
        params.sort.as_deref(),
        ALLOWED_SORT,
    )
    .map_err(|e| ApiError::bad_request(e.to_string()))?;

    let pending_only = matches!(status_filter, Some(EscalationStatus::Pending));
    let result = ctx.storage.escalations()
        .list_escalations(&ctx.repo_id, pending_only, &list_query)
        .await
        .map_err(ApiError::from)?;

    // If a specific status other than Pending was requested (e.g. Resolved),
    // filter client-side since the trait only supports pending_only flag.
    let (items, total) = if let Some(ref sf) = status_filter {
        if !pending_only {
            let filtered: Vec<_> = result.items.into_iter().filter(|e| &e.status == sf).collect();
            let total = filtered.len() as u64;
            (filtered, total)
        } else {
            (result.items, result.total)
        }
    } else {
        (result.items, result.total)
    };

    let responses: Vec<EscalationResponse> = items.into_iter().map(EscalationResponse::from).collect();
    Ok(Json(PaginatedResponse::new(responses, total, &list_query)))
}

#[utoipa::path(
    get,
    path = "/api/repos/{repo}/escalations/{id}",
    params(
        ("repo" = String, Path, description = "Repository name"),
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
pub(super) async fn get_escalation_handler(
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
    path = "/api/repos/{repo}/escalations/{id}/resolve",
    params(
        ("repo" = String, Path, description = "Repository name"),
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
pub(super) async fn resolve_escalation_handler(
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

    let option = ResolutionOption::from_db_str(&body.option).ok_or_else(|| {
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

    tracing::info!(
        event = "escalation.resolved",
        actor = %identity.name,
        repo = %ctx.repo_id,
        escalation_id = %esc_id,
        option = %body.option,
        "escalation resolved"
    );
    Ok(Json(EscalationResponse::from(escalation)))
}
