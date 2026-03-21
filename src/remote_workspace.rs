//! Remote workspace operations — register, upload files, and submit to a vai server.
//!
//! Used by `vai workspace create` and `vai workspace submit` in cloned
//! repositories. Complements `sync.rs` (which handles pulling server changes)
//! by handling the agent→server direction:
//!
//! 1. **Register** — `POST /api/workspaces` creates a workspace on the server
//!    and returns a server-assigned UUID that becomes the canonical workspace ID.
//! 2. **Upload** — `POST /api/workspaces/:id/files` pushes locally modified
//!    files from the workspace overlay into the server's workspace overlay.
//! 3. **Submit** — `POST /api/workspaces/:id/submit` triggers the server-side
//!    semantic merge and returns the resulting version or conflict details.
//! 4. **List** — `GET /api/workspaces` fetches all active workspaces on the
//!    server (used by `vai status --others`).

use std::path::Path;

use base64::prelude::{Engine, BASE64_STANDARD as BASE64};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::clone::RemoteConfig;

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
/// Calls `POST /api/workspaces` with `{"intent": "<intent>"}` and returns the
/// server-assigned workspace UUID string. The caller should use this ID as the
/// local workspace ID so both sides share the same identifier.
pub async fn register_workspace(
    remote: &RemoteConfig,
    intent: &str,
) -> Result<RemoteWorkspaceMeta, RemoteWorkspaceError> {
    let client = reqwest::Client::new();
    let url = format!("{}/api/workspaces", remote.server_url);
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
/// `POST /api/workspaces/:id/files` request with base64-encoded content.
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
    let url = format!("{}/api/workspaces/{}/files", remote.server_url, workspace_id);
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

/// Submits a workspace on the server for semantic merge.
///
/// Calls `POST /api/workspaces/:id/submit` and returns the merge result on
/// success. Returns [`RemoteWorkspaceError::MergeConflict`] when the server
/// reports a 409 Conflict (unresolvable semantic conflicts).
pub async fn submit_workspace(
    remote: &RemoteConfig,
    workspace_id: &str,
) -> Result<RemoteSubmitResult, RemoteWorkspaceError> {
    let client = reqwest::Client::new();
    let url = format!("{}/api/workspaces/{}/submit", remote.server_url, workspace_id);
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
/// Calls `GET /api/workspaces` and returns the list of workspace metadata.
pub async fn list_workspaces(
    remote: &RemoteConfig,
) -> Result<Vec<RemoteWorkspaceMeta>, RemoteWorkspaceError> {
    let client = reqwest::Client::new();
    let url = format!("{}/api/workspaces", remote.server_url);
    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", remote.api_key))
        .send()
        .await?;

    let body = ensure_success(resp).await?;
    serde_json::from_str::<Vec<RemoteWorkspaceMeta>>(&body).map_err(RemoteWorkspaceError::Json)
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
}
