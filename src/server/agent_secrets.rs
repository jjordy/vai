//! HTTP handlers for per-repo agent secret management (PRD 28, issue #351).
//!
//! Endpoints:
//! - `POST   /api/repos/:repo/agent-secrets`        — set or overwrite a secret
//! - `GET    /api/repos/:repo/agent-secrets`         — list key names (never values)
//! - `DELETE /api/repos/:repo/agent-secrets/:key`   — delete a single secret

use std::{collections::HashMap, sync::Arc};

/// OpenAPI spec fragment for agent-secrets endpoints.
///
/// Merged into the main `VaiApi` spec in `openapi_handler` so these paths
/// appear in `/api/openapi.json` without the compile-time feature-gate
/// limitations of the `#[derive(OpenApi)]` proc macro.
#[derive(utoipa::OpenApi)]
#[openapi(
    paths(set_agent_secret_handler, list_agent_secrets_handler, delete_agent_secret_handler),
    components(schemas(SetAgentSecretRequest, ListAgentSecretsResponse)),
)]
pub(super) struct AgentSecretsApiDoc;

use axum::{
    extract::{Path as AxumPath, State},
    http::StatusCode,
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{
    require_repo_permission, AgentIdentity, ApiError, AppState, BroadcastEvent, ErrorBody, RepoCtx,
};
use crate::{
    event_log::EventKind,
    storage::{RepoRole, StorageBackend},
};

/// Validate a secret key name: non-empty, only `[A-Z0-9_]`.
fn validate_key(key: &str) -> Result<(), ApiError> {
    if key.is_empty() {
        return Err(ApiError::bad_request("key must not be empty"));
    }
    if !key.chars().all(|c| matches!(c, 'A'..='Z' | '0'..='9' | '_')) {
        return Err(ApiError::bad_request(
            "key must contain only uppercase letters, digits, and underscores (A-Z, 0-9, _)",
        ));
    }
    Ok(())
}

/// Extract a Postgres pool from the storage backend.
///
/// Returns 500 if the backend is local (SQLite) mode — secrets require Postgres.
fn require_pg_pool(storage: &StorageBackend) -> Result<sqlx::PgPool, ApiError> {
    match storage {
        StorageBackend::Server(pg)
        | StorageBackend::ServerWithS3(pg, _)
        | StorageBackend::ServerWithMemFs(pg, _) => Ok(pg.pool().clone()),
        StorageBackend::Local(_) => Err(ApiError::internal(
            "agent secrets require server mode (Postgres)",
        )),
    }
}

/// Request body for `POST /api/repos/:repo/agent-secrets`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct SetAgentSecretRequest {
    /// Secret name — must match `[A-Z][A-Z0-9_]*`.
    pub key: String,
    /// Plaintext value to encrypt and store.
    pub value: String,
}

/// Response body for `GET /api/repos/:repo/agent-secrets`.
#[derive(Debug, Serialize, ToSchema)]
pub struct ListAgentSecretsResponse {
    /// Key names stored for this repo. Values are never returned.
    pub keys: Vec<String>,
}

/// Set or overwrite a per-repo agent secret.
///
/// The value is encrypted with AES-256-GCM before storage. Setting a key that
/// already exists overwrites it (upsert semantics). Requires admin repo role.
#[utoipa::path(
    post,
    path = "/api/repos/{repo}/agent-secrets",
    tag = "agent-secrets",
    request_body = SetAgentSecretRequest,
    params(
        ("repo" = String, Path, description = "Repository name"),
    ),
    responses(
        (status = 200, description = "Secret stored successfully"),
        (status = 400, description = "Invalid key name or empty value", body = ErrorBody),
        (status = 401, description = "Unauthorized", body = ErrorBody),
        (status = 403, description = "Forbidden — caller lacks admin role", body = ErrorBody),
        (status = 404, description = "Repository not found", body = ErrorBody),
        (status = 500, description = "Encryption failure (misconfigured master key)", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
)]
pub(super) async fn set_agent_secret_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    ctx: RepoCtx,
    Json(body): Json<SetAgentSecretRequest>,
) -> Result<StatusCode, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, RepoRole::Admin).await?;
    validate_key(&body.key)?;
    if body.value.is_empty() {
        return Err(ApiError::bad_request("value must not be empty"));
    }

    let pool = require_pg_pool(&ctx.storage)?;
    super::secrets::set_secret(&pool, &ctx.repo_id, &body.key, &body.value)
        .await
        .map_err(|e| match e {
            super::secrets::SecretsError::MasterKeyMissing
            | super::secrets::SecretsError::MasterKeyInvalid(_) => {
                ApiError::internal("VAI_SECRETS_MASTER_KEY is misconfigured")
            }
            _ => ApiError::internal(e.to_string()),
        })?;

    tracing::info!(
        event = "agent_secret.set",
        repo_id = %ctx.repo_id,
        key = %body.key,
        "agent secret set"
    );

    let _ = ctx
        .storage
        .events()
        .append(
            &ctx.repo_id,
            EventKind::RepoAgentSecretSet {
                repo_id: ctx.repo_id,
                key: body.key.clone(),
            },
        )
        .await;

    state.broadcast(BroadcastEvent {
        event_type: "RepoAgentSecretSet".to_string(),
        event_id: 0,
        workspace_id: None,
        timestamp: chrono::Utc::now().to_rfc3339(),
        data: serde_json::json!({
            "repo_id": ctx.repo_id.to_string(),
            "key": body.key,
        }),
    });

    Ok(StatusCode::OK)
}

/// List the key names stored for a repo.
///
/// Never returns values — only the key names. Requires admin repo role.
#[utoipa::path(
    get,
    path = "/api/repos/{repo}/agent-secrets",
    tag = "agent-secrets",
    params(
        ("repo" = String, Path, description = "Repository name"),
    ),
    responses(
        (status = 200, description = "Key list", body = ListAgentSecretsResponse),
        (status = 401, description = "Unauthorized", body = ErrorBody),
        (status = 403, description = "Forbidden — caller lacks admin role", body = ErrorBody),
        (status = 404, description = "Repository not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
)]
pub(super) async fn list_agent_secrets_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
) -> Result<Json<ListAgentSecretsResponse>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, RepoRole::Admin).await?;

    let pool = require_pg_pool(&ctx.storage)?;
    let keys = super::secrets::list_secret_keys(&pool, &ctx.repo_id)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok(Json(ListAgentSecretsResponse { keys }))
}

/// Delete a single per-repo agent secret.
///
/// Idempotent — returns 200 whether or not the key existed. Requires admin repo role.
#[utoipa::path(
    delete,
    path = "/api/repos/{repo}/agent-secrets/{key}",
    tag = "agent-secrets",
    params(
        ("repo" = String, Path, description = "Repository name"),
        ("key" = String, Path, description = "Secret key name"),
    ),
    responses(
        (status = 200, description = "Secret deleted (or did not exist)"),
        (status = 401, description = "Unauthorized", body = ErrorBody),
        (status = 403, description = "Forbidden — caller lacks admin role", body = ErrorBody),
        (status = 404, description = "Repository not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
)]
pub(super) async fn delete_agent_secret_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    ctx: RepoCtx,
    AxumPath(params): AxumPath<HashMap<String, String>>,
) -> Result<StatusCode, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, RepoRole::Admin).await?;

    let key = params
        .get("key")
        .cloned()
        .ok_or_else(|| ApiError::bad_request("missing `:key` path parameter"))?;

    let pool = require_pg_pool(&ctx.storage)?;
    super::secrets::delete_secret(&pool, &ctx.repo_id, &key)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    tracing::info!(
        event = "agent_secret.deleted",
        repo_id = %ctx.repo_id,
        key = %key,
        "agent secret deleted"
    );

    let _ = ctx
        .storage
        .events()
        .append(
            &ctx.repo_id,
            EventKind::RepoAgentSecretDeleted {
                repo_id: ctx.repo_id,
                key: key.clone(),
            },
        )
        .await;

    state.broadcast(BroadcastEvent {
        event_type: "RepoAgentSecretDeleted".to_string(),
        event_id: 0,
        workspace_id: None,
        timestamp: chrono::Utc::now().to_rfc3339(),
        data: serde_json::json!({
            "repo_id": ctx.repo_id.to_string(),
            "key": key,
        }),
    });

    Ok(StatusCode::OK)
}
