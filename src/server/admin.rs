//! Admin and management handlers — repos, users, orgs, collaborators, API keys (PRD 24).
//!
//! Endpoints:
//!   - `POST /api/repos` — register a new repository
//!   - `GET /api/repos` — list registered repositories
//!   - `POST /api/users` — create a new user account
//!   - `GET /api/users/:user` — get user by UUID or email
//!   - `GET /api/repos/:repo/me` — authenticated caller's identity and effective role
//!   - `POST /api/orgs` — create organization
//!   - `GET /api/orgs` — list organizations
//!   - `GET /api/orgs/:org` — get organization by slug
//!   - `DELETE /api/orgs/:org` — delete organization
//!   - `POST /api/orgs/:org/members` — add org member
//!   - `GET /api/orgs/:org/members` — list org members
//!   - `PATCH /api/orgs/:org/members/:user` — update member role
//!   - `DELETE /api/orgs/:org/members/:user` — remove org member
//!   - `POST /api/orgs/:org/repos/:repo/collaborators` — add collaborator
//!   - `GET /api/orgs/:org/repos/:repo/collaborators` — list collaborators
//!   - `PATCH /api/orgs/:org/repos/:repo/collaborators/:user` — update collaborator role
//!   - `DELETE /api/orgs/:org/repos/:repo/collaborators/:user` — remove collaborator
//!   - `GET /api/repos/:repo/members` — search repo members for @mention autocomplete
//!   - `POST /api/keys` — create API key
//!   - `GET /api/keys` — list API keys
//!   - `DELETE /api/keys/:id` — revoke key by record UUID
//!   - `DELETE /api/keys` — bulk-revoke keys by repo or user (admin only)

use std::sync::Arc;

use axum::extract::{Extension, Path as AxumPath, Query as AxumQuery, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{AgentIdentity, ApiError, AppState, AuthSource, RepoCtx, require_repo_permission, require_server_admin};

// ── Request / response types for /api/repos ───────────────────────────────────

/// Request body for `POST /api/repos`.
#[derive(Debug, Deserialize, ToSchema)]
pub(super) struct CreateRepoRequest {
    /// Short name for the new repository (alphanumeric, hyphens, underscores).
    name: String,
}

/// Response body for repo list and creation endpoints.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct RepoResponse {
    /// Server-assigned repository UUID.
    ///
    /// Present on `POST /api/repos` (201 Created) and any endpoint that knows
    /// the UUID. Clients MUST overwrite their local `repo_id` with this value.
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<uuid::Uuid>,
    /// Short name of the repository.
    name: String,
    /// Absolute filesystem path to the repository root.
    ///
    /// Only present for bootstrap admin callers; omitted for regular users.
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    /// ISO-8601 timestamp when the repo was registered.
    created_at: String,
    /// Current HEAD version string (e.g. `"v1"`).
    head_version: String,
    /// Number of active workspaces.
    workspace_count: usize,
}

impl RepoResponse {
    fn from_entry(entry: &super::RepoRegistryEntry) -> Self {
        let vai_dir = entry.path.join(".vai");
        // ALLOW_FS: local mode repo listing; tracked by issue #173 to use Postgres in server mode
        let head_version = crate::repo::read_head(&vai_dir).unwrap_or_else(|_| "unknown".to_string());
        let workspace_count = crate::workspace::list(&vai_dir).map(|w| w.len()).unwrap_or(0);
        RepoResponse {
            id: None,
            name: entry.name.clone(),
            // Only local-mode admin listing; path is safe here but we hide it
            // for consistency — non-admin users never call this path.
            path: Some(entry.path.display().to_string()),
            created_at: entry.created_at.to_rfc3339(),
            head_version,
            workspace_count,
        }
    }
}

// ── Repository management handlers ────────────────────────────────────────────

/// Response body when a user's repo quota has been exceeded.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct QuotaExceededBody {
    /// Fixed error string: `"repo quota exceeded"`.
    pub error: String,
    /// The per-user repo limit.
    pub limit: u64,
    /// How many repos the user currently owns.
    pub current: u64,
}

#[utoipa::path(
    post,
    path = "/api/repos",
    request_body = CreateRepoRequest,
    responses(
        (status = 201, description = "Repository created", body = RepoResponse),
        (status = 400, description = "Bad request or multi-repo mode not enabled", body = super::ErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Quota exceeded (non-admin users only)", body = QuotaExceededBody),
        (status = 409, description = "Repository already exists", body = super::ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "repos"
)]
/// `POST /api/repos` — registers and initialises a new repository.
///
/// Any authenticated user may create a repo, subject to a per-user quota
/// (default 100, configurable via `VAI_MAX_REPOS_PER_USER`). Admin keys bypass
/// the quota. On success the creating user is automatically added as an `admin`
/// collaborator on the new repo.
///
/// Returns 400 if multi-repo mode is not enabled (`storage_root` not set) or
/// if the name is already taken. Returns 403 if the quota is exceeded.
pub(super) async fn create_repo_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateRepoRequest>,
) -> Result<(StatusCode, Json<RepoResponse>), ApiError> {
    // Non-admin users must have an associated user_id to create repos.
    if !identity.is_admin && identity.user_id.is_none() {
        return Err(ApiError::forbidden(
            "this key is not associated with a user; cannot create repositories",
        ));
    }
    let storage_root = state.storage_root.as_ref().ok_or_else(|| {
        ApiError::bad_request(
            "server is not in multi-repo mode; set storage_root in ~/.vai/server.toml",
        )
    })?;

    // Validate the repo name: alphanumeric, hyphens, underscores only.
    if body.name.is_empty()
        || !body
            .name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return Err(ApiError::bad_request(
            "repo name must be non-empty and contain only alphanumeric characters, hyphens, and underscores",
        ));
    }

    let _lock = state.repo_lock.lock().await;

    let repo_root = storage_root.join(&body.name);

    // ── Server mode (Postgres / S3) ───────────────────────────────────────────
    // All repo metadata lives in Postgres; no filesystem state is written.
    // Duplicate check queries the `repos` table; no registry.json involved.
    if let crate::storage::StorageBackend::Server(ref pg)
    | crate::storage::StorageBackend::ServerWithS3(ref pg, _)
    | crate::storage::StorageBackend::ServerWithMemFs(ref pg, _) = state.storage
    {
        // Enforce per-user quota (admins are exempt).
        if !identity.is_admin {
            if let Some(user_id) = &identity.user_id {
                let current = state
                    .storage
                    .orgs()
                    .count_repos_owned_by_user(user_id)
                    .await
                    .map_err(|e| ApiError::internal(format!("quota check failed: {e}")))?;
                if current >= state.max_repos_per_user {
                    return Err(ApiError::quota_exceeded(
                        state.max_repos_per_user,
                        current,
                    ));
                }
            }
        }

        // Duplicate check: query Postgres instead of registry.json.
        let existing = state
            .storage
            .get_repo_by_name(&body.name)
            .await
            .map_err(|e| ApiError::internal(format!("failed to check for existing repo: {e}")))?;
        if existing.is_some() {
            return Err(ApiError::conflict(format!(
                "repository '{}' is already registered",
                body.name
            )));
        }

        let repo_id = uuid::Uuid::new_v4();
        let created_at = chrono::Utc::now();

        // Insert repo row into Postgres.
        sqlx::query(
            "INSERT INTO repos (id, name, created_at) VALUES ($1, $2, $3) ON CONFLICT (id) DO NOTHING",
        )
        .bind(repo_id)
        .bind(&body.name)
        .bind(created_at)
        .execute(pg.pool())
        .await
        .map_err(|e| ApiError::internal(format!("failed to insert repo into Postgres: {e}")))?;
        tracing::debug!(repo_id = %repo_id, name = %body.name, "repo inserted into Postgres");

        // Seed the initial v1 version and HEAD in Postgres so version
        // queries never return empty for a brand-new repo.
        let v1 = crate::storage::NewVersion {
            version_id: "v1".to_string(),
            parent_version_id: None,
            intent: "initial repository".to_string(),
            created_by: "system".to_string(),
            merge_event_id: None,
        };
        state
            .storage
            .versions()
            .create_version(&repo_id, v1)
            .await
            .map_err(|e| ApiError::internal(format!("failed to create initial version: {e}")))?;
        state
            .storage
            .versions()
            .advance_head(&repo_id, "v1")
            .await
            .map_err(|e| ApiError::internal(format!("failed to advance head: {e}")))?;

        // Auto-grant the creating user admin collaborator access.
        if let Some(user_id) = &identity.user_id {
            state
                .storage
                .orgs()
                .add_collaborator(&repo_id, user_id, crate::storage::RepoRole::Admin)
                .await
                .map_err(|e| {
                    ApiError::internal(format!("failed to grant creator access: {e}"))
                })?;
            tracing::info!(
                repo_id = %repo_id,
                user_id = %user_id,
                "creator granted admin collaborator role"
            );
        }

        tracing::info!(repo_id = %repo_id, name = %body.name, "repo registered (server mode, no filesystem writes)");

        let response = RepoResponse {
            id: Some(repo_id),
            name: body.name.clone(),
            path: None,
            created_at: created_at.to_rfc3339(),
            head_version: "v1".to_string(),
            workspace_count: 0,
        };
        return Ok((StatusCode::CREATED, Json(response)));
    }

    // ── Local mode (SQLite + filesystem) ──────────────────────────────────────
    // Load registry and check for duplicates via registry.json.
    let mut registry = super::RepoRegistry::load(storage_root)
        .map_err(|e| ApiError::internal(e.to_string()))?;
    if registry.contains(&body.name) {
        return Err(ApiError::conflict(format!(
            "repository '{}' is already registered",
            body.name
        )));
    }

    // Run the full vai init so that the SQLite storage and filesystem-backed
    // helpers (read_head, workspace::list) find the expected directory layout.
    // ALLOW_FS: local-mode repo init writes full .vai/ directory structure
    let repo_root_clone = repo_root.clone();
    let init_result = tokio::task::spawn_blocking(move || crate::repo::init(&repo_root_clone))
        .await
        .map_err(|e| ApiError::internal(format!("task join error: {e}")))?
        .map_err(|e| ApiError::internal(format!("vai init failed: {e}")))?;
    let (repo_id, created_at) = (init_result.config.repo_id, init_result.config.created_at);

    let entry = super::RepoRegistryEntry {
        name: body.name.clone(),
        path: repo_root,
        created_at,
    };

    // Persist the updated registry (used by repo_resolve_middleware to map name → path).
    registry.repos.push(entry.clone());
    registry
        .save(storage_root)
        .map_err(|e| ApiError::internal(e.to_string()))?;

    tracing::info!(repo_id = %repo_id, name = %entry.name, path = %entry.path.display(), "repo registered");

    let response = RepoResponse {
        id: Some(repo_id),
        name: entry.name.clone(),
        // Admin-only: local mode exposes path for backward compat.
        path: if identity.is_admin { Some(entry.path.display().to_string()) } else { None },
        created_at: entry.created_at.to_rfc3339(),
        head_version: "v1".to_string(),
        workspace_count: 0,
    };
    Ok((StatusCode::CREATED, Json(response)))
}

#[utoipa::path(
    get,
    path = "/api/repos",
    responses(
        (status = 200, description = "List of registered repositories", body = Vec<RepoResponse>),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = [])),
    tag = "repos"
)]
/// `GET /api/repos` — lists all registered repositories with basic stats.
///
/// Admin key returns all repos. JWT or API-key users receive only repos they
/// have access to (via direct `repo_collaborators` entry or org owner/admin
/// membership). Unauthenticated requests are rejected by the auth middleware.
pub(super) async fn list_repos_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<RepoResponse>>, ApiError> {
    use crate::storage::ListQuery;

    // In server mode (Postgres), read repo list and stats from Postgres so that
    // this handler works without touching the filesystem.
    if let crate::storage::StorageBackend::Server(ref pg)
    | crate::storage::StorageBackend::ServerWithS3(ref pg, _)
    | crate::storage::StorageBackend::ServerWithMemFs(ref pg, _) = state.storage
    {
        use sqlx::Row as _;

        // Fetch the list of (id, name, created_at) rows the caller can see.
        let rows = if identity.is_admin {
            // Admin key — return every repo.
            sqlx::query("SELECT id, name, created_at FROM repos ORDER BY created_at ASC")
                .fetch_all(pg.pool())
                .await
                .map_err(|e| ApiError::internal(format!("failed to query repos: {e}")))?
        } else if let Some(user_id) = identity.user_id {
            // JWT / API-key user — return only repos the user can access:
            //   1. direct repo_collaborators entry, OR
            //   2. org owner/admin membership (org members need an explicit
            //      collaborator row; only owner/admin roles confer implicit access).
            sqlx::query(
                "SELECT DISTINCT r.id, r.name, r.created_at
                 FROM repos r
                 WHERE (
                     EXISTS (
                         SELECT 1 FROM repo_collaborators rc
                         WHERE rc.repo_id = r.id AND rc.user_id = $1
                     )
                     OR EXISTS (
                         SELECT 1 FROM org_members om
                         WHERE om.org_id = r.org_id
                           AND om.user_id = $1
                           AND om.role IN ('owner', 'admin')
                     )
                 )
                 ORDER BY r.created_at ASC",
            )
            .bind(user_id)
            .fetch_all(pg.pool())
            .await
            .map_err(|e| ApiError::internal(format!("failed to query repos: {e}")))?
        } else {
            // Non-admin API key without an associated user — no repo access.
            vec![]
        };

        let mut responses = Vec::with_capacity(rows.len());
        for row in rows {
            let repo_id: uuid::Uuid = row.get("id");
            let name: String = row.get("name");
            let created_at: chrono::DateTime<chrono::Utc> = row.get("created_at");

            let head_version = state
                .storage
                .versions()
                .read_head(&repo_id)
                .await
                .unwrap_or(None)
                .unwrap_or_else(|| "v1".to_string());

            let workspace_count = state
                .storage
                .workspaces()
                .list_workspaces(&repo_id, false, &ListQuery::default())
                .await
                .map(|r| r.total as usize)
                .unwrap_or(0);

            // Expose internal path only to bootstrap admin callers.
            let path = if identity.is_admin {
                state
                    .storage_root
                    .as_ref()
                    .map(|sr| sr.join(&name).display().to_string())
            } else {
                None
            };

            responses.push(RepoResponse {
                id: Some(repo_id),
                name,
                path,
                created_at: created_at.to_rfc3339(),
                head_version,
                workspace_count,
            });
        }
        return Ok(Json(responses));
    }

    // Local mode: admin sees all repos from the on-disk registry; non-admin
    // users have no RBAC data available so return an empty list.
    if !identity.is_admin {
        return Ok(Json(vec![]));
    }

    let storage_root = match state.storage_root.as_ref() {
        Some(sr) => sr,
        None => return Ok(Json(vec![])),
    };

    let registry = super::RepoRegistry::load(storage_root).map_err(|e| ApiError::internal(e.to_string()))?;
    let responses: Vec<RepoResponse> = registry
        .repos
        .iter()
        .map(RepoResponse::from_entry)
        .collect();

    Ok(Json(responses))
}

// ── Org / User API types ──────────────────────────────────────────────────────

/// Request body for `POST /api/orgs`.
#[derive(Debug, Deserialize, ToSchema)]
pub(super) struct CreateOrgRequest {
    name: String,
    slug: String,
}

/// Request body for `POST /api/users`.
#[derive(Debug, Deserialize, ToSchema)]
pub(super) struct CreateUserRequest {
    email: String,
    name: String,
}

/// Request body for `POST /api/orgs/:org/members`.
#[derive(Debug, Deserialize, ToSchema)]
pub(super) struct AddMemberRequest {
    /// User UUID to add.
    #[schema(value_type = String)]
    user_id: uuid::Uuid,
    /// Role within the org: `"owner"`, `"admin"`, or `"member"`.
    role: String,
}

/// Request body for `PATCH /api/orgs/:org/members/:user`.
#[derive(Debug, Deserialize, ToSchema)]
pub(super) struct UpdateMemberRequest {
    /// New role: `"owner"`, `"admin"`, or `"member"`.
    role: String,
}

/// Response body for org endpoints.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct OrgResponse {
    id: String,
    name: String,
    slug: String,
    created_at: String,
}

impl From<crate::storage::Organization> for OrgResponse {
    fn from(o: crate::storage::Organization) -> Self {
        OrgResponse {
            id: o.id.to_string(),
            name: o.name,
            slug: o.slug,
            created_at: o.created_at.to_rfc3339(),
        }
    }
}

/// Response body for user endpoints.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct UserResponse {
    id: String,
    email: String,
    name: String,
    created_at: String,
}

impl From<crate::storage::User> for UserResponse {
    fn from(u: crate::storage::User) -> Self {
        UserResponse {
            id: u.id.to_string(),
            email: u.email,
            name: u.name,
            created_at: u.created_at.to_rfc3339(),
        }
    }
}

/// Response body for `GET /api/repos/:repo/me`.
#[derive(Debug, Serialize, ToSchema)]
pub struct MeResponse {
    /// The authenticated user's identifier.
    ///
    /// `"admin"` for the bootstrap admin key; a UUID string for scoped keys
    /// associated with a user account; the key record ID for legacy keys
    /// without a user association (local mode).
    pub user_id: String,
    /// The user's email address, or `null` for admin and legacy keys.
    pub email: Option<String>,
    /// Effective role on this repository.
    ///
    /// One of `"owner"`, `"admin"`, `"write"`, or `"read"`.
    /// The bootstrap admin key always returns `"admin"`.
    pub role: String,
    /// Authentication method used for this request.
    ///
    /// One of `"api_key"` (API key or admin key) or `"jwt"` (JWT access token).
    pub auth_type: String,
}

/// Response body for org membership endpoints.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct OrgMemberResponse {
    org_id: String,
    user_id: String,
    role: String,
    created_at: String,
}

impl From<crate::storage::OrgMember> for OrgMemberResponse {
    fn from(m: crate::storage::OrgMember) -> Self {
        OrgMemberResponse {
            org_id: m.org_id.to_string(),
            user_id: m.user_id.to_string(),
            role: m.role.as_str().to_string(),
            created_at: m.created_at.to_rfc3339(),
        }
    }
}

// ── Org / User handlers ───────────────────────────────────────────────────────

#[utoipa::path(
    post,
    path = "/api/orgs",
    request_body = CreateOrgRequest,
    responses(
        (status = 201, description = "Organization created", body = OrgResponse),
        (status = 400, description = "Bad request", body = super::ErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden — requires admin key"),
        (status = 409, description = "Slug already exists", body = super::ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "orgs"
)]
/// `POST /api/orgs` — creates a new organization.
pub(super) async fn create_org_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateOrgRequest>,
) -> Result<(StatusCode, Json<OrgResponse>), ApiError> {
    require_server_admin(&identity)?;
    use crate::storage::NewOrg;

    if body.slug.is_empty()
        || !body.slug.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return Err(ApiError::bad_request(
            "slug must be non-empty and contain only alphanumeric characters, hyphens, and underscores",
        ));
    }

    let org = state
        .storage
        .orgs()
        .create_org(NewOrg { name: body.name, slug: body.slug })
        .await
        .map_err(ApiError::from)?;

    tracing::info!(
        event = "admin.org.created",
        actor = %identity.name,
        org_id = %org.id,
        org_slug = %org.slug,
        "organization created"
    );
    Ok((StatusCode::CREATED, Json(OrgResponse::from(org))))
}

#[utoipa::path(
    get,
    path = "/api/orgs",
    responses(
        (status = 200, description = "List of organizations", body = Vec<OrgResponse>),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden — requires admin key"),
    ),
    security(("bearer_auth" = [])),
    tag = "orgs"
)]
/// `GET /api/orgs` — lists all organizations (server-level admin use).
pub(super) async fn list_orgs_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<OrgResponse>>, ApiError> {
    require_server_admin(&identity)?;
    let orgs = state.storage.orgs().list_orgs().await.map_err(ApiError::from)?;
    Ok(Json(orgs.into_iter().map(OrgResponse::from).collect()))
}

#[utoipa::path(
    get,
    path = "/api/orgs/{org}",
    params(
        ("org" = String, Path, description = "Organization slug"),
    ),
    responses(
        (status = 200, description = "Organization found", body = OrgResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden — requires admin key"),
        (status = 404, description = "Not found", body = super::ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "orgs"
)]
/// `GET /api/orgs/:org` — returns the organization with the given slug.
pub(super) async fn get_org_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    AxumPath(slug): AxumPath<String>,
) -> Result<Json<OrgResponse>, ApiError> {
    require_server_admin(&identity)?;
    let org = state.storage.orgs().get_org_by_slug(&slug).await.map_err(ApiError::from)?;
    Ok(Json(OrgResponse::from(org)))
}

#[utoipa::path(
    delete,
    path = "/api/orgs/{org}",
    params(
        ("org" = String, Path, description = "Organization slug"),
    ),
    responses(
        (status = 204, description = "Deleted"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden — requires admin key"),
        (status = 404, description = "Not found", body = super::ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "orgs"
)]
/// `DELETE /api/orgs/:org` — permanently deletes an org by slug (cascades to repos).
pub(super) async fn delete_org_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    AxumPath(slug): AxumPath<String>,
) -> Result<StatusCode, ApiError> {
    require_server_admin(&identity)?;
    let org = state.storage.orgs().get_org_by_slug(&slug).await.map_err(ApiError::from)?;
    state.storage.orgs().delete_org(&org.id).await.map_err(ApiError::from)?;
    tracing::info!(
        event = "admin.org.deleted",
        actor = %identity.name,
        org_id = %org.id,
        org_slug = %slug,
        "organization deleted"
    );
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/api/users",
    request_body = CreateUserRequest,
    responses(
        (status = 201, description = "User created", body = UserResponse),
        (status = 400, description = "Bad request", body = super::ErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden — requires admin key"),
        (status = 409, description = "User already exists", body = super::ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "auth"
)]
/// `POST /api/users` — creates a new user account.
pub(super) async fn create_user_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateUserRequest>,
) -> Result<(StatusCode, Json<UserResponse>), ApiError> {
    require_server_admin(&identity)?;
    use crate::storage::NewUser;

    let user = state
        .storage
        .orgs()
        .create_user(NewUser { email: body.email, name: body.name, better_auth_id: None })
        .await
        .map_err(ApiError::from)?;

    tracing::info!(
        event = "admin.user.created",
        actor = %identity.name,
        user_id = %user.id,
        user_email = %user.email,
        "user created"
    );
    Ok((StatusCode::CREATED, Json(UserResponse::from(user))))
}

#[utoipa::path(
    get,
    path = "/api/users/{user}",
    params(
        ("user" = String, Path, description = "User UUID or email address"),
    ),
    responses(
        (status = 200, description = "User found", body = UserResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden — requires admin key"),
        (status = 404, description = "Not found", body = super::ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "auth"
)]
/// `GET /api/users/:user` — fetches a user by UUID or email.
///
/// The `:user` path segment is tried first as a UUID; if it cannot be parsed as
/// one it is treated as an email address.
pub(super) async fn get_user_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    AxumPath(user_ref): AxumPath<String>,
) -> Result<Json<UserResponse>, ApiError> {
    require_server_admin(&identity)?;
    let orgs = state.storage.orgs();
    let user = if let Ok(id) = uuid::Uuid::parse_str(&user_ref) {
        orgs.get_user(&id).await.map_err(ApiError::from)?
    } else {
        orgs.get_user_by_email(&user_ref).await.map_err(ApiError::from)?
    };
    Ok(Json(UserResponse::from(user)))
}

#[utoipa::path(
    get,
    path = "/api/repos/{repo}/me",
    params(
        ("repo" = String, Path, description = "Repository name"),
    ),
    responses(
        (status = 200, description = "Authenticated user info for this repo", body = MeResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "No access to this repository"),
    ),
    security(("bearer_auth" = [])),
    tag = "auth"
)]
/// `GET /api/repos/:repo/me` — returns the authenticated caller's identity and
/// effective role on the named repository.
///
/// Reads the [`AgentIdentity`] injected by the auth middleware and resolves:
/// - Bootstrap admin key → role `"admin"`, no email.
/// - JWT or scoped key with user → look up email and resolve repo role via [`OrgStore`].
/// - Legacy/local key without user → role `"owner"` (local mode grants full access).
pub(super) async fn get_me_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
) -> Result<Json<MeResponse>, ApiError> {
    let auth_type = match identity.auth_source {
        AuthSource::Jwt => "jwt",
        AuthSource::ApiKey => "api_key",
        AuthSource::AdminKey => "api_key",
    }
    .to_string();

    // Bootstrap admin key always has full access.
    if identity.is_admin {
        return Ok(Json(MeResponse {
            user_id: "admin".to_string(),
            email: None,
            role: "admin".to_string(),
            auth_type,
        }));
    }

    // Scoped key or JWT associated with a user: resolve effective repo role via OrgStore.
    if let Some(uid) = &identity.user_id {
        let user = ctx
            .storage
            .orgs()
            .get_user(uid)
            .await
            .map_err(|e| ApiError::internal(format!("user lookup failed: {e}")))?;

        let resolved = ctx
            .storage
            .orgs()
            .resolve_repo_role(uid, &ctx.repo_id)
            .await
            .map_err(|e| ApiError::internal(format!("role resolution failed: {e}")))?;

        let effective = match resolved {
            None => return Err(ApiError::forbidden("no access to this repository")),
            Some(r) => r,
        };

        // Apply the key-level role cap if one is set.
        let effective = if let Some(cap_str) = &identity.role_override {
            let cap = crate::storage::RepoRole::from_db_str(cap_str);
            if effective.rank() > cap.rank() { cap } else { effective }
        } else {
            effective
        };

        return Ok(Json(MeResponse {
            user_id: uid.to_string(),
            email: Some(user.email),
            role: effective.as_str().to_string(),
            auth_type,
        }));
    }

    // Legacy key with no user association (local SQLite mode).
    // Any authenticated key has full owner access in local mode.
    Ok(Json(MeResponse {
        user_id: identity.key_id.clone(),
        email: None,
        role: "owner".to_string(),
        auth_type,
    }))
}

#[utoipa::path(
    get,
    path = "/api/orgs/{org}/members",
    params(
        ("org" = String, Path, description = "Organization slug"),
    ),
    responses(
        (status = 200, description = "List of org members", body = Vec<OrgMemberResponse>),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden — requires admin key"),
        (status = 404, description = "Organization not found", body = super::ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "orgs"
)]
/// `GET /api/orgs/:org/members` — lists all members of an organization.
pub(super) async fn list_org_members_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    AxumPath(slug): AxumPath<String>,
) -> Result<Json<Vec<OrgMemberResponse>>, ApiError> {
    require_server_admin(&identity)?;
    let orgs = state.storage.orgs();
    let org = orgs.get_org_by_slug(&slug).await.map_err(ApiError::from)?;
    let members = orgs.list_org_members(&org.id).await.map_err(ApiError::from)?;
    Ok(Json(members.into_iter().map(OrgMemberResponse::from).collect()))
}

#[utoipa::path(
    post,
    path = "/api/orgs/{org}/members",
    params(
        ("org" = String, Path, description = "Organization slug"),
    ),
    request_body = AddMemberRequest,
    responses(
        (status = 201, description = "Member added", body = OrgMemberResponse),
        (status = 400, description = "Bad request", body = super::ErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden — requires admin key"),
        (status = 404, description = "Organization or user not found", body = super::ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "orgs"
)]
/// `POST /api/orgs/:org/members` — adds a user as a member of an organization.
pub(super) async fn add_org_member_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    AxumPath(slug): AxumPath<String>,
    Json(body): Json<AddMemberRequest>,
) -> Result<(StatusCode, Json<OrgMemberResponse>), ApiError> {
    require_server_admin(&identity)?;
    use crate::storage::OrgRole;

    let role = match body.role.as_str() {
        "owner" => OrgRole::Owner,
        "admin" => OrgRole::Admin,
        "member" => OrgRole::Member,
        other => {
            return Err(ApiError::bad_request(format!(
                "unknown org role `{other}`; expected one of: owner, admin, member"
            )));
        }
    };

    let orgs = state.storage.orgs();
    let org = orgs.get_org_by_slug(&slug).await.map_err(ApiError::from)?;
    let member = orgs.add_org_member(&org.id, &body.user_id, role).await.map_err(ApiError::from)?;
    tracing::info!(
        event = "admin.member.added",
        actor = %identity.name,
        org_slug = %slug,
        user_id = %body.user_id,
        role = %body.role,
        "org member added"
    );
    Ok((StatusCode::CREATED, Json(OrgMemberResponse::from(member))))
}

#[utoipa::path(
    patch,
    path = "/api/orgs/{org}/members/{user}",
    params(
        ("org" = String, Path, description = "Organization slug"),
        ("user" = String, Path, description = "User UUID"),
    ),
    request_body = UpdateMemberRequest,
    responses(
        (status = 200, description = "Member role updated", body = OrgMemberResponse),
        (status = 400, description = "Bad request", body = super::ErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden — requires admin key"),
        (status = 404, description = "Organization or member not found", body = super::ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "orgs"
)]
/// `PATCH /api/orgs/:org/members/:user` — updates a member's role.
pub(super) async fn update_org_member_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    AxumPath((slug, user_id)): AxumPath<(String, uuid::Uuid)>,
    Json(body): Json<UpdateMemberRequest>,
) -> Result<Json<OrgMemberResponse>, ApiError> {
    require_server_admin(&identity)?;
    use crate::storage::OrgRole;

    let role = match body.role.as_str() {
        "owner" => OrgRole::Owner,
        "admin" => OrgRole::Admin,
        "member" => OrgRole::Member,
        other => {
            return Err(ApiError::bad_request(format!(
                "unknown org role `{other}`; expected one of: owner, admin, member"
            )));
        }
    };

    let orgs = state.storage.orgs();
    let org = orgs.get_org_by_slug(&slug).await.map_err(ApiError::from)?;
    let member = orgs.update_org_member(&org.id, &user_id, role).await.map_err(ApiError::from)?;
    tracing::info!(
        event = "admin.member.updated",
        actor = %identity.name,
        org_slug = %slug,
        user_id = %user_id,
        role = %body.role,
        "org member role updated"
    );
    Ok(Json(OrgMemberResponse::from(member)))
}

#[utoipa::path(
    delete,
    path = "/api/orgs/{org}/members/{user}",
    params(
        ("org" = String, Path, description = "Organization slug"),
        ("user" = String, Path, description = "User UUID"),
    ),
    responses(
        (status = 204, description = "Deleted"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden — requires admin key"),
        (status = 404, description = "Organization or member not found", body = super::ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "orgs"
)]
/// `DELETE /api/orgs/:org/members/:user` — removes a user from an organization.
pub(super) async fn remove_org_member_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    AxumPath((slug, user_id)): AxumPath<(String, uuid::Uuid)>,
) -> Result<StatusCode, ApiError> {
    require_server_admin(&identity)?;
    let orgs = state.storage.orgs();
    let org = orgs.get_org_by_slug(&slug).await.map_err(ApiError::from)?;
    orgs.remove_org_member(&org.id, &user_id).await.map_err(ApiError::from)?;
    tracing::info!(
        event = "admin.member.removed",
        actor = %identity.name,
        org_slug = %slug,
        user_id = %user_id,
        "org member removed"
    );
    Ok(StatusCode::NO_CONTENT)
}

// ── Repo collaborator handlers (PRD 10.3) ─────────────────────────────────────

/// Request body for `POST /api/orgs/:org/repos/:repo/collaborators`.
#[derive(Debug, Deserialize, ToSchema)]
pub(super) struct AddCollaboratorRequest {
    /// User UUID to add as a collaborator.
    #[schema(value_type = String)]
    user_id: uuid::Uuid,
    /// Role on the repository: `"owner"`, `"admin"`, `"write"`, or `"read"`.
    role: String,
}

/// Request body for `PATCH /api/orgs/:org/repos/:repo/collaborators/:user`.
#[derive(Debug, Deserialize, ToSchema)]
pub(super) struct UpdateCollaboratorRequest {
    /// New role: `"owner"`, `"admin"`, `"write"`, or `"read"`.
    role: String,
}

/// Response body for repo collaborator endpoints.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct CollaboratorResponse {
    repo_id: String,
    user_id: String,
    role: String,
    created_at: String,
}

impl From<crate::storage::RepoCollaborator> for CollaboratorResponse {
    fn from(c: crate::storage::RepoCollaborator) -> Self {
        CollaboratorResponse {
            repo_id: c.repo_id.to_string(),
            user_id: c.user_id.to_string(),
            role: c.role.as_str().to_string(),
            created_at: c.created_at.to_rfc3339(),
        }
    }
}

/// Parses a repo role string, returning a 400 error for unknown values.
fn parse_repo_role(s: &str) -> Result<crate::storage::RepoRole, ApiError> {
    use crate::storage::RepoRole;
    match s {
        "owner" => Ok(RepoRole::Owner),
        "admin" => Ok(RepoRole::Admin),
        "write" => Ok(RepoRole::Write),
        "read" => Ok(RepoRole::Read),
        other => Err(ApiError::bad_request(format!(
            "unknown repo role `{other}`; expected one of: owner, admin, write, read"
        ))),
    }
}

#[utoipa::path(
    post,
    path = "/api/orgs/{org}/repos/{repo}/collaborators",
    params(
        ("org" = String, Path, description = "Organization slug"),
        ("repo" = String, Path, description = "Repository name"),
    ),
    request_body = AddCollaboratorRequest,
    responses(
        (status = 201, description = "Collaborator added", body = CollaboratorResponse),
        (status = 400, description = "Bad request", body = super::ErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Not found", body = super::ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "repos"
)]
/// `POST /api/orgs/:org/repos/:repo/collaborators` — adds a collaborator to a repo.
pub(super) async fn add_collaborator_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    AxumPath((org_slug, repo_name)): AxumPath<(String, String)>,
    Json(body): Json<AddCollaboratorRequest>,
) -> Result<(StatusCode, Json<CollaboratorResponse>), ApiError> {
    let role = parse_repo_role(&body.role)?;
    let orgs = state.storage.orgs();
    let org = orgs.get_org_by_slug(&org_slug).await.map_err(ApiError::from)?;
    let repo_id = orgs.get_repo_id_in_org(&org.id, &repo_name).await.map_err(ApiError::from)?;
    // Write permission required to add collaborators (invite).
    require_repo_permission(&state.storage, &identity, &repo_id, crate::storage::RepoRole::Write).await?;
    let collaborator = orgs.add_collaborator(&repo_id, &body.user_id, role).await.map_err(ApiError::from)?;
    tracing::info!(
        event = "admin.collaborator.added",
        actor = %identity.name,
        org_slug = %org_slug,
        repo = %repo_id,
        user_id = %body.user_id,
        role = %body.role,
        "repo collaborator added"
    );
    Ok((StatusCode::CREATED, Json(CollaboratorResponse::from(collaborator))))
}

#[utoipa::path(
    get,
    path = "/api/orgs/{org}/repos/{repo}/collaborators",
    params(
        ("org" = String, Path, description = "Organization slug"),
        ("repo" = String, Path, description = "Repository name"),
    ),
    responses(
        (status = 200, description = "List of collaborators", body = Vec<CollaboratorResponse>),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = [])),
    tag = "repos"
)]
/// `GET /api/orgs/:org/repos/:repo/collaborators` — lists all collaborators on a repo.
pub(super) async fn list_collaborators_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    AxumPath((org_slug, repo_name)): AxumPath<(String, String)>,
) -> Result<Json<Vec<CollaboratorResponse>>, ApiError> {
    let orgs = state.storage.orgs();
    let org = orgs.get_org_by_slug(&org_slug).await.map_err(ApiError::from)?;
    let repo_id = orgs.get_repo_id_in_org(&org.id, &repo_name).await.map_err(ApiError::from)?;
    require_repo_permission(&state.storage, &identity, &repo_id, crate::storage::RepoRole::Read).await?;
    let collaborators = orgs.list_collaborators(&repo_id).await.map_err(ApiError::from)?;
    Ok(Json(collaborators.into_iter().map(CollaboratorResponse::from).collect()))
}

#[utoipa::path(
    patch,
    path = "/api/orgs/{org}/repos/{repo}/collaborators/{user}",
    params(
        ("org" = String, Path, description = "Organization slug"),
        ("repo" = String, Path, description = "Repository name"),
        ("user" = String, Path, description = "User ID"),
    ),
    request_body = UpdateCollaboratorRequest,
    responses(
        (status = 200, description = "Collaborator updated", body = CollaboratorResponse),
        (status = 400, description = "Bad request", body = super::ErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Not found", body = super::ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "repos"
)]
/// `PATCH /api/orgs/:org/repos/:repo/collaborators/:user` — updates a collaborator's role.
pub(super) async fn update_collaborator_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    AxumPath((org_slug, repo_name, user_id)): AxumPath<(String, String, uuid::Uuid)>,
    Json(body): Json<UpdateCollaboratorRequest>,
) -> Result<Json<CollaboratorResponse>, ApiError> {
    let role = parse_repo_role(&body.role)?;
    let orgs = state.storage.orgs();
    let org = orgs.get_org_by_slug(&org_slug).await.map_err(ApiError::from)?;
    let repo_id = orgs.get_repo_id_in_org(&org.id, &repo_name).await.map_err(ApiError::from)?;
    // Admin permission required to change roles.
    require_repo_permission(&state.storage, &identity, &repo_id, crate::storage::RepoRole::Admin).await?;
    let collaborator = orgs.update_collaborator(&repo_id, &user_id, role).await.map_err(ApiError::from)?;
    tracing::info!(
        event = "admin.collaborator.updated",
        actor = %identity.name,
        org_slug = %org_slug,
        repo = %repo_id,
        user_id = %user_id,
        role = %body.role,
        "repo collaborator role updated"
    );
    Ok(Json(CollaboratorResponse::from(collaborator)))
}

#[utoipa::path(
    delete,
    path = "/api/orgs/{org}/repos/{repo}/collaborators/{user}",
    params(
        ("org" = String, Path, description = "Organization slug"),
        ("repo" = String, Path, description = "Repository name"),
        ("user" = String, Path, description = "User ID"),
    ),
    responses(
        (status = 204, description = "Collaborator removed"),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Not found", body = super::ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "repos"
)]
/// `DELETE /api/orgs/:org/repos/:repo/collaborators/:user` — removes a collaborator from a repo.
pub(super) async fn remove_collaborator_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    AxumPath((org_slug, repo_name, user_id)): AxumPath<(String, String, uuid::Uuid)>,
) -> Result<StatusCode, ApiError> {
    let orgs = state.storage.orgs();
    let org = orgs.get_org_by_slug(&org_slug).await.map_err(ApiError::from)?;
    let repo_id = orgs.get_repo_id_in_org(&org.id, &repo_name).await.map_err(ApiError::from)?;
    // Admin permission required to remove collaborators.
    require_repo_permission(&state.storage, &identity, &repo_id, crate::storage::RepoRole::Admin).await?;
    orgs.remove_collaborator(&repo_id, &user_id).await.map_err(ApiError::from)?;
    tracing::info!(
        event = "admin.collaborator.removed",
        actor = %identity.name,
        org_slug = %org_slug,
        repo = %repo_id,
        user_id = %user_id,
        "repo collaborator removed"
    );
    Ok(StatusCode::NO_CONTENT)
}

// ── Repo members search (PRD 22, Issue 5) ────────────────────────────────────

/// Query parameters for `GET /api/repos/:repo/members`.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub(super) struct MembersSearchParams {
    /// Case-insensitive prefix to filter members by name or email.
    /// An empty or absent `q` returns the first 10 members alphabetically.
    #[serde(default)]
    q: String,
}

/// A repo member — either a human user or an agent API key.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct RepoMemberResponse {
    /// Stable UUID — user ID for humans, API key ID for agents.
    id: String,
    /// Display name.
    name: String,
    /// `"human"` for users, `"agent"` for API keys.
    #[serde(rename = "type")]
    member_type: String,
}

impl From<crate::storage::RepoMember> for RepoMemberResponse {
    fn from(m: crate::storage::RepoMember) -> Self {
        RepoMemberResponse {
            id: m.id,
            name: m.name,
            member_type: m.member_type,
        }
    }
}

#[utoipa::path(
    get,
    path = "/api/repos/{repo}/members",
    params(
        ("repo" = String, Path, description = "Repository slug"),
        MembersSearchParams,
    ),
    responses(
        (status = 200, description = "List of matching repo members", body = Vec<RepoMemberResponse>),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden"),
    ),
    security(("bearer_auth" = [])),
    tag = "repos"
)]
/// `GET /api/repos/:repo/members` — searches for repo members for @mention autocomplete.
///
/// Returns up to 10 users (with access via collaborators or org membership) and
/// agent API keys whose names match the `q` prefix (case-insensitive).
pub(super) async fn search_repo_members_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    AxumQuery(params): AxumQuery<MembersSearchParams>,
) -> Result<Json<Vec<RepoMemberResponse>>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Read).await?;

    let members = ctx.storage.orgs()
        .search_repo_members(&ctx.repo_id, &params.q, 10)
        .await
        .map_err(ApiError::from)?;

    Ok(Json(members.into_iter().map(RepoMemberResponse::from).collect()))
}

// ── API key management (PRD 10.3) ─────────────────────────────────────────────

/// Request body for `POST /api/keys`.
#[derive(Debug, Deserialize, ToSchema)]
pub(super) struct CreateKeyRequest {
    /// Human-readable name for this key.
    name: String,
    /// Repository UUID to scope this key to. `None` for server-level keys.
    #[schema(value_type = Option<String>)]
    repo_id: Option<uuid::Uuid>,
    /// Optional role cap. When set, the key's effective permissions are the
    /// lesser of the creator's role and this value.
    /// Accepted values: `"owner"`, `"admin"`, `"write"`, `"read"`.
    role_override: Option<String>,
    /// Optional label for the kind of agent that will use this key
    /// (e.g. `"ci"`, `"worker"`, `"human"`).
    agent_type: Option<String>,
    /// Optional expiry timestamp (RFC-3339). `None` means the key never expires.
    #[schema(value_type = Option<String>)]
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Admin-only: create the key on behalf of this user UUID rather than
    /// the authenticated admin identity. Ignored for non-admin callers.
    #[schema(value_type = Option<String>)]
    for_user_id: Option<uuid::Uuid>,
}

/// Response body for key creation.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct CreateKeyResponse {
    /// Key metadata (same shape as `ApiKeyResponse`).
    key: ApiKeyResponse,
    /// The plaintext token — shown only once.
    token: String,
}

/// Response body for key list/get endpoints.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct ApiKeyResponse {
    id: String,
    name: String,
    key_prefix: String,
    created_at: String,
    last_used_at: Option<String>,
    user_id: Option<String>,
    role_override: Option<String>,
    /// Optional agent type label (e.g. `"ci"`, `"worker"`, `"human"`).
    agent_type: Option<String>,
    /// Optional expiry timestamp (RFC-3339). `null` means the key never expires.
    expires_at: Option<String>,
}

impl From<crate::auth::ApiKey> for ApiKeyResponse {
    fn from(k: crate::auth::ApiKey) -> Self {
        ApiKeyResponse {
            id: k.id,
            name: k.name,
            key_prefix: k.key_prefix,
            created_at: k.created_at.to_rfc3339(),
            last_used_at: k.last_used_at.map(|t| t.to_rfc3339()),
            user_id: k.user_id.map(|u| u.to_string()),
            role_override: k.role_override,
            agent_type: k.agent_type,
            expires_at: k.expires_at.map(|t| t.to_rfc3339()),
        }
    }
}

#[utoipa::path(
    post,
    path = "/api/keys",
    request_body = CreateKeyRequest,
    responses(
        (status = 201, description = "API key created", body = CreateKeyResponse),
        (status = 400, description = "Bad request", body = super::ErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden", body = super::ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "auth"
)]
/// `POST /api/keys` — creates a new API key scoped to the authenticated user.
///
/// The key's effective permissions are the lesser of the creator's own role and
/// the requested `role_override`. A user cannot create a key with more
/// permissions than they currently have.
pub(super) async fn create_key_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateKeyRequest>,
) -> Result<(StatusCode, Json<CreateKeyResponse>), ApiError> {
    // Admin keys can create keys without a user_id association, or on behalf
    // of a specific user via `for_user_id`. User-linked keys require a user_id
    // on the identity.
    let user_id = if identity.is_admin {
        body.for_user_id
    } else {
        match identity.user_id {
            Some(uid) => Some(uid),
            None => {
                return Err(ApiError::forbidden(
                    "this key is not associated with a user; cannot create scoped keys",
                ));
            }
        }
    };

    // Validate role_override: must be a recognised role string.
    let role_override = match &body.role_override {
        Some(r) => {
            let parsed = crate::storage::RepoRole::from_db_str(r);
            // Ensure the creator is not escalating beyond their own permissions.
            if !identity.is_admin {
                if let Some(repo_id) = &body.repo_id {
                    let effective = require_repo_permission(
                        &state.storage,
                        &identity,
                        repo_id,
                        crate::storage::RepoRole::Read,
                    )
                    .await?;
                    if parsed.rank() > effective.rank() {
                        return Err(ApiError::bad_request(
                            "role_override cannot exceed your own effective role on this repo",
                        ));
                    }
                }
            }
            Some(parsed.as_str().to_string())
        }
        None => None,
    };

    let auth = state.storage.auth();
    let (key_meta, token) = auth
        .create_key(
            body.repo_id.as_ref(),
            &body.name,
            user_id.as_ref(),
            role_override.as_deref(),
            body.agent_type.as_deref(),
            body.expires_at,
        )
        .await
        .map_err(ApiError::from)?;

    tracing::info!(
        event = "key.created",
        actor = %identity.name,
        key_id = %key_meta.id,
        key_name = %key_meta.name,
        key_prefix = %key_meta.key_prefix,
        "API key created"
    );
    Ok((
        StatusCode::CREATED,
        Json(CreateKeyResponse {
            key: ApiKeyResponse::from(key_meta),
            token,
        }),
    ))
}

#[utoipa::path(
    get,
    path = "/api/keys",
    responses(
        (status = 200, description = "List of API keys", body = Vec<ApiKeyResponse>),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = [])),
    tag = "auth"
)]
/// `GET /api/keys` — lists all active keys belonging to the authenticated user.
///
/// Admin keys see all server-level keys; user keys see only keys owned by
/// that user.
pub(super) async fn list_keys_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<ApiKeyResponse>>, ApiError> {
    let auth = state.storage.auth();

    let keys = if identity.is_admin {
        auth.list_keys(None).await.map_err(ApiError::from)?
    } else {
        match identity.user_id {
            Some(uid) => auth.list_keys_by_user(&uid).await.map_err(ApiError::from)?,
            None => {
                return Err(ApiError::forbidden(
                    "this key is not associated with a user; cannot list keys",
                ));
            }
        }
    };

    Ok(Json(keys.into_iter().map(ApiKeyResponse::from).collect()))
}

#[utoipa::path(
    delete,
    path = "/api/keys/{id}",
    params(
        ("id" = String, Path, description = "API key record UUID"),
    ),
    responses(
        (status = 204, description = "Key revoked"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden", body = super::ErrorBody),
        (status = 404, description = "Not found", body = super::ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "auth"
)]
/// `DELETE /api/keys/:id` — revokes a key by its record UUID.
///
/// Users can only revoke their own keys; admin can revoke any key.
pub(super) async fn revoke_key_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    AxumPath(key_id): AxumPath<String>,
) -> Result<StatusCode, ApiError> {
    let auth = state.storage.auth();

    // Non-admin users may only revoke their own keys. Verify ownership by
    // checking that the key is in their key list before revoking.
    if !identity.is_admin {
        let user_id = match identity.user_id {
            Some(uid) => uid,
            None => {
                return Err(ApiError::forbidden(
                    "this key is not associated with a user; cannot revoke keys",
                ));
            }
        };
        let user_keys = auth.list_keys_by_user(&user_id).await.map_err(ApiError::from)?;
        if !user_keys.iter().any(|k| k.id == key_id) {
            return Err(ApiError::forbidden("you do not own this API key"));
        }
    }

    auth.revoke_key(&key_id).await.map_err(ApiError::from)?;
    tracing::info!(
        event = "key.revoked",
        actor = %identity.name,
        key_id = %key_id,
        "API key revoked"
    );
    Ok(StatusCode::NO_CONTENT)
}

/// Query parameters for `DELETE /api/keys` (bulk revocation).
///
/// Exactly one of `repo_id` or `created_by` must be supplied.
#[derive(Debug, Deserialize)]
pub(super) struct BulkRevokeQuery {
    /// Revoke all keys scoped to this repository UUID.
    repo_id: Option<uuid::Uuid>,
    /// Revoke all keys owned by this user UUID.
    created_by: Option<uuid::Uuid>,
}

/// Response body for `DELETE /api/keys` (bulk revocation).
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct BulkRevokeResponse {
    /// Number of API keys that were revoked.
    revoked: u64,
}

#[utoipa::path(
    delete,
    path = "/api/keys",
    params(
        ("repo_id" = Option<String>, Query, description = "Revoke all keys scoped to this repository UUID"),
        ("created_by" = Option<String>, Query, description = "Revoke all keys owned by this user UUID"),
    ),
    responses(
        (status = 200, description = "Bulk revocation successful", body = BulkRevokeResponse),
        (status = 400, description = "Neither or both query params provided", body = super::ErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden — admin role required", body = super::ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "auth"
)]
/// `DELETE /api/keys` — revokes all keys for a repo or all keys created by a user.
///
/// Requires admin role. Exactly one of `repo_id` or `created_by` must be provided.
/// Returns the count of keys that were revoked.
pub(super) async fn bulk_revoke_keys_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    AxumQuery(params): AxumQuery<BulkRevokeQuery>,
) -> Result<Json<BulkRevokeResponse>, ApiError> {
    if !identity.is_admin {
        return Err(ApiError::forbidden("admin role required for bulk key revocation"));
    }

    let auth = state.storage.auth();
    let revoked = match (params.repo_id, params.created_by) {
        (Some(repo_id), None) => {
            let count = auth
                .revoke_keys_by_repo(&repo_id)
                .await
                .map_err(ApiError::from)?;
            tracing::info!(
                event = "keys.bulk_revoked",
                actor = %identity.name,
                repo_id = %repo_id,
                count = count,
                "bulk revoked keys for repo"
            );
            count
        }
        (None, Some(user_id)) => {
            let count = auth
                .revoke_keys_by_user(&user_id)
                .await
                .map_err(ApiError::from)?;
            tracing::info!(
                event = "keys.bulk_revoked",
                actor = %identity.name,
                user_id = %user_id,
                count = count,
                "bulk revoked keys for user"
            );
            count
        }
        (Some(_), Some(_)) => {
            return Err(ApiError::bad_request(
                "provide either repo_id or created_by, not both",
            ));
        }
        (None, None) => {
            return Err(ApiError::bad_request(
                "one of repo_id or created_by is required",
            ));
        }
    };

    Ok(Json(BulkRevokeResponse { revoked }))
}
