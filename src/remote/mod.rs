//! Unified push/pull/sync operations behind a [`RemoteAdapter`] port.
//!
//! # Overview
//!
//! This module consolidates the formerly separate `push`, `pull`, and `sync`
//! modules into a single, testable interface:
//!
//! - **[`push`] / [`pull`] / [`pull_force`] / [`sync`]** — free functions for the
//!   common case. They read `.vai/config.toml` (or `.vai/remote.toml` for sync)
//!   to build a [`Session`] automatically.
//! - **[`Session`] + [`Builder`]** — escape hatch for CLI flag overrides and tests.
//!   Inject an [`InMemoryAdapter`] via [`Builder::adapter`] to run boundary
//!   tests without HTTP or a filesystem.
//! - **[`RemoteAdapter`]** — the port. Production code uses [`HttpAdapter`];
//!   tests use [`InMemoryAdapter`].
//!
//! # Common case (free functions)
//!
//! ```ignore
//! # use std::path::Path;
//! let push_outcome  = vai::remote::push(root, "refactor auth").await?;
//! let pull_outcome  = vai::remote::pull(root).await?;
//! let sync_outcome  = vai::remote::sync(root).await?;
//! ```
//!
//! # Escape hatch (Session + Builder)
//!
//! ```ignore
//! use vai::remote::Session;
//!
//! let outcome = Session::builder(root)
//!     .remote_url("https://vai.example.com")
//!     .api_key("vai_key_xxx")
//!     .repo("myrepo")
//!     .build()?
//!     .push("add feature X")
//!     .await?;
//! ```

pub mod http_adapter;
#[cfg(test)]
pub mod memory;
mod tarball;

pub use http_adapter::HttpAdapter;
#[cfg(test)]
pub use memory::InMemoryAdapter;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use base64::prelude::{Engine, BASE64_STANDARD as BASE64};
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};

// ── Error type ────────────────────────────────────────────────────────────────

/// All errors that can occur during remote push, pull, or sync operations.
#[derive(Debug, thiserror::Error)]
pub enum RemoteError {
    #[error("no remote configured — run `vai remote add <url> --key <key>` or pass --to/--key/--repo flags")]
    NoRemote,

    #[error("--repo is required when using an explicit server URL")]
    MissingRepo,

    #[error("--key / VAI_API_KEY is required when using an explicit server URL")]
    MissingKey,

    #[error("-m / --message is required")]
    MissingMessage,

    #[error("nothing to push — working directory matches server state")]
    NothingToPush,

    #[error("merge conflict — server rejected push:\n{0}\nRun `vai pull` to sync, then retry.")]
    MergeConflict(String),

    #[error("server error: {0}")]
    Server(String),

    #[error("transport error: {0}")]
    Transport(#[from] reqwest::Error),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("base64 decode error: {0}")]
    Base64(#[from] base64::DecodeError),

    #[error("tarball error: {0}")]
    Tarball(String),

    #[error("config error: {0}")]
    Config(String),

    #[error("not a cloned repository — no .vai/remote.toml found")]
    NotAClone,

    #[error("local HEAD version not found on server — full re-clone may be needed")]
    LocalVersionNotFound,
}

// ── Adapter I/O types ─────────────────────────────────────────────────────────

/// Server manifest: current version and per-file checksums.
#[derive(Debug)]
pub struct ManifestResult {
    pub version: String,
    pub files: Vec<ManifestEntry>,
}

/// A single entry in the server file manifest.
#[derive(Debug)]
pub struct ManifestEntry {
    pub path: String,
    pub sha256: String,
}

/// Stats returned after uploading a snapshot.
#[derive(Debug)]
pub struct UploadStats {
    pub added: usize,
    pub modified: usize,
    pub deleted: usize,
}

/// Stats returned after submitting a workspace for merge.
#[derive(Debug)]
pub struct SubmitResult {
    pub version: String,
    pub files_applied: usize,
}

/// Kind of change applied to a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Modified,
    Removed,
}

/// A single file entry in an incremental pull response.
#[derive(Debug)]
pub struct FilePullEntry {
    pub path: String,
    pub change: ChangeKind,
    /// Base64-encoded file content (present for Added/Modified).
    pub content_base64: Option<String>,
}

/// Response from the incremental pull endpoint.
#[derive(Debug)]
pub struct IncrementalPullResult {
    pub base_version: String,
    pub head_version: String,
    pub files: Vec<FilePullEntry>,
}

/// Response from the full-tarball download endpoint.
#[derive(Debug)]
pub struct FullDownload {
    pub head_version: String,
    pub tarball_gz: Vec<u8>,
}

/// A version entry returned by the versions list endpoint.
#[derive(Debug)]
pub struct VersionSummary {
    pub version_id: String,
}

/// A file change within a version, used during sync.
#[derive(Debug)]
pub struct VersionFileChange {
    pub path: String,
    pub change: ChangeKind,
}

// ── RemoteAdapter trait ───────────────────────────────────────────────────────

/// The port that separates operation logic (push/pull/sync) from transport.
///
/// Production code uses [`HttpAdapter`]. Tests use [`InMemoryAdapter`].
/// Inject an alternative via [`Builder::adapter`].
#[async_trait]
pub trait RemoteAdapter: Send + Sync {
    // ── Push operations ────────────────────────────────────────────────────

    /// Returns the current server manifest (file list + checksums + HEAD version).
    async fn get_manifest(&self, repo: &str) -> Result<ManifestResult, RemoteError>;

    /// Creates a workspace on the server and returns its ID.
    async fn create_workspace(&self, repo: &str, intent: &str) -> Result<String, RemoteError>;

    /// Uploads a gzip snapshot tarball to the given workspace.
    async fn upload_snapshot(
        &self,
        repo: &str,
        workspace_id: &str,
        tarball_gz: Vec<u8>,
    ) -> Result<UploadStats, RemoteError>;

    /// Submits a workspace for merge and returns the resulting version.
    async fn submit_workspace(
        &self,
        repo: &str,
        workspace_id: &str,
    ) -> Result<SubmitResult, RemoteError>;

    // ── Pull operations ────────────────────────────────────────────────────

    /// Returns all file changes since `since` (incremental pull).
    async fn pull_incremental(
        &self,
        repo: &str,
        since: &str,
    ) -> Result<IncrementalPullResult, RemoteError>;

    /// Downloads the full file tarball for the repo.
    async fn download_full(&self, repo: &str) -> Result<FullDownload, RemoteError>;

    // ── Sync operations ────────────────────────────────────────────────────

    /// Returns the current server HEAD version (cheap check for sync).
    async fn get_server_head(&self, repo: &str) -> Result<String, RemoteError>;

    /// Returns all versions oldest-first.
    async fn list_versions(&self, repo: &str) -> Result<Vec<VersionSummary>, RemoteError>;

    /// Returns file changes introduced in a specific version.
    async fn get_version_changes(
        &self,
        repo: &str,
        version_id: &str,
    ) -> Result<Vec<VersionFileChange>, RemoteError>;

    /// Downloads the latest content of a file by its repo-relative path.
    async fn download_file(&self, repo: &str, path: &str) -> Result<Vec<u8>, RemoteError>;
}

// ── Outcome types ─────────────────────────────────────────────────────────────

/// Outcome of a push operation.
#[derive(Debug, serde::Serialize)]
pub struct PushOutcome {
    /// New version created on the server.
    pub version: String,
    pub files_added: usize,
    pub files_modified: usize,
    pub files_deleted: usize,
    pub files_applied: usize,
    /// `true` if this was a dry run (no data was sent).
    pub dry_run: bool,
}

impl PushOutcome {
    /// Prints a human-readable summary to stdout.
    pub fn print(&self) {
        if self.dry_run {
            println!(
                "{} Dry run — {} file(s) would be pushed",
                "·".dimmed(),
                self.files_added + self.files_modified + self.files_deleted,
            );
            return;
        }
        println!("{} Pushed — version {}", "✓".green().bold(), self.version.bold());
        let total = self.files_added + self.files_modified + self.files_deleted;
        println!("  {} file(s) changed", total);
        if self.files_added > 0 {
            println!("    {} {} added", "+".green(), self.files_added);
        }
        if self.files_modified > 0 {
            println!("    {} {} modified", "~".yellow(), self.files_modified);
        }
        if self.files_deleted > 0 {
            println!("    {} {} deleted", "-".red(), self.files_deleted);
        }
    }
}

/// Outcome of a pull operation.
#[derive(Debug, serde::Serialize)]
pub struct PullOutcome {
    pub previous_version: String,
    pub new_version: String,
    pub files_updated: Vec<String>,
    pub files_removed: Vec<String>,
    pub already_up_to_date: bool,
    /// `true` if this was a force (full-tarball) pull.
    pub force: bool,
}

impl PullOutcome {
    /// Prints a human-readable summary to stdout.
    pub fn print(&self) {
        if self.already_up_to_date {
            println!(
                "{} Already up to date ({})",
                "✓".green().bold(),
                self.new_version,
            );
            println!(
                "  {} If local files have been modified externally, run {} to force a full re-sync.",
                "hint:".dimmed(),
                "vai pull --force".bold(),
            );
            return;
        }
        let mode = if self.force { " (force)" } else { "" };
        println!(
            "{} Pulled{} {} → {}",
            "✓".green().bold(),
            mode,
            self.previous_version.dimmed(),
            self.new_version.bold(),
        );
        if !self.files_updated.is_empty() {
            println!("  Updated  : {} file(s)", self.files_updated.len());
            for f in &self.files_updated {
                println!("    {} {f}", "+".green());
            }
        }
        if !self.files_removed.is_empty() {
            println!("  Removed  : {} file(s)", self.files_removed.len());
            for f in &self.files_removed {
                println!("    {} {f}", "-".red());
            }
        }
    }
}

/// A conflict between server changes and a local active workspace.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WorkspaceConflict {
    pub path: String,
    pub workspace_intent: String,
}

/// Outcome of a sync operation.
#[derive(Debug, serde::Serialize)]
pub struct SyncOutcome {
    pub previous_version: String,
    pub new_version: String,
    pub files_updated: Vec<String>,
    pub files_removed: Vec<String>,
    pub already_up_to_date: bool,
    pub workspace_conflicts: Vec<WorkspaceConflict>,
}

impl SyncOutcome {
    /// Prints a human-readable summary to stdout.
    pub fn print(&self) {
        if self.already_up_to_date {
            println!("{} Already up to date ({})", "✓".green().bold(), self.new_version);
            return;
        }
        println!(
            "{} Synced {} → {}",
            "✓".green().bold(),
            self.previous_version.dimmed(),
            self.new_version.bold(),
        );
        if !self.files_updated.is_empty() {
            println!("  Updated  : {} file(s)", self.files_updated.len());
            for f in &self.files_updated {
                println!("    {} {f}", "+".green());
            }
        }
        if !self.files_removed.is_empty() {
            println!("  Removed  : {} file(s)", self.files_removed.len());
            for f in &self.files_removed {
                println!("    {} {f}", "-".red());
            }
        }
        if !self.workspace_conflicts.is_empty() {
            println!(
                "\n  {} Your active workspace has modified files that changed on the server:",
                "⚠".yellow().bold()
            );
            for c in &self.workspace_conflicts {
                println!("    {} (workspace: {})", c.path.yellow(), c.workspace_intent);
            }
            println!("  Consider reviewing these files before submitting your workspace.");
        }
    }
}

// ── Session ───────────────────────────────────────────────────────────────────

/// A configured push/pull/sync session.
///
/// Holds the adapter, repo root path, and repo name. All operations are
/// performed through the injected [`RemoteAdapter`].
pub struct Session {
    adapter: Arc<dyn RemoteAdapter>,
    repo_root: PathBuf,
    repo_name: String,
}

impl Session {
    /// Returns a [`Builder`] that loads connection details from `.vai/config.toml`.
    pub fn open(repo_root: &Path) -> Result<Builder, RemoteError> {
        let vai_dir = repo_root.join(".vai");
        let config = crate::repo::read_config(&vai_dir)
            .map_err(|e| RemoteError::Config(e.to_string()))?;
        let remote = config.remote.ok_or(RemoteError::NoRemote)?;
        let (api_key, _) = crate::credentials::load_api_key()
            .map_err(|e| RemoteError::Config(format!("credentials error: {e}")))?;
        let repo_name = remote.repo_name.unwrap_or(config.name);
        Ok(Builder {
            repo_root: repo_root.to_path_buf(),
            server_url: Some(remote.url),
            api_key: Some(api_key),
            repo_name: Some(repo_name),
            adapter: None,
        })
    }

    /// Returns a [`Builder`] without reading any config (all fields must be set).
    pub fn builder(repo_root: &Path) -> Builder {
        Builder {
            repo_root: repo_root.to_path_buf(),
            server_url: None,
            api_key: None,
            repo_name: None,
            adapter: None,
        }
    }

    // ── Push ──────────────────────────────────────────────────────────────────

    /// Pushes the local working directory to the server as a new version.
    ///
    /// Steps:
    /// 1. Fetch server manifest; compute local changes.
    /// 2. If nothing to push, return [`RemoteError::NothingToPush`].
    /// 3. Create workspace → upload snapshot → submit.
    /// 4. Update `.vai/head`.
    pub async fn push(&self, message: &str, dry_run: bool) -> Result<PushOutcome, RemoteError> {
        let vai_dir = self.repo_root.join(".vai");

        // ── 1. Fetch manifest & compute diff ──────────────────────────────
        let manifest = self.adapter.get_manifest(&self.repo_name).await?;
        let server_map: HashMap<String, String> =
            manifest.files.into_iter().map(|e| (e.path, e.sha256)).collect();
        let local_map = tarball::collect_local_hashes(&self.repo_root)?;

        let mut files_modified = 0usize;
        let mut files_added = 0usize;
        let mut files_deleted = 0usize;

        for (path, local_hash) in &local_map {
            match server_map.get(path) {
                Some(server_hash) if server_hash != local_hash => files_modified += 1,
                None => files_added += 1,
                _ => {}
            }
        }
        for path in server_map.keys() {
            if !local_map.contains_key(path) {
                files_deleted += 1;
            }
        }

        if files_modified == 0 && files_added == 0 && files_deleted == 0 {
            return Err(RemoteError::NothingToPush);
        }

        if dry_run {
            return Ok(PushOutcome {
                version: manifest.version,
                files_added,
                files_modified,
                files_deleted,
                files_applied: 0,
                dry_run: true,
            });
        }

        // ── 2. Create workspace ────────────────────────────────────────────
        let workspace_id = self.adapter.create_workspace(&self.repo_name, message).await?;

        // ── 3. Build and upload snapshot ───────────────────────────────────
        let tarball = tarball::build_full_tarball(&self.repo_root)?;
        let upload_stats = self
            .adapter
            .upload_snapshot(&self.repo_name, &workspace_id, tarball)
            .await?;

        // ── 4. Submit for merge ────────────────────────────────────────────
        let submit_result = self.adapter.submit_workspace(&self.repo_name, &workspace_id).await?;

        // ── 5. Update local HEAD ───────────────────────────────────────────
        if vai_dir.exists() {
            std::fs::write(
                vai_dir.join("head"),
                format!("{}\n", submit_result.version),
            )?;
        }

        Ok(PushOutcome {
            version: submit_result.version,
            files_added: upload_stats.added,
            files_modified: upload_stats.modified,
            files_deleted: upload_stats.deleted,
            files_applied: submit_result.files_applied,
            dry_run: false,
        })
    }

    // ── Pull ──────────────────────────────────────────────────────────────────

    /// Pulls changes from the server since the local HEAD version.
    pub async fn pull(&self) -> Result<PullOutcome, RemoteError> {
        let vai_dir = self.repo_root.join(".vai");
        let local_head = read_local_head(&vai_dir)?;

        let result =
            self.adapter.pull_incremental(&self.repo_name, &local_head).await?;

        if result.head_version == local_head || result.files.is_empty() {
            return Ok(PullOutcome {
                previous_version: local_head.clone(),
                new_version: result.head_version,
                files_updated: vec![],
                files_removed: vec![],
                already_up_to_date: true,
                force: false,
            });
        }

        let mut files_updated = Vec::new();
        let mut files_removed = Vec::new();

        let to_write: Vec<&FilePullEntry> = result
            .files
            .iter()
            .filter(|e| matches!(e.change, ChangeKind::Added | ChangeKind::Modified))
            .collect();
        let to_remove: Vec<&FilePullEntry> = result
            .files
            .iter()
            .filter(|e| matches!(e.change, ChangeKind::Removed))
            .collect();

        if !to_write.is_empty() {
            let pb = make_progress_bar(to_write.len() as u64, "Pulling");
            for entry in &to_write {
                pb.set_message(entry.path.clone());
                if let Some(ref b64) = entry.content_base64 {
                    let content = BASE64.decode(b64.as_bytes())?;
                    let dest = self.repo_root.join(&entry.path);
                    if let Some(parent) = dest.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::write(&dest, &content)?;
                    tarball::set_executable_if_shebang(&dest, &content)?;
                    files_updated.push(entry.path.clone());
                }
                pb.inc(1);
            }
            pb.finish_and_clear();
        }

        for entry in &to_remove {
            let dest = self.repo_root.join(&entry.path);
            if dest.exists() {
                std::fs::remove_file(&dest)?;
            }
            files_removed.push(entry.path.clone());
        }

        std::fs::write(vai_dir.join("head"), format!("{}\n", result.head_version))?;

        Ok(PullOutcome {
            previous_version: local_head,
            new_version: result.head_version,
            files_updated,
            files_removed,
            already_up_to_date: false,
            force: false,
        })
    }

    /// Performs a full re-sync by downloading and extracting the complete tarball.
    pub async fn pull_force(&self) -> Result<PullOutcome, RemoteError> {
        let vai_dir = self.repo_root.join(".vai");
        let local_head = read_local_head(&vai_dir).unwrap_or_default();

        let dl = self.adapter.download_full(&self.repo_name).await?;

        let server_paths = tarball::tarball_paths(&dl.tarball_gz)?;
        let server_path_set: HashSet<&str> = server_paths.iter().map(String::as_str).collect();

        let files_removed =
            tarball::remove_stale_local_files(&self.repo_root, &server_path_set)?;
        let files_updated = tarball::extract_tarball(&self.repo_root, &dl.tarball_gz)?;

        if dl.head_version != "unknown" {
            std::fs::write(vai_dir.join("head"), format!("{}\n", dl.head_version))?;
        }

        Ok(PullOutcome {
            previous_version: local_head,
            new_version: dl.head_version,
            files_updated,
            files_removed,
            already_up_to_date: false,
            force: true,
        })
    }

    // ── Sync ──────────────────────────────────────────────────────────────────

    /// Syncs a cloned repository with the remote server (incremental, version-by-version).
    pub async fn sync(&self) -> Result<SyncOutcome, RemoteError> {
        let vai_dir = self.repo_root.join(".vai");
        let local_head = read_local_head(&vai_dir)?;

        let server_head = self.adapter.get_server_head(&self.repo_name).await?;

        if local_head == server_head {
            return Ok(SyncOutcome {
                previous_version: local_head.clone(),
                new_version: local_head,
                files_updated: vec![],
                files_removed: vec![],
                already_up_to_date: true,
                workspace_conflicts: vec![],
            });
        }

        let all_versions = self.adapter.list_versions(&self.repo_name).await?;

        let local_head_pos = all_versions.iter().position(|v| v.version_id == local_head);
        let new_versions: Vec<&VersionSummary> = match local_head_pos {
            Some(pos) => all_versions[(pos + 1)..].iter().collect(),
            None => return Err(RemoteError::LocalVersionNotFound),
        };

        // Collect changes across all new versions; later wins.
        let mut changes: HashMap<String, ChangeKind> = HashMap::new();
        for v in &new_versions {
            let version_changes =
                self.adapter.get_version_changes(&self.repo_name, &v.version_id).await?;
            for fc in version_changes {
                changes.insert(fc.path, fc.change);
            }
        }

        let mut to_download: Vec<String> = Vec::new();
        let mut to_remove: Vec<String> = Vec::new();
        for (path, kind) in &changes {
            match kind {
                ChangeKind::Added | ChangeKind::Modified => to_download.push(path.clone()),
                ChangeKind::Removed => to_remove.push(path.clone()),
            }
        }
        to_download.sort();
        to_remove.sort();

        if !to_download.is_empty() {
            let pb = make_progress_bar(to_download.len() as u64, "Syncing");
            for rel_path in &to_download {
                pb.set_message(rel_path.clone());
                let content =
                    self.adapter.download_file(&self.repo_name, rel_path).await?;
                let local_path = self.repo_root.join(rel_path);
                if let Some(parent) = local_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&local_path, &content)?;
                pb.inc(1);
            }
            pb.finish_and_clear();
        }

        for rel_path in &to_remove {
            let local_path = self.repo_root.join(rel_path);
            if local_path.exists() {
                std::fs::remove_file(&local_path)?;
            }
        }

        std::fs::write(vai_dir.join("head"), format!("{server_head}\n"))?;

        let changed_paths: HashSet<&str> = to_download
            .iter()
            .chain(to_remove.iter())
            .map(|s| s.as_str())
            .collect();
        let workspace_conflicts = detect_workspace_conflicts(&vai_dir, &changed_paths);

        Ok(SyncOutcome {
            previous_version: local_head,
            new_version: server_head,
            files_updated: to_download,
            files_removed: to_remove,
            already_up_to_date: false,
            workspace_conflicts,
        })
    }
}

// ── Builder ───────────────────────────────────────────────────────────────────

/// Builder for [`Session`].
///
/// Created via [`Session::open`] (reads config) or [`Session::builder`] (blank slate).
pub struct Builder {
    repo_root: PathBuf,
    server_url: Option<String>,
    api_key: Option<String>,
    repo_name: Option<String>,
    adapter: Option<Arc<dyn RemoteAdapter>>,
}

impl Builder {
    /// Overrides the server URL.
    pub fn remote_url(mut self, url: impl Into<String>) -> Self {
        self.server_url = Some(url.into());
        self
    }

    /// Overrides the API key.
    pub fn api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(key.into());
        self
    }

    /// Overrides the repository name.
    pub fn repo(mut self, repo: impl Into<String>) -> Self {
        self.repo_name = Some(repo.into());
        self
    }

    /// Injects a custom [`RemoteAdapter`] (useful for tests or future adapters).
    ///
    /// When set, `server_url` and `api_key` are not required.
    pub fn adapter(mut self, a: Arc<dyn RemoteAdapter>) -> Self {
        self.adapter = Some(a);
        self
    }

    /// Builds the [`Session`].
    ///
    /// Fails if any required field is missing (unless a custom adapter is set).
    pub fn build(self) -> Result<Session, RemoteError> {
        let repo_name = self.repo_name.ok_or(RemoteError::MissingRepo)?;
        let adapter: Arc<dyn RemoteAdapter> = if let Some(a) = self.adapter {
            a
        } else {
            let url = self.server_url.ok_or(RemoteError::NoRemote)?;
            let key = self.api_key.ok_or(RemoteError::MissingKey)?;
            Arc::new(HttpAdapter::new(&url, &key))
        };
        Ok(Session { adapter, repo_root: self.repo_root, repo_name })
    }
}

// ── Free functions (common case) ──────────────────────────────────────────────

/// Pushes local changes to the server.
///
/// Reads connection details from `.vai/config.toml`. For CLI flag overrides,
/// use [`Session::builder`] instead.
pub async fn push(repo_root: &Path, message: &str) -> Result<PushOutcome, RemoteError> {
    Session::open(repo_root)?.build()?.push(message, false).await
}

/// Pushes local changes to the server (dry-run: no data is sent).
pub async fn push_dry_run(repo_root: &Path, message: &str) -> Result<PushOutcome, RemoteError> {
    Session::open(repo_root)?.build()?.push(message, true).await
}

/// Pulls changes from the server since the local HEAD version.
///
/// Reads connection details from `.vai/config.toml`.
pub async fn pull(repo_root: &Path) -> Result<PullOutcome, RemoteError> {
    Session::open(repo_root)?.build()?.pull().await
}

/// Pulls the full server state (force re-sync).
///
/// Reads connection details from `.vai/config.toml`.
pub async fn pull_force(repo_root: &Path) -> Result<PullOutcome, RemoteError> {
    Session::open(repo_root)?.build()?.pull_force().await
}

/// Syncs a cloned repository with its remote server.
///
/// Reads connection details from `.vai/remote.toml` (set by `vai clone`).
/// For repos configured via `.vai/config.toml`, use [`pull`] instead.
pub async fn sync(repo_root: &Path) -> Result<SyncOutcome, RemoteError> {
    let vai_dir = repo_root.join(".vai");
    let remote = crate::clone::read_remote_config(&vai_dir).ok_or(RemoteError::NotAClone)?;
    Session::builder(repo_root)
        .remote_url(remote.server_url)
        .api_key(remote.api_key)
        .repo(remote.repo_name)
        .build()?
        .sync()
        .await
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Reads `.vai/head`, returning the trimmed version string.
fn read_local_head(vai_dir: &Path) -> Result<String, RemoteError> {
    let head_path = vai_dir.join("head");
    let raw = std::fs::read_to_string(&head_path)?;
    Ok(raw.trim().to_string())
}

/// Creates a standard progress bar for file operations.
fn make_progress_bar(len: u64, prefix: &str) -> ProgressBar {
    let pb = ProgressBar::new(len);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{prefix:.bold} [{bar:40}] {pos}/{len} {msg}")
            .unwrap_or_else(|_| ProgressStyle::default_bar())
            .progress_chars("=> "),
    );
    pb.set_prefix(prefix.to_string());
    pb
}

/// Checks whether the active workspace has modified any of the paths that the
/// server sync just changed.
fn detect_workspace_conflicts(
    vai_dir: &Path,
    changed_paths: &HashSet<&str>,
) -> Vec<WorkspaceConflict> {
    let mut conflicts = Vec::new();

    let active_file = vai_dir.join("workspaces").join("active");
    let active_id = match std::fs::read_to_string(&active_file) {
        Ok(s) => s.trim().to_string(),
        Err(_) => return conflicts,
    };
    if active_id.is_empty() {
        return conflicts;
    }

    let meta_path = vai_dir.join("workspaces").join(&active_id).join("meta.toml");
    let intent = match std::fs::read_to_string(&meta_path) {
        Ok(raw) => extract_intent_from_toml(&raw),
        Err(_) => return conflicts,
    };

    let overlay_dir = vai_dir.join("workspaces").join(&active_id).join("overlay");
    if !overlay_dir.exists() {
        return conflicts;
    }

    let overlay_paths = collect_overlay_paths(&overlay_dir, &overlay_dir);
    for overlay_path in overlay_paths {
        if changed_paths.contains(overlay_path.as_str()) {
            conflicts.push(WorkspaceConflict { path: overlay_path, workspace_intent: intent.clone() });
        }
    }

    conflicts
}

fn collect_overlay_paths(overlay_dir: &Path, base: &Path) -> Vec<String> {
    let mut paths = Vec::new();
    let entries = match std::fs::read_dir(overlay_dir) {
        Ok(e) => e,
        Err(_) => return paths,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            paths.extend(collect_overlay_paths(&path, base));
        } else if let Ok(rel) = path.strip_prefix(base) {
            if let Some(s) = rel.to_str() {
                paths.push(s.replace('\\', "/"));
            }
        }
    }
    paths
}

fn extract_intent_from_toml(raw: &str) -> String {
    for line in raw.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("intent") {
            let rest = rest.trim_start();
            if let Some(rest) = rest.strip_prefix('=') {
                let val = rest.trim().trim_matches('"');
                return val.to_string();
            }
        }
    }
    String::new()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use crate::remote::memory::InMemoryAdapter;

    fn setup_repo(root: &Path, head: &str) {
        let vai_dir = root.join(".vai");
        fs::create_dir_all(&vai_dir).unwrap();
        fs::write(vai_dir.join("head"), format!("{head}\n")).unwrap();
    }

    fn make_session(root: &Path, adapter: Arc<dyn RemoteAdapter>) -> Session {
        Session {
            adapter,
            repo_root: root.to_path_buf(),
            repo_name: "testrepo".to_string(),
        }
    }

    // ── push tests ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn push_uploads_modified_files_and_updates_head() {
        let root = tempfile::tempdir().unwrap();
        setup_repo(root.path(), "v1");
        fs::write(root.path().join("main.rs"), b"fn main() {}").unwrap();

        let adapter = Arc::new(InMemoryAdapter::new());
        adapter.seed_version("v1", vec![]);

        let session = make_session(root.path(), adapter.clone());
        let outcome = session.push("initial commit", false).await.unwrap();

        assert!(!outcome.version.is_empty());
        assert!(!outcome.dry_run);

        // HEAD was updated
        let new_head = fs::read_to_string(root.path().join(".vai/head")).unwrap();
        assert_eq!(new_head.trim(), outcome.version);
    }

    #[tokio::test]
    async fn push_surfaces_merge_conflict_as_error() {
        let root = tempfile::tempdir().unwrap();
        setup_repo(root.path(), "v1");
        fs::write(root.path().join("main.rs"), b"fn main() {}").unwrap();

        let adapter = Arc::new(InMemoryAdapter::new());
        adapter.seed_version("v1", vec![]);
        adapter.set_force_merge_conflict(true);

        let session = make_session(root.path(), adapter);
        let err = session.push("conflicting work", false).await.unwrap_err();
        assert!(matches!(err, RemoteError::MergeConflict(_)));
    }

    #[tokio::test]
    async fn push_preserves_head_on_upload_failure() {
        let root = tempfile::tempdir().unwrap();
        setup_repo(root.path(), "v1");
        fs::write(root.path().join("main.rs"), b"fn main() {}").unwrap();

        let adapter = Arc::new(InMemoryAdapter::new());
        adapter.seed_version("v1", vec![]);
        adapter.set_force_upload_failure(true);

        let session = make_session(root.path(), adapter);
        let _ = session.push("will fail", false).await;

        // HEAD should remain unchanged
        let head = fs::read_to_string(root.path().join(".vai/head")).unwrap();
        assert_eq!(head.trim(), "v1");
    }

    #[tokio::test]
    async fn push_nothing_to_push_returns_error() {
        let root = tempfile::tempdir().unwrap();
        setup_repo(root.path(), "v1");

        // Seed the adapter with the same file as local.
        let adapter = Arc::new(InMemoryAdapter::new());
        let content = b"fn main() {}";
        adapter.seed_file("main.rs", content);
        adapter.seed_version("v1", vec![("main.rs", ChangeKind::Added)]);
        fs::write(root.path().join("main.rs"), content).unwrap();

        let session = make_session(root.path(), adapter);
        let err = session.push("nothing changed", false).await.unwrap_err();
        assert!(matches!(err, RemoteError::NothingToPush));
    }

    // ── pull tests ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn pull_applies_added_modified_removed_changes() {
        let root = tempfile::tempdir().unwrap();
        setup_repo(root.path(), "v1");

        // Pre-create a file that will be "removed".
        fs::write(root.path().join("old.rs"), b"old").unwrap();

        let adapter = Arc::new(InMemoryAdapter::new());
        adapter.seed_file("new.rs", b"new content");
        adapter.seed_version("v1", vec![]);
        adapter.seed_version(
            "v2",
            vec![
                ("new.rs", ChangeKind::Added),
                ("old.rs", ChangeKind::Removed),
            ],
        );

        let session = make_session(root.path(), adapter);
        let outcome = session.pull().await.unwrap();

        assert_eq!(outcome.new_version, "v2");
        assert!(outcome.files_updated.contains(&"new.rs".to_string()));
        assert!(outcome.files_removed.contains(&"old.rs".to_string()));
        assert!(root.path().join("new.rs").exists());
        assert!(!root.path().join("old.rs").exists());
    }

    #[tokio::test]
    async fn pull_already_up_to_date_when_heads_match() {
        let root = tempfile::tempdir().unwrap();
        setup_repo(root.path(), "v1");

        let adapter = Arc::new(InMemoryAdapter::new());
        adapter.seed_version("v1", vec![]);

        let session = make_session(root.path(), adapter);
        let outcome = session.pull().await.unwrap();

        assert!(outcome.already_up_to_date);
    }

    #[tokio::test]
    async fn pull_full_overrides_tracked_files_preserving_ignored_paths() {
        let root = tempfile::tempdir().unwrap();
        setup_repo(root.path(), "v1");

        // Create a local file not on server (should be removed).
        fs::write(root.path().join("stale.rs"), b"stale").unwrap();
        // Create .git dir (should NOT be removed).
        fs::create_dir_all(root.path().join(".git")).unwrap();
        fs::write(root.path().join(".git/config"), b"git config").unwrap();

        let adapter = Arc::new(InMemoryAdapter::new());
        adapter.seed_file("fresh.rs", b"fresh");
        adapter.seed_version("v1", vec![("fresh.rs", ChangeKind::Added)]);

        let session = make_session(root.path(), adapter);
        let outcome = session.pull_force().await.unwrap();

        assert!(!outcome.already_up_to_date);
        assert!(root.path().join("fresh.rs").exists());
        assert!(!root.path().join("stale.rs").exists());
        // Ignored dirs preserved
        assert!(root.path().join(".git/config").exists());
    }

    // ── sync tests ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn sync_applies_versions_in_order_since_local_head() {
        let root = tempfile::tempdir().unwrap();
        setup_repo(root.path(), "v1");

        let adapter = Arc::new(InMemoryAdapter::new());
        adapter.seed_file("a.rs", b"v2 content");
        adapter.seed_file("b.rs", b"v3 content");
        adapter.seed_version("v1", vec![]);
        adapter.seed_version("v2", vec![("a.rs", ChangeKind::Added)]);
        adapter.seed_version("v3", vec![("b.rs", ChangeKind::Added)]);

        let session = make_session(root.path(), adapter);
        let outcome = session.sync().await.unwrap();

        assert_eq!(outcome.new_version, "v3");
        assert!(outcome.files_updated.contains(&"a.rs".to_string()));
        assert!(outcome.files_updated.contains(&"b.rs".to_string()));
        assert!(root.path().join("a.rs").exists());
        assert!(root.path().join("b.rs").exists());
    }

    #[tokio::test]
    async fn sync_already_up_to_date() {
        let root = tempfile::tempdir().unwrap();
        setup_repo(root.path(), "v2");

        let adapter = Arc::new(InMemoryAdapter::new());
        adapter.seed_version("v1", vec![]);
        adapter.seed_version("v2", vec![]);

        let session = make_session(root.path(), adapter);
        let outcome = session.sync().await.unwrap();

        assert!(outcome.already_up_to_date);
    }

    #[tokio::test]
    async fn no_remote_configured_returns_no_remote() {
        let root = tempfile::tempdir().unwrap();
        let vai_dir = root.path().join(".vai");
        fs::create_dir_all(&vai_dir).unwrap();
        // Write a minimal valid config with no [remote] section.
        let config_toml = r#"repo_id = "00000000-0000-0000-0000-000000000001"
name = "test"
created_at = "2025-01-01T00:00:00Z"
vai_version = "0.1.0"
"#.to_string();
        fs::write(vai_dir.join("config.toml"), config_toml).unwrap();

        // Session::open returns Result<Builder, RemoteError>.
        let result = Session::open(root.path());
        assert!(matches!(result, Err(RemoteError::NoRemote)));
    }

    // ── helper tests ──────────────────────────────────────────────────────────

    #[test]
    fn read_local_head_trims_newline() {
        let dir = tempfile::tempdir().unwrap();
        let vai_dir = dir.path().join(".vai");
        fs::create_dir_all(&vai_dir).unwrap();
        fs::write(vai_dir.join("head"), "v42\n").unwrap();
        assert_eq!(read_local_head(&vai_dir).unwrap(), "v42");
    }

    #[test]
    fn extract_intent_from_toml_basic() {
        let toml = "id = \"abc\"\nintent = \"refactor auth\"\nstatus = \"active\"\n";
        assert_eq!(extract_intent_from_toml(toml), "refactor auth");
    }

    #[test]
    fn extract_intent_missing_returns_empty() {
        assert_eq!(extract_intent_from_toml("id = \"abc\""), "");
    }
}
