//! `vai agent loop run` — pre-flight validation then exec the loop script.
//!
//! ## Steps
//!
//! 1. Resolve which agent configuration to use (from `--name`, single-dir
//!    auto-detection, `agent.toml` default, or an explicit error).
//! 2. Run pre-flight checks against the `.env` file.
//! 3. Write `.vai/agents/<name>/.last-run` with the current ISO-8601 timestamp.
//! 4. `exec` `.vai/agents/<name>/loop.sh`, replacing this process.

use std::path::{Path, PathBuf};

use crate::cli::CliError;

use super::env::{parse as parse_env, ParsedEnv};
use super::env_writer::AgentKind;

// ── Error ─────────────────────────────────────────────────────────────────────

/// Errors specific to `vai agent loop run`.
#[derive(Debug, thiserror::Error)]
enum RunError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    PreFlight(String),

    #[error("{0}")]
    Config(String),
}

// ── Agent resolution ──────────────────────────────────────────────────────────

/// List the names of all configured agent loops.
///
/// Returns the names of every subdirectory under `.vai/agents/` that contains
/// a `loop.sh` file.
fn list_agents(vai_dir: &Path) -> Vec<String> {
    let agents_dir = vai_dir.join("agents");
    let Ok(entries) = std::fs::read_dir(&agents_dir) else {
        return Vec::new();
    };

    let mut names: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            let loop_sh = entry.path().join("loop.sh");
            if loop_sh.exists() {
                if let Some(name) = entry.file_name().to_str() {
                    names.push(name.to_string());
                }
            }
        }
    }
    names.sort();
    names
}

/// Resolve the agent name to use for this run.
///
/// Resolution order:
/// 1. `--name <name>` flag.
/// 2. Exactly one configured agent → use it automatically.
/// 3. `agent.toml` `default_agent` field.
/// 4. Error: multiple configs, pass `--name`.
fn resolve_agent_name(
    name_flag: Option<&str>,
    vai_dir: &Path,
) -> Result<String, RunError> {
    // 1. Explicit flag wins.
    if let Some(name) = name_flag {
        return Ok(name.to_string());
    }

    let available = list_agents(vai_dir);

    // 2. Exactly one configured agent.
    if available.len() == 1 {
        return Ok(available.into_iter().next().unwrap());
    }

    // 3. Check agent.toml for default_agent.
    let cwd = std::env::current_dir()
        .map_err(|e| RunError::Config(format!("cannot determine current directory: {e}")))?;
    if let Ok(config) = crate::agent::load_config(&cwd) {
        if let Some(default) = config.default_agent {
            // Verify the default exists.
            let agent_dir = vai_dir.join("agents").join(&default);
            if agent_dir.join("loop.sh").exists() {
                return Ok(default);
            }
        }
    }

    // 4. Multiple (or zero) configs, no default.
    if available.is_empty() {
        return Err(RunError::Config(
            "No agent loops configured. Run 'vai agent loop init' first.".to_string(),
        ));
    }

    let list = available.join(", ");
    Err(RunError::Config(format!(
        "Multiple loops configured. Pass --name <agent> to select one.\n\nAvailable: {list}"
    )))
}

// ── .env helpers ──────────────────────────────────────────────────────────────

/// Find the 1-based line number of a key in a parsed `.env` file.
///
/// Returns `None` if the key is not present.
fn find_key_line(parsed: &ParsedEnv, key: &str) -> Option<usize> {
    use super::env::EnvLine;
    parsed
        .lines
        .iter()
        .enumerate()
        .find_map(|(i, line)| {
            if let EnvLine::KeyValue { key: k, .. } = line {
                if k == key {
                    return Some(i + 1); // 1-based
                }
            }
            None
        })
}

/// Build a user-friendly pre-flight error message for a missing / empty token.
fn preflight_error(
    key: &str,
    env_path: &Path,
    line_hint: Option<usize>,
    fix_hint: &str,
) -> String {
    let mut msg = format!("Error: {key} is empty.\n");
    if let Some(line) = line_hint {
        msg.push_str(&format!(
            "\nEdit line {line} of {} and {}",
            env_path.display(),
            fix_hint,
        ));
    } else {
        msg.push_str(&format!(
            "\nAdd {key} to {} and {}",
            env_path.display(),
            fix_hint,
        ));
    }
    msg.push_str("\n\nThen rerun: vai agent loop run");
    msg
}

// ── Docker check ──────────────────────────────────────────────────────────────

/// Returns `true` if the Docker daemon is running.
fn docker_running() -> bool {
    std::process::Command::new("docker")
        .arg("info")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ── Pre-flight checks ─────────────────────────────────────────────────────────

/// Run all pre-flight checks.  Returns `Err` on the first failure.
fn preflight(
    agent_name: &str,
    agent_dir: &Path,
    cwd: &Path,
) -> Result<(), RunError> {
    // 1. .env must exist.
    let env_path = cwd.join(".env");
    if !env_path.exists() {
        return Err(RunError::PreFlight(format!(
            ".env file not found at {}. Run 'vai agent loop init' first.",
            env_path.display()
        )));
    }

    // 2. Parse .env.
    let env_content = std::fs::read_to_string(&env_path)
        .map_err(|e| RunError::Io(e))?;
    let parsed = parse_env(&env_content);

    // 3. VAI_API_KEY must be present and non-empty.
    if !parsed.has_key_with_value("VAI_API_KEY") {
        let line = find_key_line(&parsed, "VAI_API_KEY");
        let msg = preflight_error(
            "VAI_API_KEY",
            &env_path,
            line,
            "paste your vai API key.",
        );
        return Err(RunError::PreFlight(msg));
    }

    // 4. Agent-specific token.
    let agent_kind: AgentKind = agent_name.parse().unwrap_or(AgentKind::Custom);
    match agent_kind {
        AgentKind::ClaudeCode => {
            if !parsed.has_key_with_value("CLAUDE_CODE_OAUTH_TOKEN") {
                let line = find_key_line(&parsed, "CLAUDE_CODE_OAUTH_TOKEN");
                let msg = preflight_error(
                    "CLAUDE_CODE_OAUTH_TOKEN",
                    &env_path,
                    line,
                    "paste the token from:\n  $ claude setup-token",
                );
                return Err(RunError::PreFlight(msg));
            }
        }
        AgentKind::Codex => {
            if !parsed.has_key_with_value("OPENAI_API_KEY") {
                let line = find_key_line(&parsed, "OPENAI_API_KEY");
                let msg = preflight_error(
                    "OPENAI_API_KEY",
                    &env_path,
                    line,
                    "get one at https://platform.openai.com/api-keys",
                );
                return Err(RunError::PreFlight(msg));
            }
        }
        AgentKind::Custom => {
            // No provider-specific token required.
        }
    }

    // 5. Docker mode check: if a Dockerfile is present, Docker must be running.
    let dockerfile = agent_dir.join("Dockerfile");
    if dockerfile.exists() && !docker_running() {
        return Err(RunError::PreFlight(
            "Docker is required for this loop but not running.".to_string(),
        ));
    }

    Ok(())
}

// ── Handler ────────────────────────────────────────────────────────────────────

/// Handle `vai agent loop run`.
///
/// # Parameters
///
/// - `name` — agent name from `--name`; `None` triggers auto-resolution.
/// - `_json` — reserved (output is minimal for this command).
pub(super) fn handle(name: Option<&str>, _json: bool) -> Result<(), CliError> {
    let cwd = std::env::current_dir()
        .map_err(|e| CliError::Other(format!("cannot determine current directory: {e}")))?;
    let vai_dir = cwd.join(".vai");

    // ── Resolve agent ──────────────────────────────────────────────────────────

    let agent_name = resolve_agent_name(name, &vai_dir)
        .map_err(|e| CliError::Other(e.to_string()))?;

    let agent_dir = vai_dir.join("agents").join(&agent_name);
    let loop_sh = agent_dir.join("loop.sh");

    if !loop_sh.exists() {
        return Err(CliError::Other(format!(
            "loop.sh not found at {}. Run 'vai agent loop init --name {agent_name}' first.",
            loop_sh.display()
        )));
    }

    // ── Pre-flight ─────────────────────────────────────────────────────────────

    preflight(&agent_name, &agent_dir, &cwd)
        .map_err(|e| CliError::Other(e.to_string()))?;

    // ── Write .last-run ────────────────────────────────────────────────────────

    let timestamp = chrono::Utc::now().to_rfc3339();
    let last_run_path = agent_dir.join(".last-run");
    std::fs::write(&last_run_path, format!("{timestamp}\n"))
        .map_err(|e| CliError::Other(format!("failed to write .last-run: {e}")))?;

    // ── Exec loop.sh ───────────────────────────────────────────────────────────

    exec_loop(&loop_sh)
}

/// Exec `loop.sh`, replacing this process.
///
/// On Unix, `exec` replaces the current process image so Ctrl-C propagates
/// naturally to the loop script.
///
/// Windows is not supported (consistent with the rest of the agent-loop CLI).
#[cfg(unix)]
fn exec_loop(loop_sh: &PathBuf) -> Result<(), CliError> {
    use std::os::unix::process::CommandExt;

    let err = std::process::Command::new(loop_sh)
        .exec(); // returns only on error
    Err(CliError::Other(format!(
        "failed to exec {}: {err}",
        loop_sh.display()
    )))
}

#[cfg(not(unix))]
fn exec_loop(_loop_sh: &PathBuf) -> Result<(), CliError> {
    Err(CliError::Other(
        "vai agent loop run is not supported on non-Unix platforms.".to_string(),
    ))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_tmp() -> TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    /// Create `.vai/agents/<name>/loop.sh` and return the agent dir path.
    fn scaffold_agent(vai_dir: &Path, name: &str) -> PathBuf {
        let agent_dir = vai_dir.join("agents").join(name);
        std::fs::create_dir_all(&agent_dir).unwrap();
        let loop_sh = agent_dir.join("loop.sh");
        std::fs::write(&loop_sh, "#!/bin/sh\necho hello\n").unwrap();
        agent_dir
    }

    // ── list_agents ───────────────────────────────────────────────────────────

    #[test]
    fn list_agents_empty_when_no_dir() {
        let dir = make_tmp();
        let agents = list_agents(dir.path());
        assert!(agents.is_empty());
    }

    #[test]
    fn list_agents_finds_loop_sh_dirs() {
        let dir = make_tmp();
        let vai = dir.path().join(".vai");
        scaffold_agent(&vai, "claude-code");
        scaffold_agent(&vai, "codex");
        let agents = list_agents(&vai);
        assert_eq!(agents, vec!["claude-code", "codex"]);
    }

    #[test]
    fn list_agents_ignores_dirs_without_loop_sh() {
        let dir = make_tmp();
        let vai = dir.path().join(".vai");
        // Create a dir without loop.sh.
        std::fs::create_dir_all(vai.join("agents").join("empty")).unwrap();
        scaffold_agent(&vai, "claude-code");
        let agents = list_agents(&vai);
        assert_eq!(agents, vec!["claude-code"]);
    }

    // ── resolve_agent_name ────────────────────────────────────────────────────

    #[test]
    fn resolve_explicit_name() {
        let dir = make_tmp();
        let vai = dir.path().join(".vai");
        scaffold_agent(&vai, "claude-code");
        // --name wins even when there's exactly one.
        let name = resolve_agent_name(Some("claude-code"), &vai).unwrap();
        assert_eq!(name, "claude-code");
    }

    #[test]
    fn resolve_single_agent_auto() {
        let dir = make_tmp();
        let vai = dir.path().join(".vai");
        scaffold_agent(&vai, "codex");
        let name = resolve_agent_name(None, &vai).unwrap();
        assert_eq!(name, "codex");
    }

    #[test]
    fn resolve_zero_agents_errors() {
        let dir = make_tmp();
        let vai = dir.path().join(".vai");
        std::fs::create_dir_all(vai.join("agents")).unwrap();
        let err = resolve_agent_name(None, &vai).unwrap_err();
        assert!(err.to_string().contains("vai agent loop init"));
    }

    #[test]
    fn resolve_multiple_agents_errors_without_name() {
        let dir = make_tmp();
        let vai = dir.path().join(".vai");
        scaffold_agent(&vai, "claude-code");
        scaffold_agent(&vai, "codex");
        let err = resolve_agent_name(None, &vai).unwrap_err();
        assert!(err.to_string().contains("--name"));
    }

    // ── find_key_line ─────────────────────────────────────────────────────────

    #[test]
    fn find_key_line_present() {
        let parsed = parse_env("# comment\nFOO=bar\nBAZ=qux\n");
        assert_eq!(find_key_line(&parsed, "FOO"), Some(2));
        assert_eq!(find_key_line(&parsed, "BAZ"), Some(3));
    }

    #[test]
    fn find_key_line_absent() {
        let parsed = parse_env("FOO=bar\n");
        assert_eq!(find_key_line(&parsed, "MISSING"), None);
    }

    // ── preflight ─────────────────────────────────────────────────────────────

    #[test]
    fn preflight_fails_when_no_env_file() {
        let dir = make_tmp();
        let vai = dir.path().join(".vai");
        let agent_dir = scaffold_agent(&vai, "claude-code");
        let err = preflight("claude-code", &agent_dir, dir.path()).unwrap_err();
        assert!(err.to_string().contains(".env file not found"));
    }

    #[test]
    fn preflight_fails_when_vai_api_key_empty() {
        let dir = make_tmp();
        let vai = dir.path().join(".vai");
        let agent_dir = scaffold_agent(&vai, "claude-code");
        // Write .env without VAI_API_KEY.
        std::fs::write(dir.path().join(".env"), "OTHER=val\n").unwrap();
        let err = preflight("claude-code", &agent_dir, dir.path()).unwrap_err();
        assert!(err.to_string().contains("VAI_API_KEY"));
    }

    #[test]
    fn preflight_fails_when_claude_token_empty() {
        let dir = make_tmp();
        let vai = dir.path().join(".vai");
        let agent_dir = scaffold_agent(&vai, "claude-code");
        std::fs::write(
            dir.path().join(".env"),
            "VAI_API_KEY=vk_live_abc\nCLAUDE_CODE_OAUTH_TOKEN=\n",
        )
        .unwrap();
        let err = preflight("claude-code", &agent_dir, dir.path()).unwrap_err();
        assert!(err.to_string().contains("CLAUDE_CODE_OAUTH_TOKEN"));
        assert!(err.to_string().contains("claude setup-token"));
    }

    #[test]
    fn preflight_fails_when_openai_key_missing() {
        let dir = make_tmp();
        let vai = dir.path().join(".vai");
        let agent_dir = scaffold_agent(&vai, "codex");
        std::fs::write(dir.path().join(".env"), "VAI_API_KEY=vk_live_abc\n").unwrap();
        let err = preflight("codex", &agent_dir, dir.path()).unwrap_err();
        assert!(err.to_string().contains("OPENAI_API_KEY"));
        assert!(err.to_string().contains("platform.openai.com"));
    }

    #[test]
    fn preflight_passes_custom_agent_with_only_vai_key() {
        let dir = make_tmp();
        let vai = dir.path().join(".vai");
        let agent_dir = scaffold_agent(&vai, "custom");
        std::fs::write(dir.path().join(".env"), "VAI_API_KEY=vk_live_abc\n").unwrap();
        // Custom agent has no provider-specific token check.
        assert!(preflight("custom", &agent_dir, dir.path()).is_ok());
    }

    #[test]
    fn preflight_includes_line_number_for_empty_key() {
        let dir = make_tmp();
        let vai = dir.path().join(".vai");
        let agent_dir = scaffold_agent(&vai, "claude-code");
        std::fs::write(
            dir.path().join(".env"),
            "# comment\nVAI_API_KEY=\n",
        )
        .unwrap();
        let err = preflight("claude-code", &agent_dir, dir.path()).unwrap_err();
        // VAI_API_KEY is on line 2.
        assert!(err.to_string().contains("line 2"), "expected line number in: {err}");
    }

    #[test]
    fn preflight_passes_with_all_credentials() {
        let dir = make_tmp();
        let vai = dir.path().join(".vai");
        let agent_dir = scaffold_agent(&vai, "claude-code");
        std::fs::write(
            dir.path().join(".env"),
            "VAI_API_KEY=vk_live_abc\nCLAUDE_CODE_OAUTH_TOKEN=tok_xyz\n",
        )
        .unwrap();
        assert!(preflight("claude-code", &agent_dir, dir.path()).is_ok());
    }
}
