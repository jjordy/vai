//! Graph API handlers — semantic graph queries and refresh.
//!
//! Endpoints:
//!   - `GET /api/repos/:repo/graph/entities` — list entities with optional filters (`?kind=`, `?file=`, `?name=`)
//!   - `GET /api/repos/:repo/graph/entities/:id` — entity details and relationships
//!   - `GET /api/repos/:repo/graph/entities/:id/deps` — transitive dependencies
//!   - `GET /api/repos/:repo/graph/blast-radius` — blast-radius query
//!   - `POST /api/repos/:repo/graph/refresh` — rebuild semantic graph from source files

use std::sync::Arc;

use axum::extract::{Query as AxumQuery, State};
use axum::Extension;
use axum::Json;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{AgentIdentity, ApiError, AppState, ErrorBody, PathId, RepoCtx};
use super::require_repo_permission;

// ── Request / response types ──────────────────────────────────────────────────

/// Response body for `POST /api/graph/refresh`.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct ServerGraphRefreshResponse {
    /// Number of source files scanned during the rebuild.
    files_scanned: usize,
    /// Total entities in the graph after refresh.
    entities: usize,
    /// Total relationships in the graph after refresh.
    relationships: usize,
}

/// Query parameters for `GET /api/graph/entities`.
#[derive(Debug, Default, Deserialize, ToSchema)]
pub(super) struct GraphEntityFilter {
    /// Filter by entity kind (e.g. `"function"`, `"struct"`).
    kind: Option<String>,
    /// Filter by exact file path (relative to repo root).
    file: Option<String>,
    /// Filter by entity name substring (case-insensitive).
    name: Option<String>,
}

/// Query parameters for `GET /api/graph/blast-radius`.
#[derive(Debug, Deserialize, ToSchema)]
pub(super) struct BlastRadiusQuery {
    /// Comma-separated entity IDs to use as seeds.
    entities: String,
    /// Maximum traversal depth from each seed (default: 2).
    #[serde(default = "default_hops")]
    hops: usize,
}

fn default_hops() -> usize {
    2
}

/// Lightweight entity summary returned by graph list endpoints.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct EntitySummary {
    id: String,
    kind: String,
    name: String,
    qualified_name: String,
    file: String,
    line_start: usize,
    line_end: usize,
    parent_entity: Option<String>,
}

impl From<crate::graph::Entity> for EntitySummary {
    fn from(e: crate::graph::Entity) -> Self {
        EntitySummary {
            id: e.id,
            kind: e.kind.to_string(),
            name: e.name,
            qualified_name: e.qualified_name,
            file: e.file_path,
            line_start: e.line_range.0,
            line_end: e.line_range.1,
            parent_entity: e.parent_entity,
        }
    }
}

/// Response body for `GET /api/graph/entities/:id`.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct EntityDetailResponse {
    entity: EntitySummary,
    relationships: Vec<RelationshipSummary>,
}

/// Relationship summary used in graph API responses.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct RelationshipSummary {
    id: String,
    kind: String,
    from_entity: String,
    to_entity: String,
}

impl From<crate::graph::Relationship> for RelationshipSummary {
    fn from(r: crate::graph::Relationship) -> Self {
        RelationshipSummary {
            id: r.id,
            kind: r.kind.as_str().to_string(),
            from_entity: r.from_entity,
            to_entity: r.to_entity,
        }
    }
}

/// Response body for `GET /api/graph/entities/:id/deps`.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct EntityDepsResponse {
    entity_id: String,
    deps: Vec<EntitySummary>,
    relationships: Vec<RelationshipSummary>,
}

/// Response body for `GET /api/graph/blast-radius`.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct BlastRadiusResponse {
    seed_entities: Vec<String>,
    hops: usize,
    entities: Vec<EntitySummary>,
    relationships: Vec<RelationshipSummary>,
}

// ── Graph helpers ─────────────────────────────────────────────────────────────

/// Parseable file extensions for the semantic graph engine.
const GRAPH_PARSEABLE_EXTENSIONS: &[&str] = &["rs", "ts", "js", "tsx", "jsx"];

/// Opens the graph snapshot for the repository.
fn open_graph(vai_dir: &std::path::Path) -> Result<crate::graph::GraphSnapshot, ApiError> {
    let db_path = vai_dir.join("graph").join("snapshot.db");
    crate::graph::GraphSnapshot::open(&db_path)
        .map_err(|e| ApiError::internal(format!("graph error: {e}")))
}

/// Rebuilds the semantic graph by reading source files from the `current/`
/// prefix in `file_store`.
///
/// Reads `current/vai.toml` (if present) for ignore patterns, lists all files
/// under `current/`, filters to parseable extensions, downloads each file, and
/// upserts parsed entities and relationships into `graph`.
///
/// Returns `(files_scanned, total_entities, total_relationships)`.
pub(super) async fn refresh_graph_from_files(
    graph: Arc<dyn crate::storage::GraphStore>,
    file_store: Arc<dyn crate::storage::FileStore>,
    repo_id: uuid::Uuid,
) -> Result<(usize, usize, usize), ApiError> {
    // Read ignore patterns from current/vai.toml — fall back to defaults if absent.
    let ignore: Vec<String> = match file_store.get(&repo_id, "current/vai.toml").await {
        Ok(bytes) => {
            let raw = String::from_utf8_lossy(&bytes);
            toml::from_str::<crate::repo::VaiToml>(&raw)
                .unwrap_or_default()
                .ignore
        }
        Err(_) => vec![],
    };

    // List all files under current/ and filter to parseable extensions.
    let all_files = file_store
        .list(&repo_id, "current/")
        .await
        .map_err(|e| ApiError::internal(format!("list current/ files: {e}")))?;

    let mut files_scanned = 0usize;
    let mut total_entities = 0usize;
    let mut total_relationships = 0usize;

    for meta in &all_files {
        // Strip the "current/" prefix to get the repo-relative path.
        let rel = meta.path.strip_prefix("current/").unwrap_or(&meta.path);

        // Skip files that match ignore patterns.
        if ignore.iter().any(|pat| rel.starts_with(pat.as_str())) {
            continue;
        }

        // Skip files with non-parseable extensions.
        let ext = std::path::Path::new(rel)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        if !GRAPH_PARSEABLE_EXTENSIONS.contains(&ext) {
            continue;
        }

        let source = match file_store.get(&repo_id, &meta.path).await {
            Ok(b) => b,
            Err(_) => continue, // best-effort: skip unreadable files
        };

        let (entities, rels) = match crate::graph::parse_source_file(rel, &source) {
            Ok(r) => r,
            Err(_) => continue, // best-effort: skip unparseable files
        };

        // Clear stale data for this file before upserting fresh entities.
        graph
            .clear_file(&repo_id, rel)
            .await
            .map_err(|e| ApiError::internal(format!("clear graph for {rel}: {e}")))?;

        total_entities += entities.len();
        total_relationships += rels.len();

        if !entities.is_empty() {
            graph
                .upsert_entities(&repo_id, entities)
                .await
                .map_err(|e| ApiError::internal(format!("upsert entities for {rel}: {e}")))?;
        }
        if !rels.is_empty() {
            graph
                .upsert_relationships(&repo_id, rels)
                .await
                .map_err(|e| ApiError::internal(format!("upsert relationships for {rel}: {e}")))?;
        }

        files_scanned += 1;
    }

    Ok((files_scanned, total_entities, total_relationships))
}

// ── Graph API handlers ────────────────────────────────────────────────────────

#[utoipa::path(
    post,
    path = "/api/repos/{repo}/graph/refresh",
    params(
        ("repo" = String, Path, description = "Repository name"),
    ),
    responses(
        (status = 200, description = "Graph refreshed", body = ServerGraphRefreshResponse),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = [])),
    tag = "graph"
)]
/// `POST /api/graph/refresh` — rebuilds the semantic graph from source files.
///
/// In server mode (S3 + Postgres) reads source files from the `current/` prefix
/// in S3. In local mode reads from the repo root on disk.
///
/// Should be called after `POST /api/files` completes to ensure the graph
/// reflects the uploaded source files (PRD 12.4). Also triggered automatically
/// after each successful workspace submit in server mode.
///
/// Writes entity and relationship data via the configured [`GraphStore`] backend
/// (Postgres in server mode, SQLite in local mode) so the correct store is
/// always updated.
pub(super) async fn server_graph_refresh_handler(
    Extension(identity): Extension<AgentIdentity>,
    State(state): State<Arc<AppState>>,
    ctx: RepoCtx,
) -> Result<Json<ServerGraphRefreshResponse>, ApiError> {
    require_repo_permission(
        &ctx.storage,
        &identity,
        &ctx.repo_id,
        crate::storage::RepoRole::Write,
    )
    .await?;
    let _lock = state.repo_lock.lock().await;

    let using_s3 = matches!(
        &ctx.storage,
        crate::storage::StorageBackend::ServerWithS3(_, _)
            | crate::storage::StorageBackend::ServerWithMemFs(_, _)
    );

    if using_s3 {
        // Server mode: read source files from the current/ prefix in S3.
        let (files_scanned, total_entities, total_relationships) =
            refresh_graph_from_files(ctx.storage.graph(), ctx.storage.files(), ctx.repo_id)
                .await?;

        return Ok(Json(ServerGraphRefreshResponse {
            files_scanned,
            entities: total_entities,
            relationships: total_relationships,
        }));
    }

    // Local disk mode: read ignore patterns from vai.toml and walk repo root.
    let vai_toml_path = ctx.repo_root.join("vai.toml");
    let vai_toml: crate::repo::VaiToml = if vai_toml_path.exists() {
        // ALLOW_FS: local SQLite mode only — guarded by `if using_s3` early return above
        let raw = std::fs::read_to_string(&vai_toml_path)
            .map_err(|e| ApiError::internal(format!("read vai.toml: {e}")))?;
        toml::from_str(&raw)
            .map_err(|e| ApiError::internal(format!("parse vai.toml: {e}")))?
    } else {
        crate::repo::VaiToml::default()
    };

    let source_files = crate::repo::collect_source_files(&ctx.repo_root, &vai_toml.ignore);
    let graph = ctx.storage.graph();
    let repo_id = ctx.repo_id;

    let mut files_scanned = 0usize;
    let mut total_entities = 0usize;
    let mut total_relationships = 0usize;

    for file_path in &source_files {
        let rel = file_path
            .strip_prefix(&ctx.repo_root)
            .unwrap_or(file_path)
            .to_string_lossy()
            .into_owned();

        // ALLOW_FS: local SQLite mode only — guarded by `if using_s3` early return above
        let source = match std::fs::read(file_path) {
            Ok(b) => b,
            Err(_) => continue, // best-effort: skip unreadable files
        };

        let (entities, rels) = match crate::graph::parse_source_file(&rel, &source) {
            Ok(r) => r,
            Err(_) => continue, // best-effort: skip unparseable files
        };

        // Clear stale data for this file before upserting fresh entities.
        graph
            .clear_file(&repo_id, &rel)
            .await
            .map_err(|e| ApiError::internal(format!("clear graph for {rel}: {e}")))?;

        total_entities += entities.len();
        total_relationships += rels.len();

        if !entities.is_empty() {
            graph
                .upsert_entities(&repo_id, entities)
                .await
                .map_err(|e| ApiError::internal(format!("upsert entities for {rel}: {e}")))?;
        }
        if !rels.is_empty() {
            graph
                .upsert_relationships(&repo_id, rels)
                .await
                .map_err(|e| ApiError::internal(format!("upsert relationships for {rel}: {e}")))?;
        }

        files_scanned += 1;
    }

    Ok(Json(ServerGraphRefreshResponse {
        files_scanned,
        entities: total_entities,
        relationships: total_relationships,
    }))
}

#[utoipa::path(
    get,
    path = "/api/repos/{repo}/graph/entities",
    params(
        ("repo" = String, Path, description = "Repository name"),
        ("kind" = Option<String>, Query, description = "Filter by entity kind (e.g. \"function\", \"struct\")"),
        ("file" = Option<String>, Query, description = "Filter by exact file path"),
        ("name" = Option<String>, Query, description = "Filter by entity name substring"),
    ),
    responses(
        (status = 200, description = "List of entities", body = Vec<EntitySummary>),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = [])),
    tag = "graph"
)]
/// `GET /api/graph/entities` — lists entities with optional filters.
///
/// Query params: `kind`, `file`, `name` (all optional, combined with AND).
pub(super) async fn list_graph_entities_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    AxumQuery(filter): AxumQuery<GraphEntityFilter>,
) -> Result<Json<Vec<EntitySummary>>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Read).await?;
    let graph = open_graph(&ctx.vai_dir)?;
    let entities = graph
        .filter_entities(
            filter.kind.as_deref(),
            filter.file.as_deref(),
            filter.name.as_deref(),
        )
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(entities.into_iter().map(Into::into).collect()))
}

#[utoipa::path(
    get,
    path = "/api/repos/{repo}/graph/entities/{id}",
    params(
        ("repo" = String, Path, description = "Repository name"),
        ("id" = String, Path, description = "Entity ID"),
    ),
    responses(
        (status = 200, description = "Entity details and relationships", body = EntityDetailResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Entity not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "graph"
)]
/// `GET /api/graph/entities/:id` — entity details and its relationships.
pub(super) async fn get_graph_entity_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    PathId(id): PathId,
) -> Result<Json<EntityDetailResponse>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Read).await?;
    let graph = open_graph(&ctx.vai_dir)?;
    let entity = graph
        .get_entity_by_id(&id)
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found(format!("entity '{id}' not found")))?;
    let relationships = graph
        .get_relationships_for_entity(&id)
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(EntityDetailResponse {
        entity: entity.into(),
        relationships: relationships.into_iter().map(Into::into).collect(),
    }))
}

#[utoipa::path(
    get,
    path = "/api/repos/{repo}/graph/entities/{id}/deps",
    params(
        ("repo" = String, Path, description = "Repository name"),
        ("id" = String, Path, description = "Entity ID"),
    ),
    responses(
        (status = 200, description = "Transitive dependencies of the entity", body = EntityDepsResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Entity not found", body = ErrorBody),
    ),
    security(("bearer_auth" = [])),
    tag = "graph"
)]
/// `GET /api/graph/entities/:id/deps` — all entities transitively reachable
/// from this entity following any relationship direction.
pub(super) async fn get_entity_deps_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    PathId(id): PathId,
) -> Result<Json<EntityDepsResponse>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Read).await?;
    let graph = open_graph(&ctx.vai_dir)?;
    // Verify the entity exists before traversal.
    graph
        .get_entity_by_id(&id)
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found(format!("entity '{id}' not found")))?;

    // Use a generous max-hops so we reach all transitive deps in practice.
    let (entities, relationships) = graph
        .reachable_entities(&[id.as_str()], 20)
        .map_err(|e| ApiError::internal(e.to_string()))?;

    // Exclude the seed entity itself from the deps list.
    let deps = entities
        .into_iter()
        .filter(|e| e.id != id)
        .map(Into::into)
        .collect();

    Ok(Json(EntityDepsResponse {
        entity_id: id,
        deps,
        relationships: relationships.into_iter().map(Into::into).collect(),
    }))
}

#[utoipa::path(
    get,
    path = "/api/repos/{repo}/graph/blast-radius",
    params(
        ("repo" = String, Path, description = "Repository name"),
        ("entities" = String, Query, description = "Comma-separated entity IDs"),
        ("hops" = Option<usize>, Query, description = "Maximum traversal depth (default: 2)"),
    ),
    responses(
        (status = 200, description = "Blast radius result", body = BlastRadiusResponse),
        (status = 400, description = "Bad request", body = ErrorBody),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = [])),
    tag = "graph"
)]
/// `GET /api/graph/blast-radius` — entities reachable from a set of seeds within N hops.
///
/// Query params:
/// - `entities` — comma-separated entity IDs
/// - `hops` — max traversal depth (default: 2)
pub(super) async fn get_blast_radius_handler(
    Extension(identity): Extension<AgentIdentity>,
    ctx: RepoCtx,
    AxumQuery(query): AxumQuery<BlastRadiusQuery>,
) -> Result<Json<BlastRadiusResponse>, ApiError> {
    require_repo_permission(&ctx.storage, &identity, &ctx.repo_id, crate::storage::RepoRole::Read).await?;
    let seed_ids: Vec<String> = query
        .entities
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    if seed_ids.is_empty() {
        return Err(ApiError::bad_request(
            "query param `entities` must contain at least one entity ID",
        ));
    }

    let hops = query.hops;
    let graph = open_graph(&ctx.vai_dir)?;

    let seed_refs: Vec<&str> = seed_ids.iter().map(String::as_str).collect();
    let (entities, relationships) = graph
        .reachable_entities(&seed_refs, hops)
        .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok(Json(BlastRadiusResponse {
        seed_entities: seed_ids,
        hops,
        entities: entities.into_iter().map(Into::into).collect(),
        relationships: relationships.into_iter().map(Into::into).collect(),
    }))
}
