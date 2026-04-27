//! Per-user endpoints: `/api/me/*`
//!
//! These endpoints return or mutate state scoped to the authenticated caller
//! and are not tied to any specific repository.  They require a valid user
//! identity (JWT or API key with an associated user); the bootstrap admin key
//! returns 401 because the admin has no user identity.

use std::sync::Arc;

use axum::extract::State;
use axum::Extension;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use super::{AgentIdentity, ApiError, AppState};

// ── Response types ─────────────────────────────────────────────────────────────

/// Monthly usage figures for `GET /api/me/plan`.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct PlanMonthlyUsage {
    /// Number of agent workers spawned this calendar month.
    pub workers_spawned: u64,
    /// Cumulative compute minutes consumed this calendar month.
    pub compute_minutes: u64,
    /// Estimated Anthropic token cost in US cents this calendar month.
    pub anthropic_token_cost_cents: u64,
}

/// Response body for `GET /api/me/plan`.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct PlanResponse {
    /// Plan tier name (e.g. `"Free"`, `"Pro"`, `"Team"`).
    pub name: String,
    /// Maximum number of concurrent agent workers permitted under this plan.
    pub worker_cap: u64,
    /// Usage counters for the current calendar month.
    pub monthly_usage: PlanMonthlyUsage,
}

/// Response body for `GET /api/me/onboarding`.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct OnboardingStatusResponse {
    /// ISO-8601 timestamp when the user completed onboarding, or `null` if
    /// they haven't completed it yet.
    pub completed_at: Option<DateTime<Utc>>,
}

/// Response body for `POST /api/me/onboarding-complete`.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct OnboardingCompleteResponse {
    /// ISO-8601 timestamp when onboarding was (or was previously) completed.
    pub completed_at: DateTime<Utc>,
}

// ── Helper ─────────────────────────────────────────────────────────────────────

/// Extracts the caller's user UUID from the identity.
///
/// Returns 401 if the request was made with the bootstrap admin key, which
/// has no associated user identity.
fn require_user_id(identity: &AgentIdentity) -> Result<Uuid, ApiError> {
    identity
        .user_id
        .ok_or_else(|| ApiError::unauthorized("user identity required for this endpoint"))
}

// ── Handlers ───────────────────────────────────────────────────────────────────

/// `GET /api/me/onboarding` — returns the caller's onboarding completion state.
///
/// Returns `{ "completed_at": "<iso8601>" }` when completed, or
/// `{ "completed_at": null }` when not yet completed.
#[utoipa::path(
    get,
    path = "/api/me/onboarding",
    responses(
        (status = 200, description = "Onboarding status for the authenticated user", body = OnboardingStatusResponse),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = [])),
    tag = "users"
)]
pub(super) async fn get_onboarding_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<OnboardingStatusResponse>, ApiError> {
    let user_id = require_user_id(&identity)?;
    let completed_at = state
        .storage
        .onboarding()
        .get_user_onboarding(&user_id)
        .await
        .map_err(ApiError::from)?;

    Ok(Json(OnboardingStatusResponse { completed_at }))
}

/// `POST /api/me/onboarding-complete` — marks the caller's onboarding as completed.
///
/// Idempotent: calling this multiple times returns the same timestamp from the
/// first call.
#[utoipa::path(
    post,
    path = "/api/me/onboarding-complete",
    responses(
        (status = 200, description = "Onboarding marked complete (idempotent)", body = OnboardingCompleteResponse),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = [])),
    tag = "users"
)]
pub(super) async fn complete_onboarding_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<OnboardingCompleteResponse>, ApiError> {
    let user_id = require_user_id(&identity)?;
    let completed_at = state
        .storage
        .onboarding()
        .complete_user_onboarding(&user_id)
        .await
        .map_err(ApiError::from)?;

    tracing::info!(
        event = "user.onboarding_complete",
        actor = %identity.name,
        user_id = %user_id,
        "user onboarding marked complete"
    );

    Ok(Json(OnboardingCompleteResponse { completed_at }))
}

/// `GET /api/me/plan` — returns the caller's plan tier and current-month usage.
///
/// Until real plan tiers and usage tracking land, this returns a static stub
/// with the default worker cap.  The endpoint exists now so the dashboard's
/// PlanStatusWidget stops receiving 404s on every navigation.
#[utoipa::path(
    get,
    path = "/api/me/plan",
    responses(
        (status = 200, description = "Plan tier and monthly usage for the authenticated user", body = PlanResponse),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = [])),
    tag = "users"
)]
pub(super) async fn get_plan_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(_state): State<Arc<AppState>>,
) -> Result<Json<PlanResponse>, ApiError> {
    let _ = require_user_id(&identity)?;
    Ok(Json(PlanResponse {
        name: "Free".to_string(),
        worker_cap: 3,
        monthly_usage: PlanMonthlyUsage {
            workers_spawned: 0,
            compute_minutes: 0,
            anthropic_token_cost_cents: 0,
        },
    }))
}
