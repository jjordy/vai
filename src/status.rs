//! `vai status` — compare local working directory against the server.
//!
//! Fetches a lightweight file manifest (paths + SHA-256 hashes) from the
//! remote server via `GET /api/repos/:repo/files/manifest`, then walks the
//! local working directory and reports which files are:
//!
//! - **Modified** — present in both local and server, but content differs
//! - **Untracked** — present locally only (not on server)
//! - **Missing** — present on server but absent locally
//!
//! Ignored paths (`.vai/`, `.git/`, `node_modules/`, `target/`, etc.) are
//! excluded from the comparison on both sides.

use std::collections::HashMap;
use std::path::Path;

use colored::Colorize;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

// ── Error type ────────────────────────────────────────────────────────────────

/// Errors that can occur during `vai status`.
#[derive(Debug, Error)]
pub enum StatusError {
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
}

// ── Server response shapes ────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ManifestFileEntry {
    path: String,
    sha256: String,
}

#[derive(Debug, Deserialize)]
struct FilesManifestResponse {
    version: String,
    files: Vec<ManifestFileEntry>,
}

// ── Public config / result types ──────────────────────────────────────────────

/// Connection details required to perform a status check.
pub struct StatusConfig {
    /// Base URL of the remote vai server, e.g. `http://localhost:7865`.
    pub server_url: String,
    /// API key for authentication.
    pub api_key: String,
    /// Repository name on the server.
    pub repo_name: String,
}

/// How a file differs between local and server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FileStatusKind {
    /// File exists in both places but content differs.
    Modified,
    /// File exists locally but not on the server.
    Untracked,
    /// File exists on the server but is missing locally.
    Missing,
}

/// A single file difference reported by `vai status`.
#[derive(Debug, Clone, Serialize)]
pub struct FileStatusEntry {
    /// Path relative to the repository root (e.g. `"src/lib.rs"`).
    pub path: String,
    /// How this file differs from the server.
    pub kind: FileStatusKind,
}

/// Summary returned by [`check_status`].
#[derive(Debug, Serialize)]
pub struct StatusResult {
    /// The server URL that was queried.
    pub server_url: String,
    /// The repository name on the server.
    pub repo_name: String,
    /// The HEAD version reported by the server.
    pub server_version: String,
    /// The local HEAD version read from `.vai/head` (or `"unknown"`).
    pub local_version: String,
    /// Files that differ between the local working directory and the server.
    pub files: Vec<FileStatusEntry>,
}

// ── Directory names always excluded from the comparison ───────────────────────

const IGNORE_DIRS: &[&str] = &[
    ".vai", ".git", "target", "node_modules", "dist", "__pycache__",
];

// Re-use the shared secret-file predicate from ignore_rules.
use crate::ignore_rules::is_builtin_secret_file;

// ── Public API ────────────────────────────────────────────────────────────────

/// Compares the local working directory against the remote server state.
///
/// `repo_root` is the directory that contains `.vai/` and the source files.
/// `config` provides the server URL, API key, and repository name.
pub async fn check_status(
    repo_root: &Path,
    config: StatusConfig,
) -> Result<StatusResult, StatusError> {
    let vai_dir = repo_root.join(".vai");

    // ── Read local HEAD ────────────────────────────────────────────────────
    let local_version = std::fs::read_to_string(vai_dir.join("head"))
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "unknown".to_string());

    // ── Fetch server manifest ──────────────────────────────────────────────
    let client = reqwest::Client::new();
    let url = format!(
        "{}/api/repos/{}/files/manifest",
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
        return Err(StatusError::ServerError {
            status: status.as_u16(),
            body,
        });
    }

    let manifest: FilesManifestResponse = resp.json().await?;

    // Build a map of server path → sha256.
    let server_map: HashMap<String, String> = manifest
        .files
        .into_iter()
        .map(|e| (e.path, e.sha256))
        .collect();

    // ── Walk local files ───────────────────────────────────────────────────
    let local_map = collect_local_hashes(repo_root)?;

    // ── Diff ───────────────────────────────────────────────────────────────
    let mut result_files: Vec<FileStatusEntry> = Vec::new();

    // Modified or untracked (local files not on server, or differing hash).
    for (path, local_hash) in &local_map {
        match server_map.get(path) {
            Some(server_hash) if server_hash != local_hash => {
                result_files.push(FileStatusEntry {
                    path: path.clone(),
                    kind: FileStatusKind::Modified,
                });
            }
            None => {
                result_files.push(FileStatusEntry {
                    path: path.clone(),
                    kind: FileStatusKind::Untracked,
                });
            }
            _ => {} // same hash — file is identical
        }
    }

    // Missing (server files absent locally).
    for path in server_map.keys() {
        if !local_map.contains_key(path) {
            result_files.push(FileStatusEntry {
                path: path.clone(),
                kind: FileStatusKind::Missing,
            });
        }
    }

    result_files.sort_by(|a, b| a.path.cmp(&b.path));

    Ok(StatusResult {
        server_url: config.server_url,
        repo_name: config.repo_name,
        server_version: manifest.version,
        local_version,
        files: result_files,
    })
}

/// Prints a human-readable status summary to stdout.
pub fn print_status_result(result: &StatusResult) {
    println!(
        "Server: {} ({} @ {})",
        result.server_url.cyan(),
        result.repo_name.bold(),
        result.server_version.bold(),
    );
    println!("Local:  {}", result.local_version.bold());

    let modified: Vec<_> = result
        .files
        .iter()
        .filter(|f| f.kind == FileStatusKind::Modified)
        .collect();
    let untracked: Vec<_> = result
        .files
        .iter()
        .filter(|f| f.kind == FileStatusKind::Untracked)
        .collect();
    let missing: Vec<_> = result
        .files
        .iter()
        .filter(|f| f.kind == FileStatusKind::Missing)
        .collect();

    if result.files.is_empty() {
        println!();
        println!("{} Working directory matches server state.", "✓".green().bold());
        return;
    }

    if !modified.is_empty() {
        println!();
        println!("Modified (local differs from server):");
        for f in &modified {
            println!("  {} {}", "M".yellow().bold(), f.path);
        }
    }

    if !untracked.is_empty() {
        println!();
        println!("Untracked (local only):");
        for f in &untracked {
            println!("  {} {}", "?".cyan(), f.path);
        }
    }

    if !missing.is_empty() {
        println!();
        println!("Missing (on server, absent locally):");
        for f in &missing {
            println!("  {} {}", "!".red().bold(), f.path);
        }
    }

    println!();
    let total = result.files.len();
    println!(
        "{} file{} differ from server",
        total,
        if total == 1 { "" } else { "s" }
    );
    if !modified.is_empty() || !missing.is_empty() {
        println!(
            "  {} Run {} to sync, or {} to see what changed.",
            "hint:".dimmed(),
            "vai pull".bold(),
            "vai diff".bold(),
        );
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Walks `repo_root`, skipping ignored directories, and returns a map of
/// repo-relative path → lowercase hex SHA-256 hash for each regular file.
fn collect_local_hashes(repo_root: &Path) -> Result<HashMap<String, String>, StatusError> {
    let mut map = HashMap::new();
    collect_recursive(repo_root, repo_root, &mut map)?;
    Ok(map)
}

fn collect_recursive(
    repo_root: &Path,
    current: &Path,
    map: &mut HashMap<String, String>,
) -> Result<(), StatusError> {
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
            collect_recursive(repo_root, &path, map)?;
        } else if path.is_file() {
            if is_builtin_secret_file(&name) {
                continue;
            }
            if let Ok(rel) = path.strip_prefix(repo_root) {
                let rel_str = rel
                    .components()
                    .filter_map(|c| match c {
                        std::path::Component::Normal(s) => s.to_str(),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("/");

                if rel_str.is_empty() {
                    continue;
                }

                let content = std::fs::read(&path)?;
                let mut hasher = Sha256::new();
                hasher.update(&content);
                let hash = format!("{:x}", hasher.finalize());
                map.insert(rel_str, hash);
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
    fn collect_local_hashes_skips_ignored_dirs() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path();

        // Regular files.
        fs::create_dir_all(root_path.join("src")).unwrap();
        fs::write(root_path.join("src/lib.rs"), b"lib").unwrap();
        fs::write(root_path.join("readme.md"), b"readme").unwrap();

        // Ignored directories.
        fs::create_dir_all(root_path.join(".vai")).unwrap();
        fs::write(root_path.join(".vai/head"), b"v1").unwrap();
        fs::create_dir_all(root_path.join("target/debug")).unwrap();
        fs::write(root_path.join("target/debug/binary"), b"bin").unwrap();

        let map = collect_local_hashes(root_path).unwrap();
        assert!(map.contains_key("src/lib.rs"));
        assert!(map.contains_key("readme.md"));
        assert!(!map.contains_key(".vai/head"));
        assert!(!map.contains_key("target/debug/binary"));
    }

    #[test]
    fn collect_local_hashes_skips_secret_files() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path();

        fs::write(root_path.join("src.rs"), b"code").unwrap();
        fs::write(root_path.join(".env"), b"SECRET=abc").unwrap();
        fs::write(root_path.join(".env.local"), b"SECRET=local").unwrap();
        fs::write(root_path.join(".env.production"), b"SECRET=prod").unwrap();
        fs::write(root_path.join("server.key"), b"-----BEGIN").unwrap();
        fs::write(root_path.join("cert.pem"), b"-----BEGIN").unwrap();
        fs::write(root_path.join("id_rsa"), b"-----BEGIN").unwrap();
        fs::write(root_path.join("id_ed25519"), b"-----BEGIN").unwrap();

        let map = collect_local_hashes(root_path).unwrap();
        assert!(map.contains_key("src.rs"), "regular file should be tracked");
        assert!(!map.contains_key(".env"), ".env must not be tracked");
        assert!(!map.contains_key(".env.local"));
        assert!(!map.contains_key(".env.production"));
        assert!(!map.contains_key("server.key"));
        assert!(!map.contains_key("cert.pem"));
        assert!(!map.contains_key("id_rsa"));
        assert!(!map.contains_key("id_ed25519"));
    }

    #[test]
    fn collect_local_hashes_computes_correct_sha256() {
        let root = tempfile::tempdir().unwrap();
        let content = b"hello world";
        fs::write(root.path().join("hello.txt"), content).unwrap();

        let map = collect_local_hashes(root.path()).unwrap();
        let hash = map.get("hello.txt").expect("file not found");

        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(content);
        let expected = format!("{:x}", h.finalize());
        assert_eq!(hash, &expected);
    }

    #[test]
    fn print_status_result_clean() {
        let result = StatusResult {
            server_url: "http://localhost:7865".to_string(),
            repo_name: "myrepo".to_string(),
            server_version: "v10".to_string(),
            local_version: "v10".to_string(),
            files: vec![],
        };
        print_status_result(&result); // should not panic
    }

    #[test]
    fn print_status_result_with_differences() {
        let result = StatusResult {
            server_url: "http://localhost:7865".to_string(),
            repo_name: "myrepo".to_string(),
            server_version: "v10".to_string(),
            local_version: "v9".to_string(),
            files: vec![
                FileStatusEntry { path: "src/lib.rs".to_string(), kind: FileStatusKind::Modified },
                FileStatusEntry { path: "new.rs".to_string(), kind: FileStatusKind::Untracked },
                FileStatusEntry { path: "deleted.rs".to_string(), kind: FileStatusKind::Missing },
            ],
        };
        print_status_result(&result); // should not panic
    }
}
