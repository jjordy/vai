//! Workspace API handlers — create, list, get, submit, discard, and file operations.
//!
//! Endpoints:
//!   - `POST /api/repos/:repo/workspaces` — create workspace at current HEAD
//!   - `GET /api/repos/:repo/workspaces` — list workspaces (paginated)
//!   - `GET /api/repos/:repo/workspaces/:id` — workspace details
//!   - `POST /api/repos/:repo/workspaces/:id/submit` — submit workspace for merge
//!   - `DELETE /api/repos/:repo/workspaces/:id` — discard workspace
//!   - `POST /api/repos/:repo/workspaces/:id/files` — upload files into overlay
//!   - `POST /api/repos/:repo/workspaces/:id/upload-snapshot` — upload gzip tarball
//!   - `GET /api/repos/:repo/workspaces/:id/files/*path` — download file from workspace

use std::collections::HashMap;
use std::sync::Arc;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};

use axum::extract::{Extension, Path as AxumPath, Query as AxumQuery, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::event_log::EventKind;
use crate::storage::{ListQuery, RepoRole};
use crate::{merge, workspace};

use super::pagination::{PaginatedResponse, PaginationParams};
use super::{
    AgentIdentity, ApiError, AppState, BroadcastEvent, ErrorBody, PathId, RepoCtx,
    require_repo_permission, sanitize_path, sha256_hex, validate_str_len,
    MAX_FILE_SIZE_BYTES, MAX_FILES_PER_REQUEST, MAX_INTENT_LEN, MAX_PATH_LEN,
};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Maximum uncompressed tarball payload accepted by the snapshot upload endpoint.
const MAX_SNAPSHOT_SIZE_BYTES: usize = 100 * 1024 * 1024; // 100 MiB

// ── Request / response types ──────────────────────────────────────────────────

/// Request body for `POST /api/workspaces`.
#[derive(Debug, Deserialize, ToSchema)]
pub(super) struct CreateWorkspaceRequest {
    /// Stated agent intent for this workspace.
    pub(super) intent: String,
}

/// Response body for workspace creation and detail endpoints.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct WorkspaceResponse {
    pub(super) id: String,
    pub(super) intent: String,
    pub(super) status: String,
    pub(super) base_version: String,
    pub(super) created_at: String,
    pub(super) updated_at: String,
}

impl From<workspace::WorkspaceMeta> for WorkspaceResponse {
    fn from(m: workspace::WorkspaceMeta) -> Self {
        WorkspaceResponse {
            id: m.id.to_string(),
            intent: m.intent,
            status: m.status.as_str().to_string(),
            base_version: m.base_version,
            created_at: m.created_at.to_rfc3339(),
            updated_at: m.updated_at.to_rfc3339(),
        }
    }
}

/// Response body for `POST /api/workspaces/:id/submit`.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct SubmitResponse {
    pub(super) version: String,
    pub(super) files_applied: usize,
    pub(super) entities_changed: usize,
    pub(super) auto_resolved: u32,
}

impl From<merge::SubmitResult> for SubmitResponse {
    fn from(r: merge::SubmitResult) -> Self {
        SubmitResponse {
            version: r.version.version_id.clone(),
            files_applied: r.files_applied,
            entities_changed: r.entities_changed,
            auto_resolved: r.auto_resolved,
        }
    }
}

// ── File upload / download types ──────────────────────────────────────────────

/// A single file entry within an upload request.
#[derive(Debug, Deserialize, ToSchema)]
pub(super) struct FileUploadEntry {
    /// Path relative to the repository root (e.g. `src/auth.rs`).
    pub(super) path: String,
    /// File content encoded as standard (padded) base64.
    pub(super) content_base64: String,
}

/// Request body for `POST /api/workspaces/:id/files`.
#[derive(Debug, Deserialize, ToSchema)]
pub(super) struct UploadFilesRequest {
    /// One or more files to upload into the workspace overlay.
    pub(super) files: Vec<FileUploadEntry>,
    /// Paths (relative to repo root) that the agent deleted during this session.
    ///
    /// These are accumulated into the workspace row's `deleted_paths` column
    /// via the storage trait. The submit handler removes them from `current/`
    /// and emits `FileRemoved` events; the download handler excludes them from
    /// tarballs built from merged workspace overlays.
    #[serde(default)]
    pub(super) deleted_paths: Vec<String>,
}

/// Response body for a successful file upload.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct UploadFilesResponse {
    /// Number of files successfully written to storage.
    pub(super) uploaded: usize,
    /// Number of files skipped because they were already present in storage
    /// with the same content hash (resumability — Postgres mode only).
    #[serde(default)]
    pub(super) skipped: usize,
    /// Repository-relative paths of all written files.
    pub(super) paths: Vec<String>,
}

/// Response body for `POST /api/workspaces/:id/upload-snapshot`.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct UploadSnapshotResponse {
    /// Files in the tarball that were not present in `current/`.
    pub(super) added: usize,
    /// Files with different content from `current/`.
    pub(super) modified: usize,
    /// Files present in `current/` but absent from the tarball (full mode) or
    /// listed in `.vai-delta.json` (delta mode).
    pub(super) deleted: usize,
    /// Files identical in both tarball and `current/`.
    pub(super) unchanged: usize,
    /// `true` when the upload was processed as a delta (`.vai-delta.json` was present).
    pub(super) is_delta: bool,
}

/// Manifest embedded inside a delta tarball as `.vai-delta.json`.
///
/// When this file is present in the uploaded archive the server switches to
/// delta mode: only the files actually present in the tarball are compared
/// against `current/`, and `deleted_paths` is taken verbatim from this struct
/// rather than derived from absent files.
#[derive(Debug, Deserialize, ToSchema)]
pub(super) struct DeltaManifest {
    /// The version identifier the delta was built on top of (informational).
    #[allow(dead_code)]
    pub(super) base_version: String,
    /// Repository-relative paths that were deleted relative to `base_version`.
    #[serde(default)]
    pub(super) deleted_paths: Vec<String>,
}

/// Query parameters for `POST /api/workspaces/:id/upload-snapshot`.
#[derive(Debug, Default, Deserialize, ToSchema)]
pub(super) struct UploadSnapshotQuery {
    /// When `true`, allow uploads that would delete more than 50% of the
    /// current repository files.  Defaults to `false`.  Intended for
    /// intentional mass-delete operations (e.g., repo restructuring) where
    /// the caller has explicitly confirmed the destructive intent.
    #[serde(default)]
    pub(super) allow_destructive: bool,
}

/// Response body for file download endpoints.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct FileDownloadResponse {
    /// Path relative to the repository root.
    pub(super) path: String,
    /// File content encoded as standard (padded) base64.
    pub(super) content_base64: String,
    /// File size in bytes.
    pub(super) size: usize,
    /// Where the file was sourced: `"overlay"` or `"base"`.
    pub(super) found_in: String,
}

// ── Workspace handlers ────────────────────────────────────────────────────────

/// `POST /api/workspaces` — creates a new workspace at the current HEAD.
///
/// Returns 201 Created with the workspace metadata.
/// Broadcasts a `WorkspaceCreated` event to WebSocket subscribers.
#[utoipa::path(
    post,
    path = "/api/repos/{repo}/workspaces",
    request_body = CreateWorkspaceRequest,
    params(
        ("repo" = String, Path, description = "Repository name"),
    ),
    responses(
        (status = 201, description = "Workspace created", body = WorkspaceResponse),
        (status = 400, description = "Bad request", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = [])),
    tag = "workspaces"
)]
pub(super) async fn create_workspace_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    ctx: RepoCtx,
    Json(body): Json<CreateWorkspaceRequest>,
) -> Result<(StatusCode, Json<WorkspaceResponse>), ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, RepoRole::Write).await?;
    validate_str_len(&body.intent, MAX_INTENT_LEN, "intent")?;
    let _lock = state.repo_lock.lock().await;
    let head = ctx.storage.versions()
        .read_head(&ctx.repo_id)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| "unknown".to_string());
    let ws = ctx.storage.workspaces()
        .create_workspace(&ctx.repo_id, crate::storage::NewWorkspace {
            id: None,
            intent: body.intent.clone(),
            base_version: head,
            issue_id: None,
        })
        .await
        .map_err(ApiError::from)?;

    // Append event to event store — triggers pg_notify in Postgres mode.
    let _ = ctx.storage.events()
        .append(&ctx.repo_id, EventKind::WorkspaceCreated {
            workspace_id: ws.id,
            intent: ws.intent.clone(),
            base_version: ws.base_version.clone(),
        })
        .await;

    // Broadcast the workspace creation event to all WebSocket subscribers.
    let ws_id = ws.id.to_string();
    state.broadcast(BroadcastEvent {
        event_type: "WorkspaceCreated".to_string(),
        event_id: 0,
        workspace_id: Some(ws_id.clone()),
        timestamp: ws.created_at.to_rfc3339(),
        data: serde_json::json!({
            "workspace_id": ws_id,
            "intent": ws.intent,
            "base_version": ws.base_version,
        }),
    });

    tracing::info!(
        event = "workspace.created",
        actor = %identity.name,
        repo = %ctx.repo_id,
        workspace_id = %ws.id,
        intent = %ws.intent,
        "workspace created"
    );
    Ok((StatusCode::CREATED, Json(WorkspaceResponse::from(ws))))
}

/// `GET /api/workspaces` — lists all active (non-discarded, non-merged) workspaces.
///
/// Supports pagination via `?page=1&per_page=25` and sorting via
/// `?sort=created_at:desc`.  Sortable columns: `created_at`, `updated_at`,
/// `status`, `intent`.
#[utoipa::path(
    get,
    path = "/api/repos/{repo}/workspaces",
    params(
        ("repo" = String, Path, description = "Repository name"),
        ("page" = Option<u32>, Query, description = "Page number (1-indexed, default 1)"),
        ("per_page" = Option<u32>, Query, description = "Items per page (default 25, max 100)"),
        ("sort" = Option<String>, Query, description = "Sort fields, e.g. `created_at:desc,status:asc`"),
    ),
    responses(
        (status = 200, description = "Paginated list of workspaces", body = PaginatedResponse<WorkspaceResponse>),
        (status = 400, description = "Invalid pagination or sort params", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = [])),
    tag = "workspaces"
)]
pub(super) async fn list_workspaces_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    AxumQuery(pagination): AxumQuery<PaginationParams>,
) -> Result<Json<PaginatedResponse<WorkspaceResponse>>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, RepoRole::Read).await?;
    const ALLOWED_SORT: &[&str] = &["created_at", "updated_at", "status", "intent", "id"];
    let query = ListQuery::from_params(
        pagination.page,
        pagination.per_page,
        pagination.sort.as_deref(),
        ALLOWED_SORT,
    )
    .map_err(|e| ApiError::bad_request(e.to_string()))?;
    let result = ctx.storage.workspaces()
        .list_workspaces(&ctx.repo_id, false, &query)
        .await
        .map_err(ApiError::from)?;
    let items: Vec<WorkspaceResponse> = result.items.into_iter().map(Into::into).collect();
    Ok(Json(PaginatedResponse::new(items, result.total, &query)))
}

/// `GET /api/workspaces/:id` — returns details for a single workspace.
///
/// Returns 404 if the workspace does not exist.
#[utoipa::path(
    get,
    path = "/api/repos/{repo}/workspaces/{id}",
    params(
        ("repo" = String, Path, description = "Repository name"),
        ("id" = String, Path, description = "Workspace ID"),
    ),
    responses(
        (status = 200, description = "Workspace details", body = WorkspaceResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Workspace not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "workspaces"
)]
pub(super) async fn get_workspace_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    PathId(id): PathId,
) -> Result<Json<WorkspaceResponse>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, RepoRole::Read).await?;
    let ws_id = uuid::Uuid::parse_str(&id)
        .map_err(|_| ApiError::not_found(format!("workspace `{id}` not found")))?;
    let meta = ctx.storage.workspaces()
        .get_workspace(&ctx.repo_id, &ws_id)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(WorkspaceResponse::from(meta)))
}

/// `POST /api/workspaces/:id/submit` — submits a workspace for merge.
///
/// Switches the active workspace to `id`, then runs the merge engine.
/// Returns 409 Conflict if the merge cannot be auto-resolved; in that case
/// an escalation is also created automatically.
/// Returns 404 if the workspace does not exist.
/// Broadcasts a `WorkspaceSubmitted` event to WebSocket subscribers.
#[utoipa::path(
    post,
    path = "/api/repos/{repo}/workspaces/{id}/submit",
    params(
        ("repo" = String, Path, description = "Repository name"),
        ("id" = String, Path, description = "Workspace ID"),
    ),
    responses(
        (status = 200, description = "Workspace submitted", body = SubmitResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Workspace not found", body = ErrorBody),
        (status = 409, description = "Merge conflict", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "workspaces"
)]
pub(super) async fn submit_workspace_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    ctx: RepoCtx,
    PathId(id): PathId,
) -> Result<Json<SubmitResponse>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, RepoRole::Write).await?;
    let _lock = state.repo_lock.lock().await;

    // Read workspace metadata from storage (works in both local SQLite and Postgres modes).
    let ws_uuid = uuid::Uuid::parse_str(&id)
        .map_err(|_| ApiError::not_found(format!("workspace `{id}` not found")))?;
    let meta = ctx.storage.workspaces()
        .get_workspace(&ctx.repo_id, &ws_uuid)
        .await
        .map_err(ApiError::from)?;
    let workspace_uuid = meta.id;
    let workspace_intent = meta.intent.clone();

    // Safety net: refuse to submit against a closed issue.  The primary guard
    // is the cascade in close_issue_handler, but there is a race window between
    // issue-close and submit arrival — catch it here and tear down the workspace
    // so nothing leaks.
    if let Some(issue_id) = meta.issue_id {
        if let Ok(issue) = ctx.storage.issues().get_issue(&ctx.repo_id, &issue_id).await {
            if issue.status == crate::issue::IssueStatus::Closed {
                super::cascade_workspace_teardown(
                    &state,
                    &ctx.repo_id,
                    workspace_uuid,
                    "submit rejected: linked issue is closed",
                )
                .await;
                return Err(ApiError::conflict(format!(
                    "linked issue {issue_id} is closed"
                )));
            }
        }
    }

    // Choose merge strategy based on storage backend.
    // ServerWithMemFs uses the same S3MergeFs path as ServerWithS3 (for testing).
    let using_s3_merge = matches!(
        &ctx.storage,
        crate::storage::StorageBackend::ServerWithS3(_, _)
            | crate::storage::StorageBackend::ServerWithMemFs(_, _)
    );

    let submit_result = if using_s3_merge {
        // S3 mode: read HEAD from storage, set up a minimal temporary .vai/
        // directory for the merge engine's metadata operations, and use
        // S3MergeFs for all file I/O.  No writes touch the real repo root.
        let current_head = ctx
            .storage
            .versions()
            .read_head(&ctx.repo_id)
            .await
            .map_err(|e| ApiError::internal(format!("read HEAD from storage: {e}")))?
            .unwrap_or_else(|| meta.base_version.clone());

        let tmp = setup_tmpdir_for_s3_submit(&meta, &current_head)?;
        let tmp_vai = tmp.path().join(".vai");

        let s3_fs = crate::merge_fs::S3MergeFs::new(
            ctx.storage.files(),
            ctx.repo_id,
            format!("workspaces/{id}/"),
            "current/".to_string(),
        );
        let result = merge::submit_with_fs(
            &s3_fs,
            &tmp_vai,
            &meta,
            meta.deleted_paths.clone(),
        );
        if result.is_ok() {
            s3_fs
                .flush()
                .await
                .map_err(|e| ApiError::internal(format!("S3MergeFs flush: {e}")))?;
        }
        // tmp is dropped here; the tmpdir is cleaned up automatically.
        result
    } else {
        // Non-S3 mode (local SQLite): switch to the workspace so merge::submit
        // can locate the active overlay on disk, then run the disk-based merge.
        workspace::switch(&ctx.vai_dir, &id).map_err(ApiError::from)?;
        merge::submit(&ctx.vai_dir, &ctx.repo_root)
    };

    match submit_result {
        Ok(result) => {
            // Remove from conflict engine — workspace is no longer active.
            state.conflict_engine.lock().await.remove_workspace(&workspace_uuid);

            // Append a MergeCompleted event to the storage trait so Postgres
            // has a real event record with the correct sequential ID.  In local
            // SQLite mode this duplicates the event the merge engine already
            // wrote to the event-log file, which is harmless.  We use the
            // returned event ID (the Postgres row ID) as merge_event_id so the
            // version-detail handler can look it up via query_by_type.
            let storage_merge_event = ctx
                .storage
                .events()
                .append(
                    &ctx.repo_id,
                    EventKind::MergeCompleted {
                        workspace_id: workspace_uuid,
                        new_version_id: result.version.version_id.clone(),
                        auto_resolved_conflicts: result.auto_resolved,
                    },
                )
                .await;
            let merge_event_id = storage_merge_event
                .ok()
                .map(|e| e.id)
                .or(result.version.merge_event_id);

            // Write FileRemoved events for deleted paths so the version-detail
            // handler can include them when reconstructing file_changes.  Upload
            // handlers already write FileAdded/FileModified to storage; deletions
            // are only tracked in the workspace metadata column.
            for path in &meta.deleted_paths {
                let _ = ctx
                    .storage
                    .events()
                    .append(
                        &ctx.repo_id,
                        EventKind::FileRemoved {
                            workspace_id: workspace_uuid,
                            path: path.clone(),
                        },
                    )
                    .await;
            }

            // Sync the new version and HEAD to the storage trait.
            // In Postgres server mode these writes go to the database; in local
            // SQLite mode they duplicate what merge::submit already wrote to disk,
            // which is harmless (same files, same data).
            let _ = ctx.storage.versions()
                .create_version(&ctx.repo_id, crate::storage::NewVersion {
                    version_id: result.version.version_id.clone(),
                    parent_version_id: result.version.parent_version_id.clone(),
                    intent: result.version.intent.clone(),
                    created_by: result.version.created_by.clone(),
                    merge_event_id,
                })
                .await;
            let _ = ctx.storage.versions()
                .advance_head(&ctx.repo_id, &result.version.version_id)
                .await;
            // Mark workspace as Merged in storage trait.
            let _ = ctx.storage.workspaces()
                .update_workspace(
                    &ctx.repo_id,
                    &workspace_uuid,
                    crate::storage::WorkspaceUpdate {
                        status: Some(crate::workspace::WorkspaceStatus::Merged),
                        ..Default::default()
                    },
                )
                .await;

            // Persist pre-change snapshot and update "current/" in S3.
            //
            // In S3MergeFs mode both are already handled by flush() above:
            // - save_pre_change_snapshot wrote snapshot files to pending_writes
            //   which were flushed to `versions/{ver}/snapshot/` in S3.
            // - apply_overlay wrote merged base files to pending_writes which
            //   were flushed to `current/` in S3.
            //
            // In disk mode we read from the local vai_dir tree and repo_root.
            if !using_s3_merge {
                // Persist pre-change snapshot to FileStore so diffs survive container
                // restarts and cross-server migrations.
                let snap_dir = ctx.vai_dir
                    .join("versions")
                    .join(&result.version.version_id)
                    .join("snapshot");
                let file_store = ctx.storage.files();
                for (rel, bytes) in collect_dir_files_with_content(&snap_dir) {
                    let key = format!("versions/{}/snapshot/{rel}", result.version.version_id);
                    let _ = file_store.put(&ctx.repo_id, &key, &bytes).await;
                }

                // Update "current/" prefix in S3 with the full repo state.
                // The download handler and diff engine use this as the base.
                // Read from repo_root (post-merge disk state) so that semantic merges
                // write the combined result, not just the workspace's raw overlay.
                // ALLOW_FS: local SQLite mode only — guarded by `if !using_s3_merge`
                let overlay = workspace::overlay_dir(&ctx.vai_dir, &id);
                if overlay.exists() {
                    for (rel, _) in collect_dir_files_with_content(&overlay) {
                        // Read merged content from repo_root rather than overlay.
                        // For fast-forward merges this is identical to the overlay;
                        // for semantic merges it contains the auto-resolved result.
                        let merged_path = ctx.repo_root.join(&rel);
                        // ALLOW_FS: local SQLite mode only — guarded by `if !using_s3_merge`
                        if let Ok(bytes) = std::fs::read(&merged_path) {
                            let key = format!("current/{rel}");
                            let _ = file_store.put(&ctx.repo_id, &key, &bytes).await;
                        }
                    }
                }

                // Remove deleted files from the "current/" prefix using the
                // workspace's `deleted_paths` column (set by upload handlers).
                for path in &meta.deleted_paths {
                    let _ = file_store
                        .delete(&ctx.repo_id, &format!("current/{path}"))
                        .await;
                }
            }

            // Append event to event store — triggers pg_notify in Postgres mode.
            let _ = ctx.storage.events()
                .append(&ctx.repo_id, EventKind::WorkspaceSubmitted {
                    workspace_id: workspace_uuid,
                    changes_summary: format!(
                        "{} files applied, {} entities changed, new version {}",
                        result.files_applied, result.entities_changed, result.version.version_id
                    ),
                })
                .await;

            // Broadcast the submit/merge event.
            state.broadcast(BroadcastEvent {
                event_type: "WorkspaceSubmitted".to_string(),
                event_id: 0,
                workspace_id: Some(id.clone()),
                timestamp: chrono::Utc::now().to_rfc3339(),
                data: serde_json::json!({
                    "workspace_id": id,
                    "new_version": result.version.version_id,
                    "files_applied": result.files_applied,
                    "entities_changed": result.entities_changed,
                }),
            });

            // Auto-refresh the semantic graph in server (S3) mode so entities
            // reflect the newly merged state without requiring a manual call
            // to POST /api/graph/refresh.
            if using_s3_merge {
                let _ = super::graph::refresh_graph_from_files(
                    ctx.storage.graph(),
                    ctx.storage.files(),
                    ctx.repo_id,
                )
                .await;
            }

            tracing::info!(
                event = "workspace.submitted",
                actor = %identity.name,
                repo = %ctx.repo_id,
                workspace_id = %workspace_uuid,
                "workspace submitted successfully"
            );
            Ok(Json(SubmitResponse::from(result)))
        }
        Err(merge::MergeError::SemanticConflicts { count, ref conflicts }) => {
            // Auto-create an escalation so humans can review.
            let affected_entities: Vec<String> = conflicts
                .iter()
                .flat_map(|c| c.entity_ids.iter().cloned())
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect();

            // Build a more specific summary listing the affected files.
            let unique_files: Vec<String> = {
                let mut files: Vec<String> = conflicts
                    .iter()
                    .map(|c| c.file_path.clone())
                    .collect::<std::collections::HashSet<_>>()
                    .into_iter()
                    .collect();
                files.sort();
                files
            };
            let files_summary = if unique_files.len() == 1 {
                format!("in {}", unique_files[0])
            } else {
                format!("in {} files", unique_files.len())
            };
            let summary = format!(
                "{count} unresolvable conflict(s) {files_summary} — \
                 workspace \"{workspace_intent}\" requires manual resolution"
            );

            // Convert ConflictRecord → EscalationConflict for rich detail.
            let esc_conflicts: Vec<crate::escalation::EscalationConflict> = conflicts
                .iter()
                .map(|c| crate::escalation::EscalationConflict {
                    file: c.file_path.clone(),
                    merge_level: c.merge_level,
                    entity_ids: c.entity_ids.clone(),
                    description: c.description.clone(),
                    // Content is not captured at merge time; callers can fetch
                    // files from the file store using the conflict's file path.
                    ours_content: None,
                    theirs_content: None,
                    base_content: None,
                })
                .collect();

            {
                use crate::escalation::{EscalationSeverity, EscalationType};
                use crate::storage::NewEscalation;
                let resolution_options = crate::escalation::default_resolution_options(
                    &EscalationType::MergeConflict,
                    &[workspace_uuid],
                );
                let new_esc = NewEscalation {
                    escalation_type: EscalationType::MergeConflict,
                    severity: EscalationSeverity::High,
                    summary: summary.clone(),
                    intents: vec![workspace_intent.clone()],
                    agents: vec![],
                    workspace_ids: vec![workspace_uuid],
                    affected_entities,
                    conflicts: esc_conflicts,
                    resolution_options,
                };
                if let Ok(escalation) = ctx.storage.escalations()
                    .create_escalation(&ctx.repo_id, new_esc)
                    .await
                {
                    // Append escalation event to event store.
                    let _ = ctx.storage.events()
                        .append(&ctx.repo_id, EventKind::EscalationCreated {
                            escalation_id: escalation.id,
                            escalation_type: "MergeConflict".to_string(),
                            severity: "High".to_string(),
                            workspace_ids: vec![workspace_uuid.to_string()],
                            summary: summary.clone(),
                        })
                        .await;

                    // Broadcast the escalation creation.
                    state.broadcast(BroadcastEvent {
                        event_type: "EscalationCreated".to_string(),
                        event_id: 0,
                        workspace_id: Some(id.clone()),
                        timestamp: chrono::Utc::now().to_rfc3339(),
                        data: serde_json::json!({
                            "escalation_id": escalation.id,
                            "workspace_id": id,
                            "summary": summary,
                        }),
                    });
                }
            }

            // Return 409 Conflict (same as before).
            Err(ApiError::conflict(format!(
                "Semantic merge detected {count} conflict(s) requiring manual resolution; \
                 an escalation has been created — run `vai escalations list` to view it"
            )))
        }
        Err(e) => Err(ApiError::from(e)),
    }
}

/// `DELETE /api/workspaces/:id` — discards a workspace.
///
/// Returns 404 if the workspace does not exist.
/// Returns 204 No Content on success.
/// Broadcasts a `WorkspaceDiscarded` event to WebSocket subscribers.
#[utoipa::path(
    delete,
    path = "/api/repos/{repo}/workspaces/{id}",
    params(
        ("repo" = String, Path, description = "Repository name"),
        ("id" = String, Path, description = "Workspace ID"),
    ),
    responses(
        (status = 204, description = "Workspace discarded"),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Workspace not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "workspaces"
)]
pub(super) async fn discard_workspace_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    ctx: RepoCtx,
    PathId(id): PathId,
) -> Result<StatusCode, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, RepoRole::Write).await?;
    let _lock = state.repo_lock.lock().await;
    // Resolve UUID from path parameter.
    let ws_uuid = uuid::Uuid::parse_str(&id)
        .map_err(|_| ApiError::not_found(format!("workspace `{id}` not found")))?;
    // Look up workspace via storage (works in both local and Postgres mode).
    let meta = ctx.storage.workspaces()
        .get_workspace(&ctx.repo_id, &ws_uuid)
        .await
        .map_err(ApiError::from)?;
    let issue_id = meta.issue_id;
    // Discard via storage trait — avoids filesystem-only lookup that fails in Postgres mode.
    ctx.storage.workspaces()
        .discard_workspace(&ctx.repo_id, &ws_uuid)
        .await
        .map_err(ApiError::from)?;

    // Remove from conflict engine — workspace is no longer active.
    state.conflict_engine.lock().await.remove_workspace(&ws_uuid);

    // If workspace was linked to an issue, transition it back to Open — but only
    // if the issue is currently InProgress.  If the operator already closed or
    // resolved the issue, do not overwrite their intent.
    if let Some(iid) = issue_id {
        if let Ok(current) = ctx.storage.issues().get_issue(&ctx.repo_id, &iid).await {
            if current.status == crate::issue::IssueStatus::InProgress {
                let _ = ctx.storage.issues()
                    .update_issue(
                        &ctx.repo_id,
                        &iid,
                        crate::storage::IssueUpdate {
                            status: Some(crate::issue::IssueStatus::Open),
                            ..Default::default()
                        },
                    )
                    .await;
            }
        }
    }

    // Append event to event store — triggers pg_notify in Postgres mode.
    let _ = ctx.storage.events()
        .append(&ctx.repo_id, EventKind::WorkspaceDiscarded {
            workspace_id: ws_uuid,
            reason: "discarded via API".to_string(),
        })
        .await;

    // Broadcast discard event.
    state.broadcast(BroadcastEvent {
        event_type: "WorkspaceDiscarded".to_string(),
        event_id: 0,
        workspace_id: Some(id.clone()),
        timestamp: chrono::Utc::now().to_rfc3339(),
        data: serde_json::json!({ "workspace_id": id }),
    });

    tracing::info!(
        event = "workspace.discarded",
        actor = %identity.name,
        repo = %ctx.repo_id,
        workspace_id = %ws_uuid,
        "workspace discarded"
    );
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/api/repos/{repo}/workspaces/{id}/files",
    params(
        ("repo" = String, Path, description = "Repository name"),
        ("id" = String, Path, description = "Workspace ID"),
    ),
    request_body = UploadFilesRequest,
    responses(
        (status = 201, description = "Files uploaded", body = UploadFilesResponse),
        (status = 400, description = "Bad request", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Workspace not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "workspaces"
)]
/// `POST /api/workspaces/:id/files` — uploads one or more files into the
/// workspace overlay.
///
/// Each file's content must be standard base64-encoded. Binary files are fully
/// supported. Files larger than 10 MiB are rejected with 400 Bad Request.
///
/// - If the file already exists in the overlay a `FileModified` event is
///   recorded; otherwise a `FileAdded` event is recorded.
/// - On first upload the workspace transitions from `Created` → `Active`.
/// - Broadcasts a `FilesUploaded` WebSocket event on success.
pub(super) async fn upload_workspace_files_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    ctx: RepoCtx,
    PathId(id): PathId,
    Json(body): Json<UploadFilesRequest>,
) -> Result<(StatusCode, Json<UploadFilesResponse>), ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, RepoRole::Write).await?;

    // Validate file count and path lengths before acquiring the lock.
    if body.files.len() > MAX_FILES_PER_REQUEST {
        return Err(ApiError::bad_request(format!(
            "too many files: {}, maximum is {MAX_FILES_PER_REQUEST}",
            body.files.len()
        )));
    }
    for entry in &body.files {
        validate_str_len(&entry.path, MAX_PATH_LEN, "file path")?;
    }
    for path in &body.deleted_paths {
        validate_str_len(path, MAX_PATH_LEN, "deleted_path")?;
    }

    let _lock = state.repo_lock.lock().await;

    // Read workspace metadata from storage (works in both local SQLite and Postgres modes).
    let ws_uuid = uuid::Uuid::parse_str(&id)
        .map_err(|_| ApiError::not_found(format!("workspace `{id}` not found")))?;
    let meta = ctx.storage.workspaces()
        .get_workspace(&ctx.repo_id, &ws_uuid)
        .await
        .map_err(ApiError::from)?;
    let workspace_uuid = meta.id;

    let mut uploaded_paths: Vec<String> = Vec::new();

    for entry in &body.files {
        // Decode base64 content.
        let content = BASE64
            .decode(&entry.content_base64)
            .map_err(|e| ApiError::bad_request(format!("base64 decode error for '{}': {e}", entry.path)))?;

        // Enforce per-file size limit.
        if content.len() > MAX_FILE_SIZE_BYTES {
            return Err(ApiError::bad_request(format!(
                "file '{}' exceeds 10 MiB limit ({} bytes)",
                entry.path,
                content.len()
            )));
        }

        // Validate and normalise the path.
        let rel = sanitize_path(&entry.path)
            .ok_or_else(|| ApiError::bad_request(format!("invalid path: '{}'", entry.path)))?;

        let path_str = rel.to_string_lossy().replace('\\', "/");

        // Determine whether this is an add or a modify.
        // Check the workspace overlay first (re-upload), then fall back to
        // current/ (base repo state) to distinguish new files from modifications.
        let new_hash = sha256_hex(&content);
        let store_key = format!("workspaces/{}/{}", id, path_str);
        let current_key = format!("current/{}", path_str);
        let file_store = ctx.storage.files();
        let existing = file_store.get(&ctx.repo_id, &store_key).await.ok()
            .or(file_store.get(&ctx.repo_id, &current_key).await.ok());
        let is_new = existing.is_none();
        let old_hash = existing.as_ref().map(|bytes| sha256_hex(bytes)).unwrap_or_default();

        let new_hash_blob = new_hash.clone();
        let old_hash_blob = old_hash.clone();

        // Write to FileStore (primary storage — works in both S3 and local modes).
        file_store.put(&ctx.repo_id, &store_key, &content).await
            .map_err(|e| ApiError::internal(format!("write overlay file to store: {e}")))?;
        // Also store content-addressably by hash for diffs.
        let _ = file_store.put(&ctx.repo_id, &format!("blobs/{new_hash_blob}"), &content).await;
        if let Some(old_bytes) = existing {
            let _ = file_store.put(&ctx.repo_id, &format!("blobs/{old_hash_blob}"), &old_bytes).await;
        }

        // Also write to local filesystem overlay as cache (best-effort for local mode).
        // ALLOW_FS: local filesystem cache for SQLite mode; best-effort, errors ignored
        let overlay = workspace::overlay_dir(&ctx.vai_dir, &id);
        let dest = overlay.join(&rel);
        if let Some(parent) = dest.parent() {
            // ALLOW_FS: local filesystem cache for SQLite mode; best-effort, errors ignored
            let _ = std::fs::create_dir_all(parent);
        }
        // ALLOW_FS: local filesystem cache for SQLite mode; best-effort, errors ignored
        let _ = std::fs::write(&dest, &content);

        // Append event via storage trait (Postgres pg_notify + local event log).
        let event_kind = if is_new {
            EventKind::FileAdded {
                workspace_id: workspace_uuid,
                path: path_str.clone(),
                hash: new_hash,
            }
        } else {
            EventKind::FileModified {
                workspace_id: workspace_uuid,
                path: path_str.clone(),
                old_hash,
                new_hash,
            }
        };
        let _ = ctx.storage.events().append(&ctx.repo_id, event_kind).await;

        uploaded_paths.push(path_str);
    }

    // Broadcast a WebSocket notification.
    state.broadcast(BroadcastEvent {
        event_type: "FilesUploaded".to_string(),
        event_id: 0,
        workspace_id: Some(id.clone()),
        timestamp: chrono::Utc::now().to_rfc3339(),
        data: serde_json::json!({ "paths": uploaded_paths }),
    });

    // Fetch the complete overlay path list from the FileStore so the conflict
    // engine always sees the authoritative current state of the workspace,
    // not just the files uploaded in this request.
    //
    // Files are stored as `workspaces/{id}/{rel_path}` (e.g. `workspaces/{id}/src/auth.rs`).
    // We also exclude content-addressed blobs stored under `blobs/` which share
    // no prefix with workspace paths and therefore won't appear here.
    let ws_prefix = format!("workspaces/{id}/");
    let overlay_paths: Vec<String> = ctx
        .storage
        .files()
        .list(&ctx.repo_id, &ws_prefix)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter_map(|fm| fm.path.strip_prefix(&ws_prefix).map(|s| s.to_string()))
        .filter(|p| !p.is_empty())
        .collect();

    // Run conflict overlap detection and notify affected workspaces.
    {
        let mut engine = state.conflict_engine.lock().await;
        let overlaps = engine.update_scope(workspace_uuid, &meta.intent, &overlay_paths);
        for overlap in overlaps {
            let ts = chrono::Utc::now().to_rfc3339();
            let payload = serde_json::json!({
                "type": "overlap_detected",
                "severity": overlap.level.as_str(),
                "your_workspace": overlap.your_workspace.to_string(),
                "other_workspace": overlap.other_workspace.to_string(),
                "other_intent": overlap.other_intent,
                "overlapping_files": overlap.overlapping_files,
                "overlapping_entities": overlap.overlapping_entities,
                "recommendation": overlap.recommendation,
            });
            // Notify the workspace whose scope was just updated.
            state.broadcast(BroadcastEvent {
                event_type: "OverlapDetected".to_string(),
                event_id: 0,
                workspace_id: Some(overlap.your_workspace.to_string()),
                timestamp: ts.clone(),
                data: payload.clone(),
            });
            // Also notify the other overlapping workspace.
            let mirrored = serde_json::json!({
                "type": "overlap_detected",
                "severity": overlap.level.as_str(),
                "your_workspace": overlap.other_workspace.to_string(),
                "other_workspace": overlap.your_workspace.to_string(),
                "other_intent": meta.intent,
                "overlapping_files": overlap.overlapping_files,
                "overlapping_entities": overlap.overlapping_entities,
                "recommendation": overlap.recommendation,
            });
            state.broadcast(BroadcastEvent {
                event_type: "OverlapDetected".to_string(),
                event_id: 0,
                workspace_id: Some(overlap.other_workspace.to_string()),
                timestamp: ts,
                data: mirrored,
            });
        }
    }

    // ── Process deleted_paths and status transition ────────────────────────────
    //
    // Merge new deletions into the workspace row's `deleted_paths` column.
    // Also transition workspace from Created → Active on first content upload.
    {
        let mut merged_deleted = meta.deleted_paths.clone();

        for raw_path in &body.deleted_paths {
            let rel = match sanitize_path(raw_path) {
                Some(p) => p.to_string_lossy().replace('\\', "/"),
                None => {
                    return Err(ApiError::bad_request(format!(
                        "invalid deleted path: '{raw_path}'"
                    )));
                }
            };
            if !merged_deleted.contains(&rel) {
                merged_deleted.push(rel.clone());
            }
            // Emit FileRemoved event via storage trait.
            let _ = ctx
                .storage
                .events()
                .append(
                    &ctx.repo_id,
                    EventKind::FileRemoved {
                        workspace_id: workspace_uuid,
                        path: rel,
                    },
                )
                .await;
        }

        let new_status = if meta.status == workspace::WorkspaceStatus::Created
            && (!uploaded_paths.is_empty() || !body.deleted_paths.is_empty())
        {
            Some(workspace::WorkspaceStatus::Active)
        } else {
            None
        };
        let deleted_changed = merged_deleted != meta.deleted_paths;
        if new_status.is_some() || deleted_changed {
            let update = crate::storage::WorkspaceUpdate {
                status: new_status,
                deleted_paths: if deleted_changed { Some(merged_deleted) } else { None },
                ..Default::default()
            };
            let _ = ctx
                .storage
                .workspaces()
                .update_workspace(&ctx.repo_id, &workspace_uuid, update)
                .await;
        }
    }

    let count = uploaded_paths.len();
    Ok((
        StatusCode::OK,
        Json(UploadFilesResponse {
            uploaded: count,
            skipped: 0,
            paths: uploaded_paths,
        }),
    ))
}

#[utoipa::path(
    post,
    path = "/api/repos/{repo}/workspaces/{id}/upload-snapshot",
    params(
        ("repo" = String, Path, description = "Repository name"),
        ("id" = String, Path, description = "Workspace ID"),
        ("allow_destructive" = Option<bool>, Query, description = "Allow uploads that delete >50% of current files (default: false)"),
    ),
    request_body(
        content = String,
        description = "Gzip-compressed tarball of the working directory (Content-Type: application/gzip). Maximum 100 MiB.",
        content_type = "application/gzip"
    ),
    responses(
        (status = 200, description = "Snapshot diffed and stored", body = UploadSnapshotResponse),
        (status = 400, description = "Bad request or invalid tarball", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Workspace not found", body = ErrorBody),
        (status = 409, description = "Upload would delete >50% of current files; use allow_destructive=true to override", body = ErrorBody),
        (status = 413, description = "Tarball exceeds 100 MiB limit", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "workspaces"
)]
/// `POST /api/workspaces/:id/upload-snapshot` — accepts a gzip-compressed
/// tarball of the agent's working directory, diffs it against the current
/// repository state in `current/`, and stores the delta as a workspace
/// overlay.
///
/// ## Full mode (default)
///
/// The endpoint compares each file in the tarball to `current/` using
/// SHA-256 content hashes:
/// - **added** — present in tarball, absent from `current/`
/// - **modified** — present in both, but with a different hash
/// - **deleted** — present in `current/`, absent from tarball
/// - **unchanged** — identical hash in both; skipped
///
/// ## Delta mode
///
/// If the tarball contains a `.vai-delta.json` manifest at its root the
/// upload is processed in delta mode.  The manifest has the form:
/// ```json
/// { "base_version": "v42", "deleted_paths": ["src/old.ts"] }
/// ```
/// In delta mode only the files actually present in the archive are compared
/// against `current/`; absent files are **not** treated as deletions.
/// Instead the explicit `deleted_paths` list from the manifest is used.
/// This allows agents to upload only changed files for large repositories.
///
/// Added and modified files are written to the workspace overlay under
/// `workspaces/{id}/{path}` in the file store.  Deleted paths are recorded
/// via the workspace row's `deleted_paths` column used by submit and download handlers.
///
/// Tarballs larger than 100 MiB (compressed) are rejected with **413**.
pub(super) async fn upload_snapshot_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    ctx: RepoCtx,
    PathId(id): PathId,
    AxumQuery(query): AxumQuery<UploadSnapshotQuery>,
    body: axum::body::Bytes,
) -> Result<(StatusCode, Json<UploadSnapshotResponse>), ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, RepoRole::Write).await?;
    let _lock = state.repo_lock.lock().await;

    // Reject tarballs above the size limit.
    if body.len() > MAX_SNAPSHOT_SIZE_BYTES {
        return Err(ApiError::payload_too_large(format!(
            "tarball exceeds 100 MiB limit ({} bytes)",
            body.len()
        )));
    }

    // Parse workspace metadata.
    let ws_uuid = uuid::Uuid::parse_str(&id)
        .map_err(|_| ApiError::not_found(format!("workspace `{id}` not found")))?;
    let meta = ctx
        .storage
        .workspaces()
        .get_workspace(&ctx.repo_id, &ws_uuid)
        .await
        .map_err(ApiError::from)?;

    // Extract the tarball to an in-memory map, filtering ignored paths.
    let mut raw_files = extract_snapshot_tarball(&body)?;

    // Detect delta mode: presence of `.vai-delta.json` in the archive switches
    // from full-snapshot semantics to delta semantics.
    let delta_manifest: Option<DeltaManifest> = if let Some(manifest_bytes) = raw_files.remove(".vai-delta.json") {
        match serde_json::from_slice::<DeltaManifest>(&manifest_bytes) {
            Ok(m) => Some(m),
            Err(e) => {
                return Err(ApiError::bad_request(format!(
                    "invalid .vai-delta.json: {e}"
                )));
            }
        }
    } else {
        None
    };
    let is_delta = delta_manifest.is_some();

    let tarball_files: HashMap<String, Vec<u8>> = raw_files
        .into_iter()
        .filter(|(path, _)| !is_snapshot_path_ignored(path))
        .collect();

    // Build a map of the current repository state: path → content_hash.
    let file_store = ctx.storage.files();
    let current_entries = file_store
        .list(&ctx.repo_id, "current/")
        .await
        .unwrap_or_default();
    let current_map: HashMap<String, String> = current_entries
        .into_iter()
        .filter_map(|fm| {
            let rel = fm.path.strip_prefix("current/")?.to_string();
            if rel.is_empty() {
                None
            } else {
                Some((rel, fm.content_hash))
            }
        })
        .collect();

    // Diff tarball against current state.
    let mut added = 0usize;
    let mut modified = 0usize;
    let mut unchanged = 0usize;
    let mut uploaded_paths: Vec<String> = Vec::new();

    for (path, content) in &tarball_files {
        let new_hash = sha256_hex(content);

        let event_kind = match current_map.get(path) {
            Some(current_hash) if current_hash == &new_hash => {
                unchanged += 1;
                continue;
            }
            Some(current_hash) => {
                modified += 1;
                EventKind::FileModified {
                    workspace_id: ws_uuid,
                    path: path.clone(),
                    old_hash: current_hash.clone(),
                    new_hash: new_hash.clone(),
                }
            }
            None => {
                added += 1;
                EventKind::FileAdded {
                    workspace_id: ws_uuid,
                    path: path.clone(),
                    hash: new_hash.clone(),
                }
            }
        };

        // Write to workspace overlay in file store.
        let store_key = format!("workspaces/{id}/{path}");
        file_store
            .put(&ctx.repo_id, &store_key, content)
            .await
            .map_err(|e| ApiError::internal(format!("write overlay file: {e}")))?;
        // Also store content-addressably for diffs.
        let _ = file_store
            .put(&ctx.repo_id, &format!("blobs/{new_hash}"), content)
            .await;

        // Record event via storage trait.
        let _ = ctx.storage.events().append(&ctx.repo_id, event_kind).await;

        uploaded_paths.push(path.clone());
    }

    // Compute deletions.
    // - Full mode: any file in current/ that is absent from the tarball is deleted.
    // - Delta mode: only files explicitly listed in the manifest are deleted.
    let deleted_paths: Vec<String> = if let Some(ref manifest) = delta_manifest {
        manifest
            .deleted_paths
            .iter()
            .filter(|p| !p.is_empty())
            .cloned()
            .collect()
    } else {
        current_map
            .keys()
            .filter(|p| !tarball_files.contains_key(*p))
            .cloned()
            .collect()
    };
    let deleted = deleted_paths.len();

    // Safety rail: reject uploads that would delete more than half of the
    // current repository files unless the caller explicitly opts in.
    //
    // A legitimate commit never wipes half a repo; mass deletion is almost
    // always the result of an incorrect overlay or a full-mode tarball that
    // is missing most files (e.g., due to excluded directories whose files
    // were previously stored in `current/`). Requiring explicit opt-in via
    // `?allow_destructive=true` prevents silent data loss.
    let current_count = current_map.len();
    if !query.allow_destructive && deleted > 0 && current_count >= 3 && deleted * 2 > current_count {
        return Err(ApiError::conflict(format!(
            "upload would delete {deleted} of {current_count} files \
             (>{pct:.0}% threshold); set ?allow_destructive=true to proceed",
            pct = (deleted as f64 / current_count as f64) * 100.0,
        )));
    }

    // Merge snapshot deletions into workspace row and transition Created → Active.
    {
        let mut merged_deleted = meta.deleted_paths.clone();
        for path in &deleted_paths {
            if !merged_deleted.contains(path) {
                merged_deleted.push(path.clone());
            }
            let _ = ctx
                .storage
                .events()
                .append(
                    &ctx.repo_id,
                    EventKind::FileRemoved {
                        workspace_id: ws_uuid,
                        path: path.clone(),
                    },
                )
                .await;
        }

        let new_status = if meta.status == workspace::WorkspaceStatus::Created
            && (!uploaded_paths.is_empty() || !deleted_paths.is_empty())
        {
            Some(workspace::WorkspaceStatus::Active)
        } else {
            None
        };
        let deleted_changed = merged_deleted != meta.deleted_paths;
        if new_status.is_some() || deleted_changed {
            let update = crate::storage::WorkspaceUpdate {
                status: new_status,
                deleted_paths: if deleted_changed { Some(merged_deleted) } else { None },
                ..Default::default()
            };
            let _ = ctx
                .storage
                .workspaces()
                .update_workspace(&ctx.repo_id, &ws_uuid, update)
                .await;
        }
    }

    // Broadcast WebSocket notification.
    state.broadcast(BroadcastEvent {
        event_type: "SnapshotUploaded".to_string(),
        event_id: 0,
        workspace_id: Some(id.clone()),
        timestamp: chrono::Utc::now().to_rfc3339(),
        data: serde_json::json!({
            "added": added,
            "modified": modified,
            "deleted": deleted,
            "unchanged": unchanged,
        }),
    });

    Ok((
        StatusCode::OK,
        Json(UploadSnapshotResponse {
            added,
            modified,
            deleted,
            unchanged,
            is_delta,
        }),
    ))
}

#[utoipa::path(
    get,
    path = "/api/repos/{repo}/workspaces/{id}/files/{path}",
    params(
        ("repo" = String, Path, description = "Repository name"),
        ("id" = String, Path, description = "Workspace ID"),
        ("path" = String, Path, description = "File path within workspace"),
    ),
    responses(
        (status = 200, description = "File content", body = FileDownloadResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "File not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "workspaces"
)]
/// `GET /api/workspaces/:id/files/*path` — downloads a file from a workspace.
///
/// The overlay is checked first; if the file is not present there the base
/// repository (repo root) is used as a fallback. Returns 404 if the file
/// exists in neither location. Response includes `found_in: "overlay"|"base"`.
pub(super) async fn get_workspace_file_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    AxumPath(params): AxumPath<HashMap<String, String>>,
) -> Result<Json<FileDownloadResponse>, ApiError> {
    let id = params
        .get("id")
        .cloned()
        .ok_or_else(|| ApiError::bad_request("missing `:id` path parameter"))?;
    let path = params
        .get("path")
        .cloned()
        .ok_or_else(|| ApiError::bad_request("missing `*path` wildcard"))?;
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, RepoRole::Read).await?;

    // Verify workspace exists via storage trait (works in both SQLite and Postgres modes).
    let ws_uuid = uuid::Uuid::parse_str(&id)
        .map_err(|_| ApiError::not_found(format!("workspace `{id}` not found")))?;
    ctx.storage.workspaces()
        .get_workspace(&ctx.repo_id, &ws_uuid)
        .await
        .map_err(ApiError::from)?;

    let rel = sanitize_path(&path)
        .ok_or_else(|| ApiError::bad_request(format!("invalid path: '{path}'")))?;
    let path_str = rel.to_string_lossy().replace('\\', "/");

    let file_store = ctx.storage.files();

    // 1. Try overlay from storage (primary path for Postgres/S3 mode).
    let overlay_key = format!("workspaces/{id}/{path_str}");
    if let Ok(bytes) = file_store.get(&ctx.repo_id, &overlay_key).await {
        let size = bytes.len();
        return Ok(Json(FileDownloadResponse {
            path: path_str,
            content_base64: BASE64.encode(&bytes),
            size,
            found_in: "overlay".to_string(),
        }));
    }

    // 2. Try overlay from local filesystem (fallback for SQLite/local mode).
    // ALLOW_FS: fallback for local/SQLite mode when FileStore has no overlay entry
    let overlay_path = workspace::overlay_dir(&ctx.vai_dir, &id).join(&rel);
    if overlay_path.exists() {
        // ALLOW_FS: fallback for local/SQLite mode when FileStore has no overlay entry
        let bytes = std::fs::read(&overlay_path)
            .map_err(|e| ApiError::internal(format!("read overlay file: {e}")))?;
        let size = bytes.len();
        return Ok(Json(FileDownloadResponse {
            path: path_str,
            content_base64: BASE64.encode(&bytes),
            size,
            found_in: "overlay".to_string(),
        }));
    }

    // 3. Try base from storage `current/` prefix (set by submit handler after each merge).
    let current_key = format!("current/{path_str}");
    if let Ok(bytes) = file_store.get(&ctx.repo_id, &current_key).await {
        let size = bytes.len();
        return Ok(Json(FileDownloadResponse {
            path: path_str,
            content_base64: BASE64.encode(&bytes),
            size,
            found_in: "base".to_string(),
        }));
    }

    // 4. Final fallback: read from repo root on disk (local/SQLite mode, migration).
    let base_path = ctx.repo_root.join(&rel);
    if !base_path.exists() {
        return Err(ApiError::not_found(format!("file not found: '{path}'")));
    }
    // ALLOW_FS: final fallback for local/SQLite mode and migration-seeded repos
    let bytes = std::fs::read(&base_path)
        .map_err(|e| ApiError::internal(format!("read base file: {e}")))?;
    let size = bytes.len();
    Ok(Json(FileDownloadResponse {
        path: path_str,
        content_base64: BASE64.encode(&bytes),
        size,
        found_in: "base".to_string(),
    }))
}

// ── Private helpers ────────────────────────────────────────────────────────────

/// Extracts a gzip-compressed tarball into an in-memory `{path → content}` map.
///
/// Paths are normalised by stripping any leading `./` prefix.  Directory
/// entries are silently skipped.  Symlinks and hard links are rejected
/// outright — they could be used to escape the workspace root.  Each file is
/// limited to `MAX_FILE_SIZE_BYTES`; the overall tarball limit is enforced by
/// the caller.  Returns an error if the bytes are not a valid gzip-compressed
/// tar archive.
fn extract_snapshot_tarball(gz_bytes: &[u8]) -> Result<HashMap<String, Vec<u8>>, ApiError> {
    use flate2::read::GzDecoder;
    use tar::Archive;
    use std::io::Read;

    let decoder = GzDecoder::new(gz_bytes);
    let mut archive = Archive::new(decoder);
    let mut files = HashMap::new();

    let entries = archive
        .entries()
        .map_err(|e| ApiError::bad_request(format!("invalid tarball: {e}")))?;

    for entry in entries {
        let mut entry = entry
            .map_err(|e| ApiError::bad_request(format!("invalid tarball entry: {e}")))?;

        let entry_type = entry.header().entry_type();

        // Reject symlinks and hard links — they can be used to traverse outside
        // the workspace root or reference paths the agent does not own.
        if entry_type.is_symlink() || entry_type.is_hard_link() {
            return Err(ApiError::bad_request(
                "tarball contains a symlink or hard link, which is not permitted",
            ));
        }

        // Skip directories and other non-regular-file entries.
        if !entry_type.is_file() {
            continue;
        }

        let raw_path = entry
            .path()
            .map_err(|e| ApiError::bad_request(format!("invalid path in tarball: {e}")))?
            .to_string_lossy()
            .to_string();

        // Normalise: strip leading "./" and validate for path traversal.
        let path = raw_path.trim_start_matches("./").to_string();
        let rel = match sanitize_path(&path) {
            Some(p) => p.to_string_lossy().replace('\\', "/"),
            None => {
                return Err(ApiError::bad_request(format!(
                    "tarball contains an unsafe path: '{path}'"
                )));
            }
        };
        if rel.is_empty() {
            continue;
        }

        let mut content = Vec::new();
        entry
            .read_to_end(&mut content)
            .map_err(|e| ApiError::bad_request(format!("read tarball entry '{rel}': {e}")))?;

        // Enforce per-file size limit.
        if content.len() > MAX_FILE_SIZE_BYTES {
            return Err(ApiError::bad_request(format!(
                "tarball entry '{rel}' exceeds 10 MiB per-file limit ({} bytes)",
                content.len()
            )));
        }

        files.insert(rel, content);
    }

    Ok(files)
}

/// Returns `true` for paths that should be excluded from snapshot uploads.
///
/// Always excludes `.vai/` and `.git/` trees which are internal to the
/// version-control tooling and must never be stored as workspace overlay
/// files.
fn is_snapshot_path_ignored(path: &str) -> bool {
    path.starts_with(".vai/")
        || path.starts_with(".git/")
        || path == ".vai"
        || path == ".git"
}

/// A self-cleaning temporary directory for use with [`setup_tmpdir_for_s3_submit`].
///
/// The inner directory is removed when this value is dropped.
struct TmpDir(std::path::PathBuf);

impl TmpDir {
    /// Returns the path to the temporary directory root.
    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TmpDir {
    fn drop(&mut self) {
        // ALLOW_FS: tmpdir cleanup for S3 submit merge engine scaffold
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Creates a minimal temporary `.vai/` directory for use with
/// [`merge::submit_with_fs`] in S3 server mode.
///
/// The merge engine expects a `.vai/` directory for metadata operations
/// (event log, workspace status, HEAD file, version directory).  In S3 mode
/// the real repo directory may not exist on disk, so this function sets up
/// the minimal structure in a temporary directory that the merge engine can
/// operate on.  The caller must keep the returned [`TmpDir`] alive for the
/// duration of the `submit_with_fs` call; it is automatically cleaned up when
/// dropped.
///
/// # Minimal structure created
///
/// | Path                              | Purpose                                      |
/// |-----------------------------------|----------------------------------------------|
/// | `.vai/head`                       | Lets `repo::read_head` return `current_head` |
/// | `.vai/workspaces/<id>/meta.toml`  | Workspace metadata for the merge engine      |
/// | `.vai/workspaces/active`          | Active workspace pointer for `diff::record_events` |
/// | `.vai/versions/<head>.toml`       | Stub so `version::next_version_id` is correct |
fn setup_tmpdir_for_s3_submit(
    ws_meta: &workspace::WorkspaceMeta,
    current_head: &str,
) -> Result<TmpDir, ApiError> {
    let tmp_path = std::env::temp_dir().join(format!("vai-submit-{}", uuid::Uuid::new_v4()));
    // ALLOW_FS: tmpdir scaffold required by the merge engine for S3 server-mode submit
    std::fs::create_dir_all(&tmp_path)
        .map_err(|e| ApiError::internal(format!("create tmpdir for submit: {e}")))?;
    let tmp = TmpDir(tmp_path);
    let vai = tmp.path().join(".vai");

    // HEAD file.
    // ALLOW_FS: tmpdir scaffold required by the merge engine for S3 server-mode submit
    std::fs::create_dir_all(&vai)
        .map_err(|e| ApiError::internal(format!("create tmpdir/.vai: {e}")))?;
    // ALLOW_FS: tmpdir scaffold required by the merge engine for S3 server-mode submit
    std::fs::write(vai.join("head"), format!("{current_head}\n"))
        .map_err(|e| ApiError::internal(format!("write tmpdir head: {e}")))?;

    // Workspace dir + meta.toml.
    let ws_dir = vai.join("workspaces").join(ws_meta.id.to_string());
    // ALLOW_FS: tmpdir scaffold required by the merge engine for S3 server-mode submit
    std::fs::create_dir_all(&ws_dir)
        .map_err(|e| ApiError::internal(format!("create tmpdir workspace dir: {e}")))?;
    // ALLOW_FS: tmpdir scaffold required by the merge engine for S3 server-mode submit
    workspace::update_meta(&vai, ws_meta)
        .map_err(|e| ApiError::internal(format!("write tmpdir workspace meta: {e}")))?;

    // Active workspace pointer (needed by diff::record_events → workspace::active).
    // ALLOW_FS: tmpdir scaffold required by the merge engine for S3 server-mode submit
    std::fs::write(vai.join("workspaces").join("active"), ws_meta.id.to_string())
        .map_err(|e| ApiError::internal(format!("set tmpdir active workspace: {e}")))?;

    // Version TOML stub for current HEAD so next_version_id returns the right value.
    let versions_dir = vai.join("versions");
    // ALLOW_FS: tmpdir scaffold required by the merge engine for S3 server-mode submit
    std::fs::create_dir_all(&versions_dir)
        .map_err(|e| ApiError::internal(format!("create tmpdir versions dir: {e}")))?;
    let stub_toml = format!(
        "version_id = \"{current_head}\"\nintent = \"placeholder\"\n\
         created_by = \"server\"\ncreated_at = \"{}\"\n",
        chrono::Utc::now().to_rfc3339()
    );
    // ALLOW_FS: tmpdir scaffold required by the merge engine for S3 server-mode submit
    std::fs::write(versions_dir.join(format!("{current_head}.toml")), stub_toml)
        .map_err(|e| ApiError::internal(format!("write tmpdir version toml: {e}")))?;

    Ok(tmp)
}

/// Recursively collects all files under `dir`, returning `(relative_path,
/// content)` pairs.  `relative_path` uses `/` separators.
///
/// Returns an empty vec if `dir` does not exist.  Silently skips any entry
/// that cannot be read.
fn collect_dir_files_with_content(dir: &std::path::Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    collect_dir_recursive(dir, dir, &mut out);
    out
}

fn collect_dir_recursive(
    base: &std::path::Path,
    cur: &std::path::Path,
    out: &mut Vec<(String, Vec<u8>)>,
) {
    // ALLOW_FS: disk traversal helper used only by local SQLite mode path in submit handler
    let entries = match std::fs::read_dir(cur) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_dir_recursive(base, &path, out);
        } else {
            let rel = path
                .strip_prefix(base)
                .map(|r| r.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            if rel.is_empty() {
                continue;
            }
            // ALLOW_FS: disk traversal helper used only by local SQLite mode path in submit handler
            if let Ok(bytes) = std::fs::read(&path) {
                out.push((rel.to_owned(), bytes));
            }
        }
    }
}
