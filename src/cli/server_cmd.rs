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
    // VAI_STORAGE_ROOT env var also triggers multi-repo mode even without server.toml.
    let is_multi_repo = global_cfg.storage_root.is_some()
        || std::env::var("VAI_STORAGE_ROOT")
            .ok()
            .map_or(false, |v| !v.is_empty());

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

            // Layer 4: environment variables — override server.toml but yield to CLI flags.
            //
            // Precedence (highest → lowest):
            //   CLI flags > env vars > ~/.vai/server.toml > built-in defaults
            //
            // DATABASE_URL uses the standard PaaS convention (Fly.io, Heroku, Railway).
            if let Ok(u) = std::env::var("DATABASE_URL") {
                if !u.is_empty() { config.database_url = Some(u); }
            }
            if let Ok(h) = std::env::var("VAI_HOST") {
                if !h.is_empty() { config.host = h; }
            }
            if let Ok(p) = std::env::var("VAI_PORT") {
                if let Ok(port_num) = p.parse::<u16>() { config.port = port_num; }
            }
            if let Ok(r) = std::env::var("VAI_STORAGE_ROOT") {
                if !r.is_empty() {
                    config.storage_root = Some(std::path::PathBuf::from(r));
                }
            }
            if let Ok(v) = std::env::var("VAI_CORS_ORIGINS") {
                if !v.is_empty() {
                    config.cors_origins = Some(
                        v.split(',')
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect(),
                    );
                }
            }
            // S3 configuration from env vars. VAI_S3_BUCKET must be set for any
            // other S3 env vars to take effect. VAI_S3_ACCESS_KEY / VAI_S3_SECRET_KEY
            // are forwarded to the standard AWS credential env vars.
            #[cfg(feature = "s3")]
            {
                if let Ok(bucket) = std::env::var("VAI_S3_BUCKET") {
                    if !bucket.is_empty() {
                        let s3 = config.s3.get_or_insert_with(|| crate::storage::s3::S3Config {
                            bucket: String::new(),
                            region: "us-east-1".to_string(),
                            endpoint_url: None,
                            force_path_style: false,
                        });
                        s3.bucket = bucket;
                        if let Ok(region) = std::env::var("VAI_S3_REGION") {
                            if !region.is_empty() { s3.region = region; }
                        }
                        if let Ok(endpoint) = std::env::var("VAI_S3_ENDPOINT") {
                            if !endpoint.is_empty() {
                                s3.force_path_style = true;
                                s3.endpoint_url = Some(endpoint);
                            }
                        }
                    }
                }
                // Forward VAI_S3_ACCESS_KEY / VAI_S3_SECRET_KEY to the AWS SDK
                // credential env vars so the default credential chain picks them up.
                if let Ok(key) = std::env::var("VAI_S3_ACCESS_KEY") {
                    if !key.is_empty() {
                        // SAFETY: single-threaded startup; AWS SDK reads these before
                        // the async runtime spawns worker threads.
                        unsafe { std::env::set_var("AWS_ACCESS_KEY_ID", key); }
                    }
                }
                if let Ok(secret) = std::env::var("VAI_S3_SECRET_KEY") {
                    if !secret.is_empty() {
                        unsafe { std::env::set_var("AWS_SECRET_ACCESS_KEY", secret); }
                    }
                }
            }

            // Layer 5: CLI flags — highest priority, override everything above.
            if let Some(h) = host { config.host = h; }
            if let Some(p) = port { config.port = p; }
            if let Some(pf) = pid_file { config.pid_file = Some(pf); }
            if let Some(u) = database_url { config.database_url = Some(u); }
            if let Some(s) = db_pool_size { config.db_pool_size = Some(s); }
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
