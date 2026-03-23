//! HTTP client for proxying CLI commands to a remote vai server.
//!
//! When a `[remote]` section is present in `.vai/config.toml`, CLI commands
//! use this module to forward requests to the remote server instead of
//! operating on the local `.vai/` directory.
//!
//! ## Usage
//!
//! ```ignore
//! let client = RemoteClient::new(&config.remote.unwrap())?;
//! let status: serde_json::Value = client.get("/api/status").await?;
//! let result: MyType = client.post("/api/issues", &body).await?;
//! ```

use reqwest::{Client, Method};
use serde::{de::DeserializeOwned, Serialize};
use thiserror::Error;

use crate::repo::{ApiKeyError, RemoteServerConfig};

// ── Error type ────────────────────────────────────────────────────────────────

/// Errors that can occur when communicating with a remote vai server.
#[derive(Debug, Error)]
pub enum RemoteClientError {
    /// The server returned a non-2xx HTTP status.
    #[error("server returned {status}: {body}")]
    HttpError { status: u16, body: String },

    /// A network-level error occurred (connection refused, timeout, etc.).
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),

    /// The API key could not be resolved from the remote config.
    #[error("API key error: {0}")]
    ApiKey(#[from] ApiKeyError),

    /// Response body was not valid JSON.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

// ── Client ────────────────────────────────────────────────────────────────────

/// A ready-to-use HTTP client for a remote vai server.
///
/// Resolves the API key from `RemoteServerConfig` at construction time and
/// injects `Authorization: Bearer <key>` into every request.
#[derive(Debug)]
pub struct RemoteClient {
    client: Client,
    /// Base URL with no trailing slash, e.g. `https://vai.example.com`.
    base_url: String,
    api_key: String,
}

impl RemoteClient {
    /// Creates a new `RemoteClient` by resolving the API key from `config`.
    ///
    /// Resolution order: `api_key_env` → `api_key_cmd` → `api_key`.
    pub fn new(config: &RemoteServerConfig) -> Result<Self, RemoteClientError> {
        let api_key = config.resolve_api_key()?;
        Ok(Self {
            client: Client::new(),
            base_url: config.url.trim_end_matches('/').to_string(),
            api_key,
        })
    }

    /// Sends a `GET` request and deserializes the JSON response body.
    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, RemoteClientError> {
        self.send::<(), T>(Method::GET, path, None).await
    }

    /// Sends a `POST` request with a JSON body and deserializes the response.
    pub async fn post<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, RemoteClientError> {
        self.send::<B, T>(Method::POST, path, Some(body)).await
    }

    /// Sends a `PATCH` request with a JSON body and deserializes the response.
    pub async fn patch<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, RemoteClientError> {
        self.send::<B, T>(Method::PATCH, path, Some(body)).await
    }

    /// Sends a `DELETE` request and deserializes the JSON response body.
    pub async fn delete<T: DeserializeOwned>(&self, path: &str) -> Result<T, RemoteClientError> {
        self.send::<(), T>(Method::DELETE, path, None).await
    }

    /// Builds, sends, and parses a request, mapping non-2xx responses to errors.
    async fn send<B: Serialize, T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
    ) -> Result<T, RemoteClientError> {
        let url = format!("{}{}", self.base_url, path);
        let mut builder = self
            .client
            .request(method, &url)
            .bearer_auth(&self.api_key)
            .header("Accept", "application/json");

        if let Some(b) = body {
            builder = builder.json(b);
        }

        let resp = builder.send().await?;
        let status = resp.status();

        if status.is_success() {
            Ok(resp.json::<T>().await?)
        } else {
            let body = resp.text().await.unwrap_or_default();
            Err(RemoteClientError::HttpError {
                status: status.as_u16(),
                body,
            })
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::RemoteServerConfig;

    fn make_config(api_key: Option<&str>) -> RemoteServerConfig {
        RemoteServerConfig {
            url: "https://vai.example.com".to_string(),
            api_key: api_key.map(|s| s.to_string()),
            api_key_env: None,
            api_key_cmd: None,
        }
    }

    #[test]
    fn client_resolves_literal_key() {
        let config = make_config(Some("vai_key_test123"));
        let client = RemoteClient::new(&config).unwrap();
        assert_eq!(client.api_key, "vai_key_test123");
        assert_eq!(client.base_url, "https://vai.example.com");
    }

    #[test]
    fn client_strips_trailing_slash() {
        let mut config = make_config(Some("key"));
        config.url = "https://vai.example.com/".to_string();
        let client = RemoteClient::new(&config).unwrap();
        assert_eq!(client.base_url, "https://vai.example.com");
    }

    #[test]
    fn client_resolves_env_var() {
        std::env::set_var("VAI_TEST_API_KEY_REMOTE_CLIENT", "env_key_abc");
        let config = RemoteServerConfig {
            url: "https://vai.example.com".to_string(),
            api_key: None,
            api_key_env: Some("VAI_TEST_API_KEY_REMOTE_CLIENT".to_string()),
            api_key_cmd: None,
        };
        let client = RemoteClient::new(&config).unwrap();
        assert_eq!(client.api_key, "env_key_abc");
        std::env::remove_var("VAI_TEST_API_KEY_REMOTE_CLIENT");
    }

    #[test]
    fn client_resolves_cmd() {
        let config = RemoteServerConfig {
            url: "https://vai.example.com".to_string(),
            api_key: None,
            api_key_env: None,
            api_key_cmd: Some("echo cmd_key_xyz".to_string()),
        };
        let client = RemoteClient::new(&config).unwrap();
        assert_eq!(client.api_key, "cmd_key_xyz");
    }

    #[test]
    fn client_errors_when_no_key_configured() {
        let config = make_config(None);
        let err = RemoteClient::new(&config).unwrap_err();
        assert!(matches!(err, RemoteClientError::ApiKey(_)));
    }
}
