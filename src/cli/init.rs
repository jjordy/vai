//! Handler for `vai init` — creates a local repository and optionally registers
//! it on the remote server and pushes an initial snapshot.

use std::io::Write as IoWrite;
use std::path::Path;

use colored::Colorize;

use crate::credentials;
use crate::ignore_rules;
use crate::remote::{RemoteError, Session};
use crate::repo;

use super::{CliError, make_rt};

/// Execute `vai init`.
///
/// Delegates to [`run_init`] after resolving the current working directory.
pub(super) fn handle(
    local_only: bool,
    no_push: bool,
    remote_name: Option<String>,
    json: bool,
) -> Result<(), CliError> {
    let cwd = std::env::current_dir()
        .map_err(|e| CliError::Other(format!("cannot determine working directory: {e}")))?;
    run_init(&cwd, local_only, no_push, remote_name, json)
}

/// Core init logic, parameterised by directory.
///
/// 1. Initialises the local `.vai/` structure via [`repo::init`].
/// 2. If `local_only`, stops here and prints the result.
/// 3. Loads credentials; exits 1 if not logged in.
/// 4. Registers the repo on the server (with collision retry).
/// 5. Persists `remote.url` and `remote.repo_name` to `.vai/config.toml`.
/// 6. Ensures `.env` is listed in `.gitignore`.
/// 7. Unless `no_push`, collects and pushes the initial snapshot.
pub fn run_init(
    cwd: &Path,
    local_only: bool,
    no_push: bool,
    remote_name: Option<String>,
    json: bool,
) -> Result<(), CliError> {
    // ── Step 1: local init ────────────────────────────────────────────────────
    let mut result = repo::init(cwd)?;

    if local_only {
        if json {
            println!("{}", serde_json::to_string_pretty(&result).unwrap());
        } else {
            repo::print_init_result(&result);
        }
        return Ok(());
    }

    // ── Step 2: load credentials ──────────────────────────────────────────────
    let (api_key, server_url_opt) = credentials::load_api_key().map_err(|e| {
        match e {
            credentials::CredentialsError::NotLoggedIn => {
                eprintln!("Not logged in. Run 'vai login' first.");
                std::process::exit(1);
            }
            other => CliError::Other(format!("credentials error: {other}")),
        }
    })?;

    let server_url = match server_url_opt {
        Some(u) if !u.is_empty() => u,
        _ => {
            eprintln!("No server URL configured. Run 'vai login' first.");
            std::process::exit(1);
        }
    };

    // ── Step 3: determine repo name ───────────────────────────────────────────
    let inferred_name = cwd
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unnamed")
        .to_string();
    let initial_name = remote_name.unwrap_or(inferred_name);

    // Validate name format.
    if !is_valid_repo_name(&initial_name) {
        eprintln!(
            "Invalid repository name '{}'. Names must match ^[a-zA-Z0-9][a-zA-Z0-9-_]*$.",
            initial_name
        );
        std::process::exit(1);
    }

    // ── Step 4: register with the server (with collision retry) ───────────────
    let (registered_name, server_repo_id) =
        register_with_retry(cwd, &server_url, &api_key, initial_name)?;

    // ── Step 5: persist remote config + server-assigned repo_id ──────────────
    let vai_dir = cwd.join(".vai");
    let mut config = repo::read_config(&vai_dir)?;
    // Overwrite the client-generated UUID with the server's canonical id.
    // All subsequent API calls that pass repo_id in the request body will now
    // use the correct UUID and match the server's repos table.
    config.repo_id = server_repo_id;
    config.remote = Some(repo::RemoteServerConfig {
        url: server_url.clone(),
        repo_name: Some(registered_name.clone()),
    });
    repo::write_config(&vai_dir, &config)?;

    // Now that we have the canonical server-assigned UUID, update the result
    // and print it — ensures stdout matches config.toml.
    result.config.repo_id = server_repo_id;
    if !json {
        repo::print_init_result(&result);
        println!(
            "{} Registered repo {} on {}",
            "✓".green().bold(),
            registered_name.bold(),
            server_url.cyan()
        );
    }

    // ── Step 6: ensure .env is in .gitignore ─────────────────────────────────
    ensure_env_in_gitignore(cwd);

    if no_push {
        if !json {
            println!("Repo ready: {}/{}", server_url, registered_name);
            println!("Next: vai agent loop init");
        }
        return Ok(());
    }

    // ── Step 7: collect files and check size ──────────────────────────────────
    let files = ignore_rules::collect_all_files(cwd, &[]);
    let total_bytes: u64 = files
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .sum();

    const MB_100: u64 = 100 * 1024 * 1024;
    if total_bytes > MB_100 {
        // Show top 5 largest files.
        let mut sized: Vec<(u64, &std::path::PathBuf)> = files
            .iter()
            .filter_map(|p| std::fs::metadata(p).ok().map(|m| (m.len(), p)))
            .collect();
        sized.sort_by_key(|b| std::cmp::Reverse(b.0));

        eprintln!(
            "Warning: total size is {:.1} MB (> 100 MB limit).",
            total_bytes as f64 / (1024.0 * 1024.0)
        );
        eprintln!("Top 5 largest files:");
        for (size, path) in sized.iter().take(5) {
            let rel = path.strip_prefix(cwd).unwrap_or(path);
            eprintln!("  {:.1} MB  {}", *size as f64 / (1024.0 * 1024.0), rel.display());
        }

        eprint!("Push anyway? [y/N]: ");
        std::io::stderr().flush().ok();
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer).ok();
        if answer.trim().to_lowercase() != "y" {
            println!("Push cancelled. Repo registered but not pushed.");
            println!("Run `vai push -m 'initial commit'` when ready.");
            return Ok(());
        }
    }

    // ── Step 8: push initial snapshot ────────────────────────────────────────
    let session = Session::builder(cwd)
        .remote_url(server_url.clone())
        .api_key(api_key)
        .repo(registered_name.clone())
        .build()?;

    let rt = make_rt()?;
    match rt.block_on(session.push("initial commit", false)) {
        Err(RemoteError::NothingToPush) => {
            if !json {
                println!("Repo registered (no files to push).");
            }
        }
        Err(e) => return Err(CliError::RemoteOps(e)),
        Ok(push_result) => {
            if !json {
                println!(
                    "{} Pushed initial snapshot ({} files)",
                    "✓".green().bold(),
                    push_result.files_applied
                );
            }
        }
    }

    if !json {
        println!("Repo ready: {}/{}", server_url, registered_name);
        println!("Next: vai agent loop init");
    }

    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Returns true if `name` matches `^[a-zA-Z0-9][a-zA-Z0-9-_]*$`.
fn is_valid_repo_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        None => return false,
        Some(c) if !c.is_ascii_alphanumeric() => return false,
        _ => {}
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Registers the repo on the server, retrying on 409 Name Conflict up to 3 times.
///
/// Returns `(registered_name, server_repo_id)`. The caller MUST persist
/// `server_repo_id` as the local `repo_id` — it is the canonical identifier.
fn register_with_retry(
    _cwd: &Path,
    server_url: &str,
    api_key: &str,
    initial_name: String,
) -> Result<(String, uuid::Uuid), CliError> {
    let rt = make_rt()?;
    let client = reqwest::Client::new();
    let base_url = server_url.trim_end_matches('/');
    let register_url = format!("{}/api/repos", base_url);

    let mut name = initial_name.clone();
    let mut tried: Vec<String> = Vec::new();

    for attempt in 0..3usize {
        tried.push(name.clone());

        // Perform the HTTP call and read the response body inside a single block_on.
        enum RegisterOutcome {
            Success(String, uuid::Uuid),
            Conflict,
            Quota,
            Err(String),
        }

        let outcome = rt.block_on(async {
            let resp = match client
                .post(&register_url)
                .header("Authorization", format!("Bearer {api_key}"))
                .json(&serde_json::json!({ "name": name }))
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => return RegisterOutcome::Err(format!("HTTP request failed: {e}")),
            };

            let status = resp.status();

            if status == reqwest::StatusCode::CREATED {
                #[derive(serde::Deserialize)]
                struct Reg { id: Option<uuid::Uuid>, name: String }
                match resp.json::<Reg>().await {
                    Ok(r) => {
                        let id = r.id.unwrap_or_else(uuid::Uuid::new_v4);
                        return RegisterOutcome::Success(r.name, id);
                    }
                    Err(e) => return RegisterOutcome::Err(format!("invalid server response: {e}")),
                }
            }

            if status == reqwest::StatusCode::FORBIDDEN {
                return RegisterOutcome::Quota;
            }

            if status == reqwest::StatusCode::CONFLICT {
                return RegisterOutcome::Conflict;
            }

            let body = resp.text().await.unwrap_or_default();
            RegisterOutcome::Err(format!("server error ({status}): {body}"))
        });

        match outcome {
            RegisterOutcome::Success(registered, server_id) => return Ok((registered, server_id)),
            RegisterOutcome::Quota => {
                eprintln!(
                    "You've hit the 100-repo limit for your account. \
                     Delete unused repos in the dashboard."
                );
                std::process::exit(2);
            }
            RegisterOutcome::Err(msg) => return Err(CliError::Other(msg)),
            RegisterOutcome::Conflict => {
                if attempt >= 2 {
                    // Exhausted retries.
                    eprintln!(
                        "Could not register repo after 3 attempts. Tried names: {}",
                        tried.join(", ")
                    );
                    std::process::exit(3);
                }

                // Suggest a new name and ask the user.
                let suggestion = format!("{}-{}", initial_name, attempt + 2);
                eprint!(
                    "Repo name '{}' already taken. Try a different name? [{}]: ",
                    name, suggestion
                );
                std::io::stderr().flush().ok();
                let mut input = String::new();
                std::io::stdin().read_line(&mut input).ok();
                let trimmed = input.trim();
                name = if trimmed.is_empty() {
                    suggestion
                } else {
                    trimmed.to_string()
                };

                // Validate user-supplied name.
                if !is_valid_repo_name(&name) {
                    eprintln!(
                        "Invalid repository name '{}'. Names must match ^[a-zA-Z0-9][a-zA-Z0-9-_]*$.",
                        name
                    );
                    std::process::exit(1);
                }
            }
        }
    }

    // Should not reach here (loop exits via return or process::exit).
    Err(CliError::Other("registration failed".to_string()))
}

/// Appends `.env` to the root `.gitignore` if it is not already listed.
fn ensure_env_in_gitignore(root: &Path) {
    let gitignore_path = root.join(".gitignore");

    // Check if the file already contains `.env`.
    if gitignore_path.exists() {
        let content = std::fs::read_to_string(&gitignore_path).unwrap_or_default();
        if content.lines().any(|l| l.trim() == ".env") {
            return;
        }
        // Append `.env` to the existing file.
        if let Ok(mut file) = std::fs::OpenOptions::new().append(true).open(&gitignore_path) {
            let entry = if content.ends_with('\n') { ".env\n" } else { "\n.env\n" };
            let _ = file.write_all(entry.as_bytes());
            println!("Added .env to .gitignore.");
        }
    } else {
        // Create a new .gitignore with just `.env`.
        if std::fs::write(&gitignore_path, ".env\n").is_ok() {
            println!("Added .env to .gitignore.");
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::is_valid_repo_name;

    #[test]
    fn valid_repo_names() {
        assert!(is_valid_repo_name("myrepo"));
        assert!(is_valid_repo_name("my-repo"));
        assert!(is_valid_repo_name("my_repo"));
        assert!(is_valid_repo_name("MyRepo2"));
        assert!(is_valid_repo_name("a"));
    }

    #[test]
    fn invalid_repo_names() {
        assert!(!is_valid_repo_name(""));
        assert!(!is_valid_repo_name("-starts-with-dash"));
        assert!(!is_valid_repo_name("has space"));
        assert!(!is_valid_repo_name("has.dot"));
        assert!(!is_valid_repo_name("_starts_underscore"));
    }
}
