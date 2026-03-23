//! Filesystem implementation of [`FileStore`].
//!
//! [`FilesystemFileStore`] stores files under a local directory tree using the
//! path convention `{storage_root}/{repo_id}/{path}`.  This makes it usable as
//! a server-mode file store when S3 is not available (e.g. development, CI) as
//! well as in other contexts that require a `FileStore` backed by the local
//! filesystem.
//!
//! Unlike [`super::sqlite::SqliteStorage`]'s `FileStore` impl (which is
//! hard-wired to `.vai/files/`), this struct accepts an arbitrary root
//! directory and includes the `repo_id` segment so multiple repositories can
//! coexist under the same root.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use chrono::Utc;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{FileMetadata, FileStore, StorageError};

// ── FilesystemFileStore ────────────────────────────────────────────────────────

/// A [`FileStore`] backed by the local filesystem.
///
/// Files are stored at `{storage_root}/{repo_id}/{path}`.  The directory tree
/// is created on demand.  All operations are synchronous under the hood (the
/// async signatures satisfy the trait requirement but do not spawn threads —
/// this is acceptable for CLI and test contexts; production server code should
/// use the S3 backend instead).
#[derive(Clone, Debug)]
pub struct FilesystemFileStore {
    storage_root: PathBuf,
}

impl FilesystemFileStore {
    /// Creates a new store rooted at `storage_root`.
    ///
    /// The directory does not need to exist yet; it is created lazily on the
    /// first `put` call.
    pub fn new(storage_root: impl Into<PathBuf>) -> Self {
        Self {
            storage_root: storage_root.into(),
        }
    }

    /// Resolves the on-disk path for `(repo_id, path)`.
    ///
    /// Result: `{storage_root}/{repo_id}/{path}`
    fn resolve(&self, repo_id: &Uuid, path: &str) -> PathBuf {
        self.storage_root.join(repo_id.to_string()).join(path)
    }
}

#[async_trait]
impl FileStore for FilesystemFileStore {
    /// Writes `content` to `{storage_root}/{repo_id}/{path}` and returns its
    /// SHA-256 hex digest.
    async fn put(
        &self,
        repo_id: &Uuid,
        path: &str,
        content: &[u8],
    ) -> Result<String, StorageError> {
        let full_path = self.resolve(repo_id, path);
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent).map_err(|e| StorageError::Io(e.to_string()))?;
        }
        let mut file =
            fs::File::create(&full_path).map_err(|e| StorageError::Io(e.to_string()))?;
        file.write_all(content)
            .map_err(|e| StorageError::Io(e.to_string()))?;
        Ok(sha256_hex(content))
    }

    /// Reads and returns the raw bytes stored at `{storage_root}/{repo_id}/{path}`.
    async fn get(&self, repo_id: &Uuid, path: &str) -> Result<Vec<u8>, StorageError> {
        let full_path = self.resolve(repo_id, path);
        fs::read(&full_path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                StorageError::NotFound(format!("file {path}"))
            } else {
                StorageError::Io(e.to_string())
            }
        })
    }

    /// Lists all files under `{storage_root}/{repo_id}/` whose path (relative
    /// to the repo root) starts with `prefix`.
    async fn list(
        &self,
        repo_id: &Uuid,
        prefix: &str,
    ) -> Result<Vec<FileMetadata>, StorageError> {
        let base = self.storage_root.join(repo_id.to_string());
        let mut results = Vec::new();
        collect_files(&base, &base, prefix, &mut results)?;
        Ok(results)
    }

    /// Deletes the file at `{storage_root}/{repo_id}/{path}`.
    async fn delete(&self, repo_id: &Uuid, path: &str) -> Result<(), StorageError> {
        let full_path = self.resolve(repo_id, path);
        fs::remove_file(&full_path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                StorageError::NotFound(format!("file {path}"))
            } else {
                StorageError::Io(e.to_string())
            }
        })
    }

    /// Returns `true` if a file exists at `{storage_root}/{repo_id}/{path}`.
    async fn exists(&self, repo_id: &Uuid, path: &str) -> Result<bool, StorageError> {
        Ok(self.resolve(repo_id, path).exists())
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Returns the SHA-256 hex digest of `data`.
fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Recursively walks `dir` and collects [`FileMetadata`] for files whose path
/// relative to `base` starts with `prefix`.
fn collect_files(
    base: &Path,
    dir: &Path,
    prefix: &str,
    results: &mut Vec<FileMetadata>,
) -> Result<(), StorageError> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(StorageError::Io(e.to_string())),
    };

    for entry in entries {
        let entry = entry.map_err(|e| StorageError::Io(e.to_string()))?;
        let entry_path = entry.path();

        if entry_path.is_dir() {
            collect_files(base, &entry_path, prefix, results)?;
        } else {
            let rel = entry_path
                .strip_prefix(base)
                .map_err(|e| StorageError::Io(e.to_string()))?;
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            if rel_str.starts_with(prefix) {
                let meta = entry.metadata().map_err(|e| StorageError::Io(e.to_string()))?;
                results.push(FileMetadata {
                    path: rel_str.to_string(),
                    size: meta.len(),
                    content_hash: String::new(),
                    updated_at: meta
                        .modified()
                        .ok()
                        .map(|t| t.into())
                        .unwrap_or_else(Utc::now),
                });
            }
        }
    }

    Ok(())
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup() -> (TempDir, FilesystemFileStore, Uuid) {
        let dir = TempDir::new().unwrap();
        let store = FilesystemFileStore::new(dir.path());
        let repo_id = Uuid::new_v4();
        (dir, store, repo_id)
    }

    #[tokio::test]
    async fn put_and_get_roundtrip() {
        let (_dir, store, repo_id) = setup();
        let content = b"hello, filesystem store";
        let hash = store.put(&repo_id, "src/main.rs", content).await.unwrap();
        assert_eq!(hash.len(), 64, "SHA-256 hex is 64 chars");
        let got = store.get(&repo_id, "src/main.rs").await.unwrap();
        assert_eq!(got, content);
    }

    #[tokio::test]
    async fn get_missing_returns_not_found() {
        let (_dir, store, repo_id) = setup();
        let err = store.get(&repo_id, "no/such/file.txt").await.unwrap_err();
        assert!(matches!(err, StorageError::NotFound(_)));
    }

    #[tokio::test]
    async fn delete_file() {
        let (_dir, store, repo_id) = setup();
        store.put(&repo_id, "a.txt", b"data").await.unwrap();
        assert!(store.exists(&repo_id, "a.txt").await.unwrap());
        store.delete(&repo_id, "a.txt").await.unwrap();
        assert!(!store.exists(&repo_id, "a.txt").await.unwrap());
    }

    #[tokio::test]
    async fn delete_missing_returns_not_found() {
        let (_dir, store, repo_id) = setup();
        let err = store.delete(&repo_id, "ghost.txt").await.unwrap_err();
        assert!(matches!(err, StorageError::NotFound(_)));
    }

    #[tokio::test]
    async fn list_with_prefix() {
        let (_dir, store, repo_id) = setup();
        store.put(&repo_id, "src/a.rs", b"a").await.unwrap();
        store.put(&repo_id, "src/b.rs", b"b").await.unwrap();
        store.put(&repo_id, "tests/c.rs", b"c").await.unwrap();

        let src_files = store.list(&repo_id, "src/").await.unwrap();
        assert_eq!(src_files.len(), 2);
        assert!(src_files.iter().all(|f| f.path.starts_with("src/")));

        let all = store.list(&repo_id, "").await.unwrap();
        assert_eq!(all.len(), 3);
    }

    #[tokio::test]
    async fn repos_are_isolated() {
        let (_dir, store, repo_a) = setup();
        let repo_b = Uuid::new_v4();

        store.put(&repo_a, "file.txt", b"from a").await.unwrap();
        // repo_b should not see repo_a's file
        let err = store.get(&repo_b, "file.txt").await.unwrap_err();
        assert!(matches!(err, StorageError::NotFound(_)));

        let listing = store.list(&repo_b, "").await.unwrap();
        assert!(listing.is_empty());
    }

    #[tokio::test]
    async fn put_overwrites_existing_file() {
        let (_dir, store, repo_id) = setup();
        store.put(&repo_id, "f.txt", b"v1").await.unwrap();
        store.put(&repo_id, "f.txt", b"v2").await.unwrap();
        let content = store.get(&repo_id, "f.txt").await.unwrap();
        assert_eq!(content, b"v2");
    }

    #[tokio::test]
    async fn sha256_hash_is_deterministic() {
        let (_dir, store, repo_id) = setup();
        let h1 = store.put(&repo_id, "x.bin", b"data").await.unwrap();
        let repo2 = Uuid::new_v4();
        let h2 = store.put(&repo2, "x.bin", b"data").await.unwrap();
        assert_eq!(h1, h2, "same content → same hash regardless of repo_id");
    }
}
