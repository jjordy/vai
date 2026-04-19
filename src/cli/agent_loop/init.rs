//! `vai agent loop init` — scaffold loop configuration and update `.env`.
//!
//! Steps performed:
//! 1. Locate the current `.vai/config.toml` to read `repo_id` and `name`.
//! 2. Detect the project type (Rust / React / TypeScript / generic).
//! 3. Parse the existing `.env` file (or treat it as empty if absent).
//! 4. If `VAI_API_KEY` is missing or empty, call `POST /api/keys` to create
//!    a repo-scoped write key and obtain the plaintext token.
//! 5. Append the vai loop block to `.env` (idempotent).

use colored::Colorize;
use serde::{Deserialize, Serialize};

use crate::cli::CliError;

use super::detection::detect_project_type;
use super::env::{parse, ParsedEnv};
use super::env_writer::{AgentKind, write_env};

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

// ── Handler ────────────────────────────────────────────────────────────────────

/// Handle `vai agent loop init`.
pub(super) fn handle(
    agent: Option<&str>,
    _project_type: Option<&str>,
    _docker: bool,
    _overwrite: bool,
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

    // Detect the project type from the working directory.
    let project_type = detect_project_type(&cwd);

    // Resolve the agent kind.
    let agent_str = agent.unwrap_or("claude-code");
    let agent_kind = AgentKind::from_str(agent_str);

    // Parse the existing .env file.
    let env_path = cwd.join(".env");
    let env_content = if env_path.exists() {
        std::fs::read_to_string(&env_path)
            .map_err(|e| CliError::Other(format!("cannot read .env: {e}")))?
    } else {
        String::new()
    };
    let parsed_env: ParsedEnv = parse(&env_content);

    // Idempotency: if the block already exists, do nothing for the .env.
    if super::env_writer::has_vai_block(&env_content) {
        if !json {
            println!("{}", "vai loop block already present in .env — skipping.".yellow());
        }
        print_summary(json, &repo_name, project_type, agent_str, false);
        return Ok(());
    }

    // Determine whether we need a new VAI_API_KEY.
    let vai_api_key: String = if parsed_env.has_key_with_value("VAI_API_KEY") {
        // Key already in .env — reuse it; don't call the server.
        if !json {
            println!("{}", "VAI_API_KEY already present in .env — skipping key creation.".dimmed());
        }
        parsed_env.keys.get("VAI_API_KEY").cloned().unwrap_or_default()
    } else {
        // Need to create a key via the server.
        let (bearer, server_url_opt) = crate::credentials::load_api_key()
            .map_err(|e| CliError::Other(format!("not logged in: {e}. Run `vai login` first.")))?;

        let server_url = server_url_opt.ok_or_else(|| {
            CliError::Other("cannot determine server URL; run `vai login` or set VAI_SERVER_URL".to_string())
        })?;

        let rt = super::super::make_rt()?;
        rt.block_on(create_api_key(&server_url, &bearer, repo_id, &repo_name))
            .map_err(|e| CliError::Other(format!("failed to create API key: {e}")))?
    };

    // Today's date for the block header.
    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();

    // Append the block.
    write_env(&env_path, &vai_api_key, agent_kind, &date)
        .map_err(|e| CliError::Other(format!("failed to write .env: {e}")))?;

    if !json {
        println!(
            "{} {}",
            "Updated".green().bold(),
            env_path.display()
        );
        println!(
            "  Project  : {:?}",
            project_type
        );
        println!("  Agent    : {agent_str}");
        println!(
            "  Key      : {}…",
            &vai_api_key[..vai_api_key.len().min(12)]
        );
    }

    print_summary(json, &repo_name, project_type, agent_str, true);
    Ok(())
}

// ── Output helpers ────────────────────────────────────────────────────────────

/// JSON output for `vai agent loop init`.
#[derive(Debug, Serialize)]
struct InitOutput {
    repo_name: String,
    project_type: String,
    agent: String,
    env_updated: bool,
}

fn print_summary(
    json: bool,
    repo_name: &str,
    project_type: super::detection::ProjectType,
    agent: &str,
    env_updated: bool,
) {
    if !json {
        return;
    }
    let out = InitOutput {
        repo_name: repo_name.to_string(),
        project_type: format!("{project_type:?}"),
        agent: agent.to_string(),
        env_updated,
    };
    println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
}
