//! `vai pull` — sync local working directory from a remote vai server.
//!
//! Downloads files changed since the local HEAD version from the server using
//! the `GET /api/repos/:repo/files/pull?since=<version>` endpoint.
//!
//! ## What pull does
//! 1. Resolves connection details from the repo remote config or CLI flags.
//! 2. Reads `.vai/head` to determine the local HEAD version.
//! 3. Calls `GET /api/repos/:repo/files/pull?since=<local_head>`.
//! 4. Writes added/modified files to the working directory.
//! 5. Removes deleted files from the working directory.
//! 6. Updates `.vai/head` to the server's HEAD version.

use std::fs;
use std::path::Path;

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
}

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
        return;
    }

    println!(
        "{} Pulled {} → {}",
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
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Reads `.vai/head`, returning the trimmed version string.
fn read_local_head(vai_dir: &Path) -> Result<String, PullError> {
    let head_path = vai_dir.join("head");
    let raw = fs::read_to_string(&head_path)?;
    Ok(raw.trim().to_string())
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
        };
        print_pull_result(&result);
    }
}
