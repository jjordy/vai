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

// ── Retry configuration ───────────────────────────────────────────────────────

/// Maximum number of retry attempts after the first failure.
const MAX_RETRIES: u32 = 3;
/// Base delay in milliseconds for exponential backoff.
#[cfg(not(test))]
const BASE_DELAY_MS: u64 = 200;
/// In tests, use a near-zero base delay so retries don't slow the test suite.
#[cfg(test)]
const BASE_DELAY_MS: u64 = 1;
/// Maximum delay cap in milliseconds.
const MAX_DELAY_MS: u64 = 5_000;

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

    /// Response body was not valid JSON.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

// ── Retry policy ──────────────────────────────────────────────────────────────

/// Controls which failure classes trigger a retry.
///
/// See the module-level docs and the retry table in issue #295 for the
/// reasoning behind each policy.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RetryPolicy {
    /// Retry on connect errors, timeouts, and 5xx responses.
    ///
    /// Use for `GET`, `PATCH`, `DELETE`, and POST calls whose server-side
    /// operation is idempotent (e.g. `/api/auth/token`).
    Idempotent,
    /// Retry only on connect errors (i.e. the request never reached the
    /// server, so no server-side work was done).
    ///
    /// Use for non-idempotent `POST` calls that create resources (workspaces,
    /// submit, issues, keys).  Retrying on timeout or 5xx could duplicate work.
    ConnectOnly,
}

// ── Failure classification ────────────────────────────────────────────────────

/// Internal classification of a single request attempt's failure.
enum FailureClass {
    /// TCP connect-level error — request never reached the server.
    ConnectError(reqwest::Error),
    /// Request timed out — server may or may not have processed it.
    Timeout(reqwest::Error),
    /// Other transport-level error (treated conservatively as connect error).
    OtherNetwork(reqwest::Error),
    /// Server returned a 5xx status.
    ServerError { status: u16, body: String },
    /// Server returned a 4xx or other non-success, non-5xx status.
    ClientError { status: u16, body: String },
}

impl FailureClass {
    /// Returns `true` if this failure is safe to retry under `policy`.
    fn should_retry(&self, policy: RetryPolicy) -> bool {
        match self {
            FailureClass::ConnectError(_) | FailureClass::OtherNetwork(_) => true,
            FailureClass::Timeout(_) | FailureClass::ServerError { .. } => {
                policy == RetryPolicy::Idempotent
            }
            FailureClass::ClientError { .. } => false,
        }
    }

    /// Converts this classification into a `RemoteClientError` to return to
    /// the caller after all retries are exhausted (or retrying is not safe).
    fn into_error(self) -> RemoteClientError {
        match self {
            FailureClass::ConnectError(e)
            | FailureClass::Timeout(e)
            | FailureClass::OtherNetwork(e) => RemoteClientError::Network(e),
            FailureClass::ServerError { status, body }
            | FailureClass::ClientError { status, body } => {
                RemoteClientError::HttpError { status, body }
            }
        }
    }

    /// A short label used in retry log messages.
    fn label(&self) -> &'static str {
        match self {
            FailureClass::ConnectError(_) => "connect error",
            FailureClass::Timeout(_) => "timeout",
            FailureClass::OtherNetwork(_) => "network error",
            FailureClass::ServerError { .. } => "5xx response",
            FailureClass::ClientError { .. } => "4xx response",
        }
    }
}

// ── Backoff helpers ───────────────────────────────────────────────────────────

/// Returns a full-jitter delay for the given zero-indexed `attempt`.
///
/// Formula: `random_in(0, min(BASE_DELAY_MS * 2^attempt, MAX_DELAY_MS))`.
/// Jitter source: low bits of `SystemTime::now()` subsecond nanos — not
/// cryptographic, but sufficient to avoid thundering-herd synchronization.
fn jitter_delay(attempt: u32) -> std::time::Duration {
    use std::time::{SystemTime, UNIX_EPOCH};

    let cap_ms = (BASE_DELAY_MS.saturating_mul(1u64 << attempt)).min(MAX_DELAY_MS);
    if cap_ms == 0 {
        return std::time::Duration::ZERO;
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64;
    std::time::Duration::from_millis(nanos % cap_ms)
}

// ── Client ────────────────────────────────────────────────────────────────────

/// A ready-to-use HTTP client for a remote vai server.
///
/// Injects `Authorization: Bearer <key>` into every request and retries
/// transient failures with exponential backoff and full jitter.
#[derive(Debug)]
pub struct RemoteClient {
    client: Client,
    /// Base URL with no trailing slash, e.g. `https://vai.example.com`.
    base_url: String,
    api_key: String,
}

impl RemoteClient {
    /// Creates a new `RemoteClient` for the given server URL and API key.
    ///
    /// `url` may have a trailing slash — it will be stripped.
    pub fn new(url: &str, api_key: &str) -> Self {
        Self {
            client: Client::new(),
            base_url: url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
        }
    }

    /// Sends a `GET` request and deserializes the JSON response body.
    ///
    /// Retries on connect error, timeout, and 5xx (idempotent — safe to retry
    /// unconditionally).
    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, RemoteClientError> {
        self.send_with_policy::<(), T>(Method::GET, path, None, RetryPolicy::Idempotent).await
    }

    /// Sends a `POST` request with a JSON body and deserializes the response.
    ///
    /// Retries on **connect errors only** (safe default for non-idempotent
    /// resource-creating endpoints).  If the server accepted the request but
    /// returned 5xx or the call timed out, the error is surfaced immediately
    /// rather than risking duplicate creation.
    ///
    /// For POST calls that are idempotent (e.g. `/api/auth/token`), use
    /// [`post_idempotent`](Self::post_idempotent) instead.
    pub async fn post<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, RemoteClientError> {
        self.send_with_policy::<B, T>(Method::POST, path, Some(body), RetryPolicy::ConnectOnly)
            .await
    }

    /// Sends an idempotent `POST` request with a JSON body and deserializes
    /// the response.
    ///
    /// Retries on connect error, timeout, and 5xx — the same policy as `GET`.
    /// Only use this for POST endpoints whose server-side operation is safe to
    /// repeat (e.g. `/api/auth/token`).
    pub async fn post_idempotent<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, RemoteClientError> {
        self.send_with_policy::<B, T>(Method::POST, path, Some(body), RetryPolicy::Idempotent)
            .await
    }

    /// Sends a `PATCH` request with a JSON body and deserializes the response.
    ///
    /// Retries on connect error, timeout, and 5xx (idempotent).
    pub async fn patch<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, RemoteClientError> {
        self.send_with_policy::<B, T>(Method::PATCH, path, Some(body), RetryPolicy::Idempotent)
            .await
    }

    /// Sends a `DELETE` request and deserializes the JSON response body.
    ///
    /// Retries on connect error, timeout, and 5xx (idempotent).
    pub async fn delete<T: DeserializeOwned>(&self, path: &str) -> Result<T, RemoteClientError> {
        self.send_with_policy::<(), T>(Method::DELETE, path, None, RetryPolicy::Idempotent).await
    }

    /// Builds, sends, and parses a request with retry logic.
    ///
    /// Retries up to [`MAX_RETRIES`] times with exponential backoff and full
    /// jitter.  Which failure classes trigger a retry is determined by
    /// `policy`.
    async fn send_with_policy<B: Serialize, T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
        policy: RetryPolicy,
    ) -> Result<T, RemoteClientError> {
        let url = format!("{}{}", self.base_url, path);
        let mut last_failure: Option<FailureClass> = None;

        for attempt in 0..=MAX_RETRIES {
            if attempt > 0 {
                let delay = jitter_delay(attempt - 1);
                eprintln!(
                    "[vai] retrying {} {} (attempt {}/{}, delay {}ms)",
                    method,
                    url,
                    attempt,
                    MAX_RETRIES,
                    delay.as_millis(),
                );
                tokio::time::sleep(delay).await;
            }

            let mut builder = self
                .client
                .request(method.clone(), &url)
                .bearer_auth(&self.api_key)
                .header("Accept", "application/json");

            if let Some(b) = body {
                builder = builder.json(b);
            }

            let failure = match builder.send().await {
                Err(e) => {
                    if e.is_connect() {
                        FailureClass::ConnectError(e)
                    } else if e.is_timeout() {
                        FailureClass::Timeout(e)
                    } else {
                        FailureClass::OtherNetwork(e)
                    }
                }
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() {
                        return Ok(resp.json::<T>().await?);
                    } else if status.is_server_error() {
                        let body_text = resp.text().await.unwrap_or_default();
                        FailureClass::ServerError { status: status.as_u16(), body: body_text }
                    } else {
                        let body_text = resp.text().await.unwrap_or_default();
                        FailureClass::ClientError { status: status.as_u16(), body: body_text }
                    }
                }
            };

            let retrying = failure.should_retry(policy) && attempt < MAX_RETRIES;
            if retrying {
                eprintln!(
                    "[vai] request failed with {} — will retry ({}/{}): {} {}",
                    failure.label(),
                    attempt + 1,
                    MAX_RETRIES,
                    method,
                    url,
                );
            }
            last_failure = Some(failure);
            if !retrying {
                break;
            }
        }

        Err(last_failure.expect("loop always sets last_failure").into_error())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_stores_key_and_url() {
        let client = RemoteClient::new("https://vai.example.com", "vai_key_test123");
        assert_eq!(client.api_key, "vai_key_test123");
        assert_eq!(client.base_url, "https://vai.example.com");
    }

    #[test]
    fn client_strips_trailing_slash() {
        let client = RemoteClient::new("https://vai.example.com/", "key");
        assert_eq!(client.base_url, "https://vai.example.com");
    }

    #[test]
    fn jitter_delay_stays_within_cap() {
        // Run many iterations to verify the delay never exceeds the cap.
        for attempt in 0u32..4 {
            let cap_ms =
                (BASE_DELAY_MS.saturating_mul(1u64 << attempt)).min(MAX_DELAY_MS);
            let delay = jitter_delay(attempt);
            assert!(
                delay.as_millis() as u64 <= cap_ms,
                "attempt {attempt}: delay {}ms exceeded cap {cap_ms}ms",
                delay.as_millis()
            );
        }
    }

    #[test]
    fn failure_class_4xx_never_retries() {
        let err = FailureClass::ClientError { status: 404, body: String::new() };
        assert!(!err.should_retry(RetryPolicy::Idempotent));
        assert!(!err.should_retry(RetryPolicy::ConnectOnly));
    }

    #[test]
    fn failure_class_5xx_only_retries_when_idempotent() {
        let err = FailureClass::ServerError { status: 503, body: String::new() };
        assert!(err.should_retry(RetryPolicy::Idempotent));
        assert!(!err.should_retry(RetryPolicy::ConnectOnly));
    }
}
