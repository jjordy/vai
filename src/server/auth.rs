//! Auth token exchange, refresh, revocation, and CLI device code flow handlers
//! (PRD 18 / PRD 26 V-3).
//!
//! Endpoints:
//!   - `POST /api/auth/token` — exchange credentials for a short-lived JWT
//!   - `POST /api/auth/refresh` — exchange a refresh token for a new access token
//!   - `POST /api/auth/revoke` — revoke a refresh token
//!   - `POST /api/auth/cli-device` — begin a CLI device code session
//!   - `GET  /api/auth/cli-device/:code` — poll device code status (CLI polls this)
//!   - `POST /api/auth/cli-device/authorize` — authorize a pending device code

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Extension;
use axum::Json;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{AgentIdentity, ApiError, AppState};

// ── Auth token exchange (PRD 18) ──────────────────────────────────────────────

/// Request body for `POST /api/auth/token`.
///
/// Two grant types are supported:
/// - `"session_exchange"` — exchange a Better Auth session token for a vai JWT.
///   Requires `session_token`. Returns an access token and a refresh token.
/// - `"api_key"` — exchange a long-lived API key for a short-lived JWT.
///   Requires `api_key`. Returns an access token only (no refresh token).
#[derive(Debug, Deserialize, ToSchema)]
pub(super) struct TokenRequest {
    /// Grant type. Accepted values: `"session_exchange"`, `"api_key"`.
    grant_type: String,
    /// Better Auth session token (required for `session_exchange`).
    session_token: Option<String>,
    /// Plaintext API key (required for `api_key`).
    api_key: Option<String>,
    /// Optional repository UUID to scope the token. When provided, the user's
    /// effective role on this repo is embedded in the JWT claims.
    #[schema(value_type = Option<String>)]
    repo_id: Option<uuid::Uuid>,
}

/// Response body for `POST /api/auth/token`.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct TokenResponse {
    /// Short-lived JWT access token (HMAC-SHA256, 15 min TTL).
    access_token: String,
    /// Token type — always `"Bearer"`.
    token_type: String,
    /// Access token TTL in seconds (900 = 15 minutes).
    expires_in: u64,
    /// Opaque refresh token. Present only for `session_exchange` grants.
    /// Use `POST /api/auth/refresh` to mint a new access token.
    #[serde(skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
}

#[utoipa::path(
    post,
    path = "/api/auth/token",
    request_body = TokenRequest,
    responses(
        (status = 200, description = "Access token issued", body = TokenResponse),
        (status = 400, description = "Missing or invalid parameters", body = super::ErrorBody),
        (status = 401, description = "Invalid credentials", body = super::ErrorBody),
    ),
    tag = "auth"
)]
/// `POST /api/auth/token` — exchanges credentials for a short-lived JWT.
///
/// # Grant types
///
/// ## `session_exchange`
/// Validates a Better Auth session token by querying the shared Postgres
/// `session` table. On success, mints a JWT scoped to the authenticated user
/// (and optionally to a specific repo) and creates a refresh token.
///
/// ## `api_key`
/// Validates a plaintext vai API key. On success, mints a JWT carrying the
/// same user and role as the key. No refresh token is issued; the agent should
/// re-exchange the long-lived key before the JWT expires.
pub(super) async fn token_exchange_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<TokenRequest>,
) -> Result<Json<TokenResponse>, ApiError> {
    let auth = state.storage.auth();

    match body.grant_type.as_str() {
        "session_exchange" => {
            let session_token = body.session_token.as_deref().ok_or_else(|| {
                ApiError::bad_request("session_token is required for session_exchange grant")
            })?;

            // Validate the Better Auth session and extract the BA user ID (opaque string).
            let ba_user_id = auth.validate_session(session_token).await.map_err(|e| {
                match e {
                    crate::storage::StorageError::NotFound(_) => {
                        ApiError::unauthorized("invalid or expired session token")
                    }
                    other => ApiError::from(other),
                }
            })?;

            // Resolve or auto-provision the vai user for this Better Auth identity.
            let orgs = state.storage.orgs();
            let (user_id, user_name) = match orgs.get_user_by_external_id(&ba_user_id).await {
                Ok(existing) => (existing.id, existing.name),
                Err(crate::storage::StorageError::NotFound(_)) => {
                    // First login — fetch BA profile and create a vai user record.
                    let (email, name) = auth
                        .get_better_auth_user(&ba_user_id)
                        .await
                        .map_err(ApiError::from)?;

                    let new_user = orgs
                        .create_user(crate::storage::NewUser {
                            email,
                            name,
                            better_auth_id: Some(ba_user_id.clone()),
                        })
                        .await
                        .map_err(ApiError::from)?;

                    tracing::info!(
                        event = "auth.user_provisioned",
                        ba_user_id = %ba_user_id,
                        vai_user_id = %new_user.id,
                        "Auto-provisioned vai user from Better Auth identity"
                    );

                    (new_user.id, new_user.name)
                }
                Err(other) => return Err(ApiError::from(other)),
            };

            // No automatic repo grants on login — users see only repos they
            // created or were explicitly added to as collaborators.

            // Resolve the user's repo role if repo_id was supplied.
            let role: Option<String> = if let Some(repo_id) = &body.repo_id {
                let orgs = state.storage.orgs();
                orgs.resolve_repo_role(&user_id, repo_id)
                    .await
                    .map_err(ApiError::from)?
                    .map(|r| r.as_str().to_string())
            } else {
                None
            };

            // Mint the JWT access token.
            let access_token = state
                .jwt_service
                .sign(
                    user_id.to_string(),
                    Some(user_name),
                    body.repo_id.as_ref().map(|id| id.to_string()),
                    role,
                )
                .map_err(|e| ApiError::internal(e.to_string()))?;

            // Mint and persist a refresh token (7-day TTL).
            let expires_at = chrono::Utc::now() + chrono::Duration::days(7);
            let refresh_token = auth
                .create_refresh_token(&user_id, expires_at)
                .await
                .map_err(ApiError::from)?;

            tracing::info!(
                event = "auth.token_issued",
                grant_type = "session_exchange",
                user_id = %user_id,
                repo_id = ?body.repo_id,
                "JWT access token issued via session exchange"
            );

            Ok(Json(TokenResponse {
                access_token,
                token_type: "Bearer".to_string(),
                expires_in: state.jwt_service.access_token_ttl,
                refresh_token: Some(refresh_token),
            }))
        }

        "api_key" => {
            let api_key_str = body.api_key.as_deref().ok_or_else(|| {
                ApiError::bad_request("api_key is required for api_key grant")
            })?;

            // Bootstrap admin key takes priority over per-repo keys.
            let (sub, name, role) = if api_key_str == state.admin_key {
                ("admin".to_string(), "admin".to_string(), Some("admin".to_string()))
            } else {
                // Validate the API key against the store.
                let key_meta = auth.validate_key(api_key_str).await.map_err(|e| {
                    match e {
                        crate::storage::StorageError::NotFound(_) => {
                            ApiError::unauthorized("invalid or revoked API key")
                        }
                        other => ApiError::from(other),
                    }
                })?;

                tracing::info!(
                    event = "auth.token_issued",
                    grant_type = "api_key",
                    key_id = %key_meta.id,
                    key_name = %key_meta.name,
                    "JWT access token issued via API key exchange"
                );

                let sub = key_meta
                    .user_id
                    .map(|u| u.to_string())
                    .unwrap_or_else(|| key_meta.id.clone());
                let key_name = key_meta.name.clone();
                let role = key_meta.role_override.clone();
                (sub, key_name, role)
            };

            let repo_id_str = body.repo_id.as_ref().map(|id| id.to_string());
            let access_token = state
                .jwt_service
                .sign(sub, Some(name), repo_id_str, role)
                .map_err(|e| ApiError::internal(e.to_string()))?;

            // No refresh token for api_key grants — the long-lived key itself
            // acts as the refresh credential.
            Ok(Json(TokenResponse {
                access_token,
                token_type: "Bearer".to_string(),
                expires_in: state.jwt_service.access_token_ttl,
                refresh_token: None,
            }))
        }

        other => Err(ApiError::bad_request(format!(
            "unsupported grant_type '{other}'; accepted: 'session_exchange', 'api_key'"
        ))),
    }
}

// ── Auth refresh and revocation (PRD 18) ──────────────────────────────────────

/// Request body for `POST /api/auth/refresh`.
#[derive(Debug, Deserialize, ToSchema)]
pub(super) struct RefreshRequest {
    /// Opaque refresh token previously issued by `POST /api/auth/token`.
    refresh_token: String,
}

/// Response body for `POST /api/auth/refresh`.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct RefreshResponse {
    /// New short-lived JWT access token (HMAC-SHA256, 15 min TTL).
    access_token: String,
    /// Token type — always `"Bearer"`.
    token_type: String,
    /// Access token TTL in seconds (900 = 15 minutes).
    expires_in: u64,
}

#[utoipa::path(
    post,
    path = "/api/auth/refresh",
    request_body = RefreshRequest,
    responses(
        (status = 200, description = "New access token issued", body = RefreshResponse),
        (status = 400, description = "Missing or malformed body", body = super::ErrorBody),
        (status = 401, description = "Invalid, expired, or revoked refresh token", body = super::ErrorBody),
    ),
    tag = "auth"
)]
/// `POST /api/auth/refresh` — exchanges a refresh token for a new access token.
///
/// Validates the opaque refresh token (checks hash, expiry, and revocation),
/// then mints a fresh short-lived JWT for the associated user.
/// The refresh token remains valid after this call until it expires or is
/// explicitly revoked via `POST /api/auth/revoke`.
pub(super) async fn refresh_token_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<RefreshRequest>,
) -> Result<Json<RefreshResponse>, ApiError> {
    let auth = state.storage.auth();

    let user_id = auth
        .validate_refresh_token(&body.refresh_token)
        .await
        .map_err(|e| match e {
            crate::storage::StorageError::NotFound(_) => {
                ApiError::unauthorized("invalid, expired, or revoked refresh token")
            }
            other => ApiError::from(other),
        })?;

    // Look up the user's display name to embed in the refreshed access token.
    let user_name = state
        .storage
        .orgs()
        .get_user(&user_id)
        .await
        .ok()
        .map(|u| u.name);

    let access_token = state
        .jwt_service
        .sign(user_id.to_string(), user_name, None, None)
        .map_err(|e| ApiError::internal(e.to_string()))?;

    tracing::info!(
        event = "auth.token_refreshed",
        user_id = %user_id,
        "JWT access token issued via refresh token"
    );

    Ok(Json(RefreshResponse {
        access_token,
        token_type: "Bearer".to_string(),
        expires_in: state.jwt_service.access_token_ttl,
    }))
}

/// Request body for `POST /api/auth/revoke`.
#[derive(Debug, Deserialize, ToSchema)]
pub(super) struct RevokeRequest {
    /// Opaque refresh token to revoke.
    refresh_token: String,
}

#[utoipa::path(
    post,
    path = "/api/auth/revoke",
    request_body = RevokeRequest,
    responses(
        (status = 200, description = "Refresh token revoked"),
        (status = 400, description = "Missing or malformed body", body = super::ErrorBody),
        (status = 401, description = "Token not found or already revoked", body = super::ErrorBody),
    ),
    tag = "auth"
)]
/// `POST /api/auth/revoke` — revokes a refresh token.
///
/// Marks the token as revoked so it can no longer be used to mint access tokens.
/// Returns 401 if the token is not found or has already been revoked.
pub(super) async fn revoke_token_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<RevokeRequest>,
) -> Result<axum::http::StatusCode, ApiError> {
    let auth = state.storage.auth();

    auth.revoke_refresh_token(&body.refresh_token)
        .await
        .map_err(|e| match e {
            crate::storage::StorageError::NotFound(_) => {
                ApiError::unauthorized("refresh token not found or already revoked")
            }
            other => ApiError::from(other),
        })?;

    tracing::info!(event = "auth.token_revoked", "refresh token revoked");

    Ok(axum::http::StatusCode::OK)
}

// ── CLI device code flow (PRD 26 V-3) ─────────────────────────────────────────

/// Response body for `POST /api/auth/cli-device`.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct DeviceCodeResponse {
    /// The short-lived device code to display to the user (`XXXX-YYYY` format).
    pub code: String,
    /// URL where the user should enter the code in their browser.
    pub verification_url: String,
    /// Recommended polling interval in seconds (always 3).
    pub poll_interval: u32,
}

/// Response body for `GET /api/auth/cli-device/:code`.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct DeviceCodeStatusResponse {
    /// One of `"pending"`, `"authorized"`, or `"expired"`.
    pub status: String,
    /// Plaintext API key. Present only when `status` is `"authorized"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

/// Request body for `POST /api/auth/cli-device/authorize`.
#[derive(Debug, Deserialize, ToSchema)]
pub(super) struct AuthorizeDeviceCodeRequest {
    /// The device code to authorize (displayed to the user in the CLI).
    pub code: String,
}

#[utoipa::path(
    post,
    path = "/api/auth/cli-device",
    responses(
        (status = 200, description = "Device code created", body = DeviceCodeResponse),
        (status = 500, description = "Internal error", body = super::ErrorBody),
    ),
    tag = "auth"
)]
/// `POST /api/auth/cli-device` — begins a CLI device code session.
///
/// Unauthenticated. Creates a pending device code with a 10-minute TTL.
/// The CLI should display `code` to the user, direct them to `verification_url`,
/// then poll `GET /api/auth/cli-device/:code` every `poll_interval` seconds
/// until it receives `{"status":"authorized","api_key":"..."}`.
pub(super) async fn create_device_code_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Json<DeviceCodeResponse>, ApiError> {
    let auth = state.storage.auth();
    let code = auth.create_device_code().await.map_err(ApiError::from)?;

    let public_url = std::env::var("VAI_PUBLIC_URL")
        .unwrap_or_else(|_| "http://localhost:7865".to_string());
    let verification_url = format!("{public_url}/cli");

    tracing::info!(event = "auth.device_code_created", code = %code, "CLI device code created");

    Ok(Json(DeviceCodeResponse {
        code,
        verification_url,
        poll_interval: 3,
    }))
}

#[utoipa::path(
    get,
    path = "/api/auth/cli-device/{code}",
    params(
        ("code" = String, Path, description = "The device code to poll")
    ),
    responses(
        (status = 200, description = "Device code status", body = DeviceCodeStatusResponse),
        (status = 404, description = "Code not found or expired", body = super::ErrorBody),
    ),
    tag = "auth"
)]
/// `GET /api/auth/cli-device/:code` — polls the status of a CLI device code.
///
/// Unauthenticated. Returns `{"status":"pending"}` while waiting, or
/// `{"status":"authorized","api_key":"..."}` once the user has authorized the
/// code.  Returns 404 if the code does not exist or has expired.
///
/// The API key is revealed exactly once — the row is deleted on the first
/// authorized response.
pub(super) async fn poll_device_code_handler(
    State(state): State<Arc<AppState>>,
    Path(code): Path<String>,
) -> Result<Json<DeviceCodeStatusResponse>, ApiError> {
    let auth = state.storage.auth();

    let status = auth.poll_device_code(&code).await.map_err(|e| match e {
        crate::storage::StorageError::NotFound(_) => {
            ApiError::not_found("device code not found or expired")
        }
        other => ApiError::from(other),
    })?;

    match status {
        crate::storage::DeviceCodeStatus::Pending => {
            Ok(Json(DeviceCodeStatusResponse {
                status: "pending".to_string(),
                api_key: None,
            }))
        }
        crate::storage::DeviceCodeStatus::Authorized { api_key } => {
            tracing::info!(
                event = "auth.device_code_authorized",
                "CLI device code key retrieved — row deleted"
            );
            Ok(Json(DeviceCodeStatusResponse {
                status: "authorized".to_string(),
                api_key: Some(api_key),
            }))
        }
    }
}

#[utoipa::path(
    post,
    path = "/api/auth/cli-device/authorize",
    request_body = AuthorizeDeviceCodeRequest,
    responses(
        (status = 200, description = "Device code authorized"),
        (status = 400, description = "Bad request", body = super::ErrorBody),
        (status = 401, description = "Unauthorized", body = super::ErrorBody),
        (status = 404, description = "Code not found or expired", body = super::ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "auth"
)]
/// `POST /api/auth/cli-device/authorize` — authorizes a pending device code.
///
/// Authenticated (requires a valid Better Auth session or API key with a
/// `user_id`). Called by the dashboard's `/cli` page after the user enters the
/// code.  Mints a new write-scoped API key for the user and associates it with
/// the pending code.  The CLI retrieves the key by polling
/// `GET /api/auth/cli-device/:code`.
pub(super) async fn authorize_device_code_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    Json(body): Json<AuthorizeDeviceCodeRequest>,
) -> Result<StatusCode, ApiError> {
    let user_id = identity.user_id.ok_or_else(|| {
        ApiError::unauthorized("this request must be authenticated as a specific user")
    })?;

    // Mint a new API key for the user with write-level access.
    let auth = state.storage.auth();
    let (_key_meta, plaintext) = auth
        .create_key(
            None,
            "CLI (device code)",
            Some(&user_id),
            Some("write"),
            Some("cli"),
            None,
        )
        .await
        .map_err(ApiError::from)?;

    // Record the authorization — the CLI will retrieve the key on next poll.
    auth.authorize_device_code(&body.code, &user_id, &plaintext)
        .await
        .map_err(|e| match e {
            crate::storage::StorageError::NotFound(_) => {
                ApiError::not_found("device code not found, expired, or already authorized")
            }
            other => ApiError::from(other),
        })?;

    tracing::info!(
        event = "auth.device_code_authorized",
        user_id = %user_id,
        code = %body.code,
        "CLI device code authorized"
    );

    Ok(StatusCode::OK)
}
