//! Remote workspace operations — register, upload files, and submit to a vai server.
//!
//! Used by `vai workspace create` and `vai workspace submit` in cloned
//! repositories. Complements `sync.rs` (which handles pulling server changes)
//! by handling the agent→server direction:
//!
//! 1. **Register** — `POST /api/repos/:repo/workspaces` creates a workspace on the server
//!    and returns a server-assigned UUID that becomes the canonical workspace ID.
//! 2. **Upload** — `POST /api/repos/:repo/workspaces/:id/upload-snapshot` uploads a
//!    gzip-compressed tarball; auto-detects full vs delta mode based on repo size.
//! 3. **Submit** — `POST /api/repos/:repo/workspaces/:id/submit` triggers the server-side
//!    semantic merge and returns the resulting version or conflict details.
//! 4. **List** — `GET /api/repos/:repo/workspaces` fetches all active workspaces on the
//!    server (used by `vai status --others`).
//!
//! ## Snapshot upload modes
//!
//! `upload_snapshot` chooses between two modes automatically:
//!
//! - **Full mode** (repo ≤ 50 MiB uncompressed): the entire working directory
//!   is packed into a tarball and uploaded. The server replaces `current/`
//!   entirely and derives the workspace overlay by diffing against the old HEAD.
//!
//! - **Delta mode** (repo > 50 MiB): only the files present in `overlay_dir`
//!   are packed, plus a `.vai-delta.json` manifest listing the base version and
//!   any deleted paths. The server applies only the uploaded files on top of
//!   `current/`, leaving untouched files intact.

use std::path::Path;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use base64::prelude::{Engine, BASE64_STANDARD as BASE64};
use flate2::{write::GzEncoder, Compression};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::clone::RemoteConfig;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Repos whose uncompressed size exceeds this threshold are uploaded in delta
/// mode to avoid sending the full working tree over the wire.
const DELTA_THRESHOLD_BYTES: u64 = 50 * 1024 * 1024; // 50 MiB

// ── Error type ────────────────────────────────────────────────────────────────

/// Errors that can occur during remote workspace operations.
#[derive(Debug, Error)]
pub enum RemoteWorkspaceError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("server error ({status}): {body}")]
    ServerError { status: u16, body: String },

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("base64 encode/decode error: {0}")]
    Base64(#[from] base64::DecodeError),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("no active workspace")]
    NoActiveWorkspace,

    #[error("merge conflict: {0}")]
    MergeConflict(String),
}

// ── Server response shapes ────────────────────────────────────────────────────

/// Workspace metadata returned by the server on creation and listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteWorkspaceMeta {
    pub id: String,
    pub intent: String,
    pub status: String,
    pub base_version: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Result of a successful snapshot upload (`POST /api/workspaces/:id/upload-snapshot`).
#[derive(Debug, Serialize, Deserialize)]
pub struct SnapshotUploadResult {
    /// Files present in the upload but absent from `current/`.
    pub added: usize,
    /// Files whose content changed relative to `current/`.
    pub modified: usize,
    /// Files removed (full mode: absent from tarball; delta mode: listed in manifest).
    pub deleted: usize,
    /// Files identical to `current/` — uploaded but unchanged.
    pub unchanged: usize,
    /// `true` when the server processed the upload in delta mode.
    pub is_delta: bool,
}

/// Result of a successful remote submit.
#[derive(Debug, Serialize, Deserialize)]
pub struct RemoteSubmitResult {
    /// New version identifier created on the server.
    pub version: String,
    /// Number of files applied in the merge.
    pub files_applied: usize,
    /// Number of entities changed.
    pub entities_changed: usize,
    /// Number of conflicts auto-resolved.
    pub auto_resolved: u32,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Registers a new workspace on the remote server.
///
/// Calls `POST /api/repos/:repo/workspaces` with `{"intent": "<intent>"}` and returns the
/// server-assigned workspace UUID string. The caller should use this ID as the
/// local workspace ID so both sides share the same identifier.
pub async fn register_workspace(
    remote: &RemoteConfig,
    intent: &str,
) -> Result<RemoteWorkspaceMeta, RemoteWorkspaceError> {
    let client = reqwest::Client::new();
    let url = format!("{}/api/repos/{}/workspaces", remote.server_url, remote.repo_name);
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", remote.api_key))
        .json(&serde_json::json!({ "intent": intent }))
        .send()
        .await?;

    ensure_success(resp).await.and_then(|b| {
        serde_json::from_str::<RemoteWorkspaceMeta>(&b).map_err(RemoteWorkspaceError::Json)
    })
}

/// Uploads all files in the workspace overlay to the server.
///
/// Reads every file under `overlay_dir` and batches them into a single
/// `POST /api/repos/:repo/workspaces/:id/files` request with base64-encoded content.
/// Returns the list of file paths that were uploaded.
pub async fn upload_overlay_files(
    remote: &RemoteConfig,
    workspace_id: &str,
    overlay_dir: &Path,
) -> Result<Vec<String>, RemoteWorkspaceError> {
    // Collect all files from the overlay.
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    collect_overlay_files(overlay_dir, overlay_dir, &mut entries)?;

    if entries.is_empty() {
        return Ok(Vec::new());
    }

    // Build the JSON body.
    let files: Vec<serde_json::Value> = entries
        .iter()
        .map(|(path, content)| {
            serde_json::json!({
                "path": path,
                "content_base64": BASE64.encode(content),
            })
        })
        .collect();

    let client = reqwest::Client::new();
    let url = format!("{}/api/repos/{}/workspaces/{}/files", remote.server_url, remote.repo_name, workspace_id);
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", remote.api_key))
        .json(&serde_json::json!({ "files": files }))
        .send()
        .await?;

    let body = ensure_success(resp).await?;
    let result: serde_json::Value = serde_json::from_str(&body)?;
    let paths: Vec<String> = result["paths"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    Ok(paths)
}

/// Uploads a snapshot of the workspace to the server using the tarball endpoint.
///
/// Automatically chooses between full and delta upload modes based on the
/// uncompressed size of `repo_dir`:
///
/// - **Full mode** (≤ 50 MiB): packs the entire `repo_dir` into a gzip tarball
///   (excluding `.vai/`, `.git/`, and common build artefacts) and uploads it.
///   The server replaces `current/` and derives the workspace overlay by diffing.
///
/// - **Delta mode** (> 50 MiB): packs only the files under `overlay_dir`, adds
///   a `.vai-delta.json` manifest with `base_version` and `deleted_paths`, and
///   uploads the compact tarball. The server applies only the uploaded files on
///   top of `current/`, leaving untouched files intact.
///
/// Returns a [`SnapshotUploadResult`] with counts of added/modified/deleted/
/// unchanged files as reported by the server.
pub async fn upload_snapshot(
    remote: &RemoteConfig,
    workspace_id: &str,
    repo_dir: &Path,
    overlay_dir: &Path,
    base_version: &str,
    deleted_paths: &[String],
) -> Result<SnapshotUploadResult, RemoteWorkspaceError> {
    let repo_size = dir_uncompressed_size(repo_dir)?;
    let use_delta = repo_size > DELTA_THRESHOLD_BYTES;

    let tarball = if use_delta {
        build_delta_tarball(overlay_dir, base_version, deleted_paths)?
    } else {
        build_full_tarball(repo_dir)?
    };

    let client = reqwest::Client::new();
    let url = format!(
        "{}/api/repos/{}/workspaces/{}/upload-snapshot",
        remote.server_url, remote.repo_name, workspace_id
    );
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", remote.api_key))
        .header("Content-Type", "application/gzip")
        .body(tarball)
        .send()
        .await?;

    let body = ensure_success(resp).await?;
    serde_json::from_str::<SnapshotUploadResult>(&body).map_err(RemoteWorkspaceError::Json)
}

/// Submits a workspace on the server for semantic merge.
///
/// Calls `POST /api/repos/:repo/workspaces/:id/submit` and returns the merge result on
/// success. Returns [`RemoteWorkspaceError::MergeConflict`] when the server
/// reports a 409 Conflict (unresolvable semantic conflicts).
pub async fn submit_workspace(
    remote: &RemoteConfig,
    workspace_id: &str,
) -> Result<RemoteSubmitResult, RemoteWorkspaceError> {
    let client = reqwest::Client::new();
    let url = format!("{}/api/repos/{}/workspaces/{}/submit", remote.server_url, remote.repo_name, workspace_id);
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", remote.api_key))
        .send()
        .await?;

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();

    if status == reqwest::StatusCode::CONFLICT {
        return Err(RemoteWorkspaceError::MergeConflict(body));
    }

    if !status.is_success() {
        return Err(RemoteWorkspaceError::ServerError {
            status: status.as_u16(),
            body,
        });
    }

    serde_json::from_str::<RemoteSubmitResult>(&body).map_err(RemoteWorkspaceError::Json)
}

/// Lists all active workspaces on the server.
///
/// Calls `GET /api/repos/:repo/workspaces` and returns the list of workspace metadata.
///
/// The endpoint returns a paginated envelope `{"data": [...], "pagination": {...}}`.
/// This function fetches the first page (up to 100 items) and returns the items.
pub async fn list_workspaces(
    remote: &RemoteConfig,
) -> Result<Vec<RemoteWorkspaceMeta>, RemoteWorkspaceError> {
    #[derive(serde::Deserialize)]
    struct Page {
        data: Vec<RemoteWorkspaceMeta>,
    }

    let client = reqwest::Client::new();
    let url = format!("{}/api/repos/{}/workspaces?per_page=100", remote.server_url, remote.repo_name);
    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", remote.api_key))
        .send()
        .await?;

    let body = ensure_success(resp).await?;
    let page: Page = serde_json::from_str(&body).map_err(RemoteWorkspaceError::Json)?;
    Ok(page.data)
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Asserts `resp.status().is_success()`, returning the body text on success
/// and a [`RemoteWorkspaceError::ServerError`] on failure.
async fn ensure_success(resp: reqwest::Response) -> Result<String, RemoteWorkspaceError> {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(RemoteWorkspaceError::ServerError {
            status: status.as_u16(),
            body,
        });
    }
    Ok(body)
}

/// Returns the total uncompressed size (in bytes) of all files under `dir`,
/// stopping early once the delta threshold has been exceeded (to avoid scanning
/// large trees unnecessarily).
fn dir_uncompressed_size(dir: &Path) -> Result<u64, RemoteWorkspaceError> {
    let mut total = 0u64;
    dir_size_inner(dir, &mut total)?;
    Ok(total)
}

fn dir_size_inner(dir: &Path, total: &mut u64) -> Result<(), RemoteWorkspaceError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(RemoteWorkspaceError::Io(e)),
    };
    for entry in entries {
        let entry = entry.map_err(RemoteWorkspaceError::Io)?;
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        // Skip directories that should not be included in snapshots.
        if path.is_dir() {
            if matches!(name_str.as_ref(), ".vai" | ".git" | "target" | "node_modules") {
                continue;
            }
            dir_size_inner(&path, total)?;
        } else {
            let meta = std::fs::metadata(&path).map_err(RemoteWorkspaceError::Io)?;
            *total += meta.len();
            // Short-circuit: once above threshold exact size doesn't matter.
            if *total > DELTA_THRESHOLD_BYTES {
                return Ok(());
            }
        }
    }
    Ok(())
}

/// Builds a gzip tarball containing every file under `repo_dir` (excluding
/// `.vai/`, `.git/`, `target/`, and `node_modules/`).
fn build_full_tarball(repo_dir: &Path) -> Result<Vec<u8>, RemoteWorkspaceError> {
    let buf = Vec::new();
    let gz = GzEncoder::new(buf, Compression::default());
    let mut tar = tar::Builder::new(gz);

    append_dir_to_tar(&mut tar, repo_dir, repo_dir)?;

    let gz = tar
        .into_inner()
        .map_err(RemoteWorkspaceError::Io)?;
    gz.finish().map_err(RemoteWorkspaceError::Io)
}

/// Builds a gzip delta tarball containing only the files in `overlay_dir` plus
/// a `.vai-delta.json` manifest.
fn build_delta_tarball(
    overlay_dir: &Path,
    base_version: &str,
    deleted_paths: &[String],
) -> Result<Vec<u8>, RemoteWorkspaceError> {
    let buf = Vec::new();
    let gz = GzEncoder::new(buf, Compression::default());
    let mut tar = tar::Builder::new(gz);

    // Add overlay files first.
    append_dir_to_tar(&mut tar, overlay_dir, overlay_dir)?;

    // Append `.vai-delta.json` manifest.
    let manifest = serde_json::json!({
        "base_version": base_version,
        "deleted_paths": deleted_paths,
    });
    let manifest_bytes = serde_json::to_vec(&manifest).map_err(RemoteWorkspaceError::Json)?;
    let mut header = tar::Header::new_gnu();
    header.set_size(manifest_bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append_data(&mut header, ".vai-delta.json", manifest_bytes.as_slice())
        .map_err(RemoteWorkspaceError::Io)?;

    let gz = tar
        .into_inner()
        .map_err(RemoteWorkspaceError::Io)?;
    gz.finish().map_err(RemoteWorkspaceError::Io)
}

/// Recursively appends all files under `dir` to a tar builder, using paths
/// relative to `base`. Skips `.vai/`, `.git/`, `target/`, and `node_modules/`.
fn append_dir_to_tar<W: std::io::Write>(
    tar: &mut tar::Builder<W>,
    dir: &Path,
    base: &Path,
) -> Result<(), RemoteWorkspaceError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(RemoteWorkspaceError::Io(e)),
    };
    for entry in entries {
        let entry = entry.map_err(RemoteWorkspaceError::Io)?;
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if path.is_dir() {
            if matches!(name_str.as_ref(), ".vai" | ".git" | "target" | "node_modules") {
                continue;
            }
            append_dir_to_tar(tar, &path, base)?;
        } else {
            let rel = path
                .strip_prefix(base)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            let data = std::fs::read(&path).map_err(RemoteWorkspaceError::Io)?;
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(file_mode_for_path(&path, &data));
            header.set_cksum();
            tar.append_data(&mut header, &rel, data.as_slice())
                .map_err(RemoteWorkspaceError::Io)?;
        }
    }
    Ok(())
}

/// Returns the Unix mode bits to use for a file in a tarball.
///
/// On Unix, reads the actual file permissions from disk so that the executable
/// bit is preserved for scripts.  On non-Unix platforms, falls back to a
/// shebang-line heuristic: files starting with `#!` get `0o755`, others `0o644`.
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

/// Recursively collects `(relative_path, content)` pairs for all files under
/// `dir`. `base` is the root overlay directory used to compute relative paths.
fn collect_overlay_files(
    dir: &Path,
    base: &Path,
    out: &mut Vec<(String, Vec<u8>)>,
) -> Result<(), RemoteWorkspaceError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(RemoteWorkspaceError::Io(e)),
    };
    for entry in entries {
        let entry = entry.map_err(RemoteWorkspaceError::Io)?;
        let path = entry.path();
        if path.is_dir() {
            collect_overlay_files(&path, base, out)?;
        } else {
            let rel = path
                .strip_prefix(base)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            let content = std::fs::read(&path).map_err(RemoteWorkspaceError::Io)?;
            out.push((rel, content));
        }
    }
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn collect_overlay_files_empty() {
        let dir = TempDir::new().unwrap();
        let mut out = Vec::new();
        collect_overlay_files(dir.path(), dir.path(), &mut out).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn collect_overlay_files_nested() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("main.rs"), b"fn main() {}").unwrap();
        std::fs::write(dir.path().join("lib.rs"), b"pub mod foo;").unwrap();

        let mut out = Vec::new();
        collect_overlay_files(dir.path(), dir.path(), &mut out).unwrap();
        out.sort_by(|a, b| a.0.cmp(&b.0));

        assert_eq!(out.len(), 2);
        assert_eq!(out[0].0, "lib.rs");
        assert_eq!(out[1].0, "src/main.rs");
    }

    #[test]
    fn collect_overlay_files_nonexistent() {
        let dir = TempDir::new().unwrap();
        let nonexistent = dir.path().join("no-such-dir");
        let mut out = Vec::new();
        // Should not error — just produce empty results.
        collect_overlay_files(&nonexistent, &nonexistent, &mut out).unwrap();
        assert!(out.is_empty());
    }

    /// Build a delta tarball and verify it contains the overlay file and
    /// a `.vai-delta.json` manifest with the expected content.
    #[test]
    fn build_delta_tarball_contains_manifest() {
        use flate2::read::GzDecoder;
        use std::io::Read;

        let overlay = TempDir::new().unwrap();
        std::fs::write(overlay.path().join("changed.rs"), b"pub fn foo() {}").unwrap();

        let tarball = build_delta_tarball(
            overlay.path(),
            "v10",
            &["src/removed.rs".to_string()],
        )
        .unwrap();

        // Decompress and scan entries.
        let gz = GzDecoder::new(tarball.as_slice());
        let mut archive = tar::Archive::new(gz);
        let mut found_manifest = false;
        let mut found_file = false;
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            let path = entry.path().unwrap().to_string_lossy().to_string();
            if path == ".vai-delta.json" {
                let mut buf = String::new();
                entry.read_to_string(&mut buf).unwrap();
                let v: serde_json::Value = serde_json::from_str(&buf).unwrap();
                assert_eq!(v["base_version"], "v10");
                assert_eq!(v["deleted_paths"][0], "src/removed.rs");
                found_manifest = true;
            } else if path == "changed.rs" {
                found_file = true;
            }
        }
        assert!(found_manifest, "manifest missing from delta tarball");
        assert!(found_file, "overlay file missing from delta tarball");
    }

    /// Small repo (below threshold) picks full mode; large repo picks delta mode.
    #[test]
    fn dir_size_threshold_detection() {
        let dir = TempDir::new().unwrap();
        // Write 1 byte — well below 50 MiB threshold.
        std::fs::write(dir.path().join("tiny.txt"), b"x").unwrap();
        let size = dir_uncompressed_size(dir.path()).unwrap();
        assert!(size <= DELTA_THRESHOLD_BYTES);

        // Verify threshold constant is sensible (50 MiB).
        assert_eq!(DELTA_THRESHOLD_BYTES, 50 * 1024 * 1024);
    }
}
