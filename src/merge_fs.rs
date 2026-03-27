//! File abstraction trait for the merge engine.
//!
//! `MergeFs` decouples the merge and diff engines from direct `std::fs` calls,
//! allowing them to operate on both local filesystems (via [`DiskMergeFs`]) and
//! remote object stores (via `S3MergeFs` in server mode).
//!
//! ## Path namespaces
//!
//! All paths passed to `MergeFs` methods use a logical key format:
//!
//! - `overlay/{rel_path}` — workspace overlay file (agent's changes)
//! - `base/{rel_path}` — current repo state (read: project root; write: merge output)
//! - `snapshot/{version_id}/{rel_path}` — pre-change snapshot for a specific version
//!
//! `list_files(prefix)` returns keys with the prefix included, so callers strip
//! the prefix to obtain the relative path.
//!
//! ## Implementations
//!
//! - [`DiskMergeFs`] — maps keys to `.vai/` and `repo_root` paths on disk (local mode)
//! - [`S3MergeFs`] — reads from S3 on demand, buffers writes in memory (server mode)

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use uuid::Uuid;

// ── Trait ─────────────────────────────────────────────────────────────────────

/// Abstracts file I/O for the merge and diff engines.
///
/// All path arguments use the logical key format described in the module docs.
pub trait MergeFs: Send + Sync {
    /// Reads the content of the file at `key`.
    fn read_file(&self, key: &str) -> io::Result<Vec<u8>>;

    /// Writes `content` to the file at `key`, creating parent directories as needed.
    fn write_file(&self, key: &str, content: &[u8]) -> io::Result<()>;

    /// Lists all keys whose path starts with `prefix`.
    ///
    /// Returns full keys (prefix included). Returns an empty vec if the prefix
    /// does not exist rather than an error.
    fn list_files(&self, prefix: &str) -> io::Result<Vec<String>>;

    /// Returns `true` if `key` exists.
    fn exists(&self, key: &str) -> io::Result<bool>;

    /// Deletes the file at `key`. No-ops if the file does not exist.
    fn delete_file(&self, key: &str) -> io::Result<()>;
}

// ── DiskMergeFs ───────────────────────────────────────────────────────────────

/// Filesystem-backed [`MergeFs`] for local mode.
///
/// Maps logical keys to physical paths:
///
/// | Key namespace            | Physical path                                        |
/// |--------------------------|------------------------------------------------------|
/// | `overlay/{path}`         | `.vai/workspaces/{workspace_id}/overlay/{path}`      |
/// | `base/{path}`            | `{repo_root}/{path}`                                 |
/// | `snapshot/{ver}/{path}`  | `.vai/versions/{ver}/snapshot/{path}`                |
pub struct DiskMergeFs {
    vai_dir: PathBuf,
    workspace_id: String,
    repo_root: PathBuf,
}

impl DiskMergeFs {
    /// Creates a new `DiskMergeFs`.
    pub fn new(vai_dir: &Path, workspace_id: &str, repo_root: &Path) -> Self {
        Self {
            vai_dir: vai_dir.to_path_buf(),
            workspace_id: workspace_id.to_string(),
            repo_root: repo_root.to_path_buf(),
        }
    }

    /// Resolves a logical key to a physical `PathBuf`.
    fn resolve(&self, key: &str) -> io::Result<PathBuf> {
        if let Some(rel) = key.strip_prefix("overlay/") {
            Ok(self
                .vai_dir
                .join("workspaces")
                .join(&self.workspace_id)
                .join("overlay")
                .join(rel))
        } else if let Some(rel) = key.strip_prefix("base/") {
            Ok(self.repo_root.join(rel))
        } else if let Some(rest) = key.strip_prefix("snapshot/") {
            // rest = "{version_id}/{rel_path}"
            let (ver, rel) = rest
                .split_once('/')
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, format!("bad snapshot key: {key}")))?;
            Ok(self
                .vai_dir
                .join("versions")
                .join(ver)
                .join("snapshot")
                .join(rel))
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unknown MergeFs namespace in key: {key}"),
            ))
        }
    }

    /// Returns the physical root directory for a given prefix, plus the
    /// canonical prefix string to use when building returned keys.
    fn prefix_root(&self, prefix: &str) -> io::Result<(PathBuf, String)> {
        if prefix == "overlay/" || prefix == "overlay" {
            let dir = self
                .vai_dir
                .join("workspaces")
                .join(&self.workspace_id)
                .join("overlay");
            Ok((dir, "overlay/".to_string()))
        } else if prefix == "base/" || prefix == "base" {
            Ok((self.repo_root.clone(), "base/".to_string()))
        } else if let Some(rest) = prefix.strip_prefix("snapshot/") {
            // rest = "{version_id}/" or "{version_id}"
            let ver = rest.trim_end_matches('/');
            if ver.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "snapshot prefix must include a version id (e.g. 'snapshot/v2/')",
                ));
            }
            let dir = self.vai_dir.join("versions").join(ver).join("snapshot");
            Ok((dir, format!("snapshot/{ver}/")))
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unknown MergeFs namespace in prefix: {prefix}"),
            ))
        }
    }
}

impl MergeFs for DiskMergeFs {
    fn read_file(&self, key: &str) -> io::Result<Vec<u8>> {
        fs::read(self.resolve(key)?)
    }

    fn write_file(&self, key: &str, content: &[u8]) -> io::Result<()> {
        let path = self.resolve(key)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, content)
    }

    fn list_files(&self, prefix: &str) -> io::Result<Vec<String>> {
        let (root, canonical_prefix) = self.prefix_root(prefix)?;
        if !root.exists() {
            return Ok(Vec::new());
        }
        let mut keys = Vec::new();
        list_recursive(&root, &root, &canonical_prefix, &mut keys)?;
        Ok(keys)
    }

    fn exists(&self, key: &str) -> io::Result<bool> {
        Ok(self.resolve(key)?.exists())
    }

    fn delete_file(&self, key: &str) -> io::Result<()> {
        let path = self.resolve(key)?;
        if path.exists() {
            fs::remove_file(path)?;
        }
        Ok(())
    }
}

// ── S3MergeFs ─────────────────────────────────────────────────────────────────

/// S3-backed [`MergeFs`] for server mode.
///
/// File reads are pulled from S3 on demand and cached in memory. Writes
/// accumulate in an in-memory buffer and are persisted to S3 by calling
/// [`S3MergeFs::flush`] after the merge completes.
///
/// ## Key → S3 path mapping
///
/// | MergeFs key            | S3 path                                 |
/// |------------------------|-----------------------------------------|
/// | `overlay/{rel}`        | `{overlay_prefix}{rel}`                 |
/// | `base/{rel}`           | `{base_prefix}{rel}`                    |
/// | `snapshot/{ver}/{rel}` | `versions/{ver}/snapshot/{rel}`         |
///
/// `overlay_prefix` is typically `"workspaces/{workspace_id}/"` and
/// `base_prefix` is `"current/"`.
pub struct S3MergeFs {
    file_store: Arc<dyn crate::storage::FileStore>,
    repo_id: Uuid,
    /// S3 prefix for overlay files, e.g. `"workspaces/{ws_id}/"`.
    overlay_prefix: String,
    /// S3 prefix for base (current repo) files, e.g. `"current/"`.
    base_prefix: String,
    /// In-memory read cache, keyed by S3 path.
    read_cache: Mutex<HashMap<String, Vec<u8>>>,
    /// Pending writes to flush to S3, keyed by S3 path.
    pending_writes: Mutex<HashMap<String, Vec<u8>>>,
    /// S3 paths scheduled for deletion on flush.
    pending_deletes: Mutex<Vec<String>>,
}

impl S3MergeFs {
    /// Creates a new `S3MergeFs`.
    ///
    /// `overlay_prefix` — S3 prefix for workspace overlay files, e.g.
    /// `"workspaces/{workspace_id}/"` (must end with `/`).
    ///
    /// `base_prefix` — S3 prefix for the current base repo state, e.g.
    /// `"current/"` (must end with `/`).
    pub fn new(
        file_store: Arc<dyn crate::storage::FileStore>,
        repo_id: Uuid,
        overlay_prefix: String,
        base_prefix: String,
    ) -> Self {
        Self {
            file_store,
            repo_id,
            overlay_prefix,
            base_prefix,
            read_cache: Mutex::new(HashMap::new()),
            pending_writes: Mutex::new(HashMap::new()),
            pending_deletes: Mutex::new(Vec::new()),
        }
    }

    /// Persists all buffered writes and deletes to S3.
    ///
    /// Call this after a successful merge to materialise the merged base-file
    /// state (under `{base_prefix}`) and any snapshot files
    /// (`versions/{ver}/snapshot/`) to the file store.
    pub async fn flush(&self) -> io::Result<()> {
        let writes: HashMap<String, Vec<u8>> = {
            let mut guard = self.pending_writes.lock().unwrap();
            std::mem::take(&mut *guard)
        };
        let deletes: Vec<String> = {
            let mut guard = self.pending_deletes.lock().unwrap();
            std::mem::take(&mut *guard)
        };

        for (s3_path, content) in writes {
            self.file_store
                .put(&self.repo_id, &s3_path, &content)
                .await
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
        }
        for s3_path in deletes {
            // Best-effort: ignore not-found errors on delete.
            let _ = self.file_store.delete(&self.repo_id, &s3_path).await;
        }
        Ok(())
    }

    /// Maps a MergeFs logical key to its S3 path.
    fn s3_path(&self, key: &str) -> io::Result<String> {
        if let Some(rel) = key.strip_prefix("overlay/") {
            Ok(format!("{}{rel}", self.overlay_prefix))
        } else if let Some(rel) = key.strip_prefix("base/") {
            Ok(format!("{}{rel}", self.base_prefix))
        } else if let Some(rest) = key.strip_prefix("snapshot/") {
            let (ver, rel) = rest.split_once('/').ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, format!("bad snapshot key: {key}"))
            })?;
            Ok(format!("versions/{ver}/snapshot/{rel}"))
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unknown MergeFs namespace in key: {key}"),
            ))
        }
    }

    /// Maps a MergeFs list prefix to `(s3_prefix, merge_prefix)`.
    fn s3_prefix(&self, prefix: &str) -> io::Result<(String, String)> {
        if prefix == "overlay/" || prefix == "overlay" {
            Ok((self.overlay_prefix.clone(), "overlay/".to_string()))
        } else if prefix == "base/" || prefix == "base" {
            Ok((self.base_prefix.clone(), "base/".to_string()))
        } else if let Some(rest) = prefix.strip_prefix("snapshot/") {
            let ver = rest.trim_end_matches('/');
            if ver.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "snapshot prefix must include a version id",
                ));
            }
            Ok((
                format!("versions/{ver}/snapshot/"),
                format!("snapshot/{ver}/"),
            ))
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unknown MergeFs namespace in prefix: {prefix}"),
            ))
        }
    }

    /// Calls `future` on the current tokio runtime handle, blocking the current
    /// thread without blocking the executor.
    ///
    /// Requires a multi-thread tokio runtime (the default for `#[tokio::main]`).
    fn block<F, T>(future: F) -> T
    where
        F: std::future::Future<Output = T>,
    {
        tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(future))
    }
}

impl MergeFs for S3MergeFs {
    fn read_file(&self, key: &str) -> io::Result<Vec<u8>> {
        let s3_path = self.s3_path(key)?;

        // Pending writes take priority (most-recent write wins).
        if let Some(content) = self.pending_writes.lock().unwrap().get(&s3_path) {
            return Ok(content.clone());
        }
        // Honour pending deletes.
        if self.pending_deletes.lock().unwrap().contains(&s3_path) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("deleted: {s3_path}"),
            ));
        }
        // Check read cache.
        if let Some(content) = self.read_cache.lock().unwrap().get(&s3_path) {
            return Ok(content.clone());
        }
        // Fetch from S3.
        let content = Self::block(self.file_store.get(&self.repo_id, &s3_path))
            .map_err(|e| io::Error::new(io::ErrorKind::NotFound, e.to_string()))?;

        self.read_cache
            .lock()
            .unwrap()
            .insert(s3_path, content.clone());
        Ok(content)
    }

    fn write_file(&self, key: &str, content: &[u8]) -> io::Result<()> {
        let s3_path = self.s3_path(key)?;
        // Remove from deletes if it was previously staged for deletion.
        self.pending_deletes
            .lock()
            .unwrap()
            .retain(|p| p != &s3_path);
        self.pending_writes
            .lock()
            .unwrap()
            .insert(s3_path, content.to_vec());
        Ok(())
    }

    fn list_files(&self, prefix: &str) -> io::Result<Vec<String>> {
        let (s3_prefix, merge_prefix) = self.s3_prefix(prefix)?;

        // Fetch listing from S3 (best-effort — empty on error).
        let s3_files = Self::block(self.file_store.list(&self.repo_id, &s3_prefix))
            .unwrap_or_default();

        // Snapshot of pending state (hold locks briefly, not across await).
        let pending_write_paths: Vec<String> = self
            .pending_writes
            .lock()
            .unwrap()
            .keys()
            .filter(|k| k.starts_with(s3_prefix.as_str()))
            .cloned()
            .collect();
        let pending_deletes: Vec<String> =
            self.pending_deletes.lock().unwrap().clone();

        // Merge S3 listing and pending writes, then remove deleted paths.
        let mut seen = std::collections::HashSet::new();
        let mut keys: Vec<String> = Vec::new();

        for fm in s3_files {
            if pending_deletes.contains(&fm.path) {
                continue;
            }
            if let Some(rel) = fm.path.strip_prefix(s3_prefix.as_str()) {
                let merge_key = format!("{merge_prefix}{rel}");
                if seen.insert(merge_key.clone()) {
                    keys.push(merge_key);
                }
            }
        }
        for s3_path in pending_write_paths {
            if pending_deletes.contains(&s3_path) {
                continue;
            }
            if let Some(rel) = s3_path.strip_prefix(s3_prefix.as_str()) {
                let merge_key = format!("{merge_prefix}{rel}");
                if seen.insert(merge_key.clone()) {
                    keys.push(merge_key);
                }
            }
        }

        keys.sort();
        Ok(keys)
    }

    fn exists(&self, key: &str) -> io::Result<bool> {
        let s3_path = self.s3_path(key)?;

        if self.pending_writes.lock().unwrap().contains_key(&s3_path) {
            return Ok(true);
        }
        if self.pending_deletes.lock().unwrap().contains(&s3_path) {
            return Ok(false);
        }
        if self.read_cache.lock().unwrap().contains_key(&s3_path) {
            return Ok(true);
        }
        Self::block(self.file_store.exists(&self.repo_id, &s3_path))
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))
    }

    fn delete_file(&self, key: &str) -> io::Result<()> {
        let s3_path = self.s3_path(key)?;
        // Unstage any buffered write for this path.
        self.pending_writes.lock().unwrap().remove(&s3_path);
        // Remove from read cache.
        self.read_cache.lock().unwrap().remove(&s3_path);
        // Stage the delete.
        let mut deletes = self.pending_deletes.lock().unwrap();
        if !deletes.contains(&s3_path) {
            deletes.push(s3_path);
        }
        Ok(())
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Recursively walks `dir`, appending `"{prefix}{rel_path}"` for each file to `out`.
fn list_recursive(root: &Path, dir: &Path, prefix: &str, out: &mut Vec<String>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            list_recursive(root, &path, prefix, out)?;
        } else {
            let rel = path
                .strip_prefix(root)
                .expect("path inside root")
                .to_string_lossy()
                .replace('\\', "/"); // normalise Windows separators
            out.push(format!("{prefix}{rel}"));
        }
    }
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup() -> (TempDir, PathBuf, PathBuf, String) {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_path_buf();
        let vai_dir = root.join(".vai");
        let repo_root = root.join("repo");
        let ws_id = "test-ws-id".to_string();

        fs::create_dir_all(&vai_dir).unwrap();
        fs::create_dir_all(&repo_root).unwrap();

        (dir, vai_dir, repo_root, ws_id)
    }

    #[test]
    fn test_write_and_read_base_file() {
        let (_dir, vai_dir, repo_root, ws_id) = setup();
        let fs = DiskMergeFs::new(&vai_dir, &ws_id, &repo_root);

        fs.write_file("base/src/lib.rs", b"fn foo() {}").unwrap();
        let content = fs.read_file("base/src/lib.rs").unwrap();
        assert_eq!(content, b"fn foo() {}");
        assert!(repo_root.join("src/lib.rs").exists());
    }

    #[test]
    fn test_write_and_read_overlay_file() {
        let (_dir, vai_dir, repo_root, ws_id) = setup();
        let fs = DiskMergeFs::new(&vai_dir, &ws_id, &repo_root);

        fs.write_file("overlay/src/lib.rs", b"fn bar() {}").unwrap();
        let content = fs.read_file("overlay/src/lib.rs").unwrap();
        assert_eq!(content, b"fn bar() {}");
    }

    #[test]
    fn test_write_and_read_snapshot_file() {
        let (_dir, vai_dir, repo_root, ws_id) = setup();
        let fs = DiskMergeFs::new(&vai_dir, &ws_id, &repo_root);

        fs.write_file("snapshot/v2/src/lib.rs", b"original").unwrap();
        let content = fs.read_file("snapshot/v2/src/lib.rs").unwrap();
        assert_eq!(content, b"original");
    }

    #[test]
    fn test_exists() {
        let (_dir, vai_dir, repo_root, ws_id) = setup();
        let fs = DiskMergeFs::new(&vai_dir, &ws_id, &repo_root);

        assert!(!fs.exists("base/missing.rs").unwrap());
        fs.write_file("base/present.rs", b"x").unwrap();
        assert!(fs.exists("base/present.rs").unwrap());
    }

    #[test]
    fn test_delete_file() {
        let (_dir, vai_dir, repo_root, ws_id) = setup();
        let fs = DiskMergeFs::new(&vai_dir, &ws_id, &repo_root);

        fs.write_file("base/src/lib.rs", b"x").unwrap();
        assert!(fs.exists("base/src/lib.rs").unwrap());
        fs.delete_file("base/src/lib.rs").unwrap();
        assert!(!fs.exists("base/src/lib.rs").unwrap());
        // No-op on missing file.
        fs.delete_file("base/src/lib.rs").unwrap();
    }

    #[test]
    fn test_list_files_overlay() {
        let (_dir, vai_dir, repo_root, ws_id) = setup();
        let fs = DiskMergeFs::new(&vai_dir, &ws_id, &repo_root);

        assert!(fs.list_files("overlay/").unwrap().is_empty());

        fs.write_file("overlay/src/a.rs", b"a").unwrap();
        fs.write_file("overlay/src/b.rs", b"b").unwrap();

        let mut keys = fs.list_files("overlay/").unwrap();
        keys.sort();
        assert_eq!(keys, vec!["overlay/src/a.rs", "overlay/src/b.rs"]);
    }

    #[test]
    fn test_list_files_snapshot() {
        let (_dir, vai_dir, repo_root, ws_id) = setup();
        let fs = DiskMergeFs::new(&vai_dir, &ws_id, &repo_root);

        assert!(fs.list_files("snapshot/v3/").unwrap().is_empty());

        fs.write_file("snapshot/v3/main.rs", b"m").unwrap();
        let keys = fs.list_files("snapshot/v3/").unwrap();
        assert_eq!(keys, vec!["snapshot/v3/main.rs"]);
    }

    #[test]
    fn test_invalid_namespace_errors() {
        let (_dir, vai_dir, repo_root, ws_id) = setup();
        let fs = DiskMergeFs::new(&vai_dir, &ws_id, &repo_root);

        assert!(fs.read_file("bogus/path").is_err());
        assert!(fs.write_file("bogus/path", b"x").is_err());
        assert!(fs.list_files("bogus/").is_err());
    }
}

// ── S3MergeFs tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod s3_tests {
    use super::*;
    use async_trait::async_trait;
    use chrono::Utc;
    use uuid::Uuid;

    use crate::storage::{FileMetadata, FileStore, StorageError};

    /// Minimal in-memory [`FileStore`] for unit tests.
    struct MockFileStore {
        files: Mutex<HashMap<String, Vec<u8>>>,
    }

    impl MockFileStore {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                files: Mutex::new(HashMap::new()),
            })
        }

        /// Seed a file directly into the store.
        fn seed(&self, path: &str, content: &[u8]) {
            self.files
                .lock()
                .unwrap()
                .insert(path.to_string(), content.to_vec());
        }

        /// Returns the raw content stored at `path`, if any.
        fn raw_get(&self, path: &str) -> Option<Vec<u8>> {
            self.files.lock().unwrap().get(path).cloned()
        }
    }

    #[async_trait]
    impl FileStore for MockFileStore {
        async fn put(
            &self,
            _repo_id: &Uuid,
            path: &str,
            content: &[u8],
        ) -> Result<String, StorageError> {
            self.files
                .lock()
                .unwrap()
                .insert(path.to_string(), content.to_vec());
            Ok("mock_hash".to_string())
        }

        async fn get(&self, _repo_id: &Uuid, path: &str) -> Result<Vec<u8>, StorageError> {
            self.files
                .lock()
                .unwrap()
                .get(path)
                .cloned()
                .ok_or_else(|| StorageError::NotFound(path.to_string()))
        }

        async fn list(
            &self,
            _repo_id: &Uuid,
            prefix: &str,
        ) -> Result<Vec<FileMetadata>, StorageError> {
            let files = self.files.lock().unwrap();
            let result = files
                .iter()
                .filter(|(k, _)| k.starts_with(prefix))
                .map(|(k, v)| FileMetadata {
                    path: k.clone(),
                    size: v.len() as u64,
                    content_hash: "mock_hash".to_string(),
                    updated_at: Utc::now(),
                })
                .collect();
            Ok(result)
        }

        async fn delete(&self, _repo_id: &Uuid, path: &str) -> Result<(), StorageError> {
            self.files.lock().unwrap().remove(path);
            Ok(())
        }

        async fn exists(&self, _repo_id: &Uuid, path: &str) -> Result<bool, StorageError> {
            Ok(self.files.lock().unwrap().contains_key(path))
        }
    }

    fn make_fs(store: Arc<MockFileStore>) -> S3MergeFs {
        S3MergeFs::new(
            store,
            Uuid::nil(),
            "workspaces/ws1/".to_string(),
            "current/".to_string(),
        )
    }

    // ── Key mapping ───────────────────────────────────────────────────────────

    #[test]
    fn s3_path_overlay() {
        let fs = make_fs(MockFileStore::new());
        assert_eq!(
            fs.s3_path("overlay/src/lib.rs").unwrap(),
            "workspaces/ws1/src/lib.rs"
        );
    }

    #[test]
    fn s3_path_base() {
        let fs = make_fs(MockFileStore::new());
        assert_eq!(fs.s3_path("base/src/lib.rs").unwrap(), "current/src/lib.rs");
    }

    #[test]
    fn s3_path_snapshot() {
        let fs = make_fs(MockFileStore::new());
        assert_eq!(
            fs.s3_path("snapshot/v3/src/lib.rs").unwrap(),
            "versions/v3/snapshot/src/lib.rs"
        );
    }

    #[test]
    fn s3_path_invalid_namespace() {
        let fs = make_fs(MockFileStore::new());
        assert!(fs.s3_path("bogus/path").is_err());
    }

    // ── read_file / write_file (require tokio multi-thread) ───────────────────

    #[tokio::test(flavor = "multi_thread")]
    async fn read_from_overlay_prefix() {
        let store = MockFileStore::new();
        store.seed("workspaces/ws1/src/lib.rs", b"fn main() {}");
        let fs = make_fs(store);
        let content = fs.read_file("overlay/src/lib.rs").unwrap();
        assert_eq!(content, b"fn main() {}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn read_from_base_prefix() {
        let store = MockFileStore::new();
        store.seed("current/README.md", b"# readme");
        let fs = make_fs(store);
        let content = fs.read_file("base/README.md").unwrap();
        assert_eq!(content, b"# readme");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn write_goes_to_pending_not_s3() {
        let store = MockFileStore::new();
        let fs = make_fs(Arc::clone(&store));
        fs.write_file("base/out.rs", b"written").unwrap();
        // Nothing in S3 yet.
        assert!(store.raw_get("current/out.rs").is_none());
        // But readable via MergeFs.
        assert_eq!(fs.read_file("base/out.rs").unwrap(), b"written");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn flush_writes_base_to_s3() {
        let store = MockFileStore::new();
        let fs = make_fs(Arc::clone(&store));
        fs.write_file("base/main.rs", b"fn main() {}").unwrap();
        fs.flush().await.unwrap();
        assert_eq!(store.raw_get("current/main.rs").unwrap(), b"fn main() {}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn flush_writes_snapshot_to_s3() {
        let store = MockFileStore::new();
        let fs = make_fs(Arc::clone(&store));
        fs.write_file("snapshot/v5/src/lib.rs", b"old content")
            .unwrap();
        fs.flush().await.unwrap();
        assert_eq!(
            store.raw_get("versions/v5/snapshot/src/lib.rs").unwrap(),
            b"old content"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn flush_deletes_from_s3() {
        let store = MockFileStore::new();
        store.seed("current/to_delete.rs", b"old");
        let fs = make_fs(Arc::clone(&store));
        fs.delete_file("base/to_delete.rs").unwrap();
        fs.flush().await.unwrap();
        assert!(store.raw_get("current/to_delete.rs").is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_files_merges_s3_and_pending() {
        let store = MockFileStore::new();
        store.seed("workspaces/ws1/src/a.rs", b"a");
        let fs = make_fs(store);
        fs.write_file("overlay/src/b.rs", b"b").unwrap();

        let mut keys = fs.list_files("overlay/").unwrap();
        keys.sort();
        assert_eq!(keys, vec!["overlay/src/a.rs", "overlay/src/b.rs"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_files_excludes_pending_deletes() {
        let store = MockFileStore::new();
        store.seed("workspaces/ws1/src/a.rs", b"a");
        store.seed("workspaces/ws1/src/b.rs", b"b");
        let fs = make_fs(store);
        fs.delete_file("overlay/src/a.rs").unwrap();

        let keys = fs.list_files("overlay/").unwrap();
        assert_eq!(keys, vec!["overlay/src/b.rs"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn exists_pending_write() {
        let store = MockFileStore::new();
        let fs = make_fs(store);
        assert!(!fs.exists("base/file.rs").unwrap());
        fs.write_file("base/file.rs", b"x").unwrap();
        assert!(fs.exists("base/file.rs").unwrap());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn pending_write_overrides_s3_read() {
        let store = MockFileStore::new();
        store.seed("current/main.rs", b"original");
        let fs = make_fs(store);
        fs.write_file("base/main.rs", b"modified").unwrap();
        // Should return the pending write, not the S3 version.
        assert_eq!(fs.read_file("base/main.rs").unwrap(), b"modified");
    }
}
