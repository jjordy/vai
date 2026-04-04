//! `vai pull` — sync local working directory from a remote vai server.
//!
//! Downloads files changed since the local HEAD version from the server using
//! the `GET /api/repos/:repo/files/pull?since=<version>` endpoint.
//!
//! With `--force`, downloads the full file tarball via
//! `GET /api/repos/:repo/files/download`, replaces all tracked files, and
//! preserves ignored paths (`.vai/`, `.git/`, `node_modules/`, etc.).
//!
//! ## What pull does
//! 1. Resolves connection details from the repo remote config or CLI flags.
//! 2. Reads `.vai/head` to determine the local HEAD version.
//! 3. Calls `GET /api/repos/:repo/files/pull?since=<local_head>`.
//! 4. Writes added/modified files to the working directory.
//! 5. Removes deleted files from the working directory.
//! 6. Updates `.vai/head` to the server's HEAD version.

use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path};

use base64::prelude::{Engine, BASE64_STANDARD as BASE64};
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use serde::{Deserialize, Serialize};
use thiserror::Error;

// ── Error type ────────────────────────────────────────────────────────────────

/// Errors that can occur during `vai pull`.
#[derive(Debug, Error)]
pub enum PullError {
    #[error("no remote configured — run `vai remote add <url> --key <key>` or use --from/--key/--repo flags")]
    NoRemote,

    #[error("--repo is required when using --from")]
    MissingRepo,

    #[error("--key is required when using --from")]
    MissingKey,

    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("server error ({status}): {body}")]
    ServerError { status: u16, body: String },

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("base64 decode error: {0}")]
    Base64(#[from] base64::DecodeError),

    #[error("tarball error: {0}")]
    Tarball(String),
}

// ── Server response shapes ────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct PullFileEntry {
    path: String,
    change_type: FileChangeType,
    content_base64: Option<String>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum FileChangeType {
    Added,
    Modified,
    Removed,
}

#[derive(Debug, Deserialize)]
struct FilesPullResponse {
    #[allow(dead_code)]
    base_version: String,
    head_version: String,
    files: Vec<PullFileEntry>,
}

// ── Public config / result types ──────────────────────────────────────────────

/// Connection details required to perform a pull.
///
/// Constructed either from `.vai/config.toml`'s `[remote]` section or from
/// explicit `--from`/`--key`/`--repo` CLI flags.
pub struct PullConfig {
    /// Base URL of the remote vai server, e.g. `http://localhost:7865`.
    pub server_url: String,
    /// API key for authentication.
    pub api_key: String,
    /// Repository name on the server.
    pub repo_name: String,
}

/// Summary returned after a successful pull.
#[derive(Debug, Serialize)]
pub struct PullResult {
    /// The local HEAD version before the pull.
    pub previous_version: String,
    /// The server HEAD version (new local HEAD after pull).
    pub new_version: String,
    /// Files written to disk (added or modified).
    pub files_updated: Vec<String>,
    /// Files removed from disk.
    pub files_removed: Vec<String>,
    /// Already up-to-date — no files were downloaded.
    pub already_up_to_date: bool,
    /// Whether this was a force (full tarball) pull.
    pub force: bool,
}

// ── Directory names always excluded from force-pull cleanup ───────────────────

const FORCE_IGNORE_DIRS: &[&str] = &[
    ".vai", ".git", "target", "node_modules", "dist", "__pycache__",
];

// ── Public API ────────────────────────────────────────────────────────────────

/// Pulls changes from the remote server into the local working directory.
///
/// `repo_root` is the directory that contains `.vai/` and the source files.
/// `config` provides the server URL, API key, and repository name.
pub async fn pull(repo_root: &Path, config: PullConfig) -> Result<PullResult, PullError> {
    let vai_dir = repo_root.join(".vai");

    // ── Read local HEAD ────────────────────────────────────────────────────
    let local_head = read_local_head(&vai_dir)?;

    // ── Fetch changed files from server ───────────────────────────────────
    let client = reqwest::Client::new();
    let url = format!(
        "{}/api/repos/{}/files/pull?since={}",
        config.server_url.trim_end_matches('/'),
        config.repo_name,
        local_head,
    );

    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", config.api_key))
        .send()
        .await?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(PullError::ServerError {
            status: status.as_u16(),
            body,
        });
    }

    let pull_resp: FilesPullResponse = resp.json().await?;

    // Short-circuit: nothing to do.
    if pull_resp.head_version == local_head || pull_resp.files.is_empty() {
        return Ok(PullResult {
            previous_version: local_head.clone(),
            new_version: pull_resp.head_version,
            files_updated: vec![],
            files_removed: vec![],
            already_up_to_date: true,
            force: false,
        });
    }

    // ── Apply changes ──────────────────────────────────────────────────────
    let mut files_updated: Vec<String> = Vec::new();
    let mut files_removed: Vec<String> = Vec::new();

    // Separate into files to write and files to delete.
    let mut to_write: Vec<&PullFileEntry> = Vec::new();
    let mut to_remove: Vec<&PullFileEntry> = Vec::new();

    for entry in &pull_resp.files {
        match entry.change_type {
            FileChangeType::Added | FileChangeType::Modified => to_write.push(entry),
            FileChangeType::Removed => to_remove.push(entry),
        }
    }

    // Write added/modified files.
    if !to_write.is_empty() {
        let pb = ProgressBar::new(to_write.len() as u64);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{prefix:.bold} [{bar:40}] {pos}/{len} {msg}")
                .unwrap_or_else(|_| ProgressStyle::default_bar())
                .progress_chars("=> "),
        );
        pb.set_prefix("Pulling");

        for entry in &to_write {
            pb.set_message(entry.path.clone());
            if let Some(ref b64) = entry.content_base64 {
                let content = BASE64.decode(b64.as_bytes())?;
                let dest = repo_root.join(&entry.path);
                if let Some(parent) = dest.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(&dest, &content)?;
                files_updated.push(entry.path.clone());
            }
            pb.inc(1);
        }
        pb.finish_and_clear();
    }

    // Remove deleted files.
    for entry in &to_remove {
        let dest = repo_root.join(&entry.path);
        if dest.exists() {
            fs::remove_file(&dest)?;
        }
        files_removed.push(entry.path.clone());
    }

    // ── Update local HEAD ──────────────────────────────────────────────────
    fs::write(vai_dir.join("head"), format!("{}\n", pull_resp.head_version))?;

    Ok(PullResult {
        previous_version: local_head,
        new_version: pull_resp.head_version,
        files_updated,
        files_removed,
        already_up_to_date: false,
        force: false,
    })
}

/// Performs a full re-sync from the server by downloading and extracting the
/// complete file tarball via `GET /api/repos/:repo/files/download`.
///
/// Unlike the incremental [`pull`], this replaces every tracked file with the
/// server's current state.  Paths in [`FORCE_IGNORE_DIRS`] (`.git/`,
/// `node_modules/`, `.vai/`, etc.) are never touched.
pub async fn pull_force(repo_root: &Path, config: PullConfig) -> Result<PullResult, PullError> {
    let vai_dir = repo_root.join(".vai");
    let local_head = read_local_head(&vai_dir).unwrap_or_default();

    // ── 1. Fetch the full tarball ──────────────────────────────────────────
    let client = reqwest::Client::new();
    let url = format!(
        "{}/api/repos/{}/files/download",
        config.server_url.trim_end_matches('/'),
        config.repo_name,
    );

    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", config.api_key))
        .send()
        .await?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(PullError::ServerError {
            status: status.as_u16(),
            body,
        });
    }

    // Read the HEAD version from the response header added by the server.
    let new_version = resp
        .headers()
        .get("X-Vai-Head")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown")
        .to_string();

    let gz_bytes = resp.bytes().await?.to_vec();

    // ── 2. Determine which paths the server tarball contains ───────────────
    let server_paths = tarball_paths(&gz_bytes)?;
    let server_path_set: HashSet<&str> = server_paths.iter().map(String::as_str).collect();

    // ── 3. Remove local files not present in the server tarball ───────────
    // Walk the repo root, skipping ignored directories, and delete stale files.
    let files_removed = remove_stale_local_files(repo_root, &server_path_set)?;

    // ── 4. Extract the tarball into the repo root ──────────────────────────
    let files_updated = extract_tarball(repo_root, &gz_bytes)?;

    // ── 5. Update local HEAD ───────────────────────────────────────────────
    if new_version != "unknown" {
        fs::write(vai_dir.join("head"), format!("{}\n", new_version))?;
    }

    Ok(PullResult {
        previous_version: local_head,
        new_version,
        files_updated,
        files_removed,
        already_up_to_date: false,
        force: true,
    })
}

// ── Display ───────────────────────────────────────────────────────────────────

/// Prints a human-readable pull summary to stdout.
pub fn print_pull_result(result: &PullResult) {
    if result.already_up_to_date {
        println!(
            "{} Already up to date ({})",
            "✓".green().bold(),
            result.new_version
        );
        println!(
            "  {} If local files have been modified externally, run {} to force a full re-sync.",
            "hint:".dimmed(),
            "vai pull --force".bold(),
        );
        return;
    }

    let mode = if result.force { " (force)" } else { "" };
    println!(
        "{} Pulled{} {} → {}",
        "✓".green().bold(),
        mode,
        result.previous_version.dimmed(),
        result.new_version.bold(),
    );

    if !result.files_updated.is_empty() {
        println!("  Updated  : {} file(s)", result.files_updated.len());
        for f in &result.files_updated {
            println!("    {} {f}", "+".green());
        }
    }
    if !result.files_removed.is_empty() {
        println!("  Removed  : {} file(s)", result.files_removed.len());
        for f in &result.files_removed {
            println!("    {} {f}", "-".red());
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Reads `.vai/head`, returning the trimmed version string.
fn read_local_head(vai_dir: &Path) -> Result<String, PullError> {
    let head_path = vai_dir.join("head");
    let raw = fs::read_to_string(&head_path)?;
    Ok(raw.trim().to_string())
}

/// Returns `true` if `name` is one of the always-ignored directory names.
fn is_ignored_dir(name: &str) -> bool {
    FORCE_IGNORE_DIRS.contains(&name)
}

/// Walks `repo_root` (skipping ignored directories) and deletes any regular
/// file whose repo-relative path is **not** in `server_paths`.
///
/// Returns the list of repo-relative paths that were deleted.
fn remove_stale_local_files(
    repo_root: &Path,
    server_paths: &HashSet<&str>,
) -> Result<Vec<String>, PullError> {
    let mut removed = Vec::new();
    remove_stale_recursive(repo_root, repo_root, server_paths, &mut removed)?;
    Ok(removed)
}

fn remove_stale_recursive(
    repo_root: &Path,
    current: &Path,
    server_paths: &HashSet<&str>,
    removed: &mut Vec<String>,
) -> Result<(), PullError> {
    for entry in fs::read_dir(current)? {
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
            // Compute repo-relative path using forward slashes.
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
                    fs::remove_file(&path)?;
                    removed.push(rel_str);
                }
            }
        }
    }
    Ok(())
}

/// Parses a gzip-compressed tarball and returns the list of relative file paths
/// it contains (regular files only).
fn tarball_paths(gz_bytes: &[u8]) -> Result<Vec<String>, PullError> {
    use flate2::read::GzDecoder;
    use tar::Archive;

    let decoder = GzDecoder::new(gz_bytes);
    let mut archive = Archive::new(decoder);
    let mut paths = Vec::new();

    for entry_result in archive
        .entries()
        .map_err(|e| PullError::Tarball(format!("cannot read tarball entries: {e}")))?
    {
        let entry = entry_result
            .map_err(|e| PullError::Tarball(format!("invalid tarball entry: {e}")))?;

        if !entry.header().entry_type().is_file() {
            continue;
        }

        let rel = entry
            .path()
            .map_err(|e| PullError::Tarball(format!("invalid path in tarball: {e}")))?
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
fn extract_tarball(dest_dir: &Path, gz_bytes: &[u8]) -> Result<Vec<String>, PullError> {
    use flate2::read::GzDecoder;
    use tar::Archive;

    let decoder = GzDecoder::new(gz_bytes);
    let mut archive = Archive::new(decoder);
    let mut written = Vec::new();

    for entry_result in archive
        .entries()
        .map_err(|e| PullError::Tarball(format!("cannot read tarball entries: {e}")))?
    {
        let mut entry = entry_result
            .map_err(|e| PullError::Tarball(format!("invalid tarball entry: {e}")))?;

        let entry_type = entry.header().entry_type();
        if !entry_type.is_file() && !entry_type.is_dir() {
            continue;
        }

        let rel_path = entry
            .path()
            .map_err(|e| PullError::Tarball(format!("invalid path in tarball: {e}")))?
            .to_path_buf();

        // Safety: reject path traversal.
        for component in rel_path.components() {
            if matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            ) {
                return Err(PullError::Tarball(format!(
                    "unsafe path in tarball: {}",
                    rel_path.display()
                )));
            }
        }

        let dest = dest_dir.join(&rel_path);

        if entry_type.is_dir() {
            fs::create_dir_all(&dest)?;
        } else {
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)?;
            }
            entry
                .unpack(&dest)
                .map_err(|e| PullError::Tarball(format!("cannot unpack '{}': {e}", rel_path.display())))?;
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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_local_head_trims_newline() {
        let dir = tempfile::tempdir().unwrap();
        let vai_dir = dir.path().join(".vai");
        fs::create_dir_all(&vai_dir).unwrap();
        fs::write(vai_dir.join("head"), "v42\n").unwrap();
        let head = read_local_head(&vai_dir).unwrap();
        assert_eq!(head, "v42");
    }

    #[test]
    fn read_local_head_missing_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let vai_dir = dir.path().join(".vai");
        fs::create_dir_all(&vai_dir).unwrap();
        assert!(read_local_head(&vai_dir).is_err());
    }

    #[test]
    fn print_pull_result_up_to_date() {
        // Just verify it doesn't panic.
        let result = PullResult {
            previous_version: "v1".to_string(),
            new_version: "v1".to_string(),
            files_updated: vec![],
            files_removed: vec![],
            already_up_to_date: true,
            force: false,
        };
        print_pull_result(&result);
    }

    #[test]
    fn print_pull_result_with_changes() {
        let result = PullResult {
            previous_version: "v1".to_string(),
            new_version: "v3".to_string(),
            files_updated: vec!["src/lib.rs".to_string()],
            files_removed: vec!["old.rs".to_string()],
            already_up_to_date: false,
            force: false,
        };
        print_pull_result(&result);
    }

    #[test]
    fn print_pull_result_force() {
        let result = PullResult {
            previous_version: "v1".to_string(),
            new_version: "v5".to_string(),
            files_updated: vec!["src/lib.rs".to_string(), "src/main.rs".to_string()],
            files_removed: vec!["stale.rs".to_string()],
            already_up_to_date: false,
            force: true,
        };
        print_pull_result(&result);
    }

    #[test]
    fn is_ignored_dir_recognises_standard_dirs() {
        assert!(is_ignored_dir(".vai"));
        assert!(is_ignored_dir(".git"));
        assert!(is_ignored_dir("node_modules"));
        assert!(is_ignored_dir("target"));
        assert!(!is_ignored_dir("src"));
    }

    #[test]
    fn remove_stale_local_files_keeps_server_files_and_ignored_dirs() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path();

        // Create files
        fs::create_dir_all(root_path.join("src")).unwrap();
        fs::write(root_path.join("src/lib.rs"), b"lib").unwrap();
        fs::write(root_path.join("src/old.rs"), b"old").unwrap();
        fs::write(root_path.join("readme.md"), b"readme").unwrap();

        // Create ignored directories
        fs::create_dir_all(root_path.join(".git")).unwrap();
        fs::write(root_path.join(".git/HEAD"), b"ref: refs/heads/main").unwrap();
        fs::create_dir_all(root_path.join("node_modules/foo")).unwrap();
        fs::write(root_path.join("node_modules/foo/index.js"), b"foo").unwrap();

        // Server tracks only src/lib.rs and readme.md
        let server_paths: HashSet<&str> = ["src/lib.rs", "readme.md"].iter().cloned().collect();
        let removed = remove_stale_local_files(root_path, &server_paths).unwrap();

        // src/old.rs should be removed
        assert!(removed.contains(&"src/old.rs".to_string()));
        assert_eq!(removed.len(), 1);

        // Kept files/dirs
        assert!(root_path.join("src/lib.rs").exists());
        assert!(root_path.join("readme.md").exists());
        assert!(root_path.join(".git/HEAD").exists());
        assert!(root_path.join("node_modules/foo/index.js").exists());
        assert!(!root_path.join("src/old.rs").exists());
    }

    #[test]
    fn tarball_round_trip() {
        use flate2::write::GzEncoder;
        use flate2::Compression;

        // Build a small tarball with two files.
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        {
            let mut archive = tar::Builder::new(&mut encoder);

            let content = b"hello world";
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            archive.append_data(&mut header, "src/hello.rs", content.as_slice()).unwrap();

            let content2 = b"rust code";
            let mut header2 = tar::Header::new_gnu();
            header2.set_size(content2.len() as u64);
            header2.set_mode(0o644);
            header2.set_cksum();
            archive.append_data(&mut header2, "main.rs", content2.as_slice()).unwrap();

            archive.finish().unwrap();
        }
        let gz_bytes = encoder.finish().unwrap();

        // tarball_paths
        let paths = tarball_paths(&gz_bytes).unwrap();
        assert!(paths.contains(&"src/hello.rs".to_string()));
        assert!(paths.contains(&"main.rs".to_string()));

        // extract_tarball
        let dest = tempfile::tempdir().unwrap();
        let written = extract_tarball(dest.path(), &gz_bytes).unwrap();
        assert!(written.contains(&"src/hello.rs".to_string()));
        assert!(written.contains(&"main.rs".to_string()));
        assert_eq!(fs::read(dest.path().join("src/hello.rs")).unwrap(), b"hello world");
        assert_eq!(fs::read(dest.path().join("main.rs")).unwrap(), b"rust code");
    }
}
