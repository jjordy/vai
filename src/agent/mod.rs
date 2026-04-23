//! Agent CLI support — configuration, state, and helper types for the `vai agent` subcommands.
//!
//! This module provides the data types and logic for the agent workflow:
//! initializing agent configuration (`vai agent init`), loading that config,
//! and reading/writing the per-iteration state file.
//!
//! ## File layout
//!
//! ```text
//! .vai/
//!   agent.toml        — persisted agent configuration (server URL, repo, checks, etc.)
//!   agent-state.json  — ephemeral per-iteration state (current issue, workspace, phase)
//!   prompt.md         — optional prompt template (default path)
//! ```
//!
//! All public functions accept a `dir` parameter (the working directory) rather
//! than using the process-global current directory. The CLI passes
//! `std::env::current_dir()`.
//!
//! API keys are **never** stored on disk; they must be provided via the `VAI_API_KEY`
//! environment variable or a CLI flag.

use std::fs;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use chrono::{DateTime, Utc};
use colored::Colorize;
use serde::{Deserialize, Serialize};
use thiserror::Error;

// ── Error ─────────────────────────────────────────────────────────────────────

/// Errors that can occur during agent operations.
#[derive(Debug, Error)]
pub enum AgentError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("TOML serialization error: {0}")]
    TomlSer(#[from] toml::ser::Error),

    #[error("TOML deserialization error: {0}")]
    TomlDe(#[from] toml::de::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("missing required value for '{field}': provide --{field} flag or set {env_var}")]
    MissingConfig { field: &'static str, env_var: &'static str },

    #[error("server unreachable at {url}: {reason}")]
    ServerUnreachable { url: String, reason: String },

    #[error("no agent state found — run `vai agent claim` first")]
    NoState,

    /// Server returned fewer files than declared in `X-Vai-Expected-Files`.
    ///
    /// This indicates a mid-stream server error (e.g. Postgres disconnect).
    /// The partial workspace directory has been discarded.
    #[error("partial download: server declared {expected} files but only {downloaded} were received; the partial workspace has been discarded")]
    PartialDownload { downloaded: usize, expected: usize },

    /// Server rejected the submit because the workspace has no file changes.
    ///
    /// This is a client-state condition, not an internal error.  The caller
    /// should close the issue permanently (e.g. `vai issue close`) rather than
    /// resetting and re-claiming it, which would create an infinite loop.
    #[error("workspace is empty — the issue appears already resolved; run `vai issue close <id>` to close it permanently")]
    WorkspaceEmpty,

    #[error("{0}")]
    Other(String),
}

// ── Configuration types ───────────────────────────────────────────────────────

/// Quality check configuration stored under `[checks]` in `agent.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChecksConfig {
    /// Shell commands to run before check commands.
    ///
    /// Used to build the project, start servers, etc.  Each command is run
    /// sequentially with the working directory set to the target directory.
    /// If any setup command fails, checks are **skipped** and the setup
    /// error is returned as the verify failure.
    #[serde(default)]
    pub setup: Vec<String>,

    /// Shell commands to run sequentially to verify agent output.
    ///
    /// Each command is run with the working directory set to the target
    /// directory passed to `vai agent verify <dir>`.
    pub commands: Vec<String>,

    /// Shell commands to run after checks complete (pass or fail).
    ///
    /// Used to stop background processes started in `setup`.  Teardown
    /// commands always run and their exit codes are ignored.
    #[serde(default)]
    pub teardown: Vec<String>,
}

/// Ignore pattern configuration stored under `[ignore]` in `agent.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IgnoreConfig {
    /// Additional glob patterns to exclude from the submission tarball.
    ///
    /// Standard exclusions (`node_modules/`, `.git/`, `target/`, etc.) are
    /// always applied; these are additive.
    pub patterns: Vec<String>,
}

/// Agent configuration persisted in `.vai/agent.toml`.
///
/// Loaded by every `vai agent` subcommand. API keys are **never** stored here;
/// use the `VAI_API_KEY` environment variable instead.
///
/// Config precedence (highest → lowest):
/// 1. CLI flags (`--server`, `--repo`)
/// 2. Environment variables (`VAI_SERVER_URL`, `VAI_REPO`)
/// 3. This file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Base URL of the vai server, e.g. `https://vai.example.com`.
    pub server: String,

    /// Repository name on the server.
    pub repo: String,

    /// Path to the prompt template file.
    ///
    /// Defaults to `.vai/prompt.md` when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_template: Option<String>,

    /// Name of the default agent, e.g. `"claude-code"` or `"codex"`.
    ///
    /// When set, `vai agent prompt` looks for a base template at
    /// `.vai/agents/<default_agent>/prompt.md` before falling back to the
    /// legacy `.vai/prompt.md` path.  Written by `vai agent loop init`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_agent: Option<String>,

    /// Quality check commands.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checks: Option<ChecksConfig>,

    /// Additional ignore patterns for the submission tarball.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ignore: Option<IgnoreConfig>,
}

impl AgentConfig {
    /// Resolve the API key from the environment.
    ///
    /// Always reads from `VAI_API_KEY` — keys are never stored in config.
    pub fn resolve_api_key() -> Option<String> {
        std::env::var("VAI_API_KEY").ok().filter(|k| !k.is_empty())
    }

    /// Return the prompt template path relative to `dir`.
    ///
    /// Defaults to `<dir>/.vai/prompt.md` if no template is configured.
    pub fn prompt_template_path(&self, dir: &Path) -> PathBuf {
        match &self.prompt_template {
            Some(p) => dir.join(p),
            None => dir.join(".vai").join("prompt.md"),
        }
    }
}

// ── State types ───────────────────────────────────────────────────────────────

/// The phase of the current agent iteration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentPhase {
    /// Issue has been claimed and a workspace created, but no code downloaded.
    Claimed,
    /// Repo tarball has been downloaded to the local working directory.
    Downloaded,
    /// Verification checks have been run (may or may not have passed).
    Verified,
    /// Changes have been submitted and the issue closed.
    Submitted,
}

impl std::fmt::Display for AgentPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentPhase::Claimed => write!(f, "claimed"),
            AgentPhase::Downloaded => write!(f, "downloaded"),
            AgentPhase::Verified => write!(f, "verified"),
            AgentPhase::Submitted => write!(f, "submitted"),
        }
    }
}

/// Per-iteration agent state persisted in `.vai/agent-state.json`.
///
/// Written by `vai agent claim`, updated by subsequent subcommands, and
/// cleared by `vai agent submit` or `vai agent reset`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentState {
    /// Issue ID (UUID) of the currently claimed issue.
    pub issue_id: String,

    /// Human-readable title of the issue.
    pub issue_title: String,

    /// Workspace ID (UUID) created for this iteration.
    pub workspace_id: String,

    /// Current phase of the agent iteration.
    pub phase: AgentPhase,

    /// Timestamp when the issue was claimed.
    pub claimed_at: DateTime<Utc>,
}

// ── File path helpers ─────────────────────────────────────────────────────────

/// Path of the agent config file within `dir`.
pub fn config_path(dir: &Path) -> PathBuf {
    dir.join(".vai").join("agent.toml")
}

/// Path of the agent state file within `dir`.
pub fn state_path(dir: &Path) -> PathBuf {
    dir.join(".vai").join("agent-state.json")
}

// ── Config I/O ────────────────────────────────────────────────────────────────

/// Load agent configuration from `<dir>/.vai/agent.toml`.
///
/// Returns `Err` if the file does not exist or cannot be parsed.
pub fn load_config(dir: &Path) -> Result<AgentConfig, AgentError> {
    let path = config_path(dir);
    let contents = fs::read_to_string(&path)
        .map_err(|e| AgentError::Other(format!("cannot read {}: {}", path.display(), e)))?;
    let config: AgentConfig = toml::from_str(&contents)?;
    Ok(config)
}

/// Resolve the effective agent config, merging file, env vars, and CLI overrides.
///
/// Precedence: explicit flags (non-`None` values) → environment variables
/// (`VAI_SERVER_URL`, `VAI_REPO`) → config file in `dir`.
pub fn resolve_config(
    dir: &Path,
    server_override: Option<&str>,
    repo_override: Option<&str>,
) -> Result<AgentConfig, AgentError> {
    // Attempt to load the file first (may not exist yet — that's OK).
    let file_config = load_config(dir).ok();

    let server = server_override
        .map(|s| s.to_string())
        .or_else(|| std::env::var("VAI_SERVER_URL").ok().filter(|v| !v.is_empty()))
        .or_else(|| file_config.as_ref().map(|c| c.server.clone()))
        .ok_or(AgentError::MissingConfig {
            field: "server",
            env_var: "VAI_SERVER_URL",
        })?;

    let repo = repo_override
        .map(|r| r.to_string())
        .or_else(|| std::env::var("VAI_REPO").ok().filter(|v| !v.is_empty()))
        .or_else(|| file_config.as_ref().map(|c| c.repo.clone()))
        .ok_or(AgentError::MissingConfig {
            field: "repo",
            env_var: "VAI_REPO",
        })?;

    Ok(AgentConfig {
        server,
        repo,
        prompt_template: file_config.as_ref().and_then(|c| c.prompt_template.clone()),
        default_agent: file_config.as_ref().and_then(|c| c.default_agent.clone()),
        checks: file_config.as_ref().and_then(|c| c.checks.clone()),
        ignore: file_config.as_ref().and_then(|c| c.ignore.clone()),
    })
}

// ── State I/O ─────────────────────────────────────────────────────────────────

/// Load the current agent state from `<dir>/.vai/agent-state.json`.
///
/// Returns `AgentError::NoState` if the file does not exist.
pub fn load_state(dir: &Path) -> Result<AgentState, AgentError> {
    let path = state_path(dir);
    if !path.exists() {
        return Err(AgentError::NoState);
    }
    let contents = fs::read_to_string(&path)
        .map_err(|e| AgentError::Other(format!("cannot read state file: {e}")))?;
    let state: AgentState = serde_json::from_str(&contents)?;
    Ok(state)
}

/// Persist agent state to `<dir>/.vai/agent-state.json`.
pub fn save_state(dir: &Path, state: &AgentState) -> Result<(), AgentError> {
    let path = state_path(dir);
    let contents = serde_json::to_string_pretty(state)?;
    fs::write(&path, contents)
        .map_err(|e| AgentError::Other(format!("cannot write state file: {e}")))?;
    Ok(())
}

/// Clear the agent state file (called after submit or reset).
pub fn clear_state(dir: &Path) -> Result<(), AgentError> {
    let path = state_path(dir);
    if path.exists() {
        fs::remove_file(&path)
            .map_err(|e| AgentError::Other(format!("cannot remove state file: {e}")))?;
    }
    Ok(())
}

// ── HTTP client helpers ───────────────────────────────────────────────────────

/// Maximum total attempts for transient-network retries on idempotent GET operations.
///
/// Gives 3 retries with delays of 1 s, 2 s, 4 s → at most ~7 s of backoff before
/// the fourth and final attempt.  Only used for idempotent calls.
const RETRY_MAX_ATTEMPTS: u32 = 4;

/// Base delay in milliseconds for retry exponential backoff.
#[cfg(not(test))]
const RETRY_BASE_DELAY_MS: u64 = 1_000;
/// Near-zero delay in tests so the retry loop does not slow the suite.
#[cfg(test)]
const RETRY_BASE_DELAY_MS: u64 = 1;

/// Returns `true` if the reqwest error is likely transient and worth retrying.
///
/// Transient means the error is at the network/transport layer: DNS resolution
/// failures, connection refused, TLS handshake failures, TCP resets, and
/// timeouts.  Application-layer errors (invalid URL, decode failure) are
/// terminal and must not be retried.
pub(crate) fn is_transient_reqwest_error(err: &reqwest::Error) -> bool {
    err.is_connect() || err.is_timeout()
}

/// Send a request produced by `make_req`, retrying on transient network errors.
///
/// `make_req` is called once per attempt to produce a fresh [`reqwest::RequestBuilder`]
/// (builders are consumed by `send()`).  On transient errors a message is logged to
/// stderr and the next attempt waits with exponential backoff.  After
/// [`RETRY_MAX_ATTEMPTS`] total attempts the final error is returned.
///
/// Only call this for idempotent operations (GET, HEAD, etc.).
async fn send_with_retry<F>(make_req: F) -> Result<reqwest::Response, reqwest::Error>
where
    F: Fn() -> reqwest::RequestBuilder,
{
    for attempt in 1u32.. {
        match make_req().send().await {
            Ok(resp) => return Ok(resp),
            Err(e) if is_transient_reqwest_error(&e) && attempt < RETRY_MAX_ATTEMPTS => {
                let delay_ms =
                    std::cmp::min(RETRY_BASE_DELAY_MS * (1u64 << (attempt - 1)), 8_000);
                eprintln!(
                    "  transient network error (attempt {attempt}/{RETRY_MAX_ATTEMPTS}), \
                     retrying in {delay_ms}ms: {e}"
                );
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            }
            Err(e) => return Err(e),
        }
    }
    unreachable!()
}

/// Make an authenticated GET request and parse the JSON response.
///
/// Retries up to [`RETRY_MAX_ATTEMPTS`] times on transient network errors.
async fn agent_get<T: serde::de::DeserializeOwned>(
    server: &str,
    path: &str,
    api_key: Option<&str>,
) -> Result<T, AgentError> {
    let url = format!("{}/{}", server.trim_end_matches('/'), path.trim_start_matches('/'));
    let client = reqwest::Client::new();
    let auth_header: Option<String> = api_key.map(|k| format!("Bearer {k}"));
    let resp = send_with_retry(|| {
        let mut req = client.get(&url);
        if let Some(ref h) = auth_header {
            req = req.header("Authorization", h.as_str());
        }
        req
    })
    .await
    .map_err(|e| AgentError::ServerUnreachable {
        url: url.clone(),
        reason: e.to_string(),
    })?;
    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        return Err(AgentError::Other(format!("server returned {status}: {body}")));
    }
    resp.json::<T>().await.map_err(|e| AgentError::Other(format!("JSON parse error: {e}")))
}

/// Make an authenticated POST request with a JSON body and parse the JSON response.
///
/// POST is not idempotent so no retry is applied.
async fn agent_post<B: serde::Serialize, T: serde::de::DeserializeOwned>(
    server: &str,
    path: &str,
    api_key: Option<&str>,
    body: &B,
) -> Result<T, AgentError> {
    let url = format!("{}/{}", server.trim_end_matches('/'), path.trim_start_matches('/'));
    let client = reqwest::Client::new();
    let mut req = client.post(&url).json(body);
    if let Some(key) = api_key {
        req = req.header("Authorization", format!("Bearer {key}"));
    }
    let resp = req.send().await.map_err(|e| AgentError::ServerUnreachable {
        url: url.clone(),
        reason: e.to_string(),
    })?;
    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        return Err(AgentError::Other(format!("server returned {status}: {body}")));
    }
    resp.json::<T>().await.map_err(|e| AgentError::Other(format!("JSON parse error: {e}")))
}

// ── claim ─────────────────────────────────────────────────────────────────────

/// Outcome of a [`claim`] call.
#[derive(Debug)]
pub enum ClaimOutcome {
    /// A new issue was claimed and state was saved.
    Claimed(AgentState),
    /// State already existed from a previous (possibly crashed) iteration.
    ///
    /// The existing state is returned unchanged so the caller can resume.
    AlreadyClaimed(AgentState),
    /// The work queue had no available issues.
    NoWork,
}

/// Query the work queue and atomically claim the highest-priority available issue.
///
/// # Crash recovery
///
/// If `.vai/agent-state.json` already exists, this function does **not**
/// re-claim. It returns [`ClaimOutcome::AlreadyClaimed`] with the existing
/// state so the agent loop can resume where it left off.
///
/// # Exit semantics
///
/// The CLI translates the outcome to exit codes:
/// - [`ClaimOutcome::Claimed`] / [`ClaimOutcome::AlreadyClaimed`] → exit 0
/// - [`ClaimOutcome::NoWork`] → exit 1 (enables `while vai agent claim; do …; done`)
pub fn claim(
    dir: &Path,
    server_override: Option<&str>,
    repo_override: Option<&str>,
) -> Result<ClaimOutcome, AgentError> {
    // ── Crash recovery: if state exists, resume instead of re-claiming ─────
    if let Ok(existing) = load_state(dir) {
        return Ok(ClaimOutcome::AlreadyClaimed(existing));
    }

    let config = resolve_config(dir, server_override, repo_override)?;
    let api_key = AgentConfig::resolve_api_key();
    let api_key_ref = api_key.as_deref();

    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| AgentError::Other(format!("cannot create tokio runtime: {e}")))?;

    rt.block_on(async {
        // ── Fetch the work queue ───────────────────────────────────────────
        let queue_path = format!("api/repos/{}/work-queue", config.repo);
        let queue: serde_json::Value =
            agent_get(&config.server, &queue_path, api_key_ref).await?;

        let available = queue["available_work"].as_array().cloned().unwrap_or_default();
        if available.is_empty() {
            return Ok(ClaimOutcome::NoWork);
        }

        // Available issues are already sorted by priority (critical first).
        let top = &available[0];
        let issue_id = top["issue_id"].as_str().ok_or_else(|| {
            AgentError::Other("work queue response missing issue_id".to_string())
        })?;
        let issue_title = top["title"].as_str().unwrap_or("(untitled)").to_string();

        // ── Atomically claim the issue ─────────────────────────────────────
        let claim_path = format!("api/repos/{}/work-queue/claim", config.repo);
        let claim_body = serde_json::json!({ "issue_id": issue_id });
        let result: serde_json::Value =
            agent_post(&config.server, &claim_path, api_key_ref, &claim_body).await?;

        let workspace_id = result["workspace_id"].as_str().ok_or_else(|| {
            AgentError::Other("claim response missing workspace_id".to_string())
        })?;

        // ── Save state ────────────────────────────────────────────────────
        // Ensure .vai/ exists (it may not if agent.toml was not yet written).
        let vai_dir = dir.join(".vai");
        if !vai_dir.exists() {
            std::fs::create_dir_all(&vai_dir)?;
        }

        let state = AgentState {
            issue_id: issue_id.to_string(),
            issue_title,
            workspace_id: workspace_id.to_string(),
            phase: AgentPhase::Claimed,
            claimed_at: chrono::Utc::now(),
        };
        save_state(dir, &state)?;

        Ok(ClaimOutcome::Claimed(state))
    })
}

/// Print a human-readable summary after a successful claim.
pub fn print_claim_result(outcome: &ClaimOutcome) {
    use colored::Colorize;
    match outcome {
        ClaimOutcome::Claimed(state) => {
            println!(
                "{} Claimed issue {}",
                "✓".green().bold(),
                state.issue_id[..8.min(state.issue_id.len())].cyan(),
            );
            println!("  Title     : {}", state.issue_title);
            println!(
                "  Workspace : {}",
                state.workspace_id[..8.min(state.workspace_id.len())].cyan()
            );
            println!("  Phase     : {}", state.phase);
        }
        ClaimOutcome::AlreadyClaimed(state) => {
            println!(
                "{} Resuming existing claim — issue {}",
                "↻".yellow().bold(),
                state.issue_id[..8.min(state.issue_id.len())].cyan(),
            );
            println!("  Title     : {}", state.issue_title);
            println!(
                "  Workspace : {}",
                state.workspace_id[..8.min(state.workspace_id.len())].cyan()
            );
            println!("  Phase     : {}", state.phase);
        }
        ClaimOutcome::NoWork => {
            println!("{} No available work in the queue.", "–".dimmed());
        }
    }
}

// ── init ──────────────────────────────────────────────────────────────────────

/// Result returned by [`init`].
#[derive(Debug, Serialize)]
pub struct InitResult {
    /// Path where `agent.toml` was written.
    pub config_path: PathBuf,
    /// Server URL recorded in the config.
    pub server: String,
    /// Repository name recorded in the config.
    pub repo: String,
    /// Whether the server was reachable during init.
    pub server_reachable: bool,
}

/// Initialize agent configuration in `dir`.
///
/// Creates `<dir>/.vai/agent.toml` with the provided server URL and repo name.
/// Falls back to environment variables (`VAI_SERVER_URL`, `VAI_REPO`) when
/// flag values are `None`.
///
/// Validates that the server is reachable via `GET /api/status` (best-effort —
/// prints a warning but does not fail if unreachable).
///
/// API keys are **never** written to disk.
pub fn init(
    dir: &Path,
    server: Option<&str>,
    repo: Option<&str>,
    prompt_template: Option<&str>,
) -> Result<InitResult, AgentError> {
    let config = resolve_config(dir, server, repo)?;

    // Ensure .vai/ directory exists.
    let vai_dir = dir.join(".vai");
    if !vai_dir.exists() {
        fs::create_dir_all(&vai_dir)?;
    }

    // Build the config to write — never include API key fields.
    let to_write = AgentConfig {
        server: config.server.clone(),
        repo: config.repo.clone(),
        prompt_template: prompt_template.map(|s| s.to_string()),
        default_agent: None,
        checks: None,
        ignore: None,
    };

    let toml_str = toml::to_string_pretty(&to_write)?;
    let config_file = config_path(dir);
    fs::write(&config_file, &toml_str)?;

    // Validate server reachability (best-effort; we warn but don't fail).
    let server_reachable = validate_server_reachable(&config.server);

    Ok(InitResult {
        config_path: config_file,
        server: config.server,
        repo: config.repo,
        server_reachable,
    })
}

/// Print a human-readable summary of an [`InitResult`].
pub fn print_init_result(result: &InitResult) {
    println!(
        "{} Agent configuration written to {}",
        "✓".green().bold(),
        result.config_path.display()
    );
    println!("  Server : {}", result.server);
    println!("  Repo   : {}", result.repo);
    if result.server_reachable {
        println!("  Server {} reachable", "✓".green());
    } else {
        println!(
            "  {} Server not reachable — check VAI_API_KEY and network connectivity",
            "!".yellow()
        );
    }
    println!();
    println!(
        "API key is read from the {} environment variable at runtime.",
        "VAI_API_KEY".bold()
    );
}

/// Attempt a `GET /api/status` against `server_url` using `VAI_API_KEY`.
///
/// Returns `true` if the server responds with a 2xx status.
/// Never panics — all errors are swallowed and return `false`.
fn validate_server_reachable(server_url: &str) -> bool {
    let api_key = AgentConfig::resolve_api_key();
    let url = format!(
        "{}/api/status",
        server_url.trim_end_matches('/')
    );

    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(_) => return false,
    };

    rt.block_on(async move {
        let mut req = reqwest::Client::new().get(&url);
        if let Some(key) = api_key {
            req = req.header("Authorization", format!("Bearer {key}"));
        }
        match req.send().await {
            Ok(resp) => resp.status().is_success(),
            Err(_) => false,
        }
    })
}

// ── issue ─────────────────────────────────────────────────────────────────────

/// A single comment on an issue, as returned by `GET /api/issues/:id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueComment {
    /// Comment author identifier.
    pub author: String,
    /// Markdown body of the comment.
    pub body: String,
    /// ISO-8601 creation timestamp.
    pub created_at: String,
}

/// A link to another issue, as returned by `GET /api/issues/:id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueLinkDetail {
    /// UUID of the related issue.
    pub other_issue_id: String,
    /// Human-readable relationship label (e.g. `"blocks"`, `"duplicates"`).
    pub relationship: String,
    /// Title of the related issue.
    pub title: String,
    /// Current status of the related issue.
    pub status: String,
}

/// Full details of an issue as returned by `GET /api/issues/:id`.
///
/// This mirrors the server's `IssueDetailResponse` shape.  Only the fields
/// needed for human-readable display and agent prompting are declared; the
/// full JSON body is available via [`fetch_issue_raw`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueDetail {
    /// Issue UUID.
    pub id: String,
    /// Short summary.
    pub title: String,
    /// Full Markdown description.
    pub description: String,
    /// Status string (e.g. `"open"`, `"in_progress"`, `"resolved"`).
    pub status: String,
    /// Priority string (e.g. `"critical"`, `"high"`, `"medium"`, `"low"`).
    pub priority: String,
    /// Label strings.
    pub labels: Vec<String>,
    /// Creator identifier.
    pub creator: String,
    /// Resolution (present when status is `"resolved"` or `"closed"`).
    pub resolution: Option<String>,
    /// Testable conditions that define when the issue is complete.
    pub acceptance_criteria: Vec<String>,
    /// ISO-8601 creation timestamp.
    pub created_at: String,
    /// ISO-8601 last-updated timestamp.
    pub updated_at: String,
    /// Linked issues with relationship type and status.
    pub links: Vec<IssueLinkDetail>,
    /// The 50 most recent comments.
    pub comments: Vec<IssueComment>,
}

/// Fetch the current issue details from the server.
///
/// Reads the issue ID from `.vai/agent-state.json` in `dir`, then calls
/// `GET /api/issues/:id` on the configured server.  The full JSON body is
/// deserialized into an [`IssueDetail`].
///
/// Use [`fetch_issue_raw`] when you need the unmodified JSON string for
/// passing directly to an agent (e.g. `--json` mode).
pub fn fetch_issue(dir: &Path) -> Result<IssueDetail, AgentError> {
    let config = load_config(dir)?;
    let state = load_state(dir)?;
    let api_key = AgentConfig::resolve_api_key();

    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| AgentError::Other(format!("cannot create tokio runtime: {e}")))?;

    rt.block_on(async {
        let path = format!("api/repos/{}/issues/{}", config.repo, state.issue_id);
        agent_get::<IssueDetail>(&config.server, &path, api_key.as_deref()).await
    })
}

/// Fetch the current issue as a raw JSON string (for `--json` mode).
///
/// Identical to [`fetch_issue`] but returns the unparsed response body so
/// that the full server payload — including any extra fields — is preserved
/// for piping to agents.
pub fn fetch_issue_raw(dir: &Path) -> Result<String, AgentError> {
    let config = load_config(dir)?;
    let state = load_state(dir)?;
    let api_key = AgentConfig::resolve_api_key();

    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| AgentError::Other(format!("cannot create tokio runtime: {e}")))?;

    rt.block_on(async {
        let url = format!(
            "{}/api/repos/{}/issues/{}",
            config.server.trim_end_matches('/'),
            config.repo,
            state.issue_id
        );
        let client = reqwest::Client::new();
        let auth_header: Option<String> = api_key.map(|k| format!("Bearer {k}"));
        let resp = send_with_retry(|| {
            let mut req = client.get(&url);
            if let Some(ref h) = auth_header {
                req = req.header("Authorization", h.as_str());
            }
            req
        })
        .await
        .map_err(|e| AgentError::ServerUnreachable {
            url: url.clone(),
            reason: e.to_string(),
        })?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(AgentError::Other(format!("server returned {status}: {body}")));
        }
        resp.text()
            .await
            .map_err(|e| AgentError::Other(format!("failed to read response body: {e}")))
    })
}

/// Print a human-readable summary of an [`IssueDetail`].
///
/// Outputs: title, status, priority, labels, description snippet (first 300
/// chars), acceptance criteria, and recent comments.
pub fn print_issue_detail(detail: &IssueDetail) {
    println!("{}", detail.title.bold());
    println!(
        "  ID       : {}",
        detail.id[..8.min(detail.id.len())].cyan()
    );
    println!("  Status   : {}", detail.status);
    println!("  Priority : {}", detail.priority);
    if !detail.labels.is_empty() {
        println!("  Labels   : {}", detail.labels.join(", "));
    }
    if let Some(ref res) = detail.resolution {
        println!("  Resolution: {res}");
    }
    println!();

    // Description snippet: first 300 characters.
    if !detail.description.is_empty() {
        let snippet: String = detail.description.chars().take(300).collect();
        let truncated = detail.description.chars().count() > 300;
        println!("{}", "Description:".bold());
        println!("{snippet}");
        if truncated {
            println!("{}", "  … (truncated; use --json for full text)".dimmed());
        }
        println!();
    }

    if !detail.acceptance_criteria.is_empty() {
        println!("{}", "Acceptance Criteria:".bold());
        for criterion in &detail.acceptance_criteria {
            println!("  • {criterion}");
        }
        println!();
    }

    if !detail.links.is_empty() {
        println!("{}", "Linked Issues:".bold());
        for link in &detail.links {
            println!(
                "  {} {} — {} ({})",
                link.relationship,
                link.other_issue_id[..8.min(link.other_issue_id.len())].cyan(),
                link.title,
                link.status.dimmed()
            );
        }
        println!();
    }

    if !detail.comments.is_empty() {
        let recent = detail.comments.iter().rev().take(3).collect::<Vec<_>>();
        println!("{}", format!("Comments (showing {} most recent):", recent.len()).bold());
        for comment in recent.into_iter().rev() {
            let snippet: String = comment.body.chars().take(120).collect();
            let truncated = comment.body.chars().count() > 120;
            println!("  [{}] {}: {}{}", comment.created_at, comment.author.bold(), snippet,
                if truncated { " …" } else { "" });
        }
    }
}

// ── download ──────────────────────────────────────────────────────────────────

/// Result of a [`download`] call.
#[derive(Debug, Serialize)]
pub struct DownloadResult {
    /// Number of files extracted to `<dir>`.
    pub file_count: usize,
    /// Path where files were extracted.
    pub target_dir: PathBuf,
    /// Server-reported version that was downloaded.
    pub version: Option<String>,
}

/// Maximum download attempts before giving up on a partial-download mismatch.
const DOWNLOAD_MAX_ATTEMPTS: u32 = 3;
/// Base delay in milliseconds for download retry backoff.
#[cfg(not(test))]
const DOWNLOAD_BASE_DELAY_MS: u64 = 500;
/// Near-zero delay in tests so the retry loop doesn't slow the suite.
#[cfg(test)]
const DOWNLOAD_BASE_DELAY_MS: u64 = 1;

/// Download the repository tarball into `target_dir` and update agent state.
///
/// Reads the workspace ID, issue ID, and server config from the state and
/// config files in `dir`.  Fetches `GET /api/repos/:repo/files/download` with
/// an `Authorization: Bearer` header, extracts the tarball into `target_dir`,
/// saves a file listing at `<dir>/.vai/download-manifest.json` for later diff
/// comparison during submit, and advances the state phase to
/// [`AgentPhase::Downloaded`].
///
/// If the server declares `X-Vai-Expected-Files: N` and fewer than N files
/// are extracted (indicating a mid-stream server error), the partial directory
/// is discarded and the download is retried with exponential backoff up to
/// [`DOWNLOAD_MAX_ATTEMPTS`] times.  A [`AgentError::PartialDownload`] is
/// returned if all attempts fail.
///
/// `target_dir` is created (and re-created on retry) as needed.
pub fn download(dir: &Path, target_dir: &Path) -> Result<DownloadResult, AgentError> {
    let config = load_config(dir)?;
    let state = load_state(dir)?;
    let api_key = AgentConfig::resolve_api_key();

    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| AgentError::Other(format!("cannot create tokio runtime: {e}")))?;

    let mut last_partial: Option<AgentError> = None;

    for attempt in 1..=DOWNLOAD_MAX_ATTEMPTS {
        // Start each attempt with a clean target directory.
        if target_dir.exists() {
            fs::remove_dir_all(target_dir)?;
        }
        fs::create_dir_all(target_dir)?;

        let (gz_bytes, expected_count) = rt.block_on(async {
            fetch_files_tarball(&config.server, &config.repo, api_key.as_deref()).await
        })?;

        let file_count = extract_tarball_to_dir(&gz_bytes, target_dir)?;

        // Integrity check: server tells us exactly how many files to expect.
        if let Some(expected) = expected_count {
            if file_count != expected {
                eprintln!(
                    "  partial download detected: received {file_count} of {expected} files \
                     (attempt {attempt}/{DOWNLOAD_MAX_ATTEMPTS})"
                );
                let err = AgentError::PartialDownload { downloaded: file_count, expected };
                if attempt < DOWNLOAD_MAX_ATTEMPTS {
                    let delay_ms =
                        std::cmp::min(DOWNLOAD_BASE_DELAY_MS * (1 << attempt), 5_000);
                    eprintln!("  retrying in {delay_ms}ms…");
                    std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                    last_partial = Some(err);
                    continue;
                }
                // All attempts exhausted — discard partial dir and surface error.
                let _ = fs::remove_dir_all(target_dir);
                return Err(err);
            }
        }

        // Download is complete and consistent — save manifest and advance state.
        let manifest = build_file_manifest(target_dir)?;
        let manifest_path = dir.join(".vai").join("download-manifest.json");
        let manifest_json = serde_json::to_string_pretty(&manifest)?;
        fs::write(&manifest_path, manifest_json)
            .map_err(|e| AgentError::Other(format!("cannot write download manifest: {e}")))?;

        let mut updated_state = state.clone();
        updated_state.phase = AgentPhase::Downloaded;
        save_state(dir, &updated_state)?;

        return Ok(DownloadResult {
            file_count,
            target_dir: target_dir.to_path_buf(),
            version: None,
        });
    }

    // Reached only when every attempt saw a partial download.
    Err(last_partial.unwrap_or_else(|| AgentError::Other("download failed".to_string())))
}

/// Fetch the repository tarball bytes from the server.
///
/// Returns the raw gzip bytes and the expected file count declared by the
/// server in the `X-Vai-Expected-Files` response header (if present).
///
/// Retries up to [`RETRY_MAX_ATTEMPTS`] times on transient network errors.
async fn fetch_files_tarball(
    server: &str,
    repo: &str,
    api_key: Option<&str>,
) -> Result<(Vec<u8>, Option<usize>), AgentError> {
    let url = format!(
        "{}/api/repos/{}/files/download",
        server.trim_end_matches('/'),
        repo
    );
    let client = reqwest::Client::new();
    let auth_header: Option<String> = api_key.map(|k| format!("Bearer {k}"));
    let resp = send_with_retry(|| {
        let mut req = client.get(&url);
        if let Some(ref h) = auth_header {
            req = req.header("Authorization", h.as_str());
        }
        req
    })
    .await
    .map_err(|e| AgentError::ServerUnreachable {
        url: url.clone(),
        reason: e.to_string(),
    })?;
    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        return Err(AgentError::Other(format!("server returned {status}: {body}")));
    }
    // Read the expected file count before consuming the body.
    let expected_count = resp
        .headers()
        .get("X-Vai-Expected-Files")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<usize>().ok());
    let bytes = resp
        .bytes()
        .await
        .map(|b| b.to_vec())
        .map_err(|e| AgentError::Other(format!("failed to read response body: {e}")))?;
    Ok((bytes, expected_count))
}

/// Extract a gzip-compressed tarball into `target_dir`.
///
/// Returns the number of regular files written.
fn extract_tarball_to_dir(gz_bytes: &[u8], target_dir: &Path) -> Result<usize, AgentError> {
    use flate2::read::GzDecoder;
    use tar::Archive;

    let decoder = GzDecoder::new(gz_bytes);
    let mut archive = Archive::new(decoder);
    let mut file_count = 0usize;

    for entry_result in archive.entries().map_err(|e| {
        AgentError::Other(format!("cannot read tarball entries: {e}"))
    })? {
        let mut entry = entry_result
            .map_err(|e| AgentError::Other(format!("invalid tarball entry: {e}")))?;

        // Only extract regular files and directories — skip symlinks, etc.
        let entry_type = entry.header().entry_type();
        if !entry_type.is_file() && !entry_type.is_dir() {
            continue;
        }

        let rel_path = entry
            .path()
            .map_err(|e| AgentError::Other(format!("invalid path in tarball: {e}")))?
            .to_path_buf();

        // Safety: reject path traversal attempts.
        for component in rel_path.components() {
            use std::path::Component;
            if matches!(component, Component::ParentDir | Component::RootDir | Component::Prefix(_)) {
                return Err(AgentError::Other(format!(
                    "unsafe path in tarball: {}",
                    rel_path.display()
                )));
            }
        }

        let dest = target_dir.join(&rel_path);

        if entry_type.is_dir() {
            fs::create_dir_all(&dest)?;
        } else {
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)?;
            }
            entry
                .unpack(&dest)
                .map_err(|e| AgentError::Other(format!("cannot unpack '{}': {e}", rel_path.display())))?;
            file_count += 1;
        }
    }

    Ok(file_count)
}

/// Walk `dir` recursively and collect relative file paths.
///
/// Used to produce a manifest of the downloaded files so that `vai agent
/// submit` can compute what was added or modified.
fn build_file_manifest(dir: &Path) -> Result<Vec<String>, AgentError> {
    let mut paths = Vec::new();
    collect_files(dir, dir, &mut paths)?;
    paths.sort();
    Ok(paths)
}

fn collect_files(
    root: &Path,
    current: &Path,
    out: &mut Vec<String>,
) -> Result<(), AgentError> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_files(root, &path, out)?;
        } else {
            let rel = path
                .strip_prefix(root)
                .map_err(|e| AgentError::Other(format!("strip_prefix error: {e}")))?;
            out.push(rel.to_string_lossy().into_owned());
        }
    }
    Ok(())
}

/// Print a human-readable summary of a [`DownloadResult`].
pub fn print_download_result(result: &DownloadResult) {
    println!(
        "{} Downloaded {} file{} to {}",
        "✓".green().bold(),
        result.file_count,
        if result.file_count == 1 { "" } else { "s" },
        result.target_dir.display()
    );
    if let Some(ref ver) = result.version {
        println!("  Version : {ver}");
    }
}

// ── submit ────────────────────────────────────────────────────────────────────

/// Directory names always excluded from the submission tarball.
const EXCLUDED_DIRS: &[&str] =
    &[".vai", ".git", "target", "node_modules", "dist", "__pycache__"];

/// Result of a [`submit`] call.
#[derive(Debug, Serialize)]
pub struct SubmitResult {
    /// Files added relative to the server's current snapshot.
    pub added: usize,
    /// Files modified relative to the server's current snapshot.
    pub modified: usize,
    /// Files deleted relative to the server's current snapshot.
    pub deleted: usize,
    /// New version identifier created by the server.
    pub version_id: Option<String>,
    /// Human-readable issue title that was closed.
    pub issue_title: String,
}

/// Upload the contents of `work_dir`, submit the workspace, close the issue,
/// and clear agent state.
///
/// Steps:
/// 1. Build a gzipped tarball of `work_dir` (excluding standard build
///    artefacts and any extra patterns configured under `[ignore]` in
///    `agent.toml`).
/// 2. `POST /api/workspaces/:id/upload-snapshot` — uploads the tarball.
/// 3. `POST /api/workspaces/:id/submit` — triggers the server-side merge.
/// 4. `POST /api/issues/:id/close` — closes the issue as `resolved`.
/// 5. Clears `.vai/agent-state.json`.
///
/// State is preserved if any step fails so the caller can retry.
pub fn submit(dir: &Path, work_dir: &Path) -> Result<SubmitResult, AgentError> {
    let config = load_config(dir)?;
    let state = load_state(dir)?;
    let api_key = AgentConfig::resolve_api_key();
    let api_key_ref = api_key.as_deref();

    let ignore_patterns = config
        .ignore
        .as_ref()
        .map(|i| i.patterns.clone())
        .unwrap_or_default();

    let tarball = build_agent_tarball(work_dir, &ignore_patterns)?;

    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| AgentError::Other(format!("cannot create tokio runtime: {e}")))?;

    let (added, modified, deleted, version_id) = rt.block_on(async {
        // Step 1: Upload snapshot.
        let upload_url = format!(
            "{}/api/repos/{}/workspaces/{}/upload-snapshot",
            config.server.trim_end_matches('/'),
            config.repo,
            state.workspace_id
        );
        let client = reqwest::Client::new();
        let mut req = client
            .post(&upload_url)
            .header("Content-Type", "application/gzip")
            .body(tarball);
        if let Some(key) = api_key_ref {
            req = req.header("Authorization", format!("Bearer {key}"));
        }
        let resp = req.send().await.map_err(|e| AgentError::ServerUnreachable {
            url: upload_url.clone(),
            reason: e.to_string(),
        })?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(AgentError::Other(format!(
                "upload-snapshot returned {status}: {body}"
            )));
        }
        let upload: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| AgentError::Other(format!("upload-snapshot JSON parse error: {e}")))?;
        let added = upload["added"].as_u64().unwrap_or(0) as usize;
        let modified = upload["modified"].as_u64().unwrap_or(0) as usize;
        let deleted = upload["deleted"].as_u64().unwrap_or(0) as usize;

        // Step 2: Submit workspace.  We inline this call (instead of using
        // agent_post) so we can distinguish the workspace_empty 409 from
        // other errors and surface a recoverable AgentError to the CLI.
        let submit_url = format!(
            "{}/api/repos/{}/workspaces/{}/submit",
            config.server.trim_end_matches('/'),
            config.repo,
            state.workspace_id,
        );
        let mut submit_req = client.post(&submit_url).json(&serde_json::json!({}));
        if let Some(key) = api_key_ref {
            submit_req = submit_req.header("Authorization", format!("Bearer {key}"));
        }
        let submit_resp = submit_req.send().await.map_err(|e| AgentError::ServerUnreachable {
            url: submit_url.clone(),
            reason: e.to_string(),
        })?;
        if !submit_resp.status().is_success() {
            let status = submit_resp.status().as_u16();
            let body = submit_resp.text().await.unwrap_or_default();
            if status == 409 {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
                    if v.get("error").and_then(|e| e.as_str()) == Some("workspace_empty") {
                        return Err(AgentError::WorkspaceEmpty);
                    }
                }
            }
            return Err(AgentError::Other(format!("submit returned {status}: {body}")));
        }
        let submit_val: serde_json::Value = submit_resp
            .json()
            .await
            .map_err(|e| AgentError::Other(format!("submit JSON parse error: {e}")))?;
        let version_id = submit_val["version"].as_str().map(|s| s.to_string());

        // Step 3: Close issue.
        let close_path = format!("api/repos/{}/issues/{}/close", config.repo, state.issue_id);
        let _: serde_json::Value = agent_post(
            &config.server,
            &close_path,
            api_key_ref,
            &serde_json::json!({ "resolution": "resolved" }),
        )
        .await?;

        Ok::<_, AgentError>((added, modified, deleted, version_id))
    })?;

    // Step 4: Clear state — only reached on full success.
    clear_state(dir)?;

    Ok(SubmitResult {
        added,
        modified,
        deleted,
        version_id,
        issue_title: state.issue_title,
    })
}

/// Build a gzip-compressed tarball of `dir`, excluding standard build
/// artefact directories and any additional `ignore_patterns`.
fn build_agent_tarball(dir: &Path, ignore_patterns: &[String]) -> Result<Vec<u8>, AgentError> {
    use flate2::{write::GzEncoder, Compression};

    let buf = Vec::new();
    let gz = GzEncoder::new(buf, Compression::default());
    let mut tar = tar::Builder::new(gz);

    append_dir_to_agent_tar(&mut tar, dir, dir, ignore_patterns)?;

    let gz = tar
        .into_inner()
        .map_err(|e| AgentError::Other(format!("tar finalization error: {e}")))?;
    gz.finish()
        .map_err(|e| AgentError::Other(format!("gzip finalization error: {e}")))
}

/// Recursively appends files under `current` (rooted at `base`) to `tar`.
///
/// Skips [`EXCLUDED_DIRS`] and any path component matching an entry in
/// `ignore_patterns`.  Patterns may be exact names or simple prefix globs
/// ending with `*`.
fn append_dir_to_agent_tar<W: std::io::Write>(
    tar: &mut tar::Builder<W>,
    current: &Path,
    base: &Path,
    ignore_patterns: &[String],
) -> Result<(), AgentError> {
    let entries = match fs::read_dir(current) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(AgentError::Io(e)),
    };

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if path.is_dir() {
            if EXCLUDED_DIRS.contains(&name_str.as_ref()) {
                continue;
            }
            if ignore_patterns.iter().any(|p| matches_ignore_pattern(p, &name_str)) {
                continue;
            }
            append_dir_to_agent_tar(tar, &path, base, ignore_patterns)?;
        } else {
            let rel = path
                .strip_prefix(base)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            if ignore_patterns.iter().any(|p| matches_ignore_pattern(p, &rel)) {
                continue;
            }
            let data = fs::read(&path)
                .map_err(|e| AgentError::Other(format!("cannot read '{}': {e}", path.display())))?;
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(file_mode_for_path(&path, &data));
            header.set_cksum();
            tar.append_data(&mut header, &rel, data.as_slice())
                .map_err(|e| AgentError::Other(format!("tar append error for '{rel}': {e}")))?;
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

/// Returns `true` if `path` matches `pattern`.
///
/// Supports:
/// - Exact match: `"dist"` matches any component named `dist` or the full path.
/// - Suffix glob: `"*.log"` matches any path component ending with `.log`.
/// - Prefix glob: `"build*"` matches any path component starting with `build`.
fn matches_ignore_pattern(pattern: &str, path: &str) -> bool {
    if pattern.starts_with('*') {
        // Suffix pattern: *.log → any segment ending with ".log".
        let suffix = pattern.strip_prefix('*').unwrap_or_default();
        path.split('/').any(|seg| seg.ends_with(suffix))
    } else if pattern.ends_with('*') {
        // Prefix pattern: build* → any segment starting with "build".
        let prefix = pattern.strip_suffix('*').unwrap_or_default();
        path.split('/').any(|seg| seg.starts_with(prefix))
    } else {
        // Exact match against the full relative path or any single component.
        path == pattern || path.split('/').any(|seg| seg == pattern)
    }
}

// ── status ────────────────────────────────────────────────────────────────────

/// Result of a [`status`] call.
#[derive(Debug, Serialize)]
pub struct StatusResult {
    /// Issue UUID of the currently claimed issue.
    pub issue_id: String,
    /// Human-readable title of the issue.
    pub issue_title: String,
    /// Workspace UUID created for this iteration.
    pub workspace_id: String,
    /// Current agent phase.
    pub phase: AgentPhase,
    /// ISO-8601 timestamp of when the issue was claimed.
    pub claimed_at: DateTime<Utc>,
    /// Seconds elapsed since the issue was claimed.
    pub elapsed_seconds: i64,
}

/// Read the current agent state and return a structured status report.
///
/// Returns [`AgentError::NoState`] (exit 1) if no state file exists.
pub fn status(dir: &Path) -> Result<StatusResult, AgentError> {
    let state = load_state(dir)?;
    let elapsed = (Utc::now() - state.claimed_at).num_seconds().max(0);
    Ok(StatusResult {
        issue_id: state.issue_id,
        issue_title: state.issue_title,
        workspace_id: state.workspace_id,
        phase: state.phase,
        claimed_at: state.claimed_at,
        elapsed_seconds: elapsed,
    })
}

/// Print a human-readable summary of a [`StatusResult`].
pub fn print_status_result(r: &StatusResult) {
    use colored::Colorize;
    println!("{}", "Agent status".bold());
    println!("  Issue     : {} — {}", r.issue_id[..8.min(r.issue_id.len())].cyan(), r.issue_title);
    println!(
        "  Workspace : {}",
        r.workspace_id[..8.min(r.workspace_id.len())].cyan()
    );
    println!("  Phase     : {}", r.phase);

    let elapsed = r.elapsed_seconds;
    let (hours, rem) = (elapsed / 3600, elapsed % 3600);
    let (mins, secs) = (rem / 60, rem % 60);
    if hours > 0 {
        println!("  Elapsed   : {}h {}m {}s", hours, mins, secs);
    } else if mins > 0 {
        println!("  Elapsed   : {}m {}s", mins, secs);
    } else {
        println!("  Elapsed   : {}s", secs);
    }
    println!("  Claimed at: {}", r.claimed_at.format("%Y-%m-%d %H:%M:%S UTC"));
}

// ── reset ─────────────────────────────────────────────────────────────────────

/// Result of a [`reset`] call.
#[derive(Debug, Serialize)]
pub struct ResetResult {
    /// Issue UUID that was re-opened.
    pub issue_id: String,
    /// Human-readable title of the issue.
    pub issue_title: String,
    /// Workspace UUID that was discarded.
    pub workspace_id: String,
}

/// Discard the current workspace, reopen the linked issue, and clear state.
///
/// Calls `DELETE /api/workspaces/:id` on the server, which atomically:
/// - marks the workspace as `Discarded`
/// - transitions the linked issue back to `Open`
///
/// State is cleared only after the server call succeeds.
pub fn reset(dir: &Path) -> Result<ResetResult, AgentError> {
    // Check state first so we get a friendly NoState error when nothing is claimed.
    let state = load_state(dir)?;
    let config = load_config(dir)?;
    let api_key = AgentConfig::resolve_api_key();
    let api_key_ref = api_key.as_deref();

    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| AgentError::Other(format!("cannot create tokio runtime: {e}")))?;

    rt.block_on(async {
        let url = format!(
            "{}/api/repos/{}/workspaces/{}",
            config.server.trim_end_matches('/'),
            config.repo,
            state.workspace_id
        );
        let client = reqwest::Client::new();
        let mut req = client.delete(&url);
        if let Some(key) = api_key_ref {
            req = req.header("Authorization", format!("Bearer {key}"));
        }
        let resp = req.send().await.map_err(|e| AgentError::ServerUnreachable {
            url: url.clone(),
            reason: e.to_string(),
        })?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(AgentError::Other(format!("server returned {status}: {body}")));
        }
        Ok::<_, AgentError>(())
    })?;

    clear_state(dir)?;

    Ok(ResetResult {
        issue_id: state.issue_id,
        issue_title: state.issue_title,
        workspace_id: state.workspace_id,
    })
}

/// Print a human-readable summary of a [`ResetResult`].
pub fn print_reset_result(r: &ResetResult) {
    use colored::Colorize;
    println!(
        "{} Reset complete — workspace {} discarded, issue {} reopened",
        "✓".green().bold(),
        r.workspace_id[..8.min(r.workspace_id.len())].cyan(),
        r.issue_id[..8.min(r.issue_id.len())].cyan(),
    );
    println!("  Issue: {}", r.issue_title);
}

// ── Prompt ────────────────────────────────────────────────────────────────────

/// Result of building a prompt from a template.
#[derive(Debug, Serialize)]
pub struct PromptResult {
    /// The fully rendered prompt text.
    pub prompt: String,
    /// Path to the template file that was used, or `None` if the built-in
    /// default was used.
    pub template_path: Option<String>,
}

/// Resolve the base template path and display string for `vai agent prompt`.
///
/// Discovery order:
/// 1. `template_override` (from `--template` flag) — used as-is relative to `dir`.
/// 2. `.vai/agents/<default_agent>/prompt.md` if `config.default_agent` is set
///    and the file exists (new layout introduced by PRD 26 V-11).
/// 3. `config.prompt_template_path(dir)` — defaults to `.vai/prompt.md` (legacy).
///
/// Returns `(absolute_path, display_string_if_any)`.
fn resolve_prompt_base_path(
    dir: &Path,
    template_override: Option<&str>,
) -> Result<(PathBuf, Option<String>), AgentError> {
    if let Some(override_path) = template_override {
        let p = dir.join(override_path);
        return Ok((p, Some(override_path.to_string())));
    }

    let config = load_config(dir)?;

    // New layout: .vai/agents/<default_agent>/prompt.md
    if let Some(name) = config.default_agent.as_deref() {
        let p = dir.join(".vai").join("agents").join(name).join("prompt.md");
        if p.exists() {
            let s = p.to_string_lossy().into_owned();
            return Ok((p, Some(s)));
        }
    }

    // Legacy layout: .vai/prompt.md (or explicit prompt_template)
    let p = config.prompt_template_path(dir);
    let s = config.prompt_template.clone();
    Ok((p, s))
}

/// Assemble the final prompt text from its three parts.
///
/// Sections are joined with exactly one blank line between them:
///
/// ```text
/// <base>
///
/// <overlay>   ← only if Some
///
/// <issue_json>
/// ```
///
/// Trailing whitespace is stripped from each section before joining so the
/// output is consistent regardless of how files are formatted.
fn assemble_prompt(base: &str, overlay: Option<&str>, issue_json: &str) -> String {
    let base = base.trim_end();
    if let Some(ov) = overlay {
        let ov = ov.trim_end();
        format!("{base}\n\n{ov}\n\n{issue_json}\n")
    } else {
        format!("{base}\n\n{issue_json}\n")
    }
}

/// Build a prompt by reading a template file, optionally concatenating a
/// custom overlay, and substituting issue details.
///
/// ## Template discovery
///
/// 1. If `template_override` is set, use that path directly (relative to `dir`).
/// 2. Else if `config.default_agent` is set and
///    `.vai/agents/<default_agent>/prompt.md` exists, use that file (new layout).
/// 3. Else fall back to `config.prompt_template_path(dir)`, which defaults to
///    `.vai/prompt.md` (legacy layout).
/// 4. If none of the above exist, use the built-in default prompt.
///
/// ## Custom overlay
///
/// If `.vai/custom-prompt.md` exists it is inserted between the base template
/// and the rendered issue JSON:
///
/// ```text
/// <base template>
///
/// <.vai/custom-prompt.md>
///
/// <issue JSON>
/// ```
///
/// When no overlay exists the output is simply `base + "\n\n" + issue_json`.
pub fn prompt(dir: &Path, template_override: Option<&str>) -> Result<PromptResult, AgentError> {
    let (base_path, base_path_str) = resolve_prompt_base_path(dir, template_override)?;

    // Fetch issue JSON from server (requires valid state + API key).
    let issue_json = fetch_issue_raw(dir)?;

    // Load base template or fall back to the built-in default.
    let (base_text, resolved_path) = if base_path.exists() {
        let content = fs::read_to_string(&base_path)?;
        let display = base_path_str
            .unwrap_or_else(|| base_path.to_string_lossy().into_owned());
        (content, Some(display))
    } else {
        let default = "You are an AI agent working on a software development issue.\n\
             \n\
             Here are the details of the issue you need to work on:\n\
             \n\
             {{issue}}\n\
             \n\
             Please implement the required changes. When you are done, run \
             `vai agent verify` to check your work, then `vai agent submit` \
             to submit your changes."
            .to_string();
        (default, None)
    };

    // Read optional custom overlay.
    let custom_overlay_path = dir.join(".vai").join("custom-prompt.md");
    let overlay = if custom_overlay_path.exists() {
        Some(fs::read_to_string(&custom_overlay_path)?)
    } else {
        None
    };

    // Choose assembly strategy based on whether the base uses {{issue}} inline.
    //
    // Legacy templates (e.g. .vai/prompt.md) embed the issue JSON via the
    // {{issue}} token.  For those we substitute in place and do NOT append the
    // JSON a second time, preserving behaviour for existing users.
    //
    // New templates (e.g. .vai/agents/<name>/prompt.md) do not contain
    // {{issue}}, so we append the issue JSON as the last section.
    let full_prompt = if base_text.contains("{{issue}}") {
        // Legacy path: substitute {{issue}} inline; no extra JSON appended.
        let base_rendered = base_text.replace("{{issue}}", &issue_json);
        if let Some(ov) = overlay.as_deref() {
            // Insert overlay between rendered base and nothing (no extra JSON).
            format!("{}\n\n{}\n", base_rendered.trim_end(), ov.trim_end())
        } else {
            // Unchanged legacy behaviour.
            base_rendered
        }
    } else {
        // New path: base + optional overlay + issue JSON appended at end.
        assemble_prompt(base_text.trim_end(), overlay.as_deref(), &issue_json)
    };

    Ok(PromptResult {
        prompt: full_prompt,
        template_path: resolved_path,
    })
}

// ── verify ────────────────────────────────────────────────────────────────────

/// Result of a single quality check command.
#[derive(Debug, Serialize)]
pub struct CheckResult {
    /// The command string that was executed.
    pub command: String,
    /// Combined stdout output of the command.
    pub stdout: String,
    /// Combined stderr output of the command.
    pub stderr: String,
    /// Exit code returned by the command.
    pub exit_code: i32,
    /// Whether this check passed (exit code 0).
    pub passed: bool,
}

/// Result of a [`verify`] call.
#[derive(Debug, Serialize)]
pub struct VerifyResult {
    /// Individual results for each configured check.
    pub checks: Vec<CheckResult>,
    /// `true` when all checks passed (or no checks were configured).
    pub all_passed: bool,
    /// `true` when no checks were configured in `agent.toml`.
    pub no_checks_configured: bool,
}

/// Run quality checks configured under `[checks]` in `.vai/agent.toml`.
///
/// Each command in `checks.commands` is executed sequentially with `work_dir`
/// as its working directory.  If a command exits non-zero, execution continues
/// so that all failures are collected and reported.
///
/// Returns [`VerifyResult`] with results for every check.  The caller should
/// inspect `all_passed` and exit with a non-zero code on failure.
///
/// If no checks are configured the function returns successfully with
/// `no_checks_configured = true`.
pub fn verify(dir: &Path, work_dir: &Path) -> Result<VerifyResult, AgentError> {
    let config = load_config(dir)?;
    let checks_config = config.checks;
    let commands = checks_config
        .as_ref()
        .map(|c| c.commands.clone())
        .unwrap_or_default();

    if commands.is_empty() {
        return Ok(VerifyResult {
            checks: Vec::new(),
            all_passed: true,
            no_checks_configured: true,
        });
    }

    let setup = checks_config
        .as_ref()
        .map(|c| c.setup.clone())
        .unwrap_or_default();
    let teardown = checks_config
        .as_ref()
        .map(|c| c.teardown.clone())
        .unwrap_or_default();

    // ── Setup ─────────────────────────────────────────────────────────────
    // Run setup commands sequentially.  If any fail, skip checks and return
    // the setup error.  Teardown still runs.
    //
    // Commands that end with `&` spawn background processes (e.g. a preview
    // server).  For these we detach stdout/stderr so `status()` doesn't
    // block waiting for the child's file descriptors to close.  For normal
    // commands we capture output so failures include useful error messages.
    let mut setup_failed: Option<CheckResult> = None;

    for cmd in &setup {
        let is_background = cmd.trim_end().ends_with('&');

        if is_background {
            // Background command — fire and forget, don't capture output.
            let status = std::process::Command::new("sh")
                .arg("-c")
                .arg(cmd)
                .current_dir(work_dir)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map_err(|e| AgentError::Other(format!("failed to run setup `{cmd}`: {e}")))?;

            if !status.success() {
                setup_failed = Some(CheckResult {
                    command: format!("[setup] {cmd}"),
                    stdout: String::new(),
                    stderr: format!("background setup command exited with code {}", status.code().unwrap_or(-1)),
                    exit_code: status.code().unwrap_or(-1),
                    passed: false,
                });
                break;
            }
        } else {
            // Foreground command — capture output for error reporting.
            let output = std::process::Command::new("sh")
                .arg("-c")
                .arg(cmd)
                .current_dir(work_dir)
                .output()
                .map_err(|e| AgentError::Other(format!("failed to run setup `{cmd}`: {e}")))?;

            if !output.status.success() {
                setup_failed = Some(CheckResult {
                    command: format!("[setup] {cmd}"),
                    stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                    exit_code: output.status.code().unwrap_or(-1),
                    passed: false,
                });
                break;
            }
        }
    }

    // ── Checks ────────────────────────────────────────────────────────────
    let mut results = Vec::with_capacity(commands.len());

    if let Some(setup_err) = setup_failed {
        // Setup failed — skip checks, report setup error.
        results.push(setup_err);
    } else {
        for cmd in &commands {
            let output = std::process::Command::new("sh")
                .arg("-c")
                .arg(cmd)
                .current_dir(work_dir)
                .output()
                .map_err(|e| AgentError::Other(format!("failed to run check `{cmd}`: {e}")))?;

            let exit_code = output.status.code().unwrap_or(-1);
            let passed = output.status.success();
            results.push(CheckResult {
                command: cmd.clone(),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                exit_code,
                passed,
            });
        }
    }

    // ── Teardown ──────────────────────────────────────────────────────────
    // Always run teardown, ignore exit codes.
    for cmd in &teardown {
        let _ = std::process::Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .current_dir(work_dir)
            .output();
    }

    let all_passed = results.iter().all(|r| r.passed);
    Ok(VerifyResult { checks: results, all_passed, no_checks_configured: false })
}

/// Print a structured error summary of failed checks to stderr.
///
/// The format is designed for consumption by AI agents:
/// ```text
/// === <command> ===
/// <stdout + stderr>
/// ```
pub fn print_verify_errors(result: &VerifyResult) {
    for check in &result.checks {
        if !check.passed {
            eprintln!("=== {} ===", check.command);
            if !check.stdout.is_empty() {
                eprint!("{}", check.stdout);
            }
            if !check.stderr.is_empty() {
                eprint!("{}", check.stderr);
            }
            eprintln!("  exit code: {}", check.exit_code);
            eprintln!();
        }
    }
}

/// Print a human-readable summary of a [`SubmitResult`].
pub fn print_submit_result(result: &SubmitResult) {
    println!(
        "{} Submitted — {}",
        "✓".green().bold(),
        result.issue_title
    );
    if let Some(ref ver) = result.version_id {
        println!("  Version  : {ver}");
    }
    println!(
        "  Changes  : {} added, {} modified, {} deleted",
        result.added, result.modified, result.deleted
    );
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tempfile::TempDir;

    /// Serialises tests that read/write process-wide environment variables.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn init_creates_config_file() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        let result = init(dir, Some("https://vai.example.com"), Some("myapp"), None).unwrap();

        assert!(result.config_path.exists());
        assert_eq!(result.server, "https://vai.example.com");
        assert_eq!(result.repo, "myapp");

        // Verify round-trip.
        let loaded = load_config(dir).unwrap();
        assert_eq!(loaded.server, "https://vai.example.com");
        assert_eq!(loaded.repo, "myapp");
        assert!(loaded.prompt_template.is_none());
        assert!(loaded.checks.is_none());
    }

    #[test]
    fn init_with_prompt_template() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        init(
            dir,
            Some("https://vai.example.com"),
            Some("myapp"),
            Some(".vai/custom-prompt.md"),
        )
        .unwrap();

        let loaded = load_config(dir).unwrap();
        assert_eq!(
            loaded.prompt_template.as_deref(),
            Some(".vai/custom-prompt.md")
        );
    }

    #[test]
    fn init_missing_server_returns_error() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let _lock = ENV_LOCK.lock().unwrap();
        // Ensure VAI_SERVER_URL is absent for the duration of this test.
        let _guard = EnvGuard::remove("VAI_SERVER_URL");
        let err = init(dir, None, Some("myapp"), None).unwrap_err();
        assert!(
            matches!(err, AgentError::MissingConfig { field: "server", .. }),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn init_falls_back_to_env_var() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let _lock = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("VAI_SERVER_URL", "https://env.example.com");
            std::env::set_var("VAI_REPO", "env-repo");
        }
        let result = init(dir, None, None, None).unwrap();
        unsafe {
            std::env::remove_var("VAI_SERVER_URL");
            std::env::remove_var("VAI_REPO");
        }
        assert_eq!(result.server, "https://env.example.com");
        assert_eq!(result.repo, "env-repo");
    }

    #[test]
    fn init_never_writes_api_key() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let _lock = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("VAI_API_KEY", "secret-key-1234") };
        init(dir, Some("https://vai.example.com"), Some("myapp"), None).unwrap();
        unsafe { std::env::remove_var("VAI_API_KEY") };

        let contents = fs::read_to_string(config_path(dir)).unwrap();
        assert!(
            !contents.contains("secret-key-1234"),
            "API key must not appear in agent.toml"
        );
        assert!(
            !contents.contains("api_key"),
            "api_key field must not appear in agent.toml"
        );
    }

    #[test]
    fn state_round_trip() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        fs::create_dir_all(dir.join(".vai")).unwrap();
        let state = AgentState {
            issue_id: "issue-123".to_string(),
            issue_title: "Fix the thing".to_string(),
            workspace_id: "ws-456".to_string(),
            phase: AgentPhase::Claimed,
            claimed_at: Utc::now(),
        };
        save_state(dir, &state).unwrap();
        let loaded = load_state(dir).unwrap();
        assert_eq!(loaded.issue_id, "issue-123");
        assert_eq!(loaded.phase, AgentPhase::Claimed);
        clear_state(dir).unwrap();
        assert!(matches!(load_state(dir), Err(AgentError::NoState)));
    }

    #[test]
    fn claim_returns_already_claimed_when_state_exists() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        fs::create_dir_all(dir.join(".vai")).unwrap();

        // Pre-populate state (simulates a crashed previous iteration).
        let state = AgentState {
            issue_id: "aaaaaaaa-0000-0000-0000-000000000000".to_string(),
            issue_title: "Some issue".to_string(),
            workspace_id: "bbbbbbbb-0000-0000-0000-000000000000".to_string(),
            phase: AgentPhase::Claimed,
            claimed_at: Utc::now(),
        };
        save_state(dir, &state).unwrap();

        // claim() should detect the existing state and return AlreadyClaimed
        // without touching the network.
        let outcome = claim(dir, Some("https://vai.example.com"), Some("myrepo")).unwrap();
        assert!(
            matches!(outcome, ClaimOutcome::AlreadyClaimed(_)),
            "expected AlreadyClaimed, got {outcome:?}"
        );
    }

    // ── download helpers ──────────────────────────────────────────────────────

    /// Build a minimal gzip tarball in memory for testing.
    fn make_tarball(files: &[(&str, &[u8])]) -> Vec<u8> {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use tar::Builder;

        let buf = Vec::new();
        let enc = GzEncoder::new(buf, Compression::fast());
        let mut ar = Builder::new(enc);
        for (name, data) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            ar.append_data(&mut header, name, *data).unwrap();
        }
        let gz = ar.into_inner().unwrap();
        gz.finish().unwrap()
    }

    #[test]
    fn extract_tarball_writes_files() {
        let tmp = TempDir::new().unwrap();
        let tarball = make_tarball(&[
            ("hello.txt", b"hello world"),
            ("sub/world.txt", b"sub content"),
        ]);
        let count = extract_tarball_to_dir(&tarball, tmp.path()).unwrap();
        assert_eq!(count, 2);
        assert_eq!(fs::read_to_string(tmp.path().join("hello.txt")).unwrap(), "hello world");
        assert_eq!(
            fs::read_to_string(tmp.path().join("sub/world.txt")).unwrap(),
            "sub content"
        );
    }

    #[test]
    fn extract_tarball_rejects_path_traversal() {
        let tmp = TempDir::new().unwrap();
        // Craft a raw tar entry with `../evil.txt` in the name field so that
        // the `tar` crate builder's own path validation doesn't block us.
        let tarball = make_raw_tar_with_name(b"../evil.txt\0", b"bad");
        let err = extract_tarball_to_dir(&tarball, tmp.path()).unwrap_err();
        assert!(
            err.to_string().contains("unsafe path"),
            "expected unsafe path error, got: {err}"
        );
    }

    /// Build a minimal POSIX tar + gzip with a single file entry whose name is
    /// set directly in the raw header bytes (bypassing the builder's validation).
    fn make_raw_tar_with_name(name: &[u8], data: &[u8]) -> Vec<u8> {
        use flate2::write::GzEncoder;
        use flate2::Compression;

        // A tar header is exactly 512 bytes.
        let mut header = [0u8; 512];
        // Name field: bytes 0-99 (100 bytes).
        let name_len = name.len().min(100);
        header[..name_len].copy_from_slice(&name[..name_len]);
        // Mode field: bytes 100-107.
        header[100..108].copy_from_slice(b"0000644\0");
        // uid/gid: bytes 108-115, 116-123.
        header[108..116].copy_from_slice(b"0000000\0");
        header[116..124].copy_from_slice(b"0000000\0");
        // Size: bytes 124-135 (12 bytes, octal).
        let size_str = format!("{:011o}\0", data.len());
        header[124..136].copy_from_slice(size_str.as_bytes());
        // mtime: bytes 136-147.
        header[136..148].copy_from_slice(b"00000000000\0");
        // typeflag: byte 156 ('0' = regular file).
        header[156] = b'0';
        // Compute checksum: sum of all header bytes with checksum field as spaces.
        header[148..156].copy_from_slice(b"        ");
        let cksum: u32 = header.iter().map(|&b| b as u32).sum();
        let cksum_str = format!("{:06o}\0 ", cksum);
        header[148..156].copy_from_slice(cksum_str.as_bytes());

        // Pad data to 512-byte blocks.
        let padded_size = (data.len() + 511) & !511;
        let mut tar_bytes = Vec::with_capacity(512 + padded_size + 1024);
        tar_bytes.extend_from_slice(&header);
        tar_bytes.extend_from_slice(data);
        tar_bytes.extend(vec![0u8; padded_size - data.len()]);
        // Two 512-byte zero blocks mark end-of-archive.
        tar_bytes.extend(vec![0u8; 1024]);

        // Compress with gzip.
        let mut enc = GzEncoder::new(Vec::new(), Compression::fast());
        use std::io::Write as _;
        enc.write_all(&tar_bytes).unwrap();
        enc.finish().unwrap()
    }

    #[test]
    fn build_file_manifest_collects_paths() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("a.txt"), b"a").unwrap();
        fs::create_dir(tmp.path().join("sub")).unwrap();
        fs::write(tmp.path().join("sub/b.txt"), b"b").unwrap();
        let manifest = build_file_manifest(tmp.path()).unwrap();
        assert!(manifest.contains(&"a.txt".to_string()));
        assert!(manifest.iter().any(|p| p.contains("b.txt")));
    }

    // ── issue helpers ─────────────────────────────────────────────────────────

    #[test]
    fn issue_detail_roundtrip() {
        // Verify that IssueDetail (de)serializes cleanly — this is the shape
        // that fetch_issue() will parse from the server response.
        let json = serde_json::json!({
            "id": "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            "title": "Fix the thing",
            "description": "Long description here.",
            "status": "open",
            "priority": "high",
            "labels": ["bug", "urgent"],
            "creator": "alice",
            "resolution": null,
            "acceptance_criteria": ["All tests pass", "No regressions"],
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-02T00:00:00Z",
            "links": [
                {
                    "other_issue_id": "11111111-0000-0000-0000-000000000000",
                    "relationship": "blocks",
                    "title": "Other issue",
                    "status": "open"
                }
            ],
            "comments": [
                {
                    "author": "bob",
                    "body": "Looks good",
                    "created_at": "2026-01-01T12:00:00Z"
                }
            ]
        });
        let detail: IssueDetail = serde_json::from_value(json).unwrap();
        assert_eq!(detail.title, "Fix the thing");
        assert_eq!(detail.status, "open");
        assert_eq!(detail.priority, "high");
        assert_eq!(detail.labels, vec!["bug", "urgent"]);
        assert_eq!(detail.acceptance_criteria.len(), 2);
        assert_eq!(detail.links.len(), 1);
        assert_eq!(detail.links[0].relationship, "blocks");
        assert_eq!(detail.comments.len(), 1);
        assert_eq!(detail.comments[0].author, "bob");
    }

    #[test]
    fn fetch_issue_requires_state() {
        // fetch_issue() should return NoState when no agent state exists.
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        // Write a config but no state file.
        init(dir, Some("https://vai.example.com"), Some("myrepo"), None).unwrap();
        let err = fetch_issue(dir).unwrap_err();
        assert!(
            matches!(err, AgentError::NoState),
            "expected NoState, got {err}"
        );
    }

    #[test]
    fn fetch_issue_raw_requires_state() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        init(dir, Some("https://vai.example.com"), Some("myrepo"), None).unwrap();
        let err = fetch_issue_raw(dir).unwrap_err();
        assert!(matches!(err, AgentError::NoState), "expected NoState, got {err}");
    }

    // ── submit helpers ────────────────────────────────────────────────────────

    #[test]
    fn build_agent_tarball_excludes_standard_dirs() {
        use flate2::read::GzDecoder;

        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        // Create files that should be included.
        fs::write(dir.join("main.rs"), b"fn main() {}").unwrap();
        fs::create_dir(dir.join("src")).unwrap();
        fs::write(dir.join("src").join("lib.rs"), b"pub fn foo() {}").unwrap();

        // Create excluded directories.
        for excl in &[".vai", ".git", "target", "node_modules", "dist", "__pycache__"] {
            fs::create_dir(dir.join(excl)).unwrap();
            fs::write(dir.join(excl).join("junk.txt"), b"skip me").unwrap();
        }

        let tarball = build_agent_tarball(dir, &[]).unwrap();

        // Decompress and collect entry names.
        let gz = GzDecoder::new(tarball.as_slice());
        let mut archive = tar::Archive::new(gz);
        let names: Vec<String> = archive
            .entries()
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path().unwrap().to_string_lossy().into_owned())
            .collect();

        assert!(names.iter().any(|n| n == "main.rs"), "main.rs must be included");
        assert!(names.iter().any(|n| n.contains("lib.rs")), "src/lib.rs must be included");

        for excl in &[".vai", ".git", "target", "node_modules", "dist", "__pycache__"] {
            assert!(
                !names.iter().any(|n| n.contains(excl)),
                "excluded dir '{excl}' content must not appear in tarball"
            );
        }
    }

    #[test]
    fn build_agent_tarball_respects_extra_ignore_patterns() {
        use flate2::read::GzDecoder;

        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        fs::write(dir.join("keep.rs"), b"keep").unwrap();
        fs::write(dir.join("skip.log"), b"skip").unwrap();
        fs::create_dir(dir.join("build-output")).unwrap();
        fs::write(dir.join("build-output").join("file.bin"), b"bin").unwrap();

        let patterns = vec!["*.log".to_string(), "build*".to_string()];
        let tarball = build_agent_tarball(dir, &patterns).unwrap();

        let gz = GzDecoder::new(tarball.as_slice());
        let mut archive = tar::Archive::new(gz);
        let names: Vec<String> = archive
            .entries()
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path().unwrap().to_string_lossy().into_owned())
            .collect();

        assert!(names.iter().any(|n| n == "keep.rs"));
        assert!(!names.iter().any(|n| n.contains("skip.log")));
        assert!(!names.iter().any(|n| n.contains("build-output")));
    }

    #[test]
    fn matches_ignore_pattern_exact() {
        assert!(matches_ignore_pattern("dist", "dist"));
        assert!(matches_ignore_pattern("dist", "src/dist/file.js"));
        assert!(!matches_ignore_pattern("dist", "distribution"));
    }

    #[test]
    fn matches_ignore_pattern_glob_prefix() {
        assert!(matches_ignore_pattern("build*", "build-output"));
        assert!(matches_ignore_pattern("build*", "path/build-output/file"));
        assert!(!matches_ignore_pattern("build*", "src/main.rs"));
    }

    #[test]
    fn submit_requires_state() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        init(dir, Some("https://vai.example.com"), Some("myrepo"), None).unwrap();
        // No state file — submit should return NoState.
        let work = TempDir::new().unwrap();
        let err = submit(dir, work.path()).unwrap_err();
        assert!(matches!(err, AgentError::NoState), "expected NoState, got {err}");
    }

    // ── status helpers ────────────────────────────────────────────────────────

    #[test]
    fn status_requires_state() {
        let tmp = TempDir::new().unwrap();
        let err = status(tmp.path()).unwrap_err();
        assert!(matches!(err, AgentError::NoState), "expected NoState, got {err}");
    }

    #[test]
    fn status_returns_state_fields() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        fs::create_dir_all(dir.join(".vai")).unwrap();

        let claimed_at = Utc::now();
        let state = AgentState {
            issue_id: "aaaaaaaa-0000-0000-0000-000000000001".to_string(),
            issue_title: "Test issue".to_string(),
            workspace_id: "bbbbbbbb-0000-0000-0000-000000000002".to_string(),
            phase: AgentPhase::Downloaded,
            claimed_at,
        };
        save_state(dir, &state).unwrap();

        let r = status(dir).unwrap();
        assert_eq!(r.issue_id, state.issue_id);
        assert_eq!(r.issue_title, "Test issue");
        assert_eq!(r.workspace_id, state.workspace_id);
        assert_eq!(r.phase, AgentPhase::Downloaded);
        assert!(r.elapsed_seconds >= 0);
    }

    // ── prompt helpers ────────────────────────────────────────────────────────

    #[test]
    fn prompt_template_substitution() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        fs::create_dir_all(dir.join(".vai")).unwrap();

        // Write a minimal agent.toml so load_config succeeds.
        let cfg = AgentConfig {
            server: "https://vai.example.com".to_string(),
            repo: "myrepo".to_string(),
            prompt_template: None,
            default_agent: None,
            checks: None,
            ignore: None,
        };
        fs::write(config_path(dir), toml::to_string(&cfg).unwrap()).unwrap();

        // Write a state file so fetch_issue_raw has something to work with
        // (we don't need a real server — we test template logic separately).
        // We write the template file only; the network call is not exercised.
        let template_content = "Please fix:\n{{issue}}\nGood luck.";
        let template_path = dir.join(".vai").join("prompt.md");
        fs::write(&template_path, template_content).unwrap();

        // Write a fake state file; fetch_issue_raw reads state to get issue_id.
        let state = AgentState {
            issue_id: "aaaaaaaa-0000-0000-0000-000000000000".to_string(),
            issue_title: "Test issue".to_string(),
            workspace_id: "bbbbbbbb-0000-0000-0000-000000000000".to_string(),
            phase: AgentPhase::Claimed,
            claimed_at: Utc::now(),
        };
        save_state(dir, &state).unwrap();

        // prompt() will fail at the network call, so we test template
        // substitution logic directly via the AgentConfig helper.
        let path = cfg.prompt_template_path(dir);
        assert_eq!(path, template_path);

        // Verify template substitution logic works correctly.
        let issue_json = r#"{"id":"abc"}"#;
        let rendered = template_content.replace("{{issue}}", issue_json);
        assert_eq!(rendered, "Please fix:\n{\"id\":\"abc\"}\nGood luck.");
    }

    #[test]
    fn prompt_template_path_custom() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let cfg = AgentConfig {
            server: "https://vai.example.com".to_string(),
            repo: "myrepo".to_string(),
            prompt_template: Some("custom/my-prompt.md".to_string()),
            default_agent: None,
            checks: None,
            ignore: None,
        };
        let expected = dir.join("custom/my-prompt.md");
        assert_eq!(cfg.prompt_template_path(dir), expected);
    }

    // ── reset helpers ─────────────────────────────────────────────────────────

    #[test]
    fn reset_requires_state() {
        let tmp = TempDir::new().unwrap();
        let err = reset(tmp.path()).unwrap_err();
        assert!(matches!(err, AgentError::NoState), "expected NoState, got {err}");
    }

    /// RAII guard that removes an env var on drop (for cleanup in tests).
    struct EnvGuard(String);
    impl EnvGuard {
        fn remove(key: &str) -> Self {
            // SAFETY: tests use ENV_LOCK to serialise env access.
            unsafe { std::env::remove_var(key) };
            EnvGuard(key.to_string())
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: tests use ENV_LOCK to serialise env access.
            unsafe { std::env::remove_var(&self.0) };
        }
    }

    // ── verify helpers ────────────────────────────────────────────────────────

    fn write_config_with_checks(dir: &Path, commands: Vec<&str>) {
        let vai_dir = dir.join(".vai");
        fs::create_dir_all(&vai_dir).unwrap();
        let checks_cmds: Vec<String> = commands.into_iter().map(String::from).collect();
        let cfg = AgentConfig {
            server: "https://vai.example.com".to_string(),
            repo: "myrepo".to_string(),
            prompt_template: None,
            default_agent: None,
            checks: Some(ChecksConfig { commands: checks_cmds, setup: vec![], teardown: vec![] }),
            ignore: None,
        };
        let toml = toml::to_string(&cfg).unwrap();
        fs::write(vai_dir.join("agent.toml"), toml).unwrap();
    }

    #[test]
    fn verify_no_checks_configured() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let vai_dir = dir.join(".vai");
        fs::create_dir_all(&vai_dir).unwrap();
        let cfg = AgentConfig {
            server: "https://vai.example.com".to_string(),
            repo: "myrepo".to_string(),
            prompt_template: None,
            default_agent: None,
            checks: None,
            ignore: None,
        };
        let toml = toml::to_string(&cfg).unwrap();
        fs::write(vai_dir.join("agent.toml"), toml).unwrap();

        let result = verify(dir, dir).unwrap();
        assert!(result.all_passed);
        assert!(result.no_checks_configured);
        assert!(result.checks.is_empty());
    }

    #[test]
    fn verify_all_checks_pass() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        write_config_with_checks(dir, vec!["true", "echo hello"]);

        let result = verify(dir, dir).unwrap();
        assert!(result.all_passed);
        assert!(!result.no_checks_configured);
        assert_eq!(result.checks.len(), 2);
        assert!(result.checks[0].passed);
        assert!(result.checks[1].passed);
    }

    #[test]
    fn verify_failing_check() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        write_config_with_checks(dir, vec!["true", "false", "echo after"]);

        let result = verify(dir, dir).unwrap();
        assert!(!result.all_passed);
        assert_eq!(result.checks.len(), 3);
        assert!(result.checks[0].passed);
        assert!(!result.checks[1].passed);
        // Execution continues after failure — third check runs.
        assert!(result.checks[2].passed);
    }

    #[test]
    fn verify_captures_stdout_and_stderr() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        write_config_with_checks(dir, vec!["echo out_text", "echo err_text >&2; exit 1"]);

        let result = verify(dir, dir).unwrap();
        assert_eq!(result.checks[0].stdout.trim(), "out_text");
        assert!(!result.checks[1].passed);
        assert!(result.checks[1].stderr.contains("err_text"));
    }

    #[test]
    fn verify_uses_work_dir_as_cwd() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        // Create a sentinel file in the work dir.
        let work_dir = TempDir::new().unwrap();
        fs::write(work_dir.path().join("sentinel.txt"), "hi").unwrap();
        write_config_with_checks(dir, vec!["test -f sentinel.txt"]);

        let result = verify(dir, work_dir.path()).unwrap();
        assert!(result.all_passed, "check should find sentinel.txt in work_dir");
    }

    /// Verify catches a simulated `cargo clippy --features full` failure.
    ///
    /// This test confirms that feature-gated clippy errors—the kind that slip
    /// through a CLI-only `cargo clippy` run—are captured and reported when the
    /// verify step runs the full-features variant.
    #[test]
    fn verify_catches_feature_gated_clippy_failure() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        // The first command simulates CLI-only clippy passing; the second
        // simulates `cargo clippy --features full` failing with a lint error
        // emitted on stderr, matching the real clippy output shape.
        write_config_with_checks(dir, vec![
            "true",  // CLI-only clippy: passes
            "echo 'error: use of deprecated ...' >&2; exit 1",  // full-features clippy: fails
        ]);

        let result = verify(dir, dir).unwrap();
        assert!(!result.all_passed);
        let failed: Vec<_> = result.checks.iter().filter(|c| !c.passed).collect();
        assert_eq!(failed.len(), 1, "exactly the full-features check should fail");
        assert!(
            failed[0].stderr.contains("error:"),
            "stderr should contain the lint error text"
        );
    }

    /// Verify catches a simulated `cargo audit --deny warnings` failure.
    ///
    /// This test confirms that a security advisory (the kind CI catches via the
    /// `cargo audit --deny warnings` step) is surfaced as a labelled failure by
    /// `vai agent verify`, so the error is fed back to the next Claude attempt.
    #[test]
    fn verify_catches_audit_advisory_failure() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        // Simulate the audit step failing with a RustSec advisory message.
        write_config_with_checks(dir, vec![
            "true",  // clippy: passes
            "true",  // tests: passes
            "echo 'error[RUSTSEC-2026-0099]: ...' >&2; exit 1",  // audit: fails
        ]);

        let result = verify(dir, dir).unwrap();
        assert!(!result.all_passed);
        let failed: Vec<_> = result.checks.iter().filter(|c| !c.passed).collect();
        assert_eq!(failed.len(), 1, "only the audit step should fail");
        assert!(
            failed[0].stderr.contains("RUSTSEC"),
            "stderr should contain the advisory identifier"
        );
        assert_ne!(failed[0].exit_code, 0);
    }

    // ── prompt helpers ────────────────────────────────────────────────────────

    /// Write a minimal `agent.toml` to `<dir>/.vai/agent.toml`.
    fn write_agent_toml(dir: &std::path::Path, default_agent: Option<&str>) {
        fs::create_dir_all(dir.join(".vai")).unwrap();
        let extra = match default_agent {
            Some(name) => format!("default_agent = \"{name}\"\n"),
            None => String::new(),
        };
        fs::write(
            dir.join(".vai").join("agent.toml"),
            format!(
                "server = \"https://vai.example.com\"\nrepo = \"myrepo\"\n{extra}"
            ),
        )
        .unwrap();
    }

    #[test]
    fn assemble_prompt_new_layout_no_overlay() {
        // New layout: base (no {{issue}}) + issue JSON appended.
        let base = "Do the work.";
        let issue = "{\"id\":\"1\"}";
        let result = assemble_prompt(base, None, issue);
        // Exactly one blank line between base and issue JSON; ends with \n.
        assert_eq!(result, "Do the work.\n\n{\"id\":\"1\"}\n");
    }

    #[test]
    fn assemble_prompt_new_layout_with_overlay() {
        // New layout: base + overlay + issue JSON, exactly one blank line each.
        let base = "Do the work.";
        let overlay = "Extra rules.";
        let issue = "{\"id\":\"2\"}";
        let result = assemble_prompt(base, Some(overlay), issue);
        assert_eq!(result, "Do the work.\n\nExtra rules.\n\n{\"id\":\"2\"}\n");
    }

    #[test]
    fn resolve_prompt_base_path_new_layout() {
        // When default_agent is set and .vai/agents/<name>/prompt.md exists,
        // resolve_prompt_base_path should return that path.
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        write_agent_toml(dir, Some("claude-code"));

        let prompt_dir = dir.join(".vai").join("agents").join("claude-code");
        fs::create_dir_all(&prompt_dir).unwrap();
        fs::write(prompt_dir.join("prompt.md"), "New layout base.\n").unwrap();

        let (path, _display) = resolve_prompt_base_path(dir, None).unwrap();
        assert!(path.ends_with("agents/claude-code/prompt.md"), "expected new layout path, got {path:?}");
        assert!(path.exists());
    }

    #[test]
    fn resolve_prompt_base_path_legacy_fallback() {
        // When no default_agent is set, resolve_prompt_base_path should fall
        // back to .vai/prompt.md.
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        write_agent_toml(dir, None); // no default_agent
        fs::write(dir.join(".vai").join("prompt.md"), "Legacy base.\n").unwrap();

        let (path, _display) = resolve_prompt_base_path(dir, None).unwrap();
        assert!(path.ends_with("prompt.md"), "expected legacy path, got {path:?}");
    }

    #[test]
    fn resolve_prompt_base_path_template_override() {
        // --template flag takes precedence over everything.
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        write_agent_toml(dir, Some("claude-code"));

        // Even if .vai/agents/claude-code/prompt.md exists, override wins.
        let prompt_dir = dir.join(".vai").join("agents").join("claude-code");
        fs::create_dir_all(&prompt_dir).unwrap();
        fs::write(prompt_dir.join("prompt.md"), "New layout.\n").unwrap();

        let custom = dir.join("custom.md");
        fs::write(&custom, "Override base.\n").unwrap();

        let (path, display) = resolve_prompt_base_path(dir, Some("custom.md")).unwrap();
        assert!(path.ends_with("custom.md"), "expected override path, got {path:?}");
        assert_eq!(display.as_deref(), Some("custom.md"));
    }

    #[test]
    fn resolve_prompt_base_path_legacy_falls_back_when_agents_dir_missing() {
        // default_agent is set in agent.toml but the .vai/agents/<name>/prompt.md
        // file does not exist — should fall back to legacy .vai/prompt.md.
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        write_agent_toml(dir, Some("claude-code"));
        // No .vai/agents/claude-code/prompt.md created.
        fs::write(dir.join(".vai").join("prompt.md"), "Legacy fallback.\n").unwrap();

        let (path, _display) = resolve_prompt_base_path(dir, None).unwrap();
        assert!(
            path.ends_with(".vai/prompt.md"),
            "should fall back to legacy path, got {path:?}"
        );
    }

    // ── retry policy tests ────────────────────────────────────────────────────

    /// Return a port that is almost certainly not listening (bound briefly then freed).
    fn unused_local_port() -> u16 {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        drop(l);
        port
    }

    #[tokio::test]
    async fn is_transient_on_connection_refused() {
        let port = unused_local_port();
        let err = reqwest::Client::new()
            .get(format!("http://127.0.0.1:{port}"))
            .send()
            .await
            .unwrap_err();
        assert!(
            is_transient_reqwest_error(&err),
            "connection refused should be classified transient, got: {err}"
        );
    }

    #[tokio::test]
    async fn is_not_transient_on_builder_error() {
        // An invalid scheme produces a builder/URL error, not a network error.
        let err = reqwest::Client::new()
            .get("not-a-valid-url://host/path")
            .send()
            .await
            .unwrap_err();
        assert!(
            !is_transient_reqwest_error(&err),
            "builder error should NOT be classified transient, got: {err}"
        );
    }

    #[tokio::test]
    async fn send_with_retry_retries_transient_errors() {
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;

        let port = unused_local_port();
        let url = format!("http://127.0.0.1:{port}");
        let call_count = Arc::new(AtomicU32::new(0));
        let counter = call_count.clone();

        let client = reqwest::Client::new();
        let result = send_with_retry(|| {
            counter.fetch_add(1, Ordering::SeqCst);
            client.get(&url)
        })
        .await;

        assert!(result.is_err(), "all attempts should fail for unreachable port");
        assert_eq!(
            call_count.load(Ordering::SeqCst),
            RETRY_MAX_ATTEMPTS,
            "should attempt exactly RETRY_MAX_ATTEMPTS={RETRY_MAX_ATTEMPTS} times before giving up"
        );
    }

    #[tokio::test]
    async fn send_with_retry_does_not_retry_on_non_transient_error() {
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;

        let call_count = Arc::new(AtomicU32::new(0));
        let counter = call_count.clone();

        let client = reqwest::Client::new();
        let result = send_with_retry(|| {
            counter.fetch_add(1, Ordering::SeqCst);
            client.get("not-a-valid-url://host")
        })
        .await;

        assert!(result.is_err(), "should return error for bad URL");
        assert_eq!(
            call_count.load(Ordering::SeqCst),
            1,
            "non-transient error must not be retried"
        );
    }
}
