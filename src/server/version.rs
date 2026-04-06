//! Version API handlers — list, get, diff, and rollback versions.
//!
//! Endpoints:
//!   - `GET /api/repos/:repo/versions` — list versions with pagination
//!   - `GET /api/repos/:repo/versions/:id` — version details with entity/file changes
//!   - `GET /api/repos/:repo/versions/:id/diff` — unified diffs for files changed in a version
//!   - `POST /api/repos/:repo/versions/rollback` — rollback to a prior version

use std::collections::HashMap;

use axum::extract::{Extension, Query as AxumQuery};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::event_log::EventKind;
use crate::storage::RepoRole;
use crate::version;
use crate::workspace;

use super::pagination::{PaginatedResponse, PaginationParams};
use super::{AgentIdentity, ApiError, ErrorBody, PathId, RepoCtx};
use super::require_repo_permission;

// ── Request / response types ──────────────────────────────────────────────────

/// Query parameters for `GET /api/versions/:id/diff`.
#[derive(Debug, Default, Deserialize)]
pub(super) struct VersionDiffQuery {
    /// Version to diff against instead of the parent. Must be an ancestor of `:id`.
    pub(super) base: Option<String>,
}

/// File-level diff entry returned by `GET /api/versions/:id/diff`.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct VersionDiffFile {
    /// File path relative to the repository root.
    pub(super) path: String,
    /// How the file was changed: `"added"`, `"modified"`, or `"removed"`.
    pub(super) change_type: String,
    /// Unified diff string for this file.
    pub(super) diff: String,
}

/// Response body for `GET /api/versions/:id/diff`.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct VersionDiffResponse {
    /// The version whose changes are shown.
    pub(super) version_id: String,
    /// The version used as the diff base (the parent, or the explicit `?base`).
    pub(super) base_version_id: String,
    /// Per-file diffs.
    pub(super) files: Vec<VersionDiffFile>,
}

/// Request body for `POST /api/versions/rollback`.
#[derive(Debug, Deserialize, ToSchema)]
pub(super) struct RollbackRequest {
    /// Version identifier to roll back (e.g., `"v3"`).
    pub(super) version: String,
    /// If `true`, proceed even when downstream versions depend on the changes.
    /// If `false` (default) and downstream impacts exist, returns 409.
    #[serde(default)]
    pub(super) force: bool,
}

// ── Handlers ──────────────────────────────────────────────────────────────────

/// `GET /api/versions` — lists versions with pagination and optional sorting.
///
/// Supports pagination via `?page=1&per_page=25` and sorting via
/// `?sort=created_at:desc`.  Sortable columns: `created_at`, `version_id`.
#[utoipa::path(
    get,
    path = "/api/repos/{repo}/versions",
    params(
        ("repo" = String, Path, description = "Repository name"),
        ("page" = Option<u32>, Query, description = "Page number (1-indexed, default 1)"),
        ("per_page" = Option<u32>, Query, description = "Items per page (default 25, max 100)"),
        ("sort" = Option<String>, Query, description = "Sort fields, e.g. `created_at:desc`"),
    ),
    responses(
        (status = 200, description = "Paginated list of versions", body = PaginatedResponse<version::VersionMeta>),
        (status = 400, description = "Invalid pagination or sort params", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = [])),
    tag = "versions"
)]
pub(super) async fn list_versions_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    AxumQuery(pagination): AxumQuery<PaginationParams>,
) -> Result<Json<PaginatedResponse<version::VersionMeta>>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, RepoRole::Read).await?;
    const ALLOWED_SORT: &[&str] = &["created_at", "version_id", "created_by"];
    let query = crate::storage::ListQuery::from_params(
        pagination.page,
        pagination.per_page,
        pagination.sort.as_deref(),
        ALLOWED_SORT,
    )
    .map_err(|e| ApiError::bad_request(e.to_string()))?;
    let result = ctx
        .storage
        .versions()
        .list_versions(&ctx.repo_id, &query)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(PaginatedResponse::new(result.items, result.total, &query)))
}

#[utoipa::path(
    get,
    path = "/api/repos/{repo}/versions/{id}",
    params(
        ("repo" = String, Path, description = "Repository name"),
        ("id" = String, Path, description = "Version ID"),
    ),
    responses(
        (status = 200, description = "Version details", body = version::VersionChanges),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Version not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "versions"
)]
/// `GET /api/versions/:id` — returns details for a single version, including
/// entity-level and file-level changes derived from the event log.
///
/// Returns 404 if the version does not exist.
pub(super) async fn get_version_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    PathId(id): PathId,
) -> Result<Json<version::VersionChanges>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, RepoRole::Read).await?;
    let meta = ctx
        .storage
        .versions()
        .get_version(&ctx.repo_id, &id)
        .await
        .map_err(ApiError::from)?;

    let Some(merge_event_id) = meta.merge_event_id else {
        return Ok(Json(version::VersionChanges {
            version: meta,
            entity_changes: vec![],
            file_changes: vec![],
        }));
    };

    // Find the MergeCompleted event to get the workspace_id, then replay
    // all workspace events to reconstruct entity and file changes.
    let merge_events = ctx
        .storage
        .events()
        .query_by_type(&ctx.repo_id, "MergeCompleted")
        .await
        .map_err(ApiError::from)?;
    let workspace_id = merge_events
        .into_iter()
        .find(|e| e.id == merge_event_id)
        .and_then(|e| e.kind.workspace_id());
    let Some(workspace_id) = workspace_id else {
        return Ok(Json(version::VersionChanges {
            version: meta,
            entity_changes: vec![],
            file_changes: vec![],
        }));
    };

    let events = ctx
        .storage
        .events()
        .query_by_workspace(&ctx.repo_id, &workspace_id)
        .await
        .map_err(ApiError::from)?;

    let mut entity_changes = Vec::new();
    let mut file_changes = Vec::new();
    for event in events {
        match event.kind {
            EventKind::EntityAdded { entity, .. } => {
                entity_changes.push(version::VersionEntityChange {
                    entity_id: entity.id,
                    change_type: version::VersionChangeType::Added,
                    kind: Some(entity.kind),
                    qualified_name: Some(entity.qualified_name),
                    file_path: Some(entity.file_path),
                    change_description: None,
                });
            }
            EventKind::EntityModified {
                entity_id,
                change_description,
                ..
            } => {
                entity_changes.push(version::VersionEntityChange {
                    entity_id,
                    change_type: version::VersionChangeType::Modified,
                    kind: None,
                    qualified_name: None,
                    file_path: None,
                    change_description: Some(change_description),
                });
            }
            EventKind::EntityRemoved { entity_id, .. } => {
                entity_changes.push(version::VersionEntityChange {
                    entity_id,
                    change_type: version::VersionChangeType::Removed,
                    kind: None,
                    qualified_name: None,
                    file_path: None,
                    change_description: None,
                });
            }
            EventKind::FileAdded { path, hash, .. } => {
                file_changes.push(version::VersionFileChange {
                    path,
                    change_type: version::VersionFileChangeType::Added,
                    hash: Some(hash),
                });
            }
            EventKind::FileModified { path, new_hash, .. } => {
                file_changes.push(version::VersionFileChange {
                    path,
                    change_type: version::VersionFileChangeType::Modified,
                    hash: Some(new_hash),
                });
            }
            EventKind::FileRemoved { path, .. } => {
                file_changes.push(version::VersionFileChange {
                    path,
                    change_type: version::VersionFileChangeType::Removed,
                    hash: None,
                });
            }
            _ => {}
        }
    }

    Ok(Json(version::VersionChanges {
        version: meta,
        entity_changes,
        file_changes,
    }))
}

#[utoipa::path(
    get,
    path = "/api/repos/{repo}/versions/{id}/diff",
    params(
        ("repo" = String, Path, description = "Repository name"),
        ("id" = String, Path, description = "Version ID"),
        ("base" = Option<String>, Query, description = "Base version ID to diff against"),
    ),
    responses(
        (status = 200, description = "Version diff", body = VersionDiffResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Version not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "versions"
)]
/// `GET /api/versions/:id/diff` — returns unified diffs for all files changed
/// in this version compared to its parent (or a specific `?base=<version_id>`).
///
/// Response includes a per-file diff string in unified diff format.
/// Returns 404 if the version does not exist.
pub(super) async fn get_version_diff_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    PathId(id): PathId,
    AxumQuery(query): AxumQuery<VersionDiffQuery>,
) -> Result<Json<VersionDiffResponse>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, RepoRole::Read).await?;

    // Fetch version metadata.
    let meta = ctx
        .storage
        .versions()
        .get_version(&ctx.repo_id, &id)
        .await
        .map_err(ApiError::from)?;

    let base_version_id = query
        .base
        .or_else(|| meta.parent_version_id.clone())
        .unwrap_or_default();

    let Some(merge_event_id) = meta.merge_event_id else {
        // Initial version — no files changed.
        return Ok(Json(VersionDiffResponse {
            version_id: id,
            base_version_id,
            files: vec![],
        }));
    };

    // Find the workspace that produced this version via MergeCompleted event.
    let merge_events = ctx
        .storage
        .events()
        .query_by_type(&ctx.repo_id, "MergeCompleted")
        .await
        .map_err(ApiError::from)?;

    let workspace_id = merge_events
        .into_iter()
        .find(|e| e.id == merge_event_id)
        .and_then(|e| e.kind.workspace_id());

    let Some(workspace_id) = workspace_id else {
        return Ok(Json(VersionDiffResponse {
            version_id: id,
            base_version_id,
            files: vec![],
        }));
    };

    // Replay workspace events to collect file-level changes.
    let events = ctx
        .storage
        .events()
        .query_by_workspace(&ctx.repo_id, &workspace_id)
        .await
        .map_err(ApiError::from)?;

    // Collect file changes with both old and new hashes directly from workspace
    // events.  `FileModified` carries both `old_hash` and `new_hash`; `FileAdded`
    // carries only `new_hash`; `FileRemoved` carries no hash (resolved below).
    struct FileChangeHashes {
        path: String,
        change_type: version::VersionFileChangeType,
        new_hash: Option<String>,
        old_hash: Option<String>,
    }

    let mut file_changes: Vec<FileChangeHashes> = Vec::new();
    let mut has_removed_files = false;

    for event in events {
        match event.kind {
            EventKind::FileAdded { path, hash, .. } => {
                file_changes.push(FileChangeHashes {
                    path,
                    change_type: version::VersionFileChangeType::Added,
                    new_hash: Some(hash),
                    old_hash: None,
                });
            }
            EventKind::FileModified { path, old_hash, new_hash, .. } => {
                file_changes.push(FileChangeHashes {
                    path,
                    change_type: version::VersionFileChangeType::Modified,
                    new_hash: Some(new_hash),
                    old_hash: Some(old_hash),
                });
            }
            EventKind::FileRemoved { path, .. } => {
                has_removed_files = true;
                file_changes.push(FileChangeHashes {
                    path,
                    change_type: version::VersionFileChangeType::Removed,
                    new_hash: None,
                    old_hash: None,
                });
            }
            _ => {}
        }
    }

    // For removed files there is no hash in the event.  Scan the full event log
    // (excluding the current workspace's events) to reconstruct the last known
    // hash for each path, i.e. the content that existed in the parent version.
    if has_removed_files {
        let all_events = ctx
            .storage
            .events()
            .query_since_id(&ctx.repo_id, 0)
            .await
            .map_err(ApiError::from)?;

        let mut path_to_hash: HashMap<String, String> = HashMap::new();
        for e in &all_events {
            if e.kind.workspace_id() == Some(workspace_id) {
                // Skip events from the current workspace — we want the state
                // *before* this version's changes were applied.
                continue;
            }
            match &e.kind {
                EventKind::FileAdded { path, hash, .. } => {
                    path_to_hash.insert(path.clone(), hash.clone());
                }
                EventKind::FileModified { path, new_hash, .. } => {
                    path_to_hash.insert(path.clone(), new_hash.clone());
                }
                EventKind::FileRemoved { path, .. } => {
                    path_to_hash.remove(path);
                }
                _ => {}
            }
        }

        for fc in &mut file_changes {
            if fc.change_type == version::VersionFileChangeType::Removed && fc.old_hash.is_none() {
                fc.old_hash = path_to_hash.get(&fc.path).cloned();
            }
        }
    }

    // Fallback paths for pre-blob-storage versions (local/SQLite mode or versions
    // created before content-addressable storage was introduced).
    let snapshot_dir = ctx.vai_dir.join("versions").join(&id).join("snapshot");
    // ALLOW_FS: fallback for pre-blob local/SQLite mode versions
    let overlay_dir = workspace::overlay_dir(&ctx.vai_dir, &workspace_id.to_string());
    let file_store = ctx.storage.files();

    let mut diff_files = Vec::new();
    for fc in file_changes {
        // Fetch old content.
        // Primary: content-addressable lookup by hash (works for all versions).
        // Fallback: snapshot directory written at merge time (pre-blob versions).
        let old_text = match fc.old_hash.as_deref() {
            Some(hash) => file_store
                .get(&ctx.repo_id, &format!("blobs/{hash}"))
                .await
                .ok()
                .and_then(|b| String::from_utf8(b).ok()),
            None => None,
        };
        let old_text = if old_text.is_none() {
            file_store
                .get(&ctx.repo_id, &format!("versions/{}/snapshot/{}", id, fc.path))
                .await
                .ok()
                .and_then(|b| String::from_utf8(b).ok())
                .or_else(|| {
                    let p = snapshot_dir.join(&fc.path);
                    // ALLOW_FS: fallback for pre-blob local/SQLite mode versions
                    if p.exists() { std::fs::read_to_string(&p).ok() } else { None }
                })
        } else {
            old_text
        };

        // Fetch new content.
        // Primary: content-addressable lookup by hash.
        // Fallback: workspace overlay path or local filesystem overlay.
        let new_text = match fc.new_hash.as_deref() {
            Some(hash) => file_store
                .get(&ctx.repo_id, &format!("blobs/{hash}"))
                .await
                .ok()
                .and_then(|b| String::from_utf8(b).ok()),
            None => None,
        };
        let new_text = if new_text.is_none() && fc.new_hash.is_some() {
            file_store
                .get(&ctx.repo_id, &format!("workspaces/{}/{}", workspace_id, fc.path))
                .await
                .ok()
                .and_then(|b| String::from_utf8(b).ok())
                .or_else(|| {
                    let p = overlay_dir.join(&fc.path);
                    // ALLOW_FS: fallback for pre-blob local/SQLite mode versions
                    if p.exists() { std::fs::read_to_string(&p).ok() } else { None }
                })
        } else {
            new_text
        };

        let change_type = match fc.change_type {
            version::VersionFileChangeType::Added => "added",
            version::VersionFileChangeType::Modified => "modified",
            version::VersionFileChangeType::Removed => "removed",
        };

        let diff = match (&old_text, &new_text) {
            (None, Some(new)) => {
                // Added: show entire file as additions.
                let patch = diffy::create_patch("", new);
                format!("{patch}")
            }
            (Some(old), None) => {
                // Removed: show entire old file as deletions.
                let patch = diffy::create_patch(old, "");
                format!("{patch}")
            }
            (Some(old), Some(new)) => {
                let patch = diffy::create_patch(old, new);
                format!("{patch}")
            }
            (None, None) => String::new(),
        };

        diff_files.push(VersionDiffFile {
            path: fc.path,
            change_type: change_type.to_string(),
            diff,
        });
    }

    Ok(Json(VersionDiffResponse {
        version_id: id,
        base_version_id,
        files: diff_files,
    }))
}

#[utoipa::path(
    post,
    path = "/api/repos/{repo}/versions/rollback",
    request_body = RollbackRequest,
    params(
        ("repo" = String, Path, description = "Repository name"),
    ),
    responses(
        (status = 200, description = "Rollback successful"),
        (status = 400, description = "Bad request", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Version not found", body = ErrorBody),
        (status = 409, description = "Downstream versions conflict", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "versions"
)]
/// `POST /api/versions/rollback` — rolls back the changes introduced by a
/// specific version by creating a new append-only version that restores the
/// prior state.
///
/// If `force` is `false` (the default) and downstream versions depend on the
/// target version's changes, returns **409 Conflict** with a JSON body
/// containing both an error message and the full `ImpactAnalysis`.
///
/// If `force` is `true`, the rollback proceeds regardless of downstream impact.
pub(super) async fn rollback_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    Json(body): Json<RollbackRequest>,
) -> Response {
    if let Err(e) = require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, RepoRole::Write).await {
        return e.into_response();
    }
    // Compute impact analysis before attempting the rollback.
    let impact = match version::analyze_rollback_impact(&ctx.vai_dir, &body.version) {
        Ok(i) => i,
        Err(e) => return ApiError::from(e).into_response(),
    };

    if !body.force && !impact.downstream_impacts.is_empty() {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "downstream versions depend on these changes; use \"force\": true to override",
                "impact": impact,
            })),
        )
            .into_response();
    }

    match version::rollback(&ctx.vai_dir, &ctx.repo_root, &body.version, None) {
        Ok(result) => Json(result).into_response(),
        Err(e) => ApiError::from(e).into_response(),
    }
}
