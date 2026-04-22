//! In-memory implementation of [`RemoteAdapter`] for use in tests.
//!
//! `InMemoryAdapter` holds a simple in-memory version graph and file store.
//! It implements the full adapter interface without any HTTP or filesystem
//! dependencies, letting boundary tests run fast and deterministically.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use base64::prelude::{Engine, BASE64_STANDARD as BASE64};

use super::{
    ChangeKind, FullDownload, IncrementalPullResult, ManifestEntry, ManifestResult, RemoteAdapter,
    RemoteError, RemoteVersionMeta, SubmitResult, UploadStats, VersionFileChange, VersionSummary,
    FilePullEntry,
};

// ── Inner state ───────────────────────────────────────────────────────────────

#[derive(Default)]
struct InnerState {
    /// Current server files: path → content bytes.
    files: HashMap<String, Vec<u8>>,
    /// Version history (oldest first).
    versions: Vec<VersionRecord>,
    /// When set, `submit_workspace` returns a MergeConflict error.
    force_merge_conflict: bool,
    /// When set, `upload_snapshot` returns a Server error.
    force_upload_failure: bool,
    /// Submitted messages in order.
    pub submitted_messages: Vec<String>,
    /// Paths that were uploaded (in the most recent snapshot upload).
    pub last_uploaded_paths: Vec<String>,
}

struct VersionRecord {
    version_id: String,
    changes: Vec<(String, ChangeKind)>,
}

// ── InMemoryAdapter ───────────────────────────────────────────────────────────

/// An in-memory `RemoteAdapter` suitable for unit tests.
///
/// ```rust
/// let adapter = InMemoryAdapter::new();
/// adapter.seed_file("src/main.rs", b"fn main() {}");
/// adapter.seed_version("v1", vec![("src/main.rs", ChangeKind::Added)]);
/// ```
pub struct InMemoryAdapter {
    state: Arc<Mutex<InnerState>>,
}

impl Default for InMemoryAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryAdapter {
    /// Creates a new adapter with an empty file store and version history.
    pub fn new() -> Self {
        Self { state: Arc::new(Mutex::new(InnerState::default())) }
    }

    /// Seeds a file into the adapter's file store.
    pub fn seed_file(&self, path: &str, content: &[u8]) {
        self.state.lock().unwrap().files.insert(path.to_string(), content.to_vec());
    }

    /// Seeds a version into the adapter's version history.
    pub fn seed_version(&self, version_id: &str, changes: Vec<(&str, ChangeKind)>) {
        let mut state = self.state.lock().unwrap();
        state.versions.push(VersionRecord {
            version_id: version_id.to_string(),
            changes: changes
                .into_iter()
                .map(|(p, c)| (p.to_string(), c))
                .collect(),
        });
    }

    /// Forces the next `submit_workspace` call to return a `MergeConflict` error.
    pub fn set_force_merge_conflict(&self, v: bool) {
        self.state.lock().unwrap().force_merge_conflict = v;
    }

    /// Forces the next `upload_snapshot` call to return a `Server` error.
    pub fn set_force_upload_failure(&self, v: bool) {
        self.state.lock().unwrap().force_upload_failure = v;
    }

    /// Returns the messages passed to `submit_workspace` so far.
    pub fn submitted_messages(&self) -> Vec<String> {
        self.state.lock().unwrap().submitted_messages.clone()
    }

    /// Returns the paths present in the most recent uploaded snapshot.
    pub fn last_uploaded_paths(&self) -> Vec<String> {
        self.state.lock().unwrap().last_uploaded_paths.clone()
    }

    /// Returns the current server HEAD version id (last in the list).
    pub fn current_head(&self) -> String {
        let state = self.state.lock().unwrap();
        state
            .versions
            .last()
            .map(|v| v.version_id.clone())
            .unwrap_or_default()
    }
}

#[async_trait]
impl RemoteAdapter for InMemoryAdapter {
    async fn get_manifest(&self, _repo: &str) -> Result<ManifestResult, RemoteError> {
        use sha2::{Digest, Sha256};
        let state = self.state.lock().unwrap();
        let head = state
            .versions
            .last()
            .map(|v| v.version_id.clone())
            .unwrap_or_default();
        let files = state
            .files
            .iter()
            .map(|(path, content)| {
                let mut hasher = Sha256::new();
                hasher.update(content);
                ManifestEntry {
                    path: path.clone(),
                    sha256: format!("{:x}", hasher.finalize()),
                }
            })
            .collect();
        Ok(ManifestResult { version: head, files })
    }

    async fn create_workspace(&self, _repo: &str, _intent: &str) -> Result<String, RemoteError> {
        Ok(uuid::Uuid::new_v4().to_string())
    }

    async fn upload_snapshot(
        &self,
        _repo: &str,
        _workspace_id: &str,
        tarball_gz: Vec<u8>,
    ) -> Result<UploadStats, RemoteError> {
        let mut state = self.state.lock().unwrap();
        if state.force_upload_failure {
            return Err(RemoteError::Server("forced upload failure".to_string()));
        }
        // Collect paths from the tarball so tests can assert on them.
        let paths = collect_tarball_paths(&tarball_gz);
        state.last_uploaded_paths = paths.clone();
        Ok(UploadStats { added: paths.len(), modified: 0, deleted: 0 })
    }

    async fn submit_workspace(
        &self,
        _repo: &str,
        _workspace_id: &str,
    ) -> Result<SubmitResult, RemoteError> {
        // Note: the "intent"/message isn't passed here; it was passed to
        // create_workspace. The InMemoryAdapter records submit calls instead.
        let mut state = self.state.lock().unwrap();
        if state.force_merge_conflict {
            return Err(RemoteError::MergeConflict("forced conflict".to_string()));
        }
        let new_version = format!("v{}", state.versions.len() + 1);
        state.versions.push(VersionRecord {
            version_id: new_version.clone(),
            changes: vec![],
        });
        let files_applied = state.last_uploaded_paths.len();
        Ok(SubmitResult { version: new_version, files_applied })
    }

    async fn pull_incremental(
        &self,
        _repo: &str,
        since: &str,
    ) -> Result<IncrementalPullResult, RemoteError> {
        let state = self.state.lock().unwrap();
        let head = state
            .versions
            .last()
            .map(|v| v.version_id.clone())
            .unwrap_or_default();

        if since == head {
            return Ok(IncrementalPullResult {
                base_version: since.to_string(),
                head_version: head,
                files: vec![],
            });
        }

        // Collect changes since `since`.
        let since_pos = state.versions.iter().position(|v| v.version_id == since);
        let changes: Vec<FilePullEntry> = match since_pos {
            Some(pos) => state.versions[(pos + 1)..]
                .iter()
                .flat_map(|v| {
                    v.changes.iter().map(|(path, kind)| {
                        let content_base64 = if matches!(kind, ChangeKind::Added | ChangeKind::Modified) {
                            state.files.get(path).map(|c| BASE64.encode(c))
                        } else {
                            None
                        };
                        FilePullEntry {
                            path: path.clone(),
                            change: kind.clone(),
                            content_base64,
                        }
                    })
                })
                .collect(),
            None => {
                return Err(RemoteError::Server(format!(
                    "version '{since}' not found"
                )));
            }
        };

        Ok(IncrementalPullResult {
            base_version: since.to_string(),
            head_version: head,
            files: changes,
        })
    }

    async fn download_full(&self, _repo: &str) -> Result<FullDownload, RemoteError> {
        let state = self.state.lock().unwrap();
        let head_version = state
            .versions
            .last()
            .map(|v| v.version_id.clone())
            .unwrap_or_default();

        // Build a minimal gzip tarball from the current files.
        use flate2::write::GzEncoder;
        use flate2::Compression;

        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        {
            let mut archive = tar::Builder::new(&mut encoder);
            for (path, content) in &state.files {
                let mut header = tar::Header::new_gnu();
                header.set_size(content.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                archive
                    .append_data(&mut header, path.as_str(), content.as_slice())
                    .ok();
            }
            archive.finish().ok();
        }
        let tarball_gz = encoder.finish().unwrap_or_default();

        Ok(FullDownload { head_version, tarball_gz })
    }

    async fn get_server_head(&self, _repo: &str) -> Result<String, RemoteError> {
        let state = self.state.lock().unwrap();
        Ok(state
            .versions
            .last()
            .map(|v| v.version_id.clone())
            .unwrap_or_default())
    }

    async fn list_versions(&self, _repo: &str) -> Result<Vec<VersionSummary>, RemoteError> {
        let state = self.state.lock().unwrap();
        Ok(state
            .versions
            .iter()
            .map(|v| VersionSummary { version_id: v.version_id.clone() })
            .collect())
    }

    async fn get_version_changes(
        &self,
        _repo: &str,
        version_id: &str,
    ) -> Result<Vec<VersionFileChange>, RemoteError> {
        let state = self.state.lock().unwrap();
        let record = state.versions.iter().find(|v| v.version_id == version_id);
        match record {
            Some(r) => Ok(r
                .changes
                .iter()
                .map(|(path, kind)| VersionFileChange { path: path.clone(), change: kind.clone() })
                .collect()),
            None => Err(RemoteError::Server(format!("version '{version_id}' not found"))),
        }
    }

    async fn download_file(&self, _repo: &str, path: &str) -> Result<Vec<u8>, RemoteError> {
        let state = self.state.lock().unwrap();
        state
            .files
            .get(path)
            .cloned()
            .ok_or_else(|| RemoteError::Server(format!("file '{path}' not found")))
    }

    async fn fetch_versions_since(
        &self,
        _repo: &str,
        since_version_num: u64,
    ) -> Result<Vec<RemoteVersionMeta>, RemoteError> {
        use chrono::Utc;
        let state = self.state.lock().unwrap();
        Ok(state
            .versions
            .iter()
            .filter(|v| super::parse_version_num_str(&v.version_id) > since_version_num)
            .map(|v| RemoteVersionMeta {
                version_id: v.version_id.clone(),
                parent_version_id: None,
                intent: String::new(),
                created_by: "unknown".to_string(),
                created_at: Utc::now(),
                merge_event_id: None,
            })
            .collect())
    }
}

/// Helper: extract file paths from a gzip tarball for test assertions.
fn collect_tarball_paths(gz_bytes: &[u8]) -> Vec<String> {
    use flate2::read::GzDecoder;
    use tar::Archive;

    let decoder = GzDecoder::new(gz_bytes);
    let mut archive = Archive::new(decoder);
    archive
        .entries()
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.header().entry_type().is_file())
        .filter_map(|e| e.path().ok().map(|p| p.to_string_lossy().replace('\\', "/")))
        .collect()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::ChangeKind;

    #[tokio::test]
    async fn get_manifest_returns_seeded_files() {
        let adapter = InMemoryAdapter::new();
        adapter.seed_file("src/main.rs", b"fn main() {}");
        adapter.seed_version("v1", vec![("src/main.rs", ChangeKind::Added)]);

        let manifest = adapter.get_manifest("repo").await.unwrap();
        assert_eq!(manifest.version, "v1");
        assert_eq!(manifest.files.len(), 1);
        assert_eq!(manifest.files[0].path, "src/main.rs");
    }

    #[tokio::test]
    async fn pull_incremental_returns_empty_when_up_to_date() {
        let adapter = InMemoryAdapter::new();
        adapter.seed_version("v1", vec![]);

        let result = adapter.pull_incremental("repo", "v1").await.unwrap();
        assert!(result.files.is_empty());
        assert!(result.head_version == "v1");
    }

    #[tokio::test]
    async fn pull_incremental_returns_changes_since_version() {
        let adapter = InMemoryAdapter::new();
        adapter.seed_file("src/lib.rs", b"lib");
        adapter.seed_version("v1", vec![]);
        adapter.seed_version("v2", vec![("src/lib.rs", ChangeKind::Added)]);

        let result = adapter.pull_incremental("repo", "v1").await.unwrap();
        assert_eq!(result.head_version, "v2");
        assert_eq!(result.files.len(), 1);
        assert_eq!(result.files[0].path, "src/lib.rs");
        assert!(matches!(result.files[0].change, ChangeKind::Added));
    }

    #[tokio::test]
    async fn submit_increments_version() {
        let adapter = InMemoryAdapter::new();
        adapter.seed_version("v1", vec![]);

        let ws_id = adapter.create_workspace("repo", "test work").await.unwrap();
        let result = adapter.submit_workspace("repo", &ws_id).await.unwrap();
        assert_eq!(result.version, "v2");
    }

    #[tokio::test]
    async fn submit_returns_merge_conflict_when_forced() {
        let adapter = InMemoryAdapter::new();
        adapter.set_force_merge_conflict(true);

        let ws_id = adapter.create_workspace("repo", "work").await.unwrap();
        let err = adapter.submit_workspace("repo", &ws_id).await.unwrap_err();
        assert!(matches!(err, RemoteError::MergeConflict(_)));
    }

    #[tokio::test]
    async fn upload_failure_returns_server_error() {
        let adapter = InMemoryAdapter::new();
        adapter.set_force_upload_failure(true);

        let ws_id = adapter.create_workspace("repo", "work").await.unwrap();
        let err = adapter.upload_snapshot("repo", &ws_id, vec![]).await.unwrap_err();
        assert!(matches!(err, RemoteError::Server(_)));
    }
}
