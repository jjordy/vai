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
//! - `S3MergeFs` — reads from S3, buffers writes in memory (server mode, separate crate feature)

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

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
