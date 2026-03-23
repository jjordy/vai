//! Remote repository cloning — `vai clone vai://<host>:<port>/<repo>`.
//!
//! Cloning downloads the current state of a remote vai repository into a new
//! local directory and writes a `.vai/remote.toml` so that subsequent commands
//! (`vai status`, `vai sync`, etc.) know how to reach the server.
//!
//! ## What a clone does
//! 1. Parses the `vai://…` URL into an HTTP base URL.
//! 2. Hits `GET /api/status` to verify the server is reachable and get the
//!    repo name and HEAD version.
//! 3. Hits `GET /api/repo/files` to get the list of files to download.
//! 4. Downloads each file via `GET /api/files/<path>` (base64-encoded) and
//!    writes it to the local directory.
//! 5. Creates a minimal `.vai/` directory with `config.toml`, `head`, and
//!    `remote.toml`.

use std::fs;
use std::path::{Path, PathBuf};

use base64::prelude::{Engine, BASE64_STANDARD as BASE64};
use chrono::Utc;
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

// ── Error type ────────────────────────────────────────────────────────────────

/// Errors that can occur during `vai clone`.
#[derive(Debug, Error)]
pub enum CloneError {
    #[error("invalid vai URL '{0}': expected vai://<host>:<port>/<repo>")]
    InvalidUrl(String),

    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("server error ({status}): {body}")]
    ServerError { status: u16, body: String },

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("TOML serialization error: {0}")]
    TomlSer(#[from] toml::ser::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("base64 decode error: {0}")]
    Base64(#[from] base64::DecodeError),

    #[error("destination directory '{0}' already exists")]
    DestExists(PathBuf),
}

// ── Remote config (stored in .vai/remote.toml) ────────────────────────────────

/// Server connection config stored in `.vai/remote.toml` for cloned repos.
///
/// Presence of this file marks a repo as a remote clone.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteConfig {
    /// Base HTTP URL of the vai server, e.g. `http://127.0.0.1:7832`.
    pub server_url: String,
    /// API key used to authenticate requests to the server.
    pub api_key: String,
    /// Repository name as reported by the server.
    pub repo_name: String,
    /// The HEAD version at clone time.
    pub cloned_at_version: String,
}

/// Reads `.vai/remote.toml` if it exists, returning `None` for local repos.
pub fn read_remote_config(vai_dir: &Path) -> Option<RemoteConfig> {
    let path = vai_dir.join("remote.toml");
    if !path.exists() {
        return None;
    }
    let raw = fs::read_to_string(&path).ok()?;
    toml::from_str(&raw).ok()
}

// ── Clone result ─────────────────────────────────────────────────────────────

/// Summary of a completed clone operation.
#[derive(Debug, Serialize)]
pub struct CloneResult {
    /// Local directory containing the cloned repository.
    pub dest: PathBuf,
    /// Repository name.
    pub repo_name: String,
    /// HEAD version on the server at clone time.
    pub head_version: String,
    /// Number of files downloaded.
    pub files_downloaded: usize,
    /// Total bytes transferred.
    pub bytes_transferred: usize,
}

// ── Server response shapes (must match server/mod.rs) ────────────────────────

#[derive(Debug, Deserialize)]
struct StatusResponse {
    repo_name: String,
}

#[derive(Debug, Deserialize)]
struct RepoFileListResponse {
    files: Vec<String>,
    head_version: String,
}

#[derive(Debug, Deserialize)]
struct FileDownloadResponse {
    content_base64: String,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Clones a remote vai repository.
///
/// `url` must have the form `vai://<host>:<port>/<repo>`.
/// `dest` is the directory to clone into (must not already exist).
/// `api_key` is the Bearer token to authenticate with the server.
pub async fn clone(url: &str, dest: &Path, api_key: &str) -> Result<CloneResult, CloneError> {
    // ── Parse vai URL → HTTP base URL ─────────────────────────────────────
    let http_base = parse_vai_url(url)?;

    // ── Verify destination does not exist ─────────────────────────────────
    if dest.exists() {
        return Err(CloneError::DestExists(dest.to_owned()));
    }

    let client = reqwest::Client::new();

    // ── Connect to server: GET /api/status ────────────────────────────────
    let status: StatusResponse = get_json(
        &client,
        &format!("{http_base}/api/status"),
        None, // status is unauthenticated
    )
    .await?;

    // ── Get file list: GET /api/repo/files ────────────────────────────────
    let file_list: RepoFileListResponse = get_json(
        &client,
        &format!("{http_base}/api/repo/files"),
        Some(api_key),
    )
    .await?;

    let file_count = file_list.files.len();

    // ── Create destination directory ──────────────────────────────────────
    fs::create_dir_all(dest)?;

    // ── Download files with progress bar ──────────────────────────────────
    let pb = ProgressBar::new(file_count as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{prefix:.bold} [{bar:40}] {pos}/{len} {msg}")
            .unwrap_or_else(|_| ProgressStyle::default_bar())
            .progress_chars("=> "),
    );
    pb.set_prefix("Cloning");

    let mut total_bytes = 0usize;
    let mut downloaded = 0usize;

    for rel_path in &file_list.files {
        pb.set_message(rel_path.clone());

        let encoded = urlencoding_encode(rel_path);
        let dl: FileDownloadResponse = get_json(
            &client,
            &format!("{http_base}/api/files/{encoded}"),
            Some(api_key),
        )
        .await?;

        let content = BASE64.decode(dl.content_base64.as_bytes())?;
        total_bytes += content.len();

        // Create parent directories as needed.
        let local_path = dest.join(rel_path);
        if let Some(parent) = local_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&local_path, &content)?;
        downloaded += 1;
        pb.inc(1);
    }
    pb.finish_and_clear();

    // ── Create .vai/ structure ────────────────────────────────────────────
    let vai_dir = dest.join(".vai");
    fs::create_dir_all(vai_dir.join("event_log"))?;
    fs::create_dir_all(vai_dir.join("graph").join("entities"))?;
    fs::create_dir_all(vai_dir.join("workspaces"))?;
    fs::create_dir_all(vai_dir.join("versions"))?;
    fs::create_dir_all(vai_dir.join("cache").join("treesitter"))?;

    // Minimal config.toml — marks this as a vai repo.
    let config = crate::repo::RepoConfig {
        repo_id: Uuid::new_v4(),
        name: status.repo_name.clone(),
        created_at: Utc::now(),
        vai_version: env!("CARGO_PKG_VERSION").to_string(),
        remote: None,
    };
    fs::write(
        vai_dir.join("config.toml"),
        toml::to_string_pretty(&config)?,
    )?;

    // HEAD pointer.
    fs::write(
        vai_dir.join("head"),
        format!("{}\n", file_list.head_version),
    )?;

    // remote.toml — marks this as a cloned (remote-backed) repo.
    let remote = RemoteConfig {
        server_url: http_base.clone(),
        api_key: api_key.to_string(),
        repo_name: status.repo_name.clone(),
        cloned_at_version: file_list.head_version.clone(),
    };
    fs::write(
        vai_dir.join("remote.toml"),
        toml::to_string_pretty(&remote)?,
    )?;

    Ok(CloneResult {
        dest: dest.to_owned(),
        repo_name: status.repo_name,
        head_version: file_list.head_version,
        files_downloaded: downloaded,
        bytes_transferred: total_bytes,
    })
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Parses `vai://<host>:<port>[/<repo>]` → `http://<host>:<port>`.
fn parse_vai_url(url: &str) -> Result<String, CloneError> {
    let rest = url
        .strip_prefix("vai://")
        .ok_or_else(|| CloneError::InvalidUrl(url.to_owned()))?;

    // Take only the host:port part (everything before the first '/').
    let host_port = rest.split('/').next().unwrap_or(rest);
    if host_port.is_empty() {
        return Err(CloneError::InvalidUrl(url.to_owned()));
    }

    Ok(format!("http://{host_port}"))
}

/// Performs a GET request, optionally with a Bearer token, and deserialises
/// the JSON response body into `T`.
async fn get_json<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    url: &str,
    api_key: Option<&str>,
) -> Result<T, CloneError> {
    let mut req = client.get(url);
    if let Some(key) = api_key {
        req = req.header("Authorization", format!("Bearer {key}"));
    }
    let resp = req.send().await?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(CloneError::ServerError {
            status: status.as_u16(),
            body,
        });
    }
    Ok(resp.json().await?)
}

/// Percent-encodes a relative file path so it is safe for use in a URL path
/// segment (encodes space, `#`, `?`, etc., but preserves `/`).
fn urlencoding_encode(path: &str) -> String {
    path.split('/')
        .map(|segment| {
            // Encode each path segment, then re-join with '/'.
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

/// Prints a human-readable clone summary to stdout.
pub fn print_clone_result(result: &CloneResult) {
    println!(
        "{} Cloned {} into {}",
        "✓".green().bold(),
        result.repo_name.bold(),
        result.dest.display().to_string().cyan()
    );
    println!("  Version  : {}", result.head_version);
    println!("  Files    : {}", result.files_downloaded);
    println!(
        "  Size     : {:.1} KB",
        result.bytes_transferred as f64 / 1024.0
    );
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_vai_url_basic() {
        assert_eq!(
            parse_vai_url("vai://127.0.0.1:7832/myrepo").unwrap(),
            "http://127.0.0.1:7832"
        );
    }

    #[test]
    fn parse_vai_url_no_repo() {
        assert_eq!(
            parse_vai_url("vai://localhost:7832").unwrap(),
            "http://localhost:7832"
        );
    }

    #[test]
    fn parse_vai_url_invalid() {
        assert!(parse_vai_url("http://localhost:7832").is_err());
        assert!(parse_vai_url("vai://").is_err());
    }

    #[test]
    fn urlencoding_encode_basic() {
        assert_eq!(urlencoding_encode("src/main.rs"), "src/main.rs");
        assert_eq!(urlencoding_encode("path/to/my file.rs"), "path/to/my%20file.rs");
        assert_eq!(urlencoding_encode("dir/a#b.rs"), "dir/a%23b.rs");
    }
}
