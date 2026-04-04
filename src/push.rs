//! `vai push` — upload local changes to the server as a new version.
//!
//! Compares the local working directory against the server manifest, creates a
//! temporary workspace, uploads the full working directory as a snapshot, submits
//! it for merge, and updates `.vai/head` to the resulting version.
//!
//! ## What push does
//! 1. Calls `GET /api/repos/:repo/files/manifest` to check for local changes.
//! 2. Aborts if there are no modified or untracked files to push.
//! 3. Creates a workspace via `POST /api/repos/:repo/workspaces`.
//! 4. Uploads the working directory via `POST /api/repos/:repo/workspaces/:id/upload-snapshot`.
//! 5. Submits via `POST /api/repos/:repo/workspaces/:id/submit`.
//! 6. Updates `.vai/head` to the new version.

use std::path::Path;

use colored::Colorize;
use flate2::{write::GzEncoder, Compression};
use serde::{Deserialize, Serialize};
use thiserror::Error;

// ── Error type ────────────────────────────────────────────────────────────────

/// Errors that can occur during `vai push`.
#[derive(Debug, Error)]
pub enum PushError {
    #[error("no remote configured — run `vai remote add <url> --key <key>` or use --to/--key/--repo flags")]
    NoRemote,

    #[error("--repo is required when using --to")]
    MissingRepo,

    #[error("--key is required when using --to")]
    MissingKey,

    #[error("-m / --message is required")]
    MissingMessage,

    #[error("nothing to push — working directory matches server state")]
    NothingToPush,

    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("server error ({status}): {body}")]
    ServerError { status: u16, body: String },

    #[error("merge conflict — server rejected push due to conflicts:\n{0}\nRun `vai pull` to sync, then retry.")]
    MergeConflict(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

// ── Server response shapes ─────────────────────────────────────────────────

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
struct WorkspaceCreatedResponse {
    id: String,
}

#[derive(Debug, Deserialize)]
struct SnapshotUploadResult {
    added: usize,
    modified: usize,
    deleted: usize,
    #[allow(dead_code)]
    unchanged: usize,
}

#[derive(Debug, Deserialize)]
struct RemoteSubmitResult {
    version: String,
    files_applied: usize,
}

// ── Public config / result types ──────────────────────────────────────────────

/// Connection details required to perform a push.
pub struct PushConfig {
    /// Base URL of the remote vai server, e.g. `http://localhost:7865`.
    pub server_url: String,
    /// API key for authentication.
    pub api_key: String,
    /// Repository name on the server.
    pub repo_name: String,
}

/// Summary returned after a successful push.
#[derive(Debug, Serialize)]
pub struct PushResult {
    /// The new version created on the server.
    pub version: String,
    /// Files added relative to the server state.
    pub files_added: usize,
    /// Files modified relative to the server state.
    pub files_modified: usize,
    /// Files deleted relative to the server state.
    pub files_deleted: usize,
    /// Total files applied in the merge.
    pub files_applied: usize,
}

// ── Directory names always excluded from snapshots ────────────────────────────

const IGNORE_DIRS: &[&str] = &[
    ".vai", ".git", "target", "node_modules", "dist", "__pycache__",
];

// ── Public API ────────────────────────────────────────────────────────────────

/// Pushes local changes to the remote server as a new version.
///
/// `repo_root` is the directory that contains `.vai/` and the source files.
/// `config` provides the server URL, API key, and repository name.
/// `message` is the intent/commit message for the workspace.
/// `dry_run` prints what would be pushed without actually doing it.
pub async fn push(
    repo_root: &Path,
    config: PushConfig,
    message: &str,
    dry_run: bool,
) -> Result<PushResult, PushError> {
    let vai_dir = repo_root.join(".vai");

    let client = reqwest::Client::new();
    let base_url = config.server_url.trim_end_matches('/');

    // ── 1. Fetch server manifest to detect changes ─────────────────────────
    let manifest_url = format!("{}/api/repos/{}/files/manifest", base_url, config.repo_name);
    let resp = client
        .get(&manifest_url)
        .header("Authorization", format!("Bearer {}", config.api_key))
        .send()
        .await?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(PushError::ServerError { status: status.as_u16(), body });
    }
    let manifest: FilesManifestResponse = resp.json().await?;

    // Build server path → sha256 map.
    let server_map: std::collections::HashMap<String, String> = manifest
        .files
        .into_iter()
        .map(|e| (e.path, e.sha256))
        .collect();

    // Walk local files and compute hashes.
    let local_map = collect_local_hashes(repo_root)?;

    // Count changes.
    let mut files_modified = 0usize;
    let mut files_added = 0usize;
    let mut files_deleted = 0usize;

    for (path, local_hash) in &local_map {
        match server_map.get(path) {
            Some(server_hash) if server_hash != local_hash => files_modified += 1,
            None => files_added += 1,
            _ => {}
        }
    }
    for path in server_map.keys() {
        if !local_map.contains_key(path) {
            files_deleted += 1;
        }
    }

    if files_modified == 0 && files_added == 0 && files_deleted == 0 {
        return Err(PushError::NothingToPush);
    }

    if dry_run {
        println!("Would push to {} ({}):", config.repo_name, manifest.version);
        if files_added > 0 {
            println!("  {} file(s) to add", files_added);
        }
        if files_modified > 0 {
            println!("  {} file(s) to update", files_modified);
        }
        if files_deleted > 0 {
            println!("  {} file(s) to delete", files_deleted);
        }
        // Return a placeholder result — nothing was sent.
        return Ok(PushResult {
            version: manifest.version,
            files_added,
            files_modified,
            files_deleted,
            files_applied: 0,
        });
    }

    // ── 2. Create a workspace ──────────────────────────────────────────────
    let workspaces_url = format!("{}/api/repos/{}/workspaces", base_url, config.repo_name);
    let resp = client
        .post(&workspaces_url)
        .header("Authorization", format!("Bearer {}", config.api_key))
        .json(&serde_json::json!({ "intent": message }))
        .send()
        .await?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(PushError::ServerError { status: status.as_u16(), body });
    }
    let created: WorkspaceCreatedResponse = resp.json().await?;
    let workspace_id = created.id;

    // ── 3. Build and upload a full snapshot tarball ────────────────────────
    let tarball = build_full_tarball(repo_root)?;

    let upload_url = format!(
        "{}/api/repos/{}/workspaces/{}/upload-snapshot",
        base_url, config.repo_name, workspace_id
    );
    let resp = client
        .post(&upload_url)
        .header("Authorization", format!("Bearer {}", config.api_key))
        .header("Content-Type", "application/gzip")
        .body(tarball)
        .send()
        .await?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(PushError::ServerError { status: status.as_u16(), body });
    }
    let upload_result: SnapshotUploadResult = resp.json().await?;

    // ── 4. Submit the workspace for merge ──────────────────────────────────
    let submit_url = format!(
        "{}/api/repos/{}/workspaces/{}/submit",
        base_url, config.repo_name, workspace_id
    );
    let resp = client
        .post(&submit_url)
        .header("Authorization", format!("Bearer {}", config.api_key))
        .send()
        .await?;

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();

    if status == reqwest::StatusCode::CONFLICT {
        return Err(PushError::MergeConflict(body));
    }
    if !status.is_success() {
        return Err(PushError::ServerError { status: status.as_u16(), body });
    }

    let submit_result: RemoteSubmitResult = serde_json::from_str(&body)?;

    // ── 5. Update local HEAD ───────────────────────────────────────────────
    if vai_dir.exists() {
        std::fs::write(vai_dir.join("head"), format!("{}\n", submit_result.version))?;
    }

    Ok(PushResult {
        version: submit_result.version,
        files_added: upload_result.added,
        files_modified: upload_result.modified,
        files_deleted: upload_result.deleted,
        files_applied: submit_result.files_applied,
    })
}

// ── Display ───────────────────────────────────────────────────────────────────

/// Prints a human-readable push summary to stdout.
pub fn print_push_result(result: &PushResult, dry_run: bool) {
    if dry_run {
        println!(
            "{} Dry run — {} file(s) would be pushed",
            "·".dimmed(),
            result.files_added + result.files_modified + result.files_deleted,
        );
        return;
    }

    println!(
        "{} Pushed — version {}",
        "✓".green().bold(),
        result.version.bold(),
    );

    let total = result.files_added + result.files_modified + result.files_deleted;
    println!("  {} file(s) changed", total);
    if result.files_added > 0 {
        println!("    {} {} added", "+".green(), result.files_added);
    }
    if result.files_modified > 0 {
        println!("    {} {} modified", "~".yellow(), result.files_modified);
    }
    if result.files_deleted > 0 {
        println!("    {} {} deleted", "-".red(), result.files_deleted);
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Walks `repo_root`, skipping ignored directories, and returns a map of
/// repo-relative path → lowercase hex SHA-256 hash for each regular file.
fn collect_local_hashes(
    repo_root: &Path,
) -> Result<std::collections::HashMap<String, String>, PushError> {
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
) -> Result<(), PushError> {
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

                let hash = hasher(&path)?;
                map.insert(rel_str, hash);
            }
        }
    }
    Ok(())
}

/// Builds a gzip tarball containing every file under `repo_dir`,
/// excluding ignored directories (`.vai/`, `.git/`, `target/`, etc.).
fn build_full_tarball(repo_dir: &Path) -> Result<Vec<u8>, PushError> {
    let buf = Vec::new();
    let gz = GzEncoder::new(buf, Compression::default());
    let mut tar = tar::Builder::new(gz);

    append_dir_to_tar(&mut tar, repo_dir, repo_dir)?;

    let gz = tar.into_inner().map_err(PushError::Io)?;
    gz.finish().map_err(PushError::Io)
}

fn append_dir_to_tar<W: std::io::Write>(
    tar: &mut tar::Builder<W>,
    dir: &Path,
    base: &Path,
) -> Result<(), PushError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(PushError::Io(e)),
    };
    for entry in entries {
        let entry = entry.map_err(PushError::Io)?;
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if path.is_dir() {
            if IGNORE_DIRS.contains(&name_str.as_ref()) {
                continue;
            }
            append_dir_to_tar(tar, &path, base)?;
        } else {
            let rel = path
                .strip_prefix(base)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            let data = std::fs::read(&path).map_err(PushError::Io)?;
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, &rel, data.as_slice())
                .map_err(PushError::Io)?;
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
    fn nothing_to_push_error_message() {
        let err = PushError::NothingToPush;
        assert!(err.to_string().contains("nothing to push"));
    }

    #[test]
    fn missing_message_error() {
        let err = PushError::MissingMessage;
        assert!(err.to_string().contains("-m"));
    }

    #[test]
    fn build_full_tarball_includes_files_and_skips_ignored() {
        use flate2::read::GzDecoder;
        use tar::Archive;

        let root = tempfile::tempdir().unwrap();
        let root_path = root.path();

        fs::create_dir_all(root_path.join("src")).unwrap();
        fs::write(root_path.join("src/lib.rs"), b"lib code").unwrap();
        fs::write(root_path.join("readme.md"), b"readme").unwrap();

        // Ignored dirs.
        fs::create_dir_all(root_path.join(".git")).unwrap();
        fs::write(root_path.join(".git/HEAD"), b"ref").unwrap();
        fs::create_dir_all(root_path.join("target/debug")).unwrap();
        fs::write(root_path.join("target/debug/binary"), b"bin").unwrap();
        fs::create_dir_all(root_path.join(".vai")).unwrap();
        fs::write(root_path.join(".vai/head"), b"v1").unwrap();

        let tarball = build_full_tarball(root_path).unwrap();

        // Inspect tarball entries.
        let decoder = GzDecoder::new(tarball.as_slice());
        let mut archive = Archive::new(decoder);
        let paths: Vec<String> = archive
            .entries()
            .unwrap()
            .filter_map(|e| e.ok())
            .filter_map(|e| e.path().ok().map(|p| p.to_string_lossy().replace('\\', "/")))
            .collect();

        assert!(paths.contains(&"src/lib.rs".to_string()), "should contain src/lib.rs");
        assert!(paths.contains(&"readme.md".to_string()), "should contain readme.md");
        assert!(!paths.iter().any(|p| p.starts_with(".git/")), "should skip .git/");
        assert!(!paths.iter().any(|p| p.starts_with("target/")), "should skip target/");
        assert!(!paths.iter().any(|p| p.starts_with(".vai/")), "should skip .vai/");
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
    fn print_push_result_normal() {
        let result = PushResult {
            version: "v42".to_string(),
            files_added: 2,
            files_modified: 3,
            files_deleted: 1,
            files_applied: 6,
        };
        print_push_result(&result, false); // should not panic
    }

    #[test]
    fn print_push_result_dry_run() {
        let result = PushResult {
            version: "v42".to_string(),
            files_added: 1,
            files_modified: 0,
            files_deleted: 0,
            files_applied: 0,
        };
        print_push_result(&result, true); // should not panic
    }
}
