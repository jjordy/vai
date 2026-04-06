//! Work queue API handlers — get available work and claim issues.
//!
//! Endpoints:
//!   - `GET /api/repos/:repo/work-queue` — list available and blocked work
//!   - `POST /api/repos/:repo/work-queue/claim` — atomically claim an issue

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Extension;
use axum::Json;
use serde::Deserialize;
use utoipa::ToSchema;

use crate::event_log::EventKind;
use crate::storage::ListQuery;
use crate::work_queue;

use super::{AgentIdentity, ApiError, AppState, BroadcastEvent, ErrorBody, RepoCtx};
use super::require_repo_permission;

// ── Request types ─────────────────────────────────────────────────────────────

/// Request body for `POST /api/work-queue/claim`.
#[derive(Debug, Deserialize, ToSchema)]
pub(super) struct ClaimWorkRequest {
    /// Issue ID to claim.
    issue_id: String,
}

// ── Handlers ──────────────────────────────────────────────────────────────────

#[utoipa::path(
    get,
    path = "/api/repos/{repo}/work-queue",
    params(
        ("repo" = String, Path, description = "Repository name"),
    ),
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
pub(super) async fn get_work_queue_handler(
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
        }, &ListQuery::default())
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .items;

    // Fetch all issues for blocker status lookups.
    let all_issues_for_deps = ctx.storage.issues()
        .list_issues(&ctx.repo_id, &IssueFilter::default(), &ListQuery::default())
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .items;

    let issue_status_map: std::collections::HashMap<uuid::Uuid, crate::issue::IssueStatus> =
        all_issues_for_deps.iter().map(|i| (i.id, i.status.clone())).collect();
    let issue_title_map: std::collections::HashMap<uuid::Uuid, String> =
        all_issues_for_deps.iter().map(|i| (i.id, i.title.clone())).collect();

    let engine = state.conflict_engine.lock().await;
    let active_scopes: Vec<_> = engine.all_scopes().cloned().collect();

    let mut available: Vec<work_queue::AvailableWork> = Vec::new();
    let mut blocked: Vec<work_queue::BlockedWork> = Vec::new();

    for issue in open_issues {
        // Check link-based blocking: issue is blocked if any open issue has a
        // `blocks` link targeting it.
        let issue_links = ctx.storage.links()
            .list_links(&ctx.repo_id, &issue.id)
            .await
            .unwrap_or_default();

        let open_blocker_ids: Vec<uuid::Uuid> = issue_links.iter()
            .filter(|l| {
                l.relationship == crate::storage::IssueLinkRelationship::Blocks
                    && l.target_id == issue.id
            })
            .map(|l| l.source_id)
            .filter(|blocker_id| {
                issue_status_map.get(blocker_id).is_some_and(|s| {
                    *s != crate::issue::IssueStatus::Closed
                        && *s != crate::issue::IssueStatus::Resolved
                })
            })
            .collect();

        if !open_blocker_ids.is_empty() {
            let open_blocker_titles: Vec<String> = open_blocker_ids.iter()
                .filter_map(|id| issue_title_map.get(id).cloned())
                .collect();
            blocked.push(work_queue::BlockedWork {
                issue_id: issue.id.to_string(),
                title: issue.title.clone(),
                priority: issue.priority.as_str().to_string(),
                blocked_by: open_blocker_ids.iter().map(|id| id.to_string()).collect(),
                reason: format!("Blocked by: {}", open_blocker_titles.join(", ")),
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

    // Secondary sort: issues with acceptance criteria come first (clear definition of done).
    let has_criteria_map: std::collections::HashMap<uuid::Uuid, bool> =
        all_issues_for_deps.iter().map(|i| (i.id, !i.acceptance_criteria.is_empty())).collect();
    available.sort_by_key(|w| {
        let issue_id = uuid::Uuid::parse_str(&w.issue_id).unwrap_or_default();
        // Lower value = higher priority. Negate has_criteria so true (1) → 0, false (0) → 1.
        let no_criteria = if has_criteria_map.get(&issue_id).copied().unwrap_or(false) { 0u8 } else { 1u8 };
        (work_queue::priority_rank(&w.priority), no_criteria)
    });
    blocked.sort_by_key(|w| work_queue::priority_rank(&w.priority));

    Ok(Json(work_queue::WorkQueue { available_work: available, blocked_work: blocked }))
}

#[utoipa::path(
    post,
    path = "/api/repos/{repo}/work-queue/claim",
    request_body = ClaimWorkRequest,
    params(
        ("repo" = String, Path, description = "Repository name"),
    ),
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
pub(super) async fn claim_work_handler(
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
