//! Unified, mode-agnostic workspace file operations.
//!
//! [`FileWorkspace`] consolidates the workspace/diff/merge/storage flow into a
//! single struct with three primary verbs: [`plan`], [`submit`], and [`pull`].
//! The backend (local disk vs remote server) lives in the constructor only; all
//! callers see the same API regardless of mode.
//!
//! ## Key invariants
//!
//! - [`FileWorkspace::submit`] refuses to proceed when [`Plan::surprises`] is
//!   non-empty. Callers must inspect the plan and either bail or call
//!   [`FileWorkspace::submit_forcing_deletions`].
//! - [`Plan::dels`] is the single canonical deletion set. The on-disk
//!   `.vai-deleted` manifest and `workspace.deleted_paths` in `meta.toml` are
//!   private implementation details, consolidated on [`open`].
//! - A successful [`submit`] is atomic: HEAD advances, the workspace closes, and
//!   deletion state updates everywhere — or none of it.
//!
//! ## Migration status
//!
//! Step 1 built the parallel module. Step 2 migrates `cli/workspace.rs::Submit`
//! and provides [`HttpRemoteRepo`] for production remote I/O.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::diff::{self, EntityChange, FileChangeType};
use crate::merge;
use crate::merge_fs::{DiskMergeFs, MergeFs};
use crate::repo;
use crate::workspace;

// ── Public types ──────────────────────────────────────────────────────────────

/// Constructor options for [`FileWorkspace::open`].
pub struct OpenOptions {
    /// Absolute path to the repository root (the directory containing `.vai/`).
    pub repo_root: PathBuf,
    /// Which backend to use for remote I/O.
    pub backend: Backend,
    /// Whether to use an existing workspace or create a new one.
    pub intent: Intent,
}

impl OpenOptions {
    /// Opens the active workspace in local disk mode.
    pub fn local(repo_root: PathBuf) -> Self {
        Self {
            repo_root,
            backend: Backend::Local,
            intent: Intent::Existing,
        }
    }

    /// Auto-detects the backend from the repo's `.vai/config.toml`.
    ///
    /// If a remote is configured, returns a [`Backend::Remote`] backed by
    /// [`HttpRemoteRepo`]. Otherwise returns [`Backend::Local`].
    pub fn from_root(repo_root: PathBuf) -> Self {
        let vai_dir = repo_root.join(".vai");
        let backend = if let Some(remote) = crate::clone::read_remote_config(&vai_dir) {
            Backend::Remote(HttpRemoteRepo::new(remote))
        } else {
            Backend::Local
        };
        Self {
            repo_root,
            backend,
            intent: Intent::Existing,
        }
    }
}

/// Backend for remote I/O.
pub enum Backend {
    /// Local disk mode — all workspace state lives in `.vai/`.
    Local,
    /// Remote server mode — workspace state lives on the server.
    Remote(Arc<dyn RemoteRepo>),
}

/// Whether to use an existing workspace or create a fresh one.
pub enum Intent {
    /// Use the currently active workspace.
    Existing,
    /// Create a new workspace for the given intent text.
    CreateFor {
        text: String,
        issue_id: Option<Uuid>,
    },
    /// Use an existing workspace identified by `workspace_id`.
    ///
    /// Unlike [`Existing`], the caller supplies the IDs directly (read from
    /// `.vai/agent-state.json` by the agent). The workspace is treated as an
    /// upload-from-root submit: `work_dir` is packed and uploaded as a full
    /// snapshot rather than tracking a workspace overlay.
    AgentWork {
        workspace_id: Uuid,
        issue_id: Option<Uuid>,
        intent: String,
        /// Directory whose files will be listed and uploaded.
        work_dir: PathBuf,
    },
}

/// Instruction for [`FileWorkspace::submit`]: how to handle the commit message
/// and what to do when the workspace is empty.
pub enum Submit {
    /// Commit the changes; fail with [`FwError::Empty`] if workspace is empty.
    Required(String),
    /// If the workspace is empty, signal the caller to close the linked issue
    /// instead of treating it as an error.
    CloseIfEmpty(String),
}

impl Submit {
    fn message(&self) -> &str {
        match self {
            Submit::Required(m) | Submit::CloseIfEmpty(m) => m,
        }
    }

    fn is_close_if_empty(&self) -> bool {
        matches!(self, Submit::CloseIfEmpty(_))
    }
}

/// Read-only snapshot of what a [`FileWorkspace::submit`] would do.
#[derive(Debug)]
pub struct Plan {
    /// Files that will be added (present in overlay, absent in base).
    pub adds: Vec<PathBuf>,
    /// Files that will be modified (changed relative to base).
    pub mods: Vec<PathBuf>,
    /// Files that will be deleted (explicitly removed by the agent).
    ///
    /// This is the canonical deletion set, sourced from both `.vai-deleted`
    /// and `workspace.deleted_paths` and deduplicated.
    pub dels: Vec<PathBuf>,
    /// Detailed per-file change records including line counts.
    ///
    /// Mirrors the `adds`/`mods`/`dels` lists but with richer detail for
    /// display purposes (e.g. `vai workspace diff`).
    pub file_diffs: Vec<diff::FileDiff>,
    /// Entity-level changes (added / modified / removed semantic entities).
    pub entity_changes: Vec<EntityChange>,
    /// State that would surprise a caller relying on plan→submit atomicity.
    pub surprises: Vec<Surprise>,
    /// The version HEAD was at when this workspace was created.
    pub base_version: String,
    /// The version that HEAD is at right now.
    pub head_version: String,
}

impl Plan {
    /// Returns `true` when the workspace has no pending changes of any kind.
    pub fn is_empty(&self) -> bool {
        self.adds.is_empty()
            && self.mods.is_empty()
            && self.dels.is_empty()
            && self.entity_changes.is_empty()
    }
}

/// Surprising state that makes a safe atomic submit impossible.
#[derive(Debug, Clone)]
pub enum Surprise {
    /// The server has a file that the local client has not pulled.
    ///
    /// Submitting without pulling first would silently delete this file from
    /// the server. Structural fix for issue #367.
    ServerHasFileLocalDoesNot(PathBuf),
    /// HEAD has advanced past the workspace's base version between plan and
    /// submit calls.
    BaseDrifted { expected: String, actual: String },
    /// A merge conflict is predicted between workspace changes and HEAD.
    ConflictPredicted { path: PathBuf, kind: ConflictKind },
}

/// Reason a conflict is predicted.
#[derive(Debug, Clone)]
pub enum ConflictKind {
    /// Both workspace and HEAD modified the same semantic entity.
    SameEntityModified,
    /// Workspace removed an entity that HEAD also modified.
    RemovedModified,
}

/// Upload statistics for a remote snapshot (delta or full).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotInfo {
    /// Files present in the upload but absent from the server's HEAD.
    pub added: usize,
    /// Files whose content changed relative to the server's HEAD.
    pub modified: usize,
    /// Files removed (listed in the delta manifest or absent from the tarball).
    pub deleted: usize,
    /// `true` when the server processed the upload in delta mode.
    pub is_delta: bool,
}

/// Result of a successful [`FileWorkspace::submit`] or [`FileWorkspace::pull`].
#[derive(Debug, Serialize)]
pub struct Applied {
    /// New version ID created (submit) or synced to (pull).
    pub version: String,
    /// Number of files written.
    pub files: usize,
    /// Number of entity-level changes applied.
    pub entities: usize,
    /// Stable IDs of entities touched (populated for local submit only).
    pub entity_ids: Vec<String>,
    /// Workspace intent text (populated for local submit only).
    pub intent: String,
    /// Remote snapshot upload stats — `Some` for remote submit, `None` for local.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<SnapshotInfo>,
}

// ── RemoteRepo port ───────────────────────────────────────────────────────────

/// Port for remote server operations used by [`FileWorkspace`] in
/// [`Backend::Remote`] mode.
///
/// Production implementation: [`HttpRemoteRepo`] (wraps `remote_workspace::*`).
/// Test implementation: [`InMemoryRemoteRepo`] (in tests module).
#[async_trait]
pub trait RemoteRepo: Send + Sync {
    /// Returns all file paths present at the server's current HEAD.
    async fn list_head_files(&self) -> Result<Vec<String>, FwError>;

    /// Returns the server's current HEAD version ID.
    async fn head_version(&self) -> Result<String, FwError>;

    /// Uploads the workspace overlay to the server.
    ///
    /// Returns upload statistics (added/modified/deleted counts and mode).
    async fn upload_workspace(
        &self,
        ws_id: &str,
        repo_root: &Path,
        overlay_dir: &Path,
        base_version: &str,
        deleted_paths: &[String],
    ) -> Result<SnapshotInfo, FwError>;

    /// Triggers a server-side merge for the workspace.
    async fn submit_workspace(&self, ws_id: &str) -> Result<Applied, FwError>;

    /// Downloads the given file paths from the server's current HEAD.
    async fn download_files(&self, paths: &[String]) -> Result<Vec<(String, Vec<u8>)>, FwError>;

    /// Creates a new workspace on the server and returns the server-assigned ID.
    ///
    /// Used by [`FileWorkspace::open`] with [`Intent::CreateFor`] in remote mode so
    /// that the client and server share the same workspace UUID.
    async fn create_workspace(&self, intent: &str) -> Result<String, FwError>;
}

// ── HttpRemoteRepo ────────────────────────────────────────────────────────────

/// Production [`RemoteRepo`] implementation that talks to a vai server over HTTP.
///
/// Wraps the HTTP functions in `remote_workspace` and the file-download endpoint
/// (`GET /api/repos/:repo/files/*path`).
pub struct HttpRemoteRepo {
    remote: crate::clone::RemoteConfig,
}

impl HttpRemoteRepo {
    /// Creates a new `HttpRemoteRepo` wrapped in an `Arc` for use with
    /// [`Backend::Remote`].
    pub fn new(remote: crate::clone::RemoteConfig) -> Arc<Self> {
        Arc::new(Self { remote })
    }
}

#[async_trait]
impl RemoteRepo for HttpRemoteRepo {
    async fn list_head_files(&self) -> Result<Vec<String>, FwError> {
        let resp = self.files_list_response().await?;
        Ok(resp.files)
    }

    async fn head_version(&self) -> Result<String, FwError> {
        let resp = self.files_list_response().await?;
        Ok(resp.head_version)
    }

    async fn upload_workspace(
        &self,
        ws_id: &str,
        repo_root: &Path,
        overlay_dir: &Path,
        base_version: &str,
        deleted_paths: &[String],
    ) -> Result<SnapshotInfo, FwError> {
        let result = crate::remote_workspace::upload_snapshot(
            &self.remote,
            ws_id,
            repo_root,
            overlay_dir,
            base_version,
            deleted_paths,
        )
        .await
        .map_err(|e| FwError::Remote(e.to_string()))?;

        Ok(SnapshotInfo {
            added: result.added,
            modified: result.modified,
            deleted: result.deleted,
            is_delta: result.is_delta,
        })
    }

    async fn submit_workspace(&self, ws_id: &str) -> Result<Applied, FwError> {
        let result = crate::remote_workspace::submit_workspace(&self.remote, ws_id)
            .await
            .map_err(|e| match e {
                crate::remote_workspace::RemoteWorkspaceError::MergeConflict(ref body)
                    if body.contains("workspace_empty") =>
                {
                    FwError::WorkspaceEmpty
                }
                crate::remote_workspace::RemoteWorkspaceError::MergeConflict(body) => {
                    FwError::Remote(format!("merge conflict on server: {body}"))
                }
                other => FwError::Remote(other.to_string()),
            })?;

        Ok(Applied {
            version: result.version,
            files: result.files_applied,
            entities: result.entities_changed,
            entity_ids: vec![],
            intent: String::new(),
            snapshot: None,
        })
    }

    async fn create_workspace(&self, intent: &str) -> Result<String, FwError> {
        let meta = crate::remote_workspace::register_workspace(&self.remote, intent)
            .await
            .map_err(|e| FwError::Remote(e.to_string()))?;
        Ok(meta.id)
    }

    async fn download_files(&self, paths: &[String]) -> Result<Vec<(String, Vec<u8>)>, FwError> {
        let client = reqwest::Client::new();
        let mut out = Vec::with_capacity(paths.len());

        for path in paths {
            let encoded = path.replace('/', "%2F");
            let url = format!(
                "{}/api/repos/{}/files/{}",
                self.remote.server_url, self.remote.repo_name, encoded
            );
            let resp = client
                .get(&url)
                .header(
                    "Authorization",
                    format!("Bearer {}", self.remote.api_key),
                )
                .send()
                .await
                .map_err(|e| FwError::Remote(e.to_string()))?;

            if !resp.status().is_success() {
                let status = resp.status().as_u16();
                let body = resp.text().await.unwrap_or_default();
                return Err(FwError::Remote(format!("download {path}: {status} {body}")));
            }

            let bytes = resp
                .bytes()
                .await
                .map_err(|e| FwError::Remote(e.to_string()))?;
            out.push((path.clone(), bytes.to_vec()));
        }

        Ok(out)
    }
}

impl HttpRemoteRepo {
    async fn files_list_response(&self) -> Result<FilesListResponse, FwError> {
        let client = reqwest::Client::new();
        let url = format!(
            "{}/api/repos/{}/files",
            self.remote.server_url, self.remote.repo_name
        );
        let resp = client
            .get(&url)
            .header(
                "Authorization",
                format!("Bearer {}", self.remote.api_key),
            )
            .send()
            .await
            .map_err(|e| FwError::Remote(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(FwError::Remote(format!("list files: {status} {body}")));
        }

        resp.json::<FilesListResponse>()
            .await
            .map_err(|e| FwError::Remote(e.to_string()))
    }
}

/// Subset of the server's `GET /api/repos/:repo/files` response shape.
#[derive(Deserialize)]
struct FilesListResponse {
    files: Vec<String>,
    head_version: String,
}

// ── Errors ────────────────────────────────────────────────────────────────────

/// Errors from [`FileWorkspace`] operations.
#[derive(Debug, Error)]
pub enum FwError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("workspace error: {0}")]
    Workspace(#[from] crate::workspace::WorkspaceError),

    #[error("merge error: {0}")]
    Merge(#[from] crate::merge::MergeError),

    #[error("diff error: {0}")]
    Diff(#[from] crate::diff::DiffError),

    #[error("repo error: {0}")]
    Repo(#[from] crate::repo::RepoError),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("issue error: {0}")]
    Issue(#[from] crate::issue::IssueError),

    #[error("event log error: {0}")]
    EventLog(#[from] crate::event_log::EventLogError),

    #[error("not inside a vai repository")]
    NotARepo,

    #[error("no active workspace")]
    NoActiveWorkspace,

    #[error("workspace has no changes to submit")]
    Empty,

    /// Server rejected the submit because the uploaded snapshot contains no
    /// file changes relative to the current server HEAD.
    ///
    /// Distinct from [`Empty`]: `Empty` is a client-side check (no files in
    /// the overlay), while `WorkspaceEmpty` is a server-side verdict (uploaded
    /// content is identical to HEAD after diffing).
    #[error("workspace has no changes relative to server HEAD")]
    WorkspaceEmpty,

    /// Submit was refused because the plan contains surprises.
    ///
    /// Inspect [`Plan::surprises`] and either abort or call
    /// [`FileWorkspace::submit_forcing_deletions`].
    #[error("submit refused: plan contains surprises — inspect Plan::surprises and re-submit with force if intentional")]
    Surprises(Box<Plan>),

    #[error("remote error: {0}")]
    Remote(String),
}

// ── FileWorkspace ─────────────────────────────────────────────────────────────

/// Unified workspace for file I/O operations.
///
/// Created via [`FileWorkspace::open`]; used via [`plan`], [`submit`], [`pull`].
pub struct FileWorkspace {
    repo_root: PathBuf,
    /// Directory used for file listing and upload in push-from-root mode.
    ///
    /// Equals `repo_root` in the standard push path; equals the agent's
    /// `work_dir` when opened with [`Intent::AgentWork`].
    upload_root: PathBuf,
    vai_dir: PathBuf,
    ws_meta: workspace::WorkspaceMeta,
    merge_fs: Box<dyn MergeFs>,
    /// Consolidated deletions from `.vai-deleted` and `workspace.deleted_paths`.
    deleted_paths: Vec<String>,
    backend: BackendKind,
    /// When `true`, [`plan`] and [`submit`] operate on `upload_root` directly
    /// (full-working-tree push) rather than a workspace overlay subdirectory.
    ///
    /// Set by [`open`] when [`Intent::CreateFor`] or [`Intent::AgentWork`] is
    /// combined with [`Backend::Remote`].
    is_push_from_root: bool,
}

enum BackendKind {
    Local,
    Remote(Arc<dyn RemoteRepo>),
}

impl FileWorkspace {
    // ── Constructor ───────────────────────────────────────────────────────────

    /// Opens or creates a workspace according to `opts`.
    ///
    /// In [`Backend::Local`] mode all state is read from and written to the
    /// `.vai/` directory under `opts.repo_root`.
    ///
    /// In [`Backend::Remote`] mode with [`Intent::Existing`] the active local
    /// workspace metadata is loaded; workspace I/O goes through the injected
    /// [`RemoteRepo`].
    pub async fn open(opts: OpenOptions) -> Result<Self, FwError> {
        let vai_dir = opts.repo_root.join(".vai");
        if !vai_dir.exists() {
            return Err(FwError::NotARepo);
        }

        let head = repo::read_head(&vai_dir)?;

        // Each arm resolves to (WorkspaceMeta, BackendKind, is_push_from_root, upload_root_override).
        // `upload_root_override` is Some when the files to list/upload differ from `repo_root`
        // (currently only for Intent::AgentWork).
        let (ws_meta, backend_kind, is_push_from_root, upload_root_override): (_, _, _, Option<PathBuf>) =
            match (opts.backend, opts.intent) {
            (Backend::Local, Intent::Existing) => {
                let meta = workspace::active(&vai_dir)?;
                (meta, BackendKind::Local, false, None)
            }
            (Backend::Local, Intent::CreateFor { text, issue_id }) => {
                let mut result = workspace::create(&vai_dir, &text, &head)?;
                if let Some(iid) = issue_id {
                    result.workspace.issue_id = Some(iid);
                    workspace::update_meta(&vai_dir, &result.workspace)?;
                }
                (result.workspace, BackendKind::Local, false, None)
            }
            (Backend::Local, Intent::AgentWork { .. }) => {
                return Err(FwError::Remote(
                    "Intent::AgentWork requires Backend::Remote".to_string(),
                ));
            }
            (Backend::Remote(remote), Intent::Existing) => {
                let meta = workspace::active(&vai_dir)?;
                (meta, BackendKind::Remote(remote), false, None)
            }
            (Backend::Remote(remote), Intent::CreateFor { text, issue_id }) => {
                // Register a workspace on the server to obtain the canonical ID,
                // then keep an in-memory WorkspaceMeta so both sides share the
                // same UUID.  We do NOT write to disk: the push workspace is
                // ephemeral and should leave no artifacts on failure.
                let ws_id_str = remote.create_workspace(&text).await?;
                let ws_uuid = ws_id_str
                    .parse::<uuid::Uuid>()
                    .map_err(|e| FwError::Remote(format!("invalid workspace ID from server: {e}")))?;
                let now = Utc::now();
                let meta = workspace::WorkspaceMeta {
                    id: ws_uuid,
                    intent: text,
                    status: workspace::WorkspaceStatus::Created,
                    base_version: head,
                    issue_id,
                    created_at: now,
                    updated_at: now,
                    deleted_paths: Vec::new(),
                };
                (meta, BackendKind::Remote(remote), true, None)
            }
            (Backend::Remote(remote), Intent::AgentWork { workspace_id, issue_id, intent, work_dir }) => {
                // The workspace already exists on the server (created by `vai agent claim`).
                // Build an in-memory WorkspaceMeta using the IDs from agent-state.json.
                // The full `work_dir` is uploaded as a snapshot; we do not track an overlay.
                let now = Utc::now();
                let meta = workspace::WorkspaceMeta {
                    id: workspace_id,
                    intent,
                    status: workspace::WorkspaceStatus::Active,
                    base_version: head,
                    issue_id,
                    created_at: now,
                    updated_at: now,
                    deleted_paths: Vec::new(),
                };
                (meta, BackendKind::Remote(remote), true, Some(work_dir))
            }
        };

        let upload_root = upload_root_override.unwrap_or_else(|| opts.repo_root.clone());

        let deleted_paths = if is_push_from_root {
            Vec::new()
        } else {
            load_deleted_paths(&vai_dir, &ws_meta)?
        };
        // DiskMergeFs uses `upload_root` for "base/" key mapping so that
        // `plan_repo_root_push` lists files from the correct directory.
        let merge_fs: Box<dyn MergeFs> = Box::new(DiskMergeFs::new(
            &vai_dir,
            &ws_meta.id.to_string(),
            &upload_root,
        ));

        Ok(FileWorkspace {
            repo_root: opts.repo_root,
            upload_root,
            vai_dir,
            ws_meta,
            merge_fs,
            deleted_paths,
            backend: backend_kind,
            is_push_from_root,
        })
    }

    // ── Primary verbs ─────────────────────────────────────────────────────────

    /// Returns a read-only plan of what [`submit`] would do.
    ///
    /// In [`Backend::Remote`] mode, also queries the server manifest to detect
    /// [`Surprise::ServerHasFileLocalDoesNot`].
    ///
    /// When [`is_push_from_root`] is set (i.e. [`Intent::CreateFor`] + remote),
    /// delegates to [`plan_repo_root_push`] which compares the repo root against
    /// the server manifest rather than the workspace overlay.
    pub async fn plan(&self) -> Result<Plan, FwError> {
        if self.is_push_from_root {
            return self.plan_repo_root_push().await;
        }
        let workspace_diff = diff::compute_with_fs(
            self.merge_fs.as_ref(),
            self.ws_meta.id,
            self.ws_meta.base_version.clone(),
            self.deleted_paths.clone(),
        )?;

        let mut adds = Vec::new();
        let mut mods = Vec::new();
        for fd in &workspace_diff.file_diffs {
            match fd.change_type {
                FileChangeType::Added => adds.push(PathBuf::from(&fd.path)),
                FileChangeType::Modified => mods.push(PathBuf::from(&fd.path)),
                FileChangeType::Deleted => {} // captured separately in dels
            }
        }
        let dels: Vec<PathBuf> = self
            .deleted_paths
            .iter()
            .map(PathBuf::from)
            .collect();

        let mut surprises = Vec::new();

        // Detect base drift: HEAD has advanced past the workspace's base version.
        //
        // In remote mode compare against the server's authoritative HEAD so we
        // catch concurrent submits from other clients.  In local mode the local
        // `.vai/head` file is the source of truth.
        let head_version = match &self.backend {
            BackendKind::Remote(remote) => remote.head_version().await?,
            BackendKind::Local => repo::read_head(&self.vai_dir)?,
        };

        if head_version != self.ws_meta.base_version {
            surprises.push(Surprise::BaseDrifted {
                expected: self.ws_meta.base_version.clone(),
                actual: head_version.clone(),
            });
        }

        // Remote-only: detect server-only files the client hasn't pulled.
        if let BackendKind::Remote(remote) = &self.backend {
            let server_files: HashSet<String> =
                remote.list_head_files().await?.into_iter().collect();

            let deleted_set: HashSet<&str> =
                self.deleted_paths.iter().map(|s| s.as_str()).collect();

            // All files the local client knows about: base + overlay - deleted.
            let local_base: HashSet<String> = self
                .merge_fs
                .list_files("base/")?
                .into_iter()
                .map(|k| k.strip_prefix("base/").unwrap_or(&k).to_string())
                .collect();
            let local_overlay: HashSet<String> = self
                .merge_fs
                .list_files("overlay/")?
                .into_iter()
                .map(|k| k.strip_prefix("overlay/").unwrap_or(&k).to_string())
                .collect();

            let all_local: HashSet<String> = local_base
                .union(&local_overlay)
                .filter(|p| !deleted_set.contains(p.as_str()))
                .cloned()
                .collect();

            for path in &server_files {
                if !all_local.contains(path) {
                    surprises.push(Surprise::ServerHasFileLocalDoesNot(PathBuf::from(path)));
                }
            }
        }

        // Record workspace activity events (side effect: transition Created → Active).
        // This is idempotent — record_events is a no-op if the workspace is
        // already Active.  Only applicable in local mode; remote mode handles
        // activation via the server's upload_snapshot endpoint.
        if matches!(self.backend, BackendKind::Local) {
            diff::record_events(&self.vai_dir, &workspace_diff).map_err(FwError::Diff)?;
        }

        Ok(Plan {
            adds,
            mods,
            dels,
            file_diffs: workspace_diff.file_diffs,
            entity_changes: workspace_diff.entity_changes,
            surprises,
            base_version: self.ws_meta.base_version.clone(),
            head_version,
        })
    }

    /// Submits the workspace atomically.
    ///
    /// Returns [`Err(FwError::Surprises(plan))`](FwError::Surprises) if the plan
    /// contains any surprises. Use [`FileWorkspace::submit_forcing_deletions`]
    /// to proceed despite surprises.
    pub async fn submit(&mut self, s: Submit) -> Result<Applied, FwError> {
        let plan = self.plan().await?;
        if !plan.surprises.is_empty() {
            return Err(FwError::Surprises(Box::new(plan)));
        }
        self.do_submit(s, &plan).await
    }

    /// Pulls server changes into the local base directory.
    ///
    /// In local-only mode this is a no-op. In remote mode it downloads files
    /// that exist on the server but are absent locally.
    pub async fn pull(&mut self) -> Result<Applied, FwError> {
        match &self.backend {
            BackendKind::Local => Ok(Applied {
                version: self.ws_meta.base_version.clone(),
                files: 0,
                entities: 0,
                entity_ids: vec![],
                intent: String::new(),
                snapshot: None,
            }),
            BackendKind::Remote(_) => self.do_pull(false).await,
        }
    }

    // ── Escape hatches ────────────────────────────────────────────────────────

    /// Like [`submit`] but proceeds even when the plan contains
    /// [`Surprise::ServerHasFileLocalDoesNot`] entries.
    ///
    /// The caller has acknowledged that the missing server files should be
    /// deleted.
    pub async fn submit_forcing_deletions(&mut self, s: Submit) -> Result<Applied, FwError> {
        let plan = self.plan().await?;
        self.do_submit(s, &plan).await
    }

    /// Like [`pull`] but overwrites local overlay changes with server content.
    pub async fn pull_discarding_local(&mut self) -> Result<Applied, FwError> {
        match &self.backend {
            BackendKind::Local => Ok(Applied {
                version: self.ws_meta.base_version.clone(),
                files: 0,
                entities: 0,
                entity_ids: vec![],
                intent: String::new(),
                snapshot: None,
            }),
            BackendKind::Remote(_) => self.do_pull(true).await,
        }
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    /// Plan variant for full-working-tree pushes ([`is_push_from_root`]).
    ///
    /// Compares the local repo root against the server manifest to compute
    /// adds/mods and detects surprises (server-only files, base drift).
    /// No workspace overlay is consulted because the overlay is empty for this
    /// push mode — the entire repo root is the intended upload.
    async fn plan_repo_root_push(&self) -> Result<Plan, FwError> {
        let remote = match &self.backend {
            BackendKind::Remote(r) => Arc::clone(r),
            BackendKind::Local => unreachable!("plan_repo_root_push called in local mode"),
        };

        let server_head = remote.head_version().await?;
        let server_files: HashSet<String> = remote.list_head_files().await?.into_iter().collect();

        // Local files come from the repo root (DiskMergeFs maps "base/" → repo_root).
        let local_files: HashSet<String> = self
            .merge_fs
            .list_files("base/")?
            .into_iter()
            .map(|k| k.strip_prefix("base/").unwrap_or(&k).to_string())
            .collect();

        // Files only locally → will be added on the server.
        let mut adds: Vec<PathBuf> = local_files
            .iter()
            .filter(|p| !server_files.contains(*p))
            .map(PathBuf::from)
            .collect();
        adds.sort();

        // Files in both → conservatively treated as modified (no hash check here).
        let mut mods: Vec<PathBuf> = local_files
            .iter()
            .filter(|p| server_files.contains(*p))
            .map(PathBuf::from)
            .collect();
        mods.sort();

        let mut surprises = Vec::new();

        // Base drift: HEAD advanced since we snapshotted base_version.
        if server_head != self.ws_meta.base_version {
            surprises.push(Surprise::BaseDrifted {
                expected: self.ws_meta.base_version.clone(),
                actual: server_head.clone(),
            });
        }

        // Server-only files would be implicitly deleted by the push.
        let mut server_only: Vec<PathBuf> = server_files
            .iter()
            .filter(|p| !local_files.contains(*p))
            .map(PathBuf::from)
            .collect();
        server_only.sort();
        for path in server_only {
            surprises.push(Surprise::ServerHasFileLocalDoesNot(path));
        }

        Ok(Plan {
            adds,
            mods,
            dels: Vec::new(), // no explicit workspace deletions in push-from-root mode
            file_diffs: Vec::new(),
            entity_changes: Vec::new(),
            surprises,
            base_version: self.ws_meta.base_version.clone(),
            head_version: server_head,
        })
    }

    async fn do_submit(&mut self, s: Submit, plan: &Plan) -> Result<Applied, FwError> {
        if plan.is_empty() {
            if s.is_close_if_empty() {
                return Err(FwError::Empty);
            }
            return Err(FwError::Empty);
        }

        let applied = match &self.backend {
            BackendKind::Local => {
                let result = merge::submit(&self.vai_dir, &self.repo_root)?;

                // Best-effort scope history recording.
                let history_path = self.vai_dir.join("graph").join("history.db");
                if let Ok(hist) = crate::scope_history::ScopeHistoryStore::open(&history_path) {
                    let terms = crate::scope_inference::extract_terms(&result.version.intent);
                    let _ = hist.record(
                        &result.version.intent,
                        &terms,
                        &[],
                        &result.entity_ids,
                        Some(&self.ws_meta.id.to_string()),
                    );
                }

                Applied {
                    version: result.version.version_id.clone(),
                    files: result.files_applied,
                    entities: result.entities_changed,
                    entity_ids: result.entity_ids,
                    intent: result.version.intent,
                    snapshot: None,
                }
            }
            BackendKind::Remote(remote) => {
                let remote = Arc::clone(remote);
                let ws_id = self.ws_meta.id.to_string();

                // For push-from-root (vai push, vai agent submit), upload `upload_root`
                // as a full snapshot; the server derives the overlay by diffing vs HEAD.
                // For workspace-overlay submits, upload only the overlay dir.
                let (upload_dir, deleted_paths_ref): (std::borrow::Cow<'_, Path>, &[String]) =
                    if self.is_push_from_root {
                        (std::borrow::Cow::Borrowed(self.upload_root.as_path()), &[])
                    } else {
                        let overlay = workspace::overlay_dir(&self.vai_dir, &ws_id);
                        (std::borrow::Cow::Owned(overlay), self.deleted_paths.as_slice())
                    };

                let snapshot = remote
                    .upload_workspace(
                        &ws_id,
                        &self.upload_root,
                        &upload_dir,
                        &self.ws_meta.base_version,
                        deleted_paths_ref,
                    )
                    .await?;

                let mut submitted = remote.submit_workspace(&ws_id).await?;
                submitted.snapshot = Some(snapshot);

                // Update local HEAD.
                fs::write(
                    self.vai_dir.join("head"),
                    format!("{}\n", submitted.version),
                )?;

                // For workspace-overlay submits, persist the updated status.
                // For push-from-root, the workspace is ephemeral (no disk state).
                if !self.is_push_from_root {
                    self.ws_meta.status = workspace::WorkspaceStatus::Submitted;
                    self.ws_meta.updated_at = Utc::now();
                    workspace::update_meta(&self.vai_dir, &self.ws_meta)?;
                }

                let _ = s.message(); // message is carried by the workspace intent on the server
                submitted
            }
        };

        // Issue resolution — best-effort in both modes.
        if let Some(issue_id) = self.ws_meta.issue_id {
            if let Ok(store) = crate::issue::IssueStore::open(&self.vai_dir) {
                if let Ok(mut event_log) =
                    crate::event_log::EventLog::open(&self.vai_dir.join("event_log"))
                {
                    let _ = store.resolve(issue_id, Some(applied.version.clone()), &mut event_log);
                }
            }
        }

        Ok(applied)
    }

    async fn do_pull(&mut self, _force: bool) -> Result<Applied, FwError> {
        let remote = match &self.backend {
            BackendKind::Remote(r) => Arc::clone(r),
            BackendKind::Local => unreachable!("do_pull called in local mode"),
        };

        let server_head = remote.head_version().await?;
        let server_files = remote.list_head_files().await?;

        // Build the set of files the local client already has.
        let local_base: HashSet<String> = self
            .merge_fs
            .list_files("base/")?
            .into_iter()
            .map(|k| k.strip_prefix("base/").unwrap_or(&k).to_string())
            .collect();
        let local_overlay: HashSet<String> = self
            .merge_fs
            .list_files("overlay/")?
            .into_iter()
            .map(|k| k.strip_prefix("overlay/").unwrap_or(&k).to_string())
            .collect();
        let all_local: HashSet<&str> = local_base
            .iter()
            .chain(local_overlay.iter())
            .map(|s| s.as_str())
            .collect();

        let missing: Vec<String> = server_files
            .iter()
            .filter(|p| !all_local.contains(p.as_str()))
            .cloned()
            .collect();

        if missing.is_empty() {
            return Ok(Applied {
                version: server_head,
                files: 0,
                entities: 0,
                entity_ids: vec![],
                intent: String::new(),
                snapshot: None,
            });
        }

        let downloaded = remote.download_files(&missing).await?;
        let files_written = downloaded.len();

        for (path, content) in &downloaded {
            self.merge_fs.write_file(&format!("base/{}", path), content)?;
        }

        // Update workspace to reflect new HEAD.
        self.ws_meta.base_version = server_head.clone();
        self.ws_meta.updated_at = Utc::now();
        workspace::update_meta(&self.vai_dir, &self.ws_meta)?;

        fs::write(
            self.vai_dir.join("head"),
            format!("{}\n", server_head),
        )?;

        Ok(Applied {
            version: server_head,
            files: files_written,
            entities: 0,
            entity_ids: vec![],
            intent: String::new(),
            snapshot: None,
        })
    }
}

// ── Private helpers ───────────────────────────────────────────────────────────

/// Loads and deduplicates deletion paths from both on-disk sources:
///
/// - `.vai/workspaces/<id>/.vai-deleted` — JSON manifest used by diff/merge
/// - `workspace.deleted_paths` — field in `meta.toml` used by the remote path
fn load_deleted_paths(
    vai_dir: &Path,
    ws_meta: &workspace::WorkspaceMeta,
) -> Result<Vec<String>, FwError> {
    let manifest_path = vai_dir
        .join("workspaces")
        .join(ws_meta.id.to_string())
        .join(".vai-deleted");

    let from_manifest: Vec<String> = if manifest_path.exists() {
        let bytes = fs::read(&manifest_path)?;
        serde_json::from_slice(&bytes).unwrap_or_default()
    } else {
        Vec::new()
    };

    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for path in from_manifest
        .into_iter()
        .chain(ws_meta.deleted_paths.iter().cloned())
    {
        if seen.insert(path.clone()) {
            result.push(path);
        }
    }
    Ok(result)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    // ── InMemoryRemoteRepo ────────────────────────────────────────────────────

    /// Test double simulating a remote server with a fixed HEAD and file set.
    struct InMemoryRemoteRepo {
        head: Mutex<String>,
        files: Mutex<HashMap<String, Vec<u8>>>,
    }

    impl InMemoryRemoteRepo {
        fn new(version: &str, files: impl IntoIterator<Item = (&'static str, Vec<u8>)>) -> Arc<Self> {
            Arc::new(Self {
                head: Mutex::new(version.to_string()),
                files: Mutex::new(files.into_iter().map(|(k, v)| (k.to_string(), v)).collect()),
            })
        }

    }

    #[async_trait]
    impl RemoteRepo for InMemoryRemoteRepo {
        async fn list_head_files(&self) -> Result<Vec<String>, FwError> {
            Ok(self.files.lock().unwrap().keys().cloned().collect())
        }

        async fn head_version(&self) -> Result<String, FwError> {
            Ok(self.head.lock().unwrap().clone())
        }

        async fn upload_workspace(
            &self,
            _ws_id: &str,
            _repo_root: &Path,
            overlay_dir: &Path,
            _base_version: &str,
            deleted_paths: &[String],
        ) -> Result<SnapshotInfo, FwError> {
            let mut files = self.files.lock().unwrap();
            let mut deleted = 0usize;
            for path in deleted_paths {
                if files.remove(path).is_some() {
                    deleted += 1;
                }
            }
            let mut added = 0usize;
            let mut modified = 0usize;
            if overlay_dir.exists() {
                for entry in walk_files(overlay_dir) {
                    let rel = entry
                        .strip_prefix(overlay_dir)
                        .unwrap()
                        .to_string_lossy()
                        .to_string();
                    let content = fs::read(&entry).unwrap_or_default();
                    if files.insert(rel, content).is_none() {
                        added += 1;
                    } else {
                        modified += 1;
                    }
                }
            }
            Ok(SnapshotInfo {
                added,
                modified,
                deleted,
                is_delta: true,
            })
        }

        async fn submit_workspace(&self, _ws_id: &str) -> Result<Applied, FwError> {
            let mut head = self.head.lock().unwrap();
            let new_ver = format!("{}-merged", *head);
            *head = new_ver.clone();
            let file_count = self.files.lock().unwrap().len();
            Ok(Applied {
                version: new_ver,
                files: file_count,
                entities: 0,
                entity_ids: vec![],
                intent: String::new(),
                snapshot: None,
            })
        }

        async fn download_files(
            &self,
            paths: &[String],
        ) -> Result<Vec<(String, Vec<u8>)>, FwError> {
            let files = self.files.lock().unwrap();
            Ok(paths
                .iter()
                .filter_map(|p| files.get(p).map(|c| (p.clone(), c.clone())))
                .collect())
        }

        async fn create_workspace(&self, _intent: &str) -> Result<String, FwError> {
            Ok(uuid::Uuid::new_v4().to_string())
        }
    }

    /// Recursively collect all file paths under `dir`.
    fn walk_files(dir: &Path) -> Vec<PathBuf> {
        let mut result = Vec::new();
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    result.push(path);
                } else if path.is_dir() {
                    result.extend(walk_files(&path));
                }
            }
        }
        result
    }

    // ── Test helpers ──────────────────────────────────────────────────────────

    /// Creates a minimal vai repo under a temp dir with the given source files.
    fn setup_local_repo(files: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().to_path_buf();

        for (rel, content) in files {
            let abs = root.join(rel);
            if let Some(p) = abs.parent() {
                fs::create_dir_all(p).unwrap();
            }
            fs::write(&abs, content).unwrap();
        }

        let vai_dir = root.join(".vai");
        fs::create_dir_all(vai_dir.join("event_log")).unwrap();
        fs::create_dir_all(vai_dir.join("workspaces")).unwrap();
        fs::write(vai_dir.join("head"), "v1\n").unwrap();
        fs::write(
            vai_dir.join("config.toml"),
            "[repo]\nname = \"test\"\nid = \"00000000-0000-0000-0000-000000000001\"\n",
        )
        .unwrap();

        (dir, root)
    }

    /// Writes files into a workspace's overlay directory.
    fn make_overlay(vai_dir: &Path, ws_id: &Uuid, files: &[(&str, &str)]) {
        let overlay = vai_dir
            .join("workspaces")
            .join(ws_id.to_string())
            .join("overlay");
        for (rel, content) in files {
            let abs = overlay.join(rel);
            if let Some(p) = abs.parent() {
                fs::create_dir_all(p).unwrap();
            }
            fs::write(&abs, content).unwrap();
        }
    }

    /// Writes a `.vai-deleted` manifest for a workspace.
    fn set_deleted_manifest(vai_dir: &Path, ws_id: &Uuid, paths: &[&str]) {
        let manifest = vai_dir
            .join("workspaces")
            .join(ws_id.to_string())
            .join(".vai-deleted");
        fs::write(&manifest, serde_json::to_string(paths).unwrap()).unwrap();
    }

    // ── #368 prevention: plan() shows deletions ───────────────────────────────

    #[tokio::test]
    async fn plan_dels_reflects_deleted_files() {
        let (_dir, root) = setup_local_repo(&[("src/to_delete.rs", "fn old() {}")]);
        let vai_dir = root.join(".vai");

        let result = workspace::create(&vai_dir, "test", "v1").unwrap();
        set_deleted_manifest(&vai_dir, &result.workspace.id, &["src/to_delete.rs"]);

        let fw = FileWorkspace::open(OpenOptions::local(root)).await.unwrap();
        let plan = fw.plan().await.unwrap();

        assert_eq!(
            plan.dels,
            vec![PathBuf::from("src/to_delete.rs")],
            "plan.dels must list deleted files (#368 prevention)"
        );
        assert!(plan.adds.is_empty());
        assert!(plan.mods.is_empty());
    }

    #[tokio::test]
    async fn plan_dels_merges_both_deletion_sources() {
        let (_dir, root) = setup_local_repo(&[
            ("src/a.rs", "fn a() {}"),
            ("src/b.rs", "fn b() {}"),
        ]);
        let vai_dir = root.join(".vai");

        let mut result = workspace::create(&vai_dir, "test", "v1").unwrap();
        // One deletion in .vai-deleted manifest, one in meta.toml.
        set_deleted_manifest(&vai_dir, &result.workspace.id, &["src/a.rs"]);
        result.workspace.deleted_paths = vec!["src/b.rs".to_string()];
        workspace::update_meta(&vai_dir, &result.workspace).unwrap();

        let fw = FileWorkspace::open(OpenOptions::local(root)).await.unwrap();
        let plan = fw.plan().await.unwrap();

        let mut dels: Vec<String> = plan
            .dels
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        dels.sort();
        assert_eq!(
            dels,
            vec!["src/a.rs", "src/b.rs"],
            "plan.dels must consolidate both deletion sources"
        );
    }

    // ── #367 prevention: submit() refuses server-only surprises ───────────────

    #[tokio::test]
    async fn submit_refuses_when_server_has_files_client_lacks() {
        let (_dir, root) = setup_local_repo(&[("src/local.rs", "fn local() {}")]);
        let vai_dir = root.join(".vai");

        let result = workspace::create(&vai_dir, "test", "v1").unwrap();
        make_overlay(
            &vai_dir,
            &result.workspace.id,
            &[("src/local.rs", "fn local() { /* v2 */ }")],
        );

        let remote = InMemoryRemoteRepo::new(
            "v1",
            [
                ("src/local.rs", b"fn local() {}".to_vec()),
                ("src/server_only.rs", b"fn server() {}".to_vec()),
            ],
        );

        let mut fw = FileWorkspace::open(OpenOptions {
            repo_root: root.clone(),
            backend: Backend::Remote(remote),
            intent: Intent::Existing,
        })
        .await
        .unwrap();

        let err = fw
            .submit(Submit::Required("test".to_string()))
            .await
            .unwrap_err();

        match err {
            FwError::Surprises(plan) => {
                let server_only: Vec<_> = plan
                    .surprises
                    .iter()
                    .filter_map(|s| {
                        if let Surprise::ServerHasFileLocalDoesNot(p) = s {
                            Some(p.clone())
                        } else {
                            None
                        }
                    })
                    .collect();
                assert_eq!(
                    server_only,
                    vec![PathBuf::from("src/server_only.rs")],
                    "submit must refuse when server has files the client lacks (#367)"
                );
            }
            other => panic!("expected FwError::Surprises, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn submit_forcing_deletions_proceeds_despite_surprises() {
        let (_dir, root) = setup_local_repo(&[("src/local.rs", "fn local() {}")]);
        let vai_dir = root.join(".vai");

        let result = workspace::create(&vai_dir, "test", "v1").unwrap();
        make_overlay(
            &vai_dir,
            &result.workspace.id,
            &[("src/local.rs", "fn local() { /* v2 */ }")],
        );

        let remote = InMemoryRemoteRepo::new(
            "v1",
            [
                ("src/local.rs", b"fn local() {}".to_vec()),
                ("src/server_only.rs", b"fn server_only() {}".to_vec()),
            ],
        );

        let mut fw = FileWorkspace::open(OpenOptions {
            repo_root: root.clone(),
            backend: Backend::Remote(remote),
            intent: Intent::Existing,
        })
        .await
        .unwrap();

        let applied = fw
            .submit_forcing_deletions(Submit::Required("force".to_string()))
            .await
            .unwrap();

        assert!(!applied.version.is_empty());
    }

    // ── #369 prevention: pull() downloads missing server files ────────────────

    #[tokio::test]
    async fn pull_downloads_server_only_files() {
        let (_dir, root) = setup_local_repo(&[]);
        let vai_dir = root.join(".vai");
        workspace::create(&vai_dir, "test", "v1").unwrap();

        let remote = InMemoryRemoteRepo::new(
            "v2",
            [
                ("src/server.rs", b"fn server() {}".to_vec()),
                ("src/other.rs", b"fn other() {}".to_vec()),
            ],
        );

        let remote_dyn: Arc<dyn RemoteRepo> = remote;
        let mut fw = FileWorkspace::open(OpenOptions {
            repo_root: root.clone(),
            backend: Backend::Remote(Arc::clone(&remote_dyn)),
            intent: Intent::Existing,
        })
        .await
        .unwrap();

        let applied = fw.pull().await.unwrap();

        assert_eq!(
            applied.files, 2,
            "pull must download files that exist on server but not locally (#369)"
        );
        assert_eq!(applied.version, "v2");

        // After pull, plan should show no server-only surprises.
        let plan = fw.plan().await.unwrap();
        let server_only_count = plan
            .surprises
            .iter()
            .filter(|s| matches!(s, Surprise::ServerHasFileLocalDoesNot(_)))
            .count();
        assert_eq!(
            server_only_count, 0,
            "plan after pull must have no ServerHasFileLocalDoesNot surprises (#369)"
        );
    }

    // ── Empty workspace ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn plan_is_empty_for_empty_workspace() {
        let (_dir, root) = setup_local_repo(&[]);
        let vai_dir = root.join(".vai");
        workspace::create(&vai_dir, "test", "v1").unwrap();

        let fw = FileWorkspace::open(OpenOptions::local(root)).await.unwrap();
        let plan = fw.plan().await.unwrap();
        assert!(plan.is_empty());
    }

    #[tokio::test]
    async fn submit_required_fails_on_empty_workspace_local() {
        let (_dir, root) = setup_local_repo(&[]);
        let vai_dir = root.join(".vai");
        workspace::create(&vai_dir, "test", "v1").unwrap();

        let mut fw = FileWorkspace::open(OpenOptions::local(root)).await.unwrap();

        let err = fw
            .submit(Submit::Required("empty".to_string()))
            .await
            .unwrap_err();

        assert!(
            matches!(err, FwError::Empty)
                || matches!(err, FwError::Merge(crate::merge::MergeError::EmptyWorkspace)),
            "submit on empty workspace must return Empty or Merge(EmptyWorkspace), got: {err:?}"
        );
    }

    // ── FailingSubmitRemoteRepo ───────────────────────────────────────────────

    /// Test double that always fails on `submit_workspace` after a successful
    /// `upload_workspace`.  Used to verify that a mid-submit server error leaves
    /// the local workspace state unchanged (RFC #371 test 4).
    struct FailingSubmitRemoteRepo {
        inner: Arc<InMemoryRemoteRepo>,
    }

    impl FailingSubmitRemoteRepo {
        fn new(
            version: &str,
            files: impl IntoIterator<Item = (&'static str, Vec<u8>)>,
        ) -> Arc<Self> {
            Arc::new(Self {
                inner: InMemoryRemoteRepo::new(version, files),
            })
        }
    }

    #[async_trait]
    impl RemoteRepo for FailingSubmitRemoteRepo {
        async fn list_head_files(&self) -> Result<Vec<String>, FwError> {
            self.inner.list_head_files().await
        }

        async fn head_version(&self) -> Result<String, FwError> {
            self.inner.head_version().await
        }

        async fn upload_workspace(
            &self,
            ws_id: &str,
            repo_root: &Path,
            overlay_dir: &Path,
            base_version: &str,
            deleted_paths: &[String],
        ) -> Result<SnapshotInfo, FwError> {
            self.inner
                .upload_workspace(ws_id, repo_root, overlay_dir, base_version, deleted_paths)
                .await
        }

        async fn submit_workspace(&self, _ws_id: &str) -> Result<Applied, FwError> {
            Err(FwError::Remote("simulated server error during submit".to_string()))
        }

        async fn download_files(
            &self,
            paths: &[String],
        ) -> Result<Vec<(String, Vec<u8>)>, FwError> {
            self.inner.download_files(paths).await
        }

        async fn create_workspace(&self, intent: &str) -> Result<String, FwError> {
            self.inner.create_workspace(intent).await
        }
    }

    // ── Test 4: atomic submit — failure leaves state unchanged ────────────────

    /// If `submit_workspace` fails on the server after `upload_workspace` succeeds,
    /// the local workspace status and HEAD must not advance (RFC #371 test 4).
    #[tokio::test]
    async fn submit_remote_failure_leaves_workspace_unchanged() {
        let (_dir, root) = setup_local_repo(&[("src/file.rs", "fn main() {}")]);
        let vai_dir = root.join(".vai");

        let result = workspace::create(&vai_dir, "test", "v1").unwrap();
        make_overlay(
            &vai_dir,
            &result.workspace.id,
            &[("src/file.rs", "fn main() { /* v2 */ }")],
        );

        let remote = FailingSubmitRemoteRepo::new(
            "v1",
            [("src/file.rs", b"fn main() {}".to_vec())],
        );

        let mut fw = FileWorkspace::open(OpenOptions {
            repo_root: root.clone(),
            backend: Backend::Remote(remote),
            intent: Intent::Existing,
        })
        .await
        .unwrap();

        let err = fw
            .submit(Submit::Required("test".to_string()))
            .await
            .unwrap_err();

        assert!(
            !matches!(err, FwError::Surprises(_)),
            "expected a server error, not a surprises error"
        );

        // Workspace status must remain unchanged (not Submitted).
        let meta = workspace::active(&vai_dir).unwrap();
        assert_ne!(
            meta.status,
            workspace::WorkspaceStatus::Submitted,
            "workspace status must not advance after failed submit (RFC #371 test 4)"
        );

        // Local HEAD must not advance.
        let head = std::fs::read_to_string(vai_dir.join("head")).unwrap();
        assert_eq!(
            head.trim(),
            "v1",
            "HEAD must not advance after failed submit (RFC #371 test 4)"
        );
    }

    // ── Test 7: issue resolution coupling ────────────────────────────────────

    /// A non-empty workspace submit in local mode must resolve the linked issue
    /// (RFC #371 test 7).
    #[tokio::test]
    async fn submit_resolves_linked_issue_in_local_mode() {
        let (_dir, root) = setup_local_repo(&[("src/file.rs", "fn func() {}")]);
        let vai_dir = root.join(".vai");

        // Create an issue.
        let issue_store = crate::issue::IssueStore::open(&vai_dir).unwrap();
        let mut event_log =
            crate::event_log::EventLog::open(&vai_dir.join("event_log")).unwrap();
        let issue = issue_store
            .create(
                "Fix the bug",
                "",
                crate::issue::IssuePriority::Medium,
                vec![],
                "test",
                &mut event_log,
            )
            .unwrap();

        // Create a workspace and link it to the issue.
        let mut result = workspace::create(&vai_dir, "fix the bug", "v1").unwrap();
        result.workspace.issue_id = Some(issue.id);
        workspace::update_meta(&vai_dir, &result.workspace).unwrap();

        // Add changed files to the overlay.
        make_overlay(
            &vai_dir,
            &result.workspace.id,
            &[("src/file.rs", "fn func() { /* fixed */ }")],
        );

        // Submit.
        let mut fw = FileWorkspace::open(OpenOptions::local(root.clone()))
            .await
            .unwrap();
        fw.submit(Submit::Required("fix the bug".to_string()))
            .await
            .unwrap();

        // Issue must be resolved.
        let updated = issue_store.get(issue.id).unwrap();
        assert_eq!(
            updated.status,
            crate::issue::IssueStatus::Resolved,
            "submit must resolve the linked issue (RFC #371 test 7)"
        );
    }

    // ── RacingRemoteRepo ──────────────────────────────────────────────────────

    /// Test double that starts at `initial_version` but returns `racing_version`
    /// from `head_version()` on its first call.  This simulates a concurrent
    /// submit on the server advancing HEAD between the time the workspace was
    /// created (base locked in to `initial_version`) and when `plan()` checks.
    struct RacingRemoteRepo {
        inner: Arc<InMemoryRemoteRepo>,
        racing_version: String,
    }

    impl RacingRemoteRepo {
        fn new(
            initial_version: &str,
            racing_version: &str,
            files: impl IntoIterator<Item = (&'static str, Vec<u8>)>,
        ) -> Arc<Self> {
            Arc::new(Self {
                inner: InMemoryRemoteRepo::new(initial_version, files),
                racing_version: racing_version.to_string(),
            })
        }
    }

    #[async_trait]
    impl RemoteRepo for RacingRemoteRepo {
        /// Always returns the racing (advanced) version so BaseDrift is detected.
        async fn head_version(&self) -> Result<String, FwError> {
            Ok(self.racing_version.clone())
        }

        async fn list_head_files(&self) -> Result<Vec<String>, FwError> {
            self.inner.list_head_files().await
        }

        async fn upload_workspace(
            &self,
            ws_id: &str,
            repo_root: &Path,
            overlay_dir: &Path,
            base_version: &str,
            deleted_paths: &[String],
        ) -> Result<SnapshotInfo, FwError> {
            self.inner
                .upload_workspace(ws_id, repo_root, overlay_dir, base_version, deleted_paths)
                .await
        }

        async fn submit_workspace(&self, ws_id: &str) -> Result<Applied, FwError> {
            self.inner.submit_workspace(ws_id).await
        }

        async fn download_files(&self, paths: &[String]) -> Result<Vec<(String, Vec<u8>)>, FwError> {
            self.inner.download_files(paths).await
        }

        async fn create_workspace(&self, intent: &str) -> Result<String, FwError> {
            self.inner.create_workspace(intent).await
        }
    }

    // ── #371 test 5: server-race → BaseDrift ─────────────────────────────────

    /// When the server HEAD advances between `plan()` and `submit()`, the
    /// surprise list must contain `BaseDrifted` so the caller can decide
    /// whether to re-plan or force-submit.
    #[tokio::test]
    async fn plan_detects_base_drift_when_head_advances_concurrently() {
        let (_dir, root) = setup_local_repo(&[("src/file.rs", "fn main() {}")]);
        let vai_dir = root.join(".vai");

        let result = workspace::create(&vai_dir, "test", "v1").unwrap();
        make_overlay(
            &vai_dir,
            &result.workspace.id,
            &[("src/file.rs", "fn main() { /* changed */ }")],
        );

        // Racing repo: starts at v1, advances to v2 after list_head_files.
        let remote = RacingRemoteRepo::new(
            "v1",
            "v2",
            [("src/file.rs", b"fn main() {}".to_vec())],
        );

        let fw = FileWorkspace::open(OpenOptions {
            repo_root: root.clone(),
            backend: Backend::Remote(remote),
            intent: Intent::Existing,
        })
        .await
        .unwrap();

        let plan = fw.plan().await.unwrap();

        let has_base_drift = plan.surprises.iter().any(|s| {
            matches!(
                s,
                Surprise::BaseDrifted { expected, actual }
                    if expected == "v1" && actual == "v2"
            )
        });

        assert!(
            has_base_drift,
            "plan() must detect BaseDrift when server HEAD advances between plan calls; \
             surprises: {:?}",
            plan.surprises
        );
    }

    // ── #371 test 6: mode parity ──────────────────────────────────────────────

    /// The same file addition is visible in `plan()` for both local and remote
    /// backends, confirming that the two modes produce equivalent results.
    #[tokio::test]
    async fn plan_add_is_consistent_between_local_and_remote_modes() {
        // ── Local mode ────────────────────────────────────────────────────────
        let (_dir_l, root_l) = setup_local_repo(&[]);
        let vai_dir_l = root_l.join(".vai");
        let result_l = workspace::create(&vai_dir_l, "add feature", "v1").unwrap();
        make_overlay(
            &vai_dir_l,
            &result_l.workspace.id,
            &[("src/new.rs", "fn new() {}")],
        );

        let fw_l = FileWorkspace::open(OpenOptions::local(root_l.clone()))
            .await
            .unwrap();
        let plan_l = fw_l.plan().await.unwrap();

        // ── Remote mode ───────────────────────────────────────────────────────
        let (_dir_r, root_r) = setup_local_repo(&[]);
        let vai_dir_r = root_r.join(".vai");
        let result_r = workspace::create(&vai_dir_r, "add feature", "v1").unwrap();
        make_overlay(
            &vai_dir_r,
            &result_r.workspace.id,
            &[("src/new.rs", "fn new() {}")],
        );

        let remote = InMemoryRemoteRepo::new("v1", []);
        let fw_r = FileWorkspace::open(OpenOptions {
            repo_root: root_r.clone(),
            backend: Backend::Remote(remote),
            intent: Intent::Existing,
        })
        .await
        .unwrap();
        let plan_r = fw_r.plan().await.unwrap();

        // Both plans must agree on the add.
        let local_adds: Vec<String> = plan_l
            .adds
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        let remote_adds: Vec<String> = plan_r
            .adds
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();

        assert_eq!(
            local_adds, remote_adds,
            "plan().adds must be identical for local and remote backends (mode parity)"
        );
        assert!(plan_l.dels.is_empty());
        assert!(plan_r.dels.is_empty());
    }
}
