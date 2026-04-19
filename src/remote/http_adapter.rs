//! HTTP implementation of `RemoteAdapter` backed by `reqwest`.
//!
//! `HttpAdapter` wraps `RemoteClient` and owns the knowledge of endpoint URLs
//! and response shapes.  All push/pull operations use `/api/repos/:repo/…`
//! paths; sync operations use the legacy `/api/repo/…` paths.

use async_trait::async_trait;
use serde::Deserialize;

use super::{
    ChangeKind, FullDownload, IncrementalPullResult, ManifestEntry, ManifestResult,
    RemoteAdapter, RemoteError, SubmitResult, UploadStats, VersionFileChange, VersionSummary,
    FilePullEntry,
};

// ── Server response shapes ────────────────────────────────────────────────────

#[derive(Deserialize)]
struct ManifestFileEntry {
    path: String,
    sha256: String,
}

#[derive(Deserialize)]
struct FilesManifestResponse {
    version: String,
    files: Vec<ManifestFileEntry>,
}

#[derive(Deserialize)]
struct WorkspaceCreatedResponse {
    id: String,
}

#[derive(Deserialize)]
struct SnapshotUploadResult {
    added: usize,
    modified: usize,
    deleted: usize,
    #[allow(dead_code)]
    unchanged: usize,
}

#[derive(Deserialize)]
struct RemoteSubmitResult {
    version: String,
    files_applied: usize,
}

#[derive(Deserialize)]
struct PullFileEntry {
    path: String,
    change_type: PullChangeType,
    content_base64: Option<String>,
}

#[derive(Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum PullChangeType {
    Added,
    Modified,
    Removed,
}

#[derive(Deserialize)]
struct FilesPullResponse {
    base_version: String,
    head_version: String,
    files: Vec<PullFileEntry>,
}

#[derive(Deserialize)]
struct RepoFileListResponse {
    head_version: String,
}

#[derive(Deserialize)]
struct SyncVersionMeta {
    version_id: String,
}

#[derive(Deserialize)]
struct SyncVersionChanges {
    file_changes: Vec<SyncFileChange>,
}

#[derive(Deserialize)]
struct SyncFileChange {
    path: String,
    change_type: SyncChangeType,
}

#[derive(Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum SyncChangeType {
    Added,
    Modified,
    Removed,
}

#[derive(Deserialize)]
struct FileDownloadResponse {
    content_base64: String,
}

// ── HttpAdapter ───────────────────────────────────────────────────────────────

/// HTTP implementation of [`RemoteAdapter`] that talks to a real vai server.
///
/// Push/pull operations use `/api/repos/:repo/…`.
/// Sync operations use the legacy `/api/repo/…` paths (no repo name in URL).
pub struct HttpAdapter {
    client: reqwest::Client,
    /// Base URL with no trailing slash.
    base_url: String,
    api_key: String,
}

impl HttpAdapter {
    /// Creates a new adapter for the given server URL and API key.
    pub fn new(server_url: &str, api_key: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: server_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
        }
    }

    /// Sends a GET request with Bearer auth and returns the response.
    async fn get_raw(&self, url: &str) -> Result<reqwest::Response, RemoteError> {
        let resp = self
            .client
            .get(url)
            .bearer_auth(&self.api_key)
            .send()
            .await?;
        Ok(resp)
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T, RemoteError> {
        let resp = self.get_raw(url).await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(RemoteError::Server(format!("{status}: {body}")));
        }
        Ok(resp.json::<T>().await?)
    }

    async fn post_json<B: serde::Serialize, T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        body: &B,
    ) -> Result<T, RemoteError> {
        let resp = self
            .client
            .post(url)
            .bearer_auth(&self.api_key)
            .json(body)
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(RemoteError::Server(format!("{status}: {body}")));
        }
        Ok(resp.json::<T>().await?)
    }
}

#[async_trait]
impl RemoteAdapter for HttpAdapter {
    async fn get_manifest(&self, repo: &str) -> Result<ManifestResult, RemoteError> {
        let url = format!("{}/api/repos/{}/files/manifest", self.base_url, repo);
        let resp: FilesManifestResponse = self.get_json(&url).await?;
        Ok(ManifestResult {
            version: resp.version,
            files: resp
                .files
                .into_iter()
                .map(|e| ManifestEntry { path: e.path, sha256: e.sha256 })
                .collect(),
        })
    }

    async fn create_workspace(&self, repo: &str, intent: &str) -> Result<String, RemoteError> {
        let url = format!("{}/api/repos/{}/workspaces", self.base_url, repo);
        let resp: WorkspaceCreatedResponse =
            self.post_json(&url, &serde_json::json!({ "intent": intent })).await?;
        Ok(resp.id)
    }

    async fn upload_snapshot(
        &self,
        repo: &str,
        workspace_id: &str,
        tarball_gz: Vec<u8>,
    ) -> Result<UploadStats, RemoteError> {
        let url = format!(
            "{}/api/repos/{}/workspaces/{}/upload-snapshot",
            self.base_url, repo, workspace_id
        );
        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .header("Content-Type", "application/gzip")
            .body(tarball_gz)
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(RemoteError::Server(format!("{status}: {body}")));
        }
        let result: SnapshotUploadResult = resp.json().await?;
        Ok(UploadStats {
            added: result.added,
            modified: result.modified,
            deleted: result.deleted,
        })
    }

    async fn submit_workspace(
        &self,
        repo: &str,
        workspace_id: &str,
    ) -> Result<SubmitResult, RemoteError> {
        let url = format!(
            "{}/api/repos/{}/workspaces/{}/submit",
            self.base_url, repo, workspace_id
        );
        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .send()
            .await?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if status == reqwest::StatusCode::CONFLICT {
            return Err(RemoteError::MergeConflict(body));
        }
        if !status.is_success() {
            return Err(RemoteError::Server(format!("{status}: {body}")));
        }
        let result: RemoteSubmitResult = serde_json::from_str(&body)?;
        Ok(SubmitResult {
            version: result.version,
            files_applied: result.files_applied,
        })
    }

    async fn pull_incremental(
        &self,
        repo: &str,
        since: &str,
    ) -> Result<IncrementalPullResult, RemoteError> {
        let url = format!(
            "{}/api/repos/{}/files/pull?since={}",
            self.base_url, repo, since
        );
        let resp: FilesPullResponse = self.get_json(&url).await?;
        Ok(IncrementalPullResult {
            base_version: resp.base_version,
            head_version: resp.head_version,
            files: resp
                .files
                .into_iter()
                .map(|e| FilePullEntry {
                    path: e.path,
                    change: match e.change_type {
                        PullChangeType::Added => ChangeKind::Added,
                        PullChangeType::Modified => ChangeKind::Modified,
                        PullChangeType::Removed => ChangeKind::Removed,
                    },
                    content_base64: e.content_base64,
                })
                .collect(),
        })
    }

    async fn download_full(&self, repo: &str) -> Result<FullDownload, RemoteError> {
        let url = format!("{}/api/repos/{}/files/download", self.base_url, repo);
        let resp = self.get_raw(&url).await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(RemoteError::Server(format!("{status}: {body}")));
        }
        let head_version = resp
            .headers()
            .get("X-Vai-Head")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("unknown")
            .to_string();
        let tarball_gz = resp.bytes().await?.to_vec();
        Ok(FullDownload { head_version, tarball_gz })
    }

    async fn get_server_head(&self, _repo: &str) -> Result<String, RemoteError> {
        // Sync uses the legacy /api/repo/files endpoint (no repo name in URL).
        let url = format!("{}/api/repo/files", self.base_url);
        let resp: RepoFileListResponse = self.get_json(&url).await?;
        Ok(resp.head_version)
    }

    async fn list_versions(&self, _repo: &str) -> Result<Vec<VersionSummary>, RemoteError> {
        let url = format!("{}/api/versions", self.base_url);
        let versions: Vec<SyncVersionMeta> = self.get_json(&url).await?;
        Ok(versions.into_iter().map(|v| VersionSummary { version_id: v.version_id }).collect())
    }

    async fn get_version_changes(
        &self,
        _repo: &str,
        version_id: &str,
    ) -> Result<Vec<VersionFileChange>, RemoteError> {
        let url = format!("{}/api/versions/{}", self.base_url, version_id);
        let resp: SyncVersionChanges = self.get_json(&url).await?;
        Ok(resp
            .file_changes
            .into_iter()
            .map(|fc| VersionFileChange {
                path: fc.path,
                change: match fc.change_type {
                    SyncChangeType::Added => ChangeKind::Added,
                    SyncChangeType::Modified => ChangeKind::Modified,
                    SyncChangeType::Removed => ChangeKind::Removed,
                },
            })
            .collect())
    }

    async fn download_file(&self, _repo: &str, path: &str) -> Result<Vec<u8>, RemoteError> {
        use base64::prelude::{Engine, BASE64_STANDARD as BASE64};
        let encoded = urlencoding_encode(path);
        let url = format!("{}/api/files/{}", self.base_url, encoded);
        let resp: FileDownloadResponse = self.get_json(&url).await?;
        Ok(BASE64.decode(resp.content_base64.as_bytes())?)
    }
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
