//! `vai diff` — show unified diffs between local files and server state.
//!
//! Fetches the server version of modified files via
//! `GET /api/repos/:repo/files/*path` and generates colourised unified diffs
//! against local content.
//!
//! ## What diff does
//! 1. Runs the same manifest comparison as [`crate::status`] to find modified
//!    files (files present on both sides with differing SHA-256 hashes).
//! 2. For each such file (or a single path when `path_filter` is set),
//!    fetches the server file content.
//! 3. Generates a standard unified diff via the `diffy` crate.
//! 4. Colorizes the output for terminal display.
//!
//! Missing (server-only) and untracked (local-only) files are not diffed; they
//! are reported as a header line only.

use std::collections::HashMap;
use std::path::Path;

use base64::prelude::{Engine, BASE64_STANDARD as BASE64};
use colored::Colorize;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

// ── Error type ────────────────────────────────────────────────────────────────

/// Errors that can occur during `vai diff`.
#[derive(Debug, Error)]
pub enum RemoteDiffError {
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

#[derive(Debug, Deserialize)]
struct FileDownloadResponse {
    content_base64: String,
}

// ── Public config / result types ──────────────────────────────────────────────

/// Connection details required to compute diffs.
pub struct DiffConfig {
    /// Base URL of the remote vai server, e.g. `http://localhost:7865`.
    pub server_url: String,
    /// API key for authentication.
    pub api_key: String,
    /// Repository name on the server.
    pub repo_name: String,
    /// When set, only diff this path (relative to repo root). When `None`,
    /// diff all modified files.
    pub path_filter: Option<String>,
}

/// Kind of difference for a single file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DiffKind {
    /// File exists on both sides with differing content; `unified_diff` is set.
    Modified,
    /// File exists locally only.
    Untracked,
    /// File exists on server only.
    Missing,
}

/// Diff result for a single file.
#[derive(Debug, Clone, Serialize)]
pub struct FileDiffEntry {
    /// Path relative to repo root.
    pub path: String,
    /// How this file differs from the server.
    pub kind: DiffKind,
    /// Unified diff text (only set for [`DiffKind::Modified`] files).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unified_diff: Option<String>,
}

/// Summary returned by [`compute_diff`].
#[derive(Debug, Serialize)]
pub struct DiffResult {
    /// The server URL that was queried.
    pub server_url: String,
    /// The repository name on the server.
    pub repo_name: String,
    /// The HEAD version reported by the server.
    pub server_version: String,
    /// Per-file diff entries.
    pub files: Vec<FileDiffEntry>,
}

// ── Directory names always excluded from the walk ─────────────────────────────

const IGNORE_DIRS: &[&str] = &[
    ".vai", ".git", "target", "node_modules", "dist", "__pycache__",
];

// ── Public API ────────────────────────────────────────────────────────────────

/// Computes diffs between the local working directory and the remote server.
///
/// `repo_root` is the directory that contains `.vai/` and the source files.
/// `config` provides the server URL, API key, repository name, and optional
/// path filter.
pub async fn compute_diff(
    repo_root: &Path,
    config: DiffConfig,
) -> Result<DiffResult, RemoteDiffError> {
    let client = reqwest::Client::new();
    let base = config.server_url.trim_end_matches('/');

    // ── Fetch server manifest ──────────────────────────────────────────────
    let manifest_url = format!("{}/api/repos/{}/files/manifest", base, config.repo_name);
    let resp = client
        .get(&manifest_url)
        .header("Authorization", format!("Bearer {}", config.api_key))
        .send()
        .await?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(RemoteDiffError::ServerError {
            status: status.as_u16(),
            body,
        });
    }

    let manifest: FilesManifestResponse = resp.json().await?;
    let server_map: HashMap<String, String> = manifest
        .files
        .into_iter()
        .map(|e| (e.path, e.sha256))
        .collect();

    // ── Walk local files ───────────────────────────────────────────────────
    let local_map = collect_local_hashes(repo_root)?;

    // ── Determine which files to diff ──────────────────────────────────────
    let mut entries: Vec<FileDiffEntry> = Vec::new();

    let paths_to_check: Vec<String> = if let Some(ref filter) = config.path_filter {
        vec![filter.clone()]
    } else {
        // All modified files from the union of local and server keys.
        let mut all: Vec<String> = local_map
            .keys()
            .chain(server_map.keys())
            .cloned()
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        all.sort();
        all
    };

    for path in paths_to_check {
        let local_hash = local_map.get(&path);
        let server_hash = server_map.get(&path);

        match (local_hash, server_hash) {
            (Some(lh), Some(sh)) if lh == sh => {
                // Identical — skip.
            }
            (Some(_), Some(_)) => {
                // Modified — fetch server content and generate diff.
                let unified = fetch_and_diff(
                    &client,
                    base,
                    &config.repo_name,
                    &config.api_key,
                    &path,
                    repo_root,
                )
                .await?;
                entries.push(FileDiffEntry {
                    path,
                    kind: DiffKind::Modified,
                    unified_diff: Some(unified),
                });
            }
            (Some(_), None) => {
                entries.push(FileDiffEntry {
                    path,
                    kind: DiffKind::Untracked,
                    unified_diff: None,
                });
            }
            (None, Some(_)) => {
                entries.push(FileDiffEntry {
                    path,
                    kind: DiffKind::Missing,
                    unified_diff: None,
                });
            }
            (None, None) => {}
        }
    }

    Ok(DiffResult {
        server_url: config.server_url,
        repo_name: config.repo_name,
        server_version: manifest.version,
        files: entries,
    })
}

/// Prints a colorized unified diff to stdout.
pub fn print_diff_result(result: &DiffResult) {
    if result.files.is_empty() {
        println!("{} Working directory matches server state.", "✓".green().bold());
        return;
    }

    for entry in &result.files {
        match entry.kind {
            DiffKind::Modified => {
                // File header mimicking `git diff`.
                println!(
                    "{}",
                    format!("diff --vai a/{} b/{}", entry.path, entry.path).bold()
                );
                println!("{}", format!("--- a/{}", entry.path).red());
                println!("{}", format!("+++ b/{}", entry.path).green());

                if let Some(ref text) = entry.unified_diff {
                    print_colorized_diff(text);
                }
            }
            DiffKind::Untracked => {
                println!(
                    "{}",
                    format!("diff --vai /dev/null b/{}", entry.path).bold()
                );
                println!("{}", "--- /dev/null".red());
                println!("{}", format!("+++ b/{}", entry.path).green());
                // Read local file and print all lines as additions.
                if let Some(ref unified) = entry.unified_diff {
                    print_colorized_diff(unified);
                } else {
                    println!("{}", "(new file — content not shown)".dimmed());
                }
            }
            DiffKind::Missing => {
                println!(
                    "{}",
                    format!("diff --vai a/{} /dev/null", entry.path).bold()
                );
                println!("{}", format!("--- a/{}", entry.path).red());
                println!("{}", "+++ /dev/null".green());
                println!("{}", "(file deleted on server)".dimmed());
            }
        }
        println!();
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Fetches `path` from the server and generates a unified diff against the
/// local file.
async fn fetch_and_diff(
    client: &reqwest::Client,
    base: &str,
    repo_name: &str,
    api_key: &str,
    path: &str,
    repo_root: &Path,
) -> Result<String, RemoteDiffError> {
    let url = format!("{}/api/repos/{}/files/{}", base, repo_name, path);
    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .send()
        .await?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(RemoteDiffError::ServerError {
            status: status.as_u16(),
            body,
        });
    }

    let dl: FileDownloadResponse = resp.json().await?;
    let server_bytes = BASE64.decode(&dl.content_base64)?;
    let server_text = String::from_utf8_lossy(&server_bytes);

    let local_path = repo_root.join(path);
    let local_bytes = std::fs::read(&local_path)?;
    let local_text = String::from_utf8_lossy(&local_bytes);

    let patch = diffy::create_patch(&server_text, &local_text);
    Ok(patch.to_string())
}

/// Prints a unified diff with ANSI colours: green for `+` lines, red for `-`
/// lines, and cyan for `@@` hunk headers.
fn print_colorized_diff(diff_text: &str) {
    for line in diff_text.lines() {
        if line.starts_with("+++") || line.starts_with("---") {
            // Already printed by caller as headers; skip embedded headers.
            continue;
        } else if line.starts_with("@@") {
            println!("{}", line.cyan());
        } else if line.starts_with('+') {
            println!("{}", line.green());
        } else if line.starts_with('-') {
            println!("{}", line.red());
        } else {
            println!("{}", line);
        }
    }
}

/// Walks `repo_root`, skipping ignored directories, and returns a map of
/// repo-relative path → lowercase hex SHA-256 hash for each regular file.
fn collect_local_hashes(repo_root: &Path) -> Result<HashMap<String, String>, RemoteDiffError> {
    let mut map = HashMap::new();
    collect_recursive(repo_root, repo_root, &mut map)?;
    Ok(map)
}

fn collect_recursive(
    repo_root: &Path,
    current: &Path,
    map: &mut HashMap<String, String>,
) -> Result<(), RemoteDiffError> {
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
    fn print_colorized_diff_does_not_panic() {
        let diff = "\
@@ -1,3 +1,3 @@\n\
 context\n\
-old line\n\
+new line\n\
 context\n";
        print_colorized_diff(diff); // should not panic
    }

    #[test]
    fn collect_local_hashes_skips_ignored_dirs() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path();

        fs::create_dir_all(root_path.join("src")).unwrap();
        fs::write(root_path.join("src/lib.rs"), b"lib").unwrap();
        fs::create_dir_all(root_path.join(".vai")).unwrap();
        fs::write(root_path.join(".vai/head"), b"v1").unwrap();
        fs::create_dir_all(root_path.join("target/debug")).unwrap();
        fs::write(root_path.join("target/debug/binary"), b"bin").unwrap();

        let map = collect_local_hashes(root_path).unwrap();
        assert!(map.contains_key("src/lib.rs"));
        assert!(!map.contains_key(".vai/head"));
        assert!(!map.contains_key("target/debug/binary"));
    }

    #[test]
    fn print_diff_result_clean() {
        let result = DiffResult {
            server_url: "http://localhost:7865".to_string(),
            repo_name: "myrepo".to_string(),
            server_version: "v5".to_string(),
            files: vec![],
        };
        print_diff_result(&result); // should not panic
    }

    #[test]
    fn print_diff_result_with_entries() {
        let result = DiffResult {
            server_url: "http://localhost:7865".to_string(),
            repo_name: "myrepo".to_string(),
            server_version: "v5".to_string(),
            files: vec![
                FileDiffEntry {
                    path: "src/lib.rs".to_string(),
                    kind: DiffKind::Modified,
                    unified_diff: Some(
                        "@@ -1,2 +1,2 @@\n context\n-old\n+new\n".to_string(),
                    ),
                },
                FileDiffEntry {
                    path: "new.rs".to_string(),
                    kind: DiffKind::Untracked,
                    unified_diff: None,
                },
                FileDiffEntry {
                    path: "gone.rs".to_string(),
                    kind: DiffKind::Missing,
                    unified_diff: None,
                },
            ],
        };
        print_diff_result(&result); // should not panic
    }
}
