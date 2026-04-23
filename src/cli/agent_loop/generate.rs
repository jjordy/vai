//! Artefact generation for `vai agent loop init`.
//!
//! This module selects the correct loop-script and Dockerfile templates,
//! substitutes `{{REPO_NAME}}`, `{{SERVER_URL}}`, and `{{AGENT_NAME}}`
//! tokens, then writes the output files to `.vai/agents/<agent>/`.
//!
//! ## Output layout
//!
//! ```text
//! .vai/agents/<agent>/
//!   loop.sh        — executable loop script (mode 0o755 on Unix)
//!   Dockerfile     — container definition (docker mode only)
//!   prompt.md      — prompt template (written by callers, not this module)
//! ```

use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use super::detection::ProjectType;
use super::env_writer::AgentKind;
use super::templates::{template, TemplateKind};

// ── Errors ────────────────────────────────────────────────────────────────────

/// Errors that can occur during artefact generation.
#[derive(Debug, thiserror::Error)]
pub enum GenerateError {
    #[error("I/O error writing {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("target directory already exists: {0}. Use --overwrite to replace it.")]
    AlreadyExists(PathBuf),

    #[error("backup failed for {path}: {source}")]
    BackupFailed {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

// ── Configuration ─────────────────────────────────────────────────────────────

/// Whether to run the loop in a Docker container or directly on the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    /// Run the agent in an isolated Docker container.
    Docker,
    /// Run the agent on the host shell (bare mode).
    Bare,
}

/// All parameters needed to generate the agent loop artefacts.
#[derive(Debug, Clone)]
pub struct GenerateConfig {
    /// The agent to use (determines script and Dockerfile templates).
    pub agent: AgentKind,
    /// Detected or user-selected project type.
    pub project_type: ProjectType,
    /// Whether to generate Docker or bare-shell artefacts.
    pub run_mode: RunMode,
    /// Repository name (substituted for `{{REPO_NAME}}`).
    pub repo_name: String,
    /// Server URL (substituted for `{{SERVER_URL}}`).
    pub server_url: String,
    /// Overwrite existing artefacts if present.
    pub overwrite: bool,
}

/// Files written by [`generate`].
#[derive(Debug, Default)]
pub struct GenerateOutput {
    /// Path to the generated `loop.sh`.
    pub loop_sh: PathBuf,
    /// Path to the generated `Dockerfile` (only present in Docker mode).
    pub dockerfile: Option<PathBuf>,
    /// Path to the generated `prompt.md`.
    pub prompt_md: PathBuf,
    /// Path to the `agent.toml` partial that was merged/written.
    pub agent_toml: PathBuf,
    /// Whether a backup of `prompt.md` was created (–overwrite path).
    pub prompt_md_backup: Option<PathBuf>,
}

// ── Token substitution ────────────────────────────────────────────────────────

/// Substitute `{{REPO_NAME}}`, `{{SERVER_URL}}`, and `{{AGENT_NAME}}` tokens.
fn substitute(template: &str, repo_name: &str, server_url: &str, agent_name: &str) -> String {
    template
        .replace("{{REPO_NAME}}", repo_name)
        .replace("{{SERVER_URL}}", server_url)
        .replace("{{AGENT_NAME}}", agent_name)
}

// ── Template selection ────────────────────────────────────────────────────────

/// Return the embedded loop-script template string for the given configuration.
fn loop_script_template(agent: AgentKind, mode: RunMode) -> &'static str {
    match (agent, mode) {
        (AgentKind::ClaudeCode, RunMode::Bare) => {
            include_str!("templates/loop-claude-code.bare.sh")
        }
        (AgentKind::ClaudeCode, RunMode::Docker) => {
            include_str!("templates/loop-claude-code.docker.sh")
        }
        (AgentKind::Codex, RunMode::Bare) => {
            include_str!("templates/loop-codex.bare.sh")
        }
        (AgentKind::Codex, RunMode::Docker) => {
            include_str!("templates/loop-codex.docker.sh")
        }
        (AgentKind::Custom, _) => {
            include_str!("templates/loop-custom.sh")
        }
    }
}

/// Return the embedded Dockerfile template for the given agent kind.
///
/// Returns `None` for `AgentKind::Custom` — custom agents don't get a
/// generated Dockerfile.
fn dockerfile_template(agent: AgentKind) -> Option<&'static str> {
    match agent {
        AgentKind::ClaudeCode => Some(include_str!("templates/Dockerfile.claude-code")),
        AgentKind::Codex => Some(include_str!("templates/Dockerfile.codex")),
        AgentKind::Custom => None,
    }
}

/// Return the canonical agent name string used in directory names and tokens.
pub fn agent_name(agent: AgentKind) -> &'static str {
    match agent {
        AgentKind::ClaudeCode => "claude-code",
        AgentKind::Codex => "codex",
        AgentKind::Custom => "custom",
    }
}

// ── File writing helpers ───────────────────────────────────────────────────────

/// Write `content` to `path`, creating parent directories as needed.
fn write_file(path: &Path, content: &str) -> Result<(), GenerateError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| GenerateError::Io {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }
    std::fs::write(path, content).map_err(|e| GenerateError::Io {
        path: path.to_path_buf(),
        source: e,
    })
}

/// Mark a file executable on Unix (no-op on other platforms).
#[allow(unused_variables)]
fn mark_executable(path: &Path) -> Result<(), GenerateError> {
    #[cfg(unix)]
    {
        let mut perms = std::fs::metadata(path)
            .map_err(|e| GenerateError::Io {
                path: path.to_path_buf(),
                source: e,
            })?
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).map_err(|e| GenerateError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
    }
    Ok(())
}

/// Back up `path` to `path.bak.YYYYMMDD-HHMMSS` before overwriting.
fn backup_file(path: &Path) -> Result<PathBuf, GenerateError> {
    let timestamp = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let backup = path.with_extension(format!(
        "{}.bak.{timestamp}",
        path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("md")
    ));
    std::fs::copy(path, &backup).map_err(|e| GenerateError::BackupFailed {
        path: path.to_path_buf(),
        source: e,
    })?;
    Ok(backup)
}

// ── agent.toml helpers ────────────────────────────────────────────────────────

/// Merge the `[checks]` partial into an existing `agent.toml` at `path`.
///
/// - If `agent.toml` does not exist, write a minimal file containing just the
///   partial (with header).
/// - If it exists and already has a `[checks]` section, leave it unchanged.
/// - If it exists but has no `[checks]` section, append the partial.
fn merge_agent_toml(
    path: &Path,
    partial: &str,
    server_url: &str,
    repo_name: &str,
    agent_name_str: &str,
) -> Result<(), GenerateError> {
    if !path.exists() {
        let content = format!(
            "# vai agent configuration\n# Generated by `vai agent loop init`\n\nserver = \"{server_url}\"\nrepo = \"{repo_name}\"\ndefault_agent = \"{agent_name_str}\"\n\n{partial}"
        );
        return write_file(path, &content);
    }

    let existing = std::fs::read_to_string(path).map_err(|e| GenerateError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;

    // If [checks] already present, preserve it.
    if existing.contains("[checks]") {
        return Ok(());
    }

    // Append the partial.
    let new_content = format!("{existing}\n{partial}");
    write_file(path, &new_content)
}

// ── Main entry point ──────────────────────────────────────────────────────────

/// Generate all artefacts into `.vai/agents/<agent>/`.
///
/// `vai_dir` is the `.vai/` directory of the repository (typically
/// `<repo_root>/.vai`).
pub fn generate(config: &GenerateConfig, vai_dir: &Path) -> Result<GenerateOutput, GenerateError> {
    let name = agent_name(config.agent);
    let agent_dir = vai_dir.join("agents").join(name);

    // Overwrite / existence check.
    if agent_dir.exists() && !config.overwrite {
        return Err(GenerateError::AlreadyExists(agent_dir));
    }

    let mut output = GenerateOutput::default();

    // ── loop.sh ──────────────────────────────────────────────────────────────

    let loop_tmpl = loop_script_template(config.agent, config.run_mode);
    let loop_content = substitute(loop_tmpl, &config.repo_name, &config.server_url, name);
    let loop_path = agent_dir.join("loop.sh");
    write_file(&loop_path, &loop_content)?;
    mark_executable(&loop_path)?;
    output.loop_sh = loop_path;

    // ── Dockerfile (docker mode only) ─────────────────────────────────────────

    if config.run_mode == RunMode::Docker {
        if let Some(tmpl) = dockerfile_template(config.agent) {
            let df_content = substitute(tmpl, &config.repo_name, &config.server_url, name);
            let df_path = agent_dir.join("Dockerfile");
            write_file(&df_path, &df_content)?;
            output.dockerfile = Some(df_path);
        }
    }

    // ── prompt.md ─────────────────────────────────────────────────────────────

    let prompt_path = agent_dir.join("prompt.md");
    let mut backup: Option<PathBuf> = None;

    if prompt_path.exists() && config.overwrite {
        backup = Some(backup_file(&prompt_path)?);
    }

    if !prompt_path.exists() || config.overwrite {
        let prompt_tmpl = template(config.project_type, TemplateKind::Prompt);
        let prompt_content = substitute(prompt_tmpl, &config.repo_name, &config.server_url, name);
        write_file(&prompt_path, &prompt_content)?;
    }
    output.prompt_md = prompt_path;
    output.prompt_md_backup = backup;

    // ── agent.toml ─────────────────────────────────────────────────────────────

    let agent_toml_path = vai_dir.join("agent.toml");
    let partial = template(config.project_type, TemplateKind::AgentTomlPartial);
    merge_agent_toml(&agent_toml_path, partial, &config.server_url, &config.repo_name, name)?;
    output.agent_toml = agent_toml_path;

    Ok(output)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn tmp() -> TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    fn basic_config(agent: AgentKind, mode: RunMode) -> GenerateConfig {
        GenerateConfig {
            agent,
            project_type: ProjectType::BackendRust,
            run_mode: mode,
            repo_name: "myrepo".to_string(),
            server_url: "https://vai.example.com".to_string(),
            overwrite: false,
        }
    }

    #[test]
    fn generates_loop_sh_bare_claude() {
        let dir = tmp();
        let vai = dir.path().join(".vai");
        let cfg = basic_config(AgentKind::ClaudeCode, RunMode::Bare);
        let out = generate(&cfg, &vai).unwrap();
        assert!(out.loop_sh.exists());
        let content = std::fs::read_to_string(&out.loop_sh).unwrap();
        assert!(content.contains("claude -p"), "bare script must invoke claude");
        assert!(content.contains("myrepo"), "repo name token must be substituted");
        assert!(!content.contains("{{REPO_NAME}}"), "token must not remain");
    }

    #[test]
    fn generates_loop_sh_docker_claude() {
        let dir = tmp();
        let vai = dir.path().join(".vai");
        let cfg = basic_config(AgentKind::ClaudeCode, RunMode::Docker);
        let out = generate(&cfg, &vai).unwrap();
        assert!(out.loop_sh.exists());
        let content = std::fs::read_to_string(&out.loop_sh).unwrap();
        assert!(content.contains("docker run"), "docker script must use docker run");
        assert!(out.dockerfile.is_some(), "docker mode must produce a Dockerfile");
    }

    #[test]
    fn no_dockerfile_in_bare_mode() {
        let dir = tmp();
        let vai = dir.path().join(".vai");
        let cfg = basic_config(AgentKind::ClaudeCode, RunMode::Bare);
        let out = generate(&cfg, &vai).unwrap();
        assert!(out.dockerfile.is_none());
    }

    #[test]
    fn generates_prompt_md() {
        let dir = tmp();
        let vai = dir.path().join(".vai");
        let cfg = basic_config(AgentKind::ClaudeCode, RunMode::Bare);
        let out = generate(&cfg, &vai).unwrap();
        assert!(out.prompt_md.exists());
    }

    #[test]
    fn error_when_exists_no_overwrite() {
        let dir = tmp();
        let vai = dir.path().join(".vai");
        let cfg = basic_config(AgentKind::ClaudeCode, RunMode::Bare);
        generate(&cfg, &vai).unwrap();
        let err = generate(&cfg, &vai).unwrap_err();
        assert!(
            matches!(err, GenerateError::AlreadyExists(_)),
            "expected AlreadyExists, got: {err}"
        );
    }

    #[test]
    fn overwrite_backs_up_prompt_md() {
        let dir = tmp();
        let vai = dir.path().join(".vai");
        let cfg = basic_config(AgentKind::ClaudeCode, RunMode::Bare);
        generate(&cfg, &vai).unwrap();

        let mut cfg2 = cfg.clone();
        cfg2.overwrite = true;
        let out = generate(&cfg2, &vai).unwrap();
        assert!(out.prompt_md_backup.is_some(), "should have a backup");
        assert!(out.prompt_md_backup.unwrap().exists());
    }

    #[test]
    fn merge_agent_toml_creates_new_file() {
        let dir = tmp();
        let vai = dir.path().join(".vai");
        std::fs::create_dir_all(&vai).unwrap();
        let cfg = basic_config(AgentKind::ClaudeCode, RunMode::Bare);
        generate(&cfg, &vai).unwrap();
        let toml_content = std::fs::read_to_string(vai.join("agent.toml")).unwrap();
        assert!(toml_content.contains("[checks]"));
        assert!(toml_content.contains("cargo check"));
    }

    #[test]
    fn merge_agent_toml_preserves_existing_checks() {
        let dir = tmp();
        let vai = dir.path().join(".vai");
        std::fs::create_dir_all(&vai).unwrap();
        let existing = "[checks]\ncommands = [\"make test\"]\n";
        std::fs::write(vai.join("agent.toml"), existing).unwrap();
        let cfg = basic_config(AgentKind::ClaudeCode, RunMode::Bare);
        generate(&cfg, &vai).unwrap();
        let content = std::fs::read_to_string(vai.join("agent.toml")).unwrap();
        assert!(content.contains("make test"), "existing checks must be preserved");
        // Should appear only once.
        assert_eq!(content.matches("[checks]").count(), 1);
    }

    #[test]
    fn codex_bare_requires_openai_key() {
        let dir = tmp();
        let vai = dir.path().join(".vai");
        let cfg = basic_config(AgentKind::Codex, RunMode::Bare);
        let out = generate(&cfg, &vai).unwrap();
        let content = std::fs::read_to_string(&out.loop_sh).unwrap();
        assert!(content.contains("OPENAI_API_KEY"), "codex script must require OPENAI_API_KEY");
    }

    #[test]
    fn custom_agent_has_no_dockerfile() {
        let dir = tmp();
        let vai = dir.path().join(".vai");
        let cfg = basic_config(AgentKind::Custom, RunMode::Docker);
        let out = generate(&cfg, &vai).unwrap();
        assert!(out.dockerfile.is_none(), "custom agent has no Dockerfile template");
    }

    #[test]
    fn loop_sh_is_executable() {
        let dir = tmp();
        let vai = dir.path().join(".vai");
        let cfg = basic_config(AgentKind::ClaudeCode, RunMode::Bare);
        let out = generate(&cfg, &vai).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&out.loop_sh).unwrap().permissions().mode();
            assert!(mode & 0o111 != 0, "loop.sh must be executable");
        }
    }

    #[test]
    fn substitute_replaces_all_tokens() {
        let tmpl = "repo={{REPO_NAME}} url={{SERVER_URL}} agent={{AGENT_NAME}}";
        let result = substitute(tmpl, "myrepo", "https://example.com", "claude-code");
        assert_eq!(result, "repo=myrepo url=https://example.com agent=claude-code");
    }

    #[test]
    fn bare_claude_guards_download() {
        let dir = tmp();
        let vai = dir.path().join(".vai");
        let cfg = basic_config(AgentKind::ClaudeCode, RunMode::Bare);
        let out = generate(&cfg, &vai).unwrap();
        let content = std::fs::read_to_string(&out.loop_sh).unwrap();
        assert!(
            content.contains("if ! vai agent download"),
            "bare claude script must guard vai agent download"
        );
        assert!(
            content.contains("vai agent reset"),
            "bare claude script must reset on download failure"
        );
    }

    #[test]
    fn bare_codex_guards_download() {
        let dir = tmp();
        let vai = dir.path().join(".vai");
        let cfg = basic_config(AgentKind::Codex, RunMode::Bare);
        let out = generate(&cfg, &vai).unwrap();
        let content = std::fs::read_to_string(&out.loop_sh).unwrap();
        assert!(
            content.contains("if ! vai agent download"),
            "bare codex script must guard vai agent download"
        );
        assert!(
            content.contains("vai agent reset"),
            "bare codex script must reset on download failure"
        );
    }

    #[test]
    fn custom_agent_guards_download() {
        let dir = tmp();
        let vai = dir.path().join(".vai");
        let cfg = basic_config(AgentKind::Custom, RunMode::Bare);
        let out = generate(&cfg, &vai).unwrap();
        let content = std::fs::read_to_string(&out.loop_sh).unwrap();
        assert!(
            content.contains("if ! vai agent download"),
            "custom loop script must guard vai agent download"
        );
        assert!(
            content.contains("vai agent reset"),
            "custom loop script must reset on download failure"
        );
    }
}
