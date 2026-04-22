//! `vai agent loop init` — scaffold loop configuration and update `.env`.
//!
//! ## Steps performed
//!
//! 1. Locate the current `.vai/config.toml` to read `repo_id`, `name`, and
//!    the remote server URL.
//! 2. Detect the project type (Rust / React / TypeScript / generic) and
//!    optionally prompt the user to confirm or change it.
//! 3. Prompt for agent choice (`claude-code`, `codex`, `custom`) unless
//!    `--agent` was given.
//! 4. Probe for Docker and prompt for run mode unless `--docker`/`--no-docker`
//!    was given.
//! 5. Handle overwrite: back up `prompt.md` when `--overwrite` is set.
//! 6. Call [`generate::generate`] to write `loop.sh`, `Dockerfile`, and
//!    `prompt.md` into `.vai/agents/<agent>/`.
//! 7. Merge `[checks]` into `.vai/agent.toml`.
//! 8. Append the vai-loop block to `.env` (idempotent).
//! 9. Print a next-steps summary.

use std::io::{self, IsTerminal, Write as _};

use colored::Colorize;
use serde::{Deserialize, Serialize};

use crate::cli::CliError;

use super::detection::{detect_project_type, ProjectType};
use super::env::{parse, ParsedEnv};
use super::env_writer::{AgentKind, write_env};
use super::generate::{self, GenerateConfig, RunMode};

// ── Error ──────────────────────────────────────────────────────────────────────

/// Errors specific to `vai agent loop init`.
#[derive(Debug, thiserror::Error)]
enum InitError {
    #[error("credentials error: {0}")]
    Credentials(#[from] crate::credentials::CredentialsError),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("repo config error: {0}")]
    Repo(#[from] crate::repo::RepoError),

    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),

    #[error("generation error: {0}")]
    Generate(#[from] generate::GenerateError),

    #[error("{0}")]
    Other(String),
}

// ── API key creation ──────────────────────────────────────────────────────────

/// Request body for `POST /api/keys`.
#[derive(Debug, Serialize)]
struct CreateKeyRequest {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    repo_id: Option<uuid::Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    role_override: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent_type: Option<String>,
}

/// Relevant part of the `POST /api/keys` response.
#[derive(Debug, Deserialize)]
struct CreateKeyResponse {
    token: String,
}

/// Calls `POST /api/keys` to create a repo-scoped write key.
///
/// Returns the plaintext token on success.
async fn create_api_key(
    server_url: &str,
    bearer_key: &str,
    repo_id: uuid::Uuid,
    repo_name: &str,
) -> Result<String, InitError> {
    let client = reqwest::Client::new();
    let url = format!("{}/api/keys", server_url.trim_end_matches('/'));
    let body = CreateKeyRequest {
        name: format!("loop-{repo_name}"),
        repo_id: Some(repo_id),
        role_override: Some("write".to_string()),
        agent_type: Some("agent".to_string()),
    };

    let resp = client
        .post(&url)
        .bearer_auth(bearer_key)
        .json(&body)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        return Err(InitError::Other(format!(
            "POST /api/keys returned HTTP {status}: {text}"
        )));
    }

    let parsed: CreateKeyResponse = resp.json().await?;
    Ok(parsed.token)
}

// ── Interactive prompt helpers ─────────────────────────────────────────────────

/// Prompt the user with `question` and return their trimmed input.
///
/// Returns `None` if stdin is not a terminal or if EOF is reached.
fn prompt(question: &str) -> Option<String> {
    if !io::stderr().is_terminal() {
        return None;
    }
    eprint!("{}", question);
    io::stderr().flush().ok();
    let mut buf = String::new();
    io::stdin().read_line(&mut buf).ok()?;
    let trimmed = buf.trim().to_string();
    Some(trimmed)
}

/// Prompt for a yes/no/pick response and return the trimmed answer.
///
/// Returns the `default_answer` string if the user presses Enter or if
/// stdin is not a terminal.
fn prompt_yn(question: &str, default_answer: &str) -> String {
    match prompt(question) {
        Some(s) if s.is_empty() => default_answer.to_string(),
        Some(s) => s.to_lowercase(),
        None => default_answer.to_string(),
    }
}

/// Detect if Docker is available by running `docker info`.
fn docker_available() -> bool {
    std::process::Command::new("docker")
        .arg("info")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ── Project-type selection ────────────────────────────────────────────────────

/// Parse a project-type string (from `--project-type` flag or user input)
/// into a [`ProjectType`].
fn parse_project_type(s: &str) -> Option<ProjectType> {
    match s.to_ascii_lowercase().trim() {
        "frontend-react" | "frontend" | "react" | "1" => Some(ProjectType::FrontendReact),
        "backend-rust" | "rust" | "2" => Some(ProjectType::BackendRust),
        "backend-typescript" | "typescript" | "ts" | "node" | "3" => {
            Some(ProjectType::BackendTypescript)
        }
        "generic" | "4" => Some(ProjectType::Generic),
        _ => None,
    }
}

/// Return the display label for a project type.
fn project_type_label(pt: ProjectType) -> &'static str {
    match pt {
        ProjectType::FrontendReact => "frontend-react",
        ProjectType::BackendRust => "backend-rust",
        ProjectType::BackendTypescript => "backend-typescript",
        ProjectType::Generic => "generic",
    }
}

/// Interactively confirm / override the detected project type.
///
/// Returns the final project type.
fn interactive_project_type(detected: ProjectType) -> ProjectType {
    let label = project_type_label(detected);
    eprintln!("Detected project type: {}", label.cyan().bold());

    let answer = prompt_yn(
        "Use this template? [Y/n/pick]: ",
        "y",
    );

    match answer.as_str() {
        "y" | "yes" => detected,
        "n" | "no" | "pick" | "p" => {
            eprintln!("Project types:");
            eprintln!("  1) frontend-react");
            eprintln!("  2) backend-rust");
            eprintln!("  3) backend-typescript");
            eprintln!("  4) generic");
            loop {
                let choice = prompt_yn("Choice [4]: ", "4");
                if let Some(pt) = parse_project_type(&choice) {
                    return pt;
                }
                eprintln!("  Invalid choice '{}', try again.", choice);
            }
        }
        _ => {
            // Try to parse as a project type directly.
            parse_project_type(&answer).unwrap_or(detected)
        }
    }
}

// ── Agent selection ───────────────────────────────────────────────────────────

/// Interactively select the agent kind.
fn interactive_agent() -> AgentKind {
    eprintln!("Which agent?");
    eprintln!("  1) claude-code  (recommended)");
    eprintln!("  2) codex");
    eprintln!("  3) custom  (empty template for other agents)");
    loop {
        let choice = prompt_yn("Choice [1]: ", "1");
        match choice.as_str() {
            "1" | "claude-code" | "claude" => return AgentKind::ClaudeCode,
            "2" | "codex" => return AgentKind::Codex,
            "3" | "custom" => return AgentKind::Custom,
            _ => eprintln!("  Invalid choice '{}', try again.", choice),
        }
    }
}

// ── Run-mode selection ────────────────────────────────────────────────────────

/// Interactively select the run mode (Docker vs bare shell).
fn interactive_run_mode() -> RunMode {
    let docker_ok = docker_available();
    let detected_note = if docker_ok {
        "detected ✓".green().to_string()
    } else {
        "(Docker Desktop not detected)".yellow().to_string()
    };

    eprintln!("How should the loop run?");
    if docker_ok {
        eprintln!(
            "  › Docker  (recommended)  — isolated container {}",
            detected_note
        );
        eprintln!("    Bare shell             — runs on your host");
    } else {
        eprintln!("  › Bare shell  — runs on your host");
        eprintln!("    Docker      — isolated container {}", detected_note);
    }

    let default_choice = if docker_ok { "Docker" } else { "Bare" };
    let prompt_str = format!("Choice [{}]: ", default_choice);
    let choice = prompt_yn(&prompt_str, default_choice);

    match choice.to_ascii_lowercase().as_str() {
        "docker" | "d" | "1" => RunMode::Docker,
        "bare" | "b" | "2" => RunMode::Bare,
        _ => {
            // Use default.
            if docker_ok {
                RunMode::Docker
            } else {
                RunMode::Bare
            }
        }
    }
}

// ── Handler ────────────────────────────────────────────────────────────────────

/// Handle `vai agent loop init`.
///
/// # Parameters
///
/// - `agent` — agent flag value (`--agent`); `None` means prompt.
/// - `project_type_flag` — project-type flag value (`--project-type`); `None`
///   means auto-detect and (if interactive) confirm.
/// - `docker` — `true` when `--docker` was explicitly given.
/// - `no_docker` — `true` when `--no-docker` was explicitly given.
/// - `overwrite` — whether to overwrite existing artefacts.
/// - `_name` — reserved for multi-config repos (not yet used).
/// - `json` — emit JSON output instead of human-readable text.
#[allow(clippy::too_many_arguments)]
pub(super) fn handle(
    agent: Option<&str>,
    project_type_flag: Option<&str>,
    docker: bool,
    no_docker: bool,
    overwrite: bool,
    _name: Option<&str>,
    json: bool,
) -> Result<(), CliError> {
    let cwd = std::env::current_dir()
        .map_err(|e| CliError::Other(format!("cannot determine current directory: {e}")))?;

    let vai_dir = cwd.join(".vai");

    // Read the repo config for repo_id and repo name.
    let config = crate::repo::read_config(&vai_dir)
        .map_err(|e| CliError::Other(format!("not a vai repository (run `vai init` first): {e}")))?;

    let repo_id = config.repo_id;
    let repo_name = config.name.clone();
    let server_url = config
        .remote
        .as_ref()
        .map(|r| r.url.clone())
        .or_else(|| std::env::var("VAI_SERVER_URL").ok())
        .unwrap_or_else(|| crate::defaults::DEFAULT_SERVER_URL.to_string());

    // ── Project type ──────────────────────────────────────────────────────────

    let project_type: ProjectType = if let Some(flag) = project_type_flag {
        parse_project_type(flag).ok_or_else(|| {
            CliError::Other(format!(
                "unknown project type '{}'. Valid: frontend-react, backend-rust, backend-typescript, generic",
                flag
            ))
        })?
    } else {
        let detected = detect_project_type(&cwd);
        // If stdin is a terminal and no flag was given, let the user confirm.
        if io::stderr().is_terminal() {
            interactive_project_type(detected)
        } else {
            detected
        }
    };

    // ── Agent kind ────────────────────────────────────────────────────────────

    let agent_str: String = if let Some(a) = agent {
        a.to_string()
    } else if io::stderr().is_terminal() {
        let kind = interactive_agent();
        generate::agent_name(kind).to_string()
    } else {
        "claude-code".to_string()
    };
    let agent_kind = agent_str.parse::<AgentKind>().unwrap_or(AgentKind::Custom);

    // ── Run mode ───────────────────────────────────────────────────────────────

    let run_mode: RunMode = if docker {
        RunMode::Docker
    } else if no_docker {
        RunMode::Bare
    } else if io::stderr().is_terminal() {
        interactive_run_mode()
    } else {
        // Non-interactive default: use Docker if available, else bare.
        if docker_available() {
            RunMode::Docker
        } else {
            RunMode::Bare
        }
    };

    // ── Generate artefacts ────────────────────────────────────────────────────

    let gen_cfg = GenerateConfig {
        agent: agent_kind,
        project_type,
        run_mode,
        repo_name: repo_name.clone(),
        server_url: server_url.clone(),
        overwrite,
    };

    let gen_out = match generate::generate(&gen_cfg, &vai_dir) {
        Ok(out) => out,
        Err(generate::GenerateError::AlreadyExists(path)) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({"status": "already_exists", "path": path.display().to_string()})
                );
            } else {
                println!(
                    "{} Loop configuration already exists at {}. No changes.",
                    "✓".green().bold(),
                    path.display()
                );
                println!("  To regenerate, re-run with --overwrite.");
            }
            return Ok(());
        }
        Err(e) => return Err(CliError::Other(e.to_string())),
    };

    // ── .env handling ─────────────────────────────────────────────────────────

    let env_path = cwd.join(".env");
    let env_content = if env_path.exists() {
        std::fs::read_to_string(&env_path)
            .map_err(|e| CliError::Other(format!("cannot read .env: {e}")))?
    } else {
        String::new()
    };
    let parsed_env: ParsedEnv = parse(&env_content);

    let env_updated: bool;
    let vai_api_key_preview: String;

    if super::env_writer::has_vai_block(&env_content) {
        if !json {
            println!("{}", "vai loop block already present in .env — skipping.".yellow());
        }
        env_updated = false;
        vai_api_key_preview = String::new();
    } else {
        // Determine whether we need a new VAI_API_KEY.
        let vai_api_key: String = if parsed_env.has_key_with_value("VAI_API_KEY") {
            if !json {
                println!("{}", "VAI_API_KEY already present in .env — skipping key creation.".dimmed());
            }
            parsed_env.keys.get("VAI_API_KEY").cloned().unwrap_or_default()
        } else {
            // Try to create a key via the server.
            match crate::credentials::load_api_key() {
                Ok((bearer, server_url_opt)) => {
                    let effective_url = server_url_opt.unwrap_or_else(|| server_url.clone());
                    let rt = super::super::make_rt()?;
                    rt.block_on(create_api_key(&effective_url, &bearer, repo_id, &repo_name))
                        .map_err(|e| CliError::Other(format!("failed to create API key: {e}")))?
                }
                Err(_) => {
                    // Not logged in — write a placeholder so the user can fill it in.
                    if !json {
                        println!(
                            "{}",
                            "Not logged in — writing VAI_API_KEY placeholder. Run `vai login` then `vai agent loop init` again.".yellow()
                        );
                    }
                    "# TODO: replace with your VAI_API_KEY".to_string()
                }
            }
        };

        let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
        write_env(&env_path, &vai_api_key, agent_kind, &date)
            .map_err(|e| CliError::Other(format!("failed to write .env: {e}")))?;

        env_updated = true;
        vai_api_key_preview = vai_api_key[..vai_api_key.len().min(12)].to_string();
    }

    // ── Output ────────────────────────────────────────────────────────────────

    if json {
        print_json(
            &repo_name,
            project_type,
            &agent_str,
            run_mode,
            env_updated,
            &gen_out,
        );
    } else {
        print_human(
            &repo_name,
            project_type,
            &agent_str,
            run_mode,
            env_updated,
            &vai_api_key_preview,
            &gen_out,
        );
    }

    Ok(())
}

// ── Output helpers ────────────────────────────────────────────────────────────

/// JSON output for `vai agent loop init`.
#[derive(Debug, Serialize)]
struct InitOutput {
    repo_name: String,
    project_type: String,
    agent: String,
    run_mode: String,
    env_updated: bool,
    loop_sh: String,
    dockerfile: Option<String>,
    prompt_md: String,
    agent_toml: String,
    prompt_md_backup: Option<String>,
}

fn print_json(
    repo_name: &str,
    project_type: ProjectType,
    agent: &str,
    run_mode: RunMode,
    env_updated: bool,
    out: &generate::GenerateOutput,
) {
    let output = InitOutput {
        repo_name: repo_name.to_string(),
        project_type: project_type_label(project_type).to_string(),
        agent: agent.to_string(),
        run_mode: match run_mode {
            RunMode::Docker => "docker".to_string(),
            RunMode::Bare => "bare".to_string(),
        },
        env_updated,
        loop_sh: out.loop_sh.display().to_string(),
        dockerfile: out.dockerfile.as_ref().map(|p| p.display().to_string()),
        prompt_md: out.prompt_md.display().to_string(),
        agent_toml: out.agent_toml.display().to_string(),
        prompt_md_backup: out
            .prompt_md_backup
            .as_ref()
            .map(|p| p.display().to_string()),
    };
    println!("{}", serde_json::to_string_pretty(&output).unwrap_or_default());
}

fn print_human(
    repo_name: &str,
    project_type: ProjectType,
    agent: &str,
    run_mode: RunMode,
    env_updated: bool,
    vai_api_key_preview: &str,
    out: &generate::GenerateOutput,
) {
    println!();
    println!("{} {}", "✓".green().bold(), format!("Wrote {}", out.loop_sh.display()).bold());
    if let Some(ref df) = out.dockerfile {
        println!("{} {}", "✓".green().bold(), format!("Wrote {}", df.display()).bold());
    }
    if let Some(ref bak) = out.prompt_md_backup {
        println!(
            "{} Backed up prompt.md → {}",
            "✓".green().bold(),
            bak.display()
        );
    }
    println!("{} {}", "✓".green().bold(), format!("Wrote {}", out.prompt_md.display()).bold());
    println!(
        "{} {}",
        "✓".green().bold(),
        format!("Merged [checks] into {}", out.agent_toml.display()).bold()
    );
    if env_updated {
        if !vai_api_key_preview.is_empty() {
            println!(
                "{} Created VAI_API_KEY in .env ({}…)",
                "✓".green().bold(),
                vai_api_key_preview
            );
        } else {
            println!("{} Updated .env", "✓".green().bold());
        }
    }

    println!();
    println!("  Repository   : {repo_name}");
    println!("  Project type : {}", project_type_label(project_type).cyan());
    println!("  Agent        : {}", agent.cyan());
    println!("  Run mode     : {}", match run_mode {
        RunMode::Docker => "Docker (containerised)".cyan().to_string(),
        RunMode::Bare => "Bare shell (host)".cyan().to_string(),
    });

    println!();
    println!("{}", "Next steps:".bold());
    match agent.parse::<AgentKind>().unwrap_or(AgentKind::Custom) {
        AgentKind::ClaudeCode => {
            println!("  1. Run `claude setup-token` and paste the token into .env as CLAUDE_CODE_OAUTH_TOKEN.");
        }
        AgentKind::Codex => {
            println!("  1. Add your OPENAI_API_KEY to .env.");
        }
        AgentKind::Custom => {
            println!("  1. Configure your agent invocation in {}.", out.loop_sh.display());
        }
    }
    println!("  2. Review {}.", out.prompt_md.display());
    println!("  3. Run `vai agent loop run` to start the loop.");
}
