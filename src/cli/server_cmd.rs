//! Server start command handler.

#[cfg(feature = "server")]
use colored::Colorize;

#[cfg(feature = "server")]
use crate::auth;
#[cfg(feature = "server")]
use crate::repo;
#[cfg(feature = "server")]
use crate::server;

#[cfg(feature = "server")]
use super::{CliError, ServerCommands, KeysCommands};

/// Handle all `vai server` subcommands.
#[cfg(feature = "server")]
pub(super) fn handle(server_cmd: ServerCommands, json: bool) -> Result<(), CliError> {
    // Load the global server config first so we can detect multi-repo
    // mode before deciding whether `find_root` is required.
    let global_cfg = repo::read_global_server_config().unwrap_or_default();
    let is_multi_repo = global_cfg.storage_root.is_some();

    // In multi-repo mode the server is not tied to any single
    // repository, so `find_root` would spuriously fail when the
    // process is started from an unrelated directory.  Use `~/.vai/`
    // as the server-level store for API keys instead.
    let vai_dir = if is_multi_repo {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .map_err(|_| CliError::Other("cannot determine home directory".to_string()))?;
        std::path::PathBuf::from(home).join(".vai")
    } else {
        let cwd = std::env::current_dir()
            .map_err(|e| CliError::Other(format!("cannot determine working directory: {e}")))?;
        let root = repo::find_root(&cwd)
            .ok_or_else(|| CliError::Other("not inside a vai repository".to_string()))?;
        root.join(".vai")
    };

    match server_cmd {
        ServerCommands::Start { port, host, pid_file, database_url, db_pool_size, cors_origins } => {
            // Config layering (lowest → highest priority):
            //   1. Built-in defaults (127.0.0.1:7865, no storage_root)
            //   2. ~/.vai/server.toml [server] section (global, optional)
            //   3. .vai/config.toml [server] section (per-repo, single-repo mode only)
            //   4. CLI flags / VAI_DATABASE_URL env var (--host, --port, --database-url)
            let mut config = server::ServerConfig::default();

            // Layer 2: global server config (already loaded above).
            if let Some(h) = global_cfg.host { config.host = h; }
            if let Some(p) = global_cfg.port { config.port = p; }
            if let Some(r) = global_cfg.storage_root { config.storage_root = Some(r); }
            if let Some(u) = global_cfg.database_url { config.database_url = Some(u); }
            if let Some(s) = global_cfg.db_pool_size { config.db_pool_size = Some(s); }
            if let Some(s3) = global_cfg.s3 { config.s3 = Some(s3); }
            if let Some(origins) = global_cfg.cors_origins { config.cors_origins = Some(origins); }

            // Layer 3: per-repo config (single-repo mode only).
            if !is_multi_repo {
                if let Ok(repo_cfg) = repo::read_config(&vai_dir) {
                    if let Some(srv) = repo_cfg.server {
                        if let Some(h) = srv.host { config.host = h; }
                        if let Some(p) = srv.port { config.port = p; }
                    }
                }
            }

            // Layer 4: CLI flags / env var
            if let Some(h) = host { config.host = h; }
            if let Some(p) = port { config.port = p; }
            if let Some(pf) = pid_file { config.pid_file = Some(pf); }
            if let Some(u) = database_url { config.database_url = Some(u); }
            if let Some(s) = db_pool_size { config.db_pool_size = Some(s); }
            // --cors-origins CLI flag overrides server.toml; VAI_CORS_ORIGINS is
            // handled later in server::start() as the lowest-priority env fallback.
            if let Some(raw) = cors_origins {
                config.cors_origins = Some(
                    raw.split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect(),
                );
            }

            tokio::runtime::Runtime::new()
                .map_err(|e| CliError::Other(format!("cannot create async runtime: {e}")))?
                .block_on(server::start(&vai_dir, config))?;
        }
        ServerCommands::Keys(keys_cmd) => {
            match keys_cmd {
                KeysCommands::Create { name } => {
                    let (meta, plaintext) = auth::create(&vai_dir, &name)?;
                    if json {
                        let out = serde_json::json!({
                            "name": meta.name,
                            "key": plaintext,
                            "key_prefix": meta.key_prefix,
                            "created_at": meta.created_at.to_rfc3339(),
                        });
                        println!("{}", serde_json::to_string_pretty(&out).unwrap());
                    } else {
                        println!("{} API key created", "✓".green().bold());
                        println!("  Name : {}", meta.name.bold());
                        println!("  Key  : {}", plaintext.yellow().bold());
                        println!();
                        println!(
                            "  {} This key will not be shown again. Store it securely.",
                            "!".yellow().bold()
                        );
                    }
                }
                KeysCommands::List => {
                    let keys = auth::list(&vai_dir)?;
                    if json {
                        println!("{}", serde_json::to_string_pretty(&keys).unwrap());
                    } else if keys.is_empty() {
                        println!("No API keys. Use `vai server keys create --name <name>` to create one.");
                    } else {
                        println!("{:<24} {:<16} {:<28} STATUS", "NAME", "PREFIX", "CREATED");
                        println!("{}", "-".repeat(80));
                        for k in &keys {
                            let status = if k.revoked {
                                "revoked".red().to_string()
                            } else {
                                "active".green().to_string()
                            };
                            let last_used = k
                                .last_used_at
                                .map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string())
                                .unwrap_or_else(|| "never".to_string());
                            println!(
                                "{:<24} {:<16} {:<28} {}  (last used: {})",
                                k.name,
                                k.key_prefix,
                                k.created_at.format("%Y-%m-%d %H:%M UTC"),
                                status,
                                last_used,
                            );
                        }
                    }
                }
                KeysCommands::Revoke { name } => {
                    auth::revoke(&vai_dir, &name)?;
                    if json {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(
                                &serde_json::json!({ "revoked": name })
                            )
                            .unwrap()
                        );
                    } else {
                        println!(
                            "{} API key '{}' revoked",
                            "✓".green().bold(),
                            name.bold()
                        );
                    }
                }
            }
        }
    }
    Ok(())
}
