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

    #[error("{0}")]
    Other(String),
}

// ── Configuration types ───────────────────────────────────────────────────────

/// Quality check configuration stored under `[checks]` in `agent.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChecksConfig {
    /// Shell commands to run sequentially to verify agent output.
    ///
    /// Each command is run with the working directory set to the target
    /// directory passed to `vai agent verify <dir>`.
    pub commands: Vec<String>,
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

/// Build an authenticated `reqwest::Client` request with `Authorization: Bearer <key>`.
fn authed_client(api_key: Option<&str>) -> reqwest::Client {
    let _ = api_key; // used at call site via header injection
    reqwest::Client::new()
}

/// Make an authenticated GET request and parse the JSON response.
async fn agent_get<T: serde::de::DeserializeOwned>(
    server: &str,
    path: &str,
    api_key: Option<&str>,
) -> Result<T, AgentError> {
    let url = format!("{}/{}", server.trim_end_matches('/'), path.trim_start_matches('/'));
    let client = authed_client(api_key);
    let mut req = client.get(&url);
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

/// Make an authenticated POST request with a JSON body and parse the JSON response.
async fn agent_post<B: serde::Serialize, T: serde::de::DeserializeOwned>(
    server: &str,
    path: &str,
    api_key: Option<&str>,
    body: &B,
) -> Result<T, AgentError> {
    let url = format!("{}/{}", server.trim_end_matches('/'), path.trim_start_matches('/'));
    let client = authed_client(api_key);
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
}
