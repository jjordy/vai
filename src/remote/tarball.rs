//! Tarball building and extraction helpers shared between push and pull.

use std::collections::HashSet;
use std::path::{Component, Path};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use flate2::{write::GzEncoder, Compression};

use super::RemoteError;

/// Directory names always excluded from snapshots and force-pull cleanup.
pub(super) const IGNORE_DIRS: &[&str] = &[
    ".vai", ".git", "target", "node_modules", "dist", "__pycache__",
];

/// Returns `true` if `name` is one of the always-ignored directory names.
pub(super) fn is_ignored_dir(name: &str) -> bool {
    IGNORE_DIRS.contains(&name)
}

use crate::ignore_rules::is_builtin_secret_file;

// ── Tarball building ──────────────────────────────────────────────────────────

/// Builds a gzip tarball containing every file under `repo_dir`,
/// excluding ignored directories (`.vai/`, `.git/`, `target/`, etc.).
pub(super) fn build_full_tarball(repo_dir: &Path) -> Result<Vec<u8>, RemoteError> {
    let buf = Vec::new();
    let gz = GzEncoder::new(buf, Compression::default());
    let mut tar = tar::Builder::new(gz);

    append_dir_to_tar(&mut tar, repo_dir, repo_dir)?;

    let gz = tar.into_inner().map_err(RemoteError::Io)?;
    gz.finish().map_err(RemoteError::Io)
}

fn append_dir_to_tar<W: std::io::Write>(
    tar: &mut tar::Builder<W>,
    dir: &Path,
    base: &Path,
) -> Result<(), RemoteError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(RemoteError::Io(e)),
    };
    for entry in entries {
        let entry = entry.map_err(RemoteError::Io)?;
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if path.is_dir() {
            if IGNORE_DIRS.contains(&name_str.as_ref()) {
                continue;
            }
            append_dir_to_tar(tar, &path, base)?;
        } else {
            if is_builtin_secret_file(name_str.as_ref()) {
                continue;
            }
            let rel = path
                .strip_prefix(base)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            let data = std::fs::read(&path).map_err(RemoteError::Io)?;
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(file_mode_for_path(&path, &data));
            header.set_cksum();
            tar.append_data(&mut header, &rel, data.as_slice())
                .map_err(RemoteError::Io)?;
        }
    }
    Ok(())
}

/// Returns the Unix mode bits to use for a file in a tarball.
#[cfg(unix)]
fn file_mode_for_path(path: &Path, content: &[u8]) -> u32 {
    std::fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o777)
        .unwrap_or_else(|_| if content.starts_with(b"#!") { 0o755 } else { 0o644 })
}

#[cfg(not(unix))]
fn file_mode_for_path(_path: &Path, content: &[u8]) -> u32 {
    if content.starts_with(b"#!") { 0o755 } else { 0o644 }
}

// ── File hashing ──────────────────────────────────────────────────────────────

/// Walks `repo_root`, skipping ignored directories, and returns a map of
/// repo-relative path → lowercase hex SHA-256 hash for each regular file.
pub(super) fn collect_local_hashes(
    repo_root: &Path,
) -> Result<std::collections::HashMap<String, String>, RemoteError> {
    use sha2::{Digest, Sha256};

    let mut map = std::collections::HashMap::new();
    collect_recursive(repo_root, repo_root, &mut map, &mut |path| {
        let content = std::fs::read(path)?;
        let mut hasher = Sha256::new();
        hasher.update(&content);
        Ok(format!("{:x}", hasher.finalize()))
    })?;
    Ok(map)
}

fn collect_recursive(
    repo_root: &Path,
    current: &Path,
    map: &mut std::collections::HashMap<String, String>,
    hasher: &mut dyn FnMut(&Path) -> Result<String, std::io::Error>,
) -> Result<(), RemoteError> {
    for entry in std::fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();

        if path.is_dir() {
            if IGNORE_DIRS.contains(&name.as_str()) {
                continue;
            }
            collect_recursive(repo_root, &path, map, hasher)?;
        } else if path.is_file() {
            if is_builtin_secret_file(&name) {
                continue;
            }
            if let Ok(rel) = path.strip_prefix(repo_root) {
                let rel_str = rel
                    .components()
                    .filter_map(|c| match c {
                        Component::Normal(s) => s.to_str(),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("/");

                if rel_str.is_empty() {
                    continue;
                }

                let hash = hasher(&path)?;
                map.insert(rel_str, hash);
            }
        }
    }
    Ok(())
}

// ── Tarball extraction ────────────────────────────────────────────────────────

/// Parses a gzip-compressed tarball and returns the list of relative file paths
/// it contains (regular files only).
pub(super) fn tarball_paths(gz_bytes: &[u8]) -> Result<Vec<String>, RemoteError> {
    use flate2::read::GzDecoder;
    use tar::Archive;

    let decoder = GzDecoder::new(gz_bytes);
    let mut archive = Archive::new(decoder);
    let mut paths = Vec::new();

    for entry_result in archive
        .entries()
        .map_err(|e| RemoteError::Tarball(format!("cannot read tarball entries: {e}")))?
    {
        let entry = entry_result
            .map_err(|e| RemoteError::Tarball(format!("invalid tarball entry: {e}")))?;

        if !entry.header().entry_type().is_file() {
            continue;
        }

        let rel = entry
            .path()
            .map_err(|e| RemoteError::Tarball(format!("invalid path in tarball: {e}")))?
            .to_string_lossy()
            .replace('\\', "/");

        paths.push(rel);
    }

    Ok(paths)
}

/// Extracts a gzip-compressed tarball into `dest_dir`, returning the list of
/// repo-relative paths written.
///
/// Rejects path traversal attempts (`..\`, absolute paths, etc.).
pub(super) fn extract_tarball(dest_dir: &Path, gz_bytes: &[u8]) -> Result<Vec<String>, RemoteError> {
    use flate2::read::GzDecoder;
    use tar::Archive;

    let decoder = GzDecoder::new(gz_bytes);
    let mut archive = Archive::new(decoder);
    let mut written = Vec::new();

    for entry_result in archive
        .entries()
        .map_err(|e| RemoteError::Tarball(format!("cannot read tarball entries: {e}")))?
    {
        let mut entry = entry_result
            .map_err(|e| RemoteError::Tarball(format!("invalid tarball entry: {e}")))?;

        let entry_type = entry.header().entry_type();
        if !entry_type.is_file() && !entry_type.is_dir() {
            continue;
        }

        let rel_path = entry
            .path()
            .map_err(|e| RemoteError::Tarball(format!("invalid path in tarball: {e}")))?
            .to_path_buf();

        // Safety: reject path traversal.
        for component in rel_path.components() {
            if matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            ) {
                return Err(RemoteError::Tarball(format!(
                    "unsafe path in tarball: {}",
                    rel_path.display()
                )));
            }
        }

        let dest = dest_dir.join(&rel_path);

        if entry_type.is_dir() {
            std::fs::create_dir_all(&dest)?;
        } else {
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            entry
                .unpack(&dest)
                .map_err(|e| RemoteError::Tarball(format!("cannot unpack '{}': {e}", rel_path.display())))?;
            written.push(
                rel_path
                    .components()
                    .filter_map(|c| match c {
                        Component::Normal(s) => s.to_str(),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("/"),
            );
        }
    }

    Ok(written)
}

// ── Force-pull helpers ────────────────────────────────────────────────────────

/// Sets the executable bit on `path` if `content` begins with a shebang (`#!`).
///
/// On non-Unix platforms this is a no-op.
#[cfg(unix)]
pub(super) fn set_executable_if_shebang(path: &Path, content: &[u8]) -> Result<(), RemoteError> {
    if content.starts_with(b"#!") {
        let meta = std::fs::metadata(path)?;
        let mut perms = meta.permissions();
        perms.set_mode(perms.mode() | 0o111);
        std::fs::set_permissions(path, perms)?;
    }
    Ok(())
}

#[cfg(not(unix))]
pub(super) fn set_executable_if_shebang(_path: &Path, _content: &[u8]) -> Result<(), RemoteError> {
    Ok(())
}

/// Walks `repo_root` (skipping ignored directories) and deletes any regular
/// file whose repo-relative path is **not** in `server_paths`.
///
/// Returns the list of repo-relative paths that were deleted.
pub(super) fn remove_stale_local_files(
    repo_root: &Path,
    server_paths: &HashSet<&str>,
) -> Result<Vec<String>, RemoteError> {
    let mut removed = Vec::new();
    remove_stale_recursive(repo_root, repo_root, server_paths, &mut removed)?;
    Ok(removed)
}

fn remove_stale_recursive(
    repo_root: &Path,
    current: &Path,
    server_paths: &HashSet<&str>,
    removed: &mut Vec<String>,
) -> Result<(), RemoteError> {
    for entry in std::fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();

        if path.is_dir() {
            if is_ignored_dir(&name) {
                continue;
            }
            remove_stale_recursive(repo_root, &path, server_paths, removed)?;
        } else if path.is_file() {
            if let Ok(rel) = path.strip_prefix(repo_root) {
                let rel_str = rel
                    .components()
                    .filter_map(|c| match c {
                        Component::Normal(s) => s.to_str(),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("/");

                if !server_paths.contains(rel_str.as_str()) {
                    std::fs::remove_file(&path)?;
                    removed.push(rel_str);
                }
            }
        }
    }
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn build_full_tarball_includes_files_and_skips_ignored() {
        use flate2::read::GzDecoder;
        use tar::Archive;

        let root = tempfile::tempdir().unwrap();
        let root_path = root.path();

        fs::create_dir_all(root_path.join("src")).unwrap();
        fs::write(root_path.join("src/lib.rs"), b"lib code").unwrap();
        fs::write(root_path.join("readme.md"), b"readme").unwrap();

        fs::create_dir_all(root_path.join(".git")).unwrap();
        fs::write(root_path.join(".git/HEAD"), b"ref").unwrap();
        fs::create_dir_all(root_path.join("target/debug")).unwrap();
        fs::write(root_path.join("target/debug/binary"), b"bin").unwrap();
        fs::create_dir_all(root_path.join(".vai")).unwrap();
        fs::write(root_path.join(".vai/head"), b"v1").unwrap();

        let tarball = build_full_tarball(root_path).unwrap();

        let decoder = GzDecoder::new(tarball.as_slice());
        let mut archive = Archive::new(decoder);
        let paths: Vec<String> = archive
            .entries()
            .unwrap()
            .filter_map(|e| e.ok())
            .filter_map(|e| e.path().ok().map(|p| p.to_string_lossy().replace('\\', "/")))
            .collect();

        assert!(paths.contains(&"src/lib.rs".to_string()));
        assert!(paths.contains(&"readme.md".to_string()));
        assert!(!paths.iter().any(|p| p.starts_with(".git/")));
        assert!(!paths.iter().any(|p| p.starts_with("target/")));
        assert!(!paths.iter().any(|p| p.starts_with(".vai/")));
    }

    #[test]
    fn collect_local_hashes_skips_ignored_dirs() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path();

        fs::create_dir_all(root_path.join("src")).unwrap();
        fs::write(root_path.join("src/main.rs"), b"fn main() {}").unwrap();
        fs::create_dir_all(root_path.join(".vai")).unwrap();
        fs::write(root_path.join(".vai/head"), b"v1").unwrap();
        fs::create_dir_all(root_path.join("target/debug")).unwrap();
        fs::write(root_path.join("target/debug/app"), b"binary").unwrap();

        let map = collect_local_hashes(root_path).unwrap();
        assert!(map.contains_key("src/main.rs"));
        assert!(!map.contains_key(".vai/head"));
        assert!(!map.contains_key("target/debug/app"));
    }

    #[test]
    fn tarball_round_trip() {
        use flate2::write::GzEncoder;
        use flate2::Compression;

        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        {
            let mut archive = tar::Builder::new(&mut encoder);

            let content = b"hello world";
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            archive.append_data(&mut header, "src/hello.rs", content.as_slice()).unwrap();

            archive.finish().unwrap();
        }
        let gz_bytes = encoder.finish().unwrap();

        let paths = tarball_paths(&gz_bytes).unwrap();
        assert!(paths.contains(&"src/hello.rs".to_string()));

        let dest = tempfile::tempdir().unwrap();
        let written = extract_tarball(dest.path(), &gz_bytes).unwrap();
        assert!(written.contains(&"src/hello.rs".to_string()));
        assert_eq!(fs::read(dest.path().join("src/hello.rs")).unwrap(), b"hello world");
    }

    #[test]
    fn remove_stale_local_files_keeps_server_files_and_ignored_dirs() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path();

        fs::create_dir_all(root_path.join("src")).unwrap();
        fs::write(root_path.join("src/lib.rs"), b"lib").unwrap();
        fs::write(root_path.join("src/old.rs"), b"old").unwrap();
        fs::write(root_path.join("readme.md"), b"readme").unwrap();
        fs::create_dir_all(root_path.join(".git")).unwrap();
        fs::write(root_path.join(".git/HEAD"), b"ref: refs/heads/main").unwrap();

        let server_paths: HashSet<&str> = ["src/lib.rs", "readme.md"].iter().cloned().collect();
        let removed = remove_stale_local_files(root_path, &server_paths).unwrap();

        assert!(removed.contains(&"src/old.rs".to_string()));
        assert_eq!(removed.len(), 1);
        assert!(root_path.join("src/lib.rs").exists());
        assert!(root_path.join(".git/HEAD").exists());
    }
}
