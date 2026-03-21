//! Remote repository sync — `vai sync`.
//!
//! Pulls the latest changes from the remote vai server into a local clone.
//! Only files that changed since the last sync are downloaded (incremental).
//!
//! ## What sync does
//! 1. Reads `.vai/remote.toml` for server URL and API key.
//! 2. Reads `.vai/head` to determine the local HEAD version.
//! 3. Calls `GET /api/versions` to retrieve all versions from the server.
//! 4. Identifies versions newer than local HEAD in order.
//! 5. For each newer version, calls `GET /api/versions/:id` to get the list
//!    of changed files.
//! 6. Downloads added/modified files via `GET /api/files/*path`; removes
//!    deleted files locally.
//! 7. Updates `.vai/head` to the server's current HEAD.
//! 8. If there is an active local workspace, checks whether any of the
//!    server-side changes touch files the workspace has modified and warns.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use base64::prelude::{Engine, BASE64_STANDARD as BASE64};
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use serde::{Deserialize, Serialize};
use thiserror::Error;

// ── Error type ────────────────────────────────────────────────────────────────

/// Errors that can occur during `vai sync`.
#[derive(Debug, Error)]
pub enum SyncError {
    #[error("not a cloned repository — no .vai/remote.toml found")]
    NotAClone,

    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("server error ({status}): {body}")]
    ServerError { status: u16, body: String },

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("TOML error: {0}")]
    Toml(String),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("base64 decode error: {0}")]
    Base64(#[from] base64::DecodeError),

    #[error("local HEAD version not found on server — full re-clone may be needed")]
    LocalVersionNotFound,
}

// ── Server response shapes ────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct VersionMeta {
    version_id: String,
}

#[derive(Debug, Deserialize)]
struct VersionChanges {
    file_changes: Vec<FileChange>,
}

#[derive(Debug, Deserialize)]
struct FileChange {
    path: String,
    change_type: FileChangeType,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum FileChangeType {
    Added,
    Modified,
    Removed,
}

#[derive(Debug, Deserialize)]
struct RepoFileListResponse {
    head_version: String,
}

#[derive(Debug, Deserialize)]
struct FileDownloadResponse {
    content_base64: String,
}

// ── Result types ──────────────────────────────────────────────────────────────

/// A file conflict between the server's changes and the local active workspace.
#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceConflict {
    /// Relative file path.
    pub path: String,
    /// Reminder of what the local workspace is trying to do.
    pub workspace_intent: String,
}

/// Summary of a completed sync operation.
#[derive(Debug, Serialize)]
pub struct SyncResult {
    /// Previous local HEAD version.
    pub previous_version: String,
    /// New local HEAD version after sync.
    pub new_version: String,
    /// Files that were downloaded (added or modified).
    pub files_updated: Vec<String>,
    /// Files that were removed.
    pub files_removed: Vec<String>,
    /// Already up-to-date (no versions to sync).
    pub already_up_to_date: bool,
    /// Files in the local active workspace that overlap with server changes.
    pub workspace_conflicts: Vec<WorkspaceConflict>,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Syncs a cloned repository with its remote server.
///
/// Must be called from within (or with the root of) a cloned vai repository.
/// `repo_root` is the directory containing `.vai/`.
pub async fn sync(repo_root: &Path) -> Result<SyncResult, SyncError> {
    let vai_dir = repo_root.join(".vai");

    // ── Load remote config ────────────────────────────────────────────────
    let remote = crate::clone::read_remote_config(&vai_dir).ok_or(SyncError::NotAClone)?;
    let server_url = &remote.server_url;
    let api_key = &remote.api_key;

    // ── Read local HEAD ────────────────────────────────────────────────────
    let local_head = read_local_head(&vai_dir)?;

    // ── Get server's current HEAD and full version list ────────────────────
    let client = reqwest::Client::new();

    // GET /api/repo/files gives us the current server HEAD cheaply.
    let file_list: RepoFileListResponse = get_json(
        &client,
        &format!("{server_url}/api/repo/files"),
        api_key,
    )
    .await?;
    let server_head = file_list.head_version.clone();

    // Short-circuit: nothing to do.
    if local_head == server_head {
        return Ok(SyncResult {
            previous_version: local_head.clone(),
            new_version: local_head,
            files_updated: vec![],
            files_removed: vec![],
            already_up_to_date: true,
            workspace_conflicts: vec![],
        });
    }

    // ── Fetch version list from server ─────────────────────────────────────
    let all_versions: Vec<VersionMeta> = get_json(
        &client,
        &format!("{server_url}/api/versions"),
        api_key,
    )
    .await?;

    // Find the index of local HEAD in the server's version list.
    // list_versions returns versions oldest-first.
    let local_head_pos = all_versions
        .iter()
        .position(|v| v.version_id == local_head);

    let new_versions: Vec<&VersionMeta> = match local_head_pos {
        Some(pos) => all_versions[(pos + 1)..].iter().collect(),
        None => {
            // Local HEAD not on server — full re-clone needed.
            return Err(SyncError::LocalVersionNotFound);
        }
    };

    // ── Collect files changed across all new versions ──────────────────────
    // Later changes win: track the *last* change type per path.
    let mut changes: HashMap<String, FileChangeType> = HashMap::new();

    for v in &new_versions {
        let version_changes: VersionChanges = get_json(
            &client,
            &format!("{server_url}/api/versions/{}", v.version_id),
            api_key,
        )
        .await?;
        for fc in version_changes.file_changes {
            changes.insert(fc.path, fc.change_type);
        }
    }

    // Separate into files to download vs. files to delete.
    let mut to_download: Vec<String> = Vec::new();
    let mut to_remove: Vec<String> = Vec::new();

    for (path, change_type) in &changes {
        match change_type {
            FileChangeType::Added | FileChangeType::Modified => to_download.push(path.clone()),
            FileChangeType::Removed => to_remove.push(path.clone()),
        }
    }
    to_download.sort();
    to_remove.sort();

    // ── Download updated files ─────────────────────────────────────────────
    if !to_download.is_empty() {
        let pb = ProgressBar::new(to_download.len() as u64);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{prefix:.bold} [{bar:40}] {pos}/{len} {msg}")
                .unwrap_or_else(|_| ProgressStyle::default_bar())
                .progress_chars("=> "),
        );
        pb.set_prefix("Syncing");

        for rel_path in &to_download {
            pb.set_message(rel_path.clone());
            let encoded = urlencoding_encode(rel_path);
            let dl: FileDownloadResponse = get_json(
                &client,
                &format!("{server_url}/api/files/{encoded}"),
                api_key,
            )
            .await?;
            let content = BASE64.decode(dl.content_base64.as_bytes())?;
            let local_path = repo_root.join(rel_path);
            if let Some(parent) = local_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&local_path, &content)?;
            pb.inc(1);
        }
        pb.finish_and_clear();
    }

    // ── Remove deleted files ───────────────────────────────────────────────
    for rel_path in &to_remove {
        let local_path = repo_root.join(rel_path);
        if local_path.exists() {
            fs::remove_file(&local_path)?;
        }
    }

    // ── Update local HEAD ──────────────────────────────────────────────────
    fs::write(vai_dir.join("head"), format!("{server_head}\n"))?;

    // ── Check for workspace conflicts ──────────────────────────────────────
    let changed_paths: HashSet<&str> = to_download
        .iter()
        .chain(to_remove.iter())
        .map(|s| s.as_str())
        .collect();

    let workspace_conflicts =
        detect_workspace_conflicts(&vai_dir, &changed_paths);

    Ok(SyncResult {
        previous_version: local_head,
        new_version: server_head,
        files_updated: to_download,
        files_removed: to_remove,
        already_up_to_date: false,
        workspace_conflicts,
    })
}

// ── Workspace conflict detection ──────────────────────────────────────────────

/// Checks whether the active local workspace has modified any of the paths
/// that the server sync just changed.
fn detect_workspace_conflicts(
    vai_dir: &Path,
    changed_paths: &HashSet<&str>,
) -> Vec<WorkspaceConflict> {
    let mut conflicts = Vec::new();

    // Read active workspace ID.
    let active_file = vai_dir.join("workspaces").join("active");
    let active_id = match fs::read_to_string(&active_file) {
        Ok(s) => s.trim().to_string(),
        Err(_) => return conflicts,
    };
    if active_id.is_empty() {
        return conflicts;
    }

    // Read workspace meta.toml for the intent.
    let meta_path = vai_dir
        .join("workspaces")
        .join(&active_id)
        .join("meta.toml");
    let intent = match fs::read_to_string(&meta_path) {
        Ok(raw) => extract_intent_from_toml(&raw),
        Err(_) => return conflicts,
    };

    // Scan the workspace overlay directory for locally modified files.
    let overlay_dir = vai_dir
        .join("workspaces")
        .join(&active_id)
        .join("overlay");
    if !overlay_dir.exists() {
        return conflicts;
    }

    let overlay_paths = collect_overlay_paths(&overlay_dir, &overlay_dir);
    for overlay_path in overlay_paths {
        if changed_paths.contains(overlay_path.as_str()) {
            conflicts.push(WorkspaceConflict {
                path: overlay_path,
                workspace_intent: intent.clone(),
            });
        }
    }

    conflicts
}

/// Recursively collects relative paths of files under `overlay_dir`.
fn collect_overlay_paths(overlay_dir: &Path, base: &Path) -> Vec<String> {
    let mut paths = Vec::new();
    let entries = match fs::read_dir(overlay_dir) {
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

/// Extracts the `intent` field from a minimal TOML string.
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

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Reads `.vai/head` returning the version string (without trailing newline).
fn read_local_head(vai_dir: &Path) -> Result<String, SyncError> {
    let head_path = vai_dir.join("head");
    let raw = fs::read_to_string(&head_path)?;
    Ok(raw.trim().to_string())
}

/// Performs a GET request with Bearer auth and deserialises the JSON body.
async fn get_json<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    url: &str,
    api_key: &str,
) -> Result<T, SyncError> {
    let resp = client
        .get(url)
        .header("Authorization", format!("Bearer {api_key}"))
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(SyncError::ServerError {
            status: status.as_u16(),
            body,
        });
    }
    Ok(resp.json().await?)
}

/// Percent-encodes a relative file path for use in a URL segment.
fn urlencoding_encode(path: &str) -> String {
    path.split('/')
        .map(|segment| {
            let mut out = String::with_capacity(segment.len());
            for byte in segment.bytes() {
                match byte {
                    b'A'..=b'Z'
                    | b'a'..=b'z'
                    | b'0'..=b'9'
                    | b'-'
                    | b'_'
                    | b'.'
                    | b'~' => out.push(byte as char),
                    b => out.push_str(&format!("%{b:02X}")),
                }
            }
            out
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Prints a human-readable sync summary to stdout.
pub fn print_sync_result(result: &SyncResult) {
    if result.already_up_to_date {
        println!("{} Already up to date ({})", "✓".green().bold(), result.new_version);
        return;
    }

    println!(
        "{} Synced {} → {}",
        "✓".green().bold(),
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
    if !result.workspace_conflicts.is_empty() {
        println!(
            "\n  {} Your active workspace has modified files that changed on the server:",
            "⚠".yellow().bold()
        );
        for c in &result.workspace_conflicts {
            println!("    {} (workspace: {})", c.path.yellow(), c.workspace_intent);
        }
        println!(
            "  Consider reviewing these files before submitting your workspace."
        );
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_intent_basic() {
        let toml = r#"
id = "abc"
intent = "refactor auth service"
status = "active"
"#;
        assert_eq!(extract_intent_from_toml(toml), "refactor auth service");
    }

    #[test]
    fn extract_intent_missing() {
        assert_eq!(extract_intent_from_toml("id = \"abc\""), "");
    }

    #[test]
    fn urlencoding_encode_basic() {
        assert_eq!(urlencoding_encode("src/main.rs"), "src/main.rs");
        assert_eq!(
            urlencoding_encode("path/to/my file.rs"),
            "path/to/my%20file.rs"
        );
    }

    #[test]
    fn collect_overlay_paths_empty() {
        let dir = std::env::temp_dir().join(format!("vai-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let paths = collect_overlay_paths(&dir, &dir);
        assert!(paths.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn collect_overlay_paths_nested() {
        let dir = std::env::temp_dir().join(format!("vai-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/main.rs"), b"fn main() {}").unwrap();
        std::fs::write(dir.join("lib.rs"), b"pub mod foo;").unwrap();
        let mut paths = collect_overlay_paths(&dir, &dir);
        paths.sort();
        assert_eq!(paths, vec!["lib.rs", "src/main.rs"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
