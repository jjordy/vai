//! Remote config command handlers (Remote subcommand + Clone/Pull/Push/Sync).

use base64::Engine as _;
use colored::Colorize;

use crate::repo;
use crate::pull as remote_pull;
use crate::push as remote_push;
use crate::sync as remote_sync;

use super::{CliError, RemoteCommands};
use super::make_rt;

/// Handle all `vai remote` subcommands.
pub(super) fn handle_remote(remote_cmd: RemoteCommands, json: bool) -> Result<(), CliError> {
    let cwd = std::env::current_dir()
        .map_err(|e| CliError::Other(format!("cannot determine working directory: {e}")))?;
    let root = repo::find_root(&cwd)
        .ok_or_else(|| CliError::Other("not inside a vai repository".to_string()))?;
    let vai_dir = root.join(".vai");
    match remote_cmd {
        RemoteCommands::Add { url, key, key_env, key_cmd } => {
            if key.is_none() && key_env.is_none() && key_cmd.is_none() {
                return Err(CliError::Other(
                    "one of --key, --key-env, or --key-cmd is required".to_string(),
                ));
            }
            let mut config = repo::read_config(&vai_dir)?;
            config.remote = Some(repo::RemoteServerConfig {
                url: url.clone(),
                api_key: key,
                api_key_env: key_env,
                api_key_cmd: key_cmd,
            });
            repo::write_config(&vai_dir, &config)?;
            if json {
                println!("{}", serde_json::json!({"status": "ok", "url": url}));
            } else {
                println!("Remote set to {}", url.cyan());
            }
        }
        RemoteCommands::Remove => {
            let mut config = repo::read_config(&vai_dir)?;
            if config.remote.is_none() {
                return Err(CliError::Other("no remote configured".to_string()));
            }
            config.remote = None;
            repo::write_config(&vai_dir, &config)?;
            if json {
                println!("{}", serde_json::json!({"status": "ok"}));
            } else {
                println!("Remote configuration removed.");
            }
        }
        RemoteCommands::Status => {
            let config = repo::read_config(&vai_dir)?;
            // Check for a migration marker written by `vai remote migrate`.
            let marker = crate::migration::MigrationMarker::read(&vai_dir);
            match &config.remote {
                None => {
                    if json {
                        println!("{}", serde_json::json!({"configured": false}));
                    } else {
                        println!("No remote configured.");
                        println!("Run `vai remote add <url> --key <api-key>` to set one.");
                    }
                }
                Some(remote) => {
                    let rt = tokio::runtime::Runtime::new()
                        .map_err(|e| CliError::Other(format!("cannot create async runtime: {e}")))?;
                    let client = crate::remote_client::RemoteClient::new(remote)
                        .map_err(|e| CliError::Other(format!("API key error: {e}")))?;

                    // Ping connectivity.
                    let reachable = rt.block_on(async {
                        client
                            .get::<serde_json::Value>("/api/status")
                            .await
                            .is_ok()
                    });

                    // If migrated, fetch remote stats for verification.
                    let repo_name = &config.name;
                    let stats: Option<serde_json::Value> = if marker.is_some() {
                        rt.block_on(async {
                            let repo_endpoint =
                                format!("/api/repos/{repo_name}/migration-stats");
                            match client
                                .get::<serde_json::Value>(
                                    &repo_endpoint,
                                )
                                .await
                            {
                                Ok(s) => Some(s),
                                Err(_) => {
                                    // Fall back to single-repo endpoint.
                                    client
                                        .get::<serde_json::Value>(
                                            "/api/migration-stats",
                                        )
                                        .await
                                        .ok()
                                }
                            }
                        })
                    } else {
                        None
                    };

                    if json {
                        let mut obj = serde_json::json!({
                            "configured": true,
                            "url": remote.url,
                            "reachable": reachable,
                        });
                        if let Some(ref m) = marker {
                            obj["migrated_at"] = serde_json::json!(m.migrated_at.to_rfc3339());
                            obj["migration"] = serde_json::json!({
                                "events_migrated": m.events_migrated,
                                "issues_migrated": m.issues_migrated,
                                "versions_migrated": m.versions_migrated,
                                "escalations_migrated": m.escalations_migrated,
                                "head_version": m.head_version,
                            });
                        }
                        if let Some(ref s) = stats {
                            obj["remote_counts"] = serde_json::json!({
                                "events": s["events"],
                                "issues": s["issues"],
                                "versions": s["versions"],
                                "escalations": s["escalations"],
                                "head_version": s["head_version"],
                            });
                        }
                        println!("{}", obj);
                    } else {
                        if reachable {
                            println!("Remote: {} {}", remote.url.cyan(), "(reachable)".green());
                        } else {
                            println!("Remote: {} {}", remote.url.cyan(), "(unreachable)".red());
                        }
                        if let Some(ref m) = marker {
                            println!();
                            println!(
                                "Migrated: {}",
                                m.migrated_at.format("%Y-%m-%d %H:%M:%S UTC")
                            );
                            println!("Data transferred:");
                            println!("  Events:      {}", m.events_migrated);
                            println!("  Issues:      {}", m.issues_migrated);
                            println!("  Versions:    {}", m.versions_migrated);
                            println!("  Escalations: {}", m.escalations_migrated);
                            if let Some(ref head) = m.head_version {
                                println!("  HEAD:        {}", head);
                            }

                            if let Some(ref s) = stats {
                                println!();
                                println!("Verification (remote counts):");
                                let r_ev = s["events"].as_i64().unwrap_or(0);
                                let r_is = s["issues"].as_i64().unwrap_or(0);
                                let r_ve = s["versions"].as_i64().unwrap_or(0);
                                let r_es = s["escalations"].as_i64().unwrap_or(0);
                                let ev_ok = r_ev == m.events_migrated as i64;
                                let is_ok = r_is == m.issues_migrated as i64;
                                let ve_ok = r_ve == m.versions_migrated as i64;
                                let es_ok = r_es == m.escalations_migrated as i64;
                                let tick = |ok: bool| {
                                    if ok { "OK".green().to_string() } else { "MISMATCH".red().to_string() }
                                };
                                println!(
                                    "  Events:      {} (expected {}) [{}]",
                                    r_ev, m.events_migrated, tick(ev_ok)
                                );
                                println!(
                                    "  Issues:      {} (expected {}) [{}]",
                                    r_is, m.issues_migrated, tick(is_ok)
                                );
                                println!(
                                    "  Versions:    {} (expected {}) [{}]",
                                    r_ve, m.versions_migrated, tick(ve_ok)
                                );
                                println!(
                                    "  Escalations: {} (expected {}) [{}]",
                                    r_es, m.escalations_migrated, tick(es_ok)
                                );
                                if ev_ok && is_ok && ve_ok && es_ok {
                                    println!("\nMigration verified successfully.");
                                } else {
                                    println!("\nWarning: some counts do not match.");
                                }
                            }
                        }
                    }
                }
            }
        }
        RemoteCommands::Migrate => {
            let config = repo::read_config(&vai_dir)?;
            let remote = config.remote.as_ref().ok_or_else(|| {
                CliError::Other(
                    "no remote configured; run `vai remote add <url> --key <key>` first"
                        .to_string(),
                )
            })?;

            if !json {
                println!("Gathering local data…");
            }

            let payload = crate::migration::gather_local_data(&vai_dir)
                .map_err(|e| CliError::Other(format!("failed to read local data: {e}")))?;

            let event_count = payload.events.len();
            let issue_count = payload.issues.len();
            let version_count = payload.versions.len();
            let esc_count = payload.escalations.len();

            if !json {
                println!(
                    "Migrating {} events, {} issues, {} versions, {} escalations…",
                    event_count, issue_count, version_count, esc_count
                );
            }

            let rt = tokio::runtime::Runtime::new()
                .map_err(|e| CliError::Other(format!("cannot create async runtime: {e}")))?;
            let client = crate::remote_client::RemoteClient::new(remote)
                .map_err(|e| CliError::Other(format!("API key error: {e}")))?;

            // Try repo-scoped endpoint first; fall back to single-repo.
            let repo_name = &config.name;
            let endpoint = format!("/api/repos/{repo_name}/migrate");

            let summary: crate::migration::MigrationSummary = rt.block_on(async {
                // Try the repo-scoped endpoint.
                match client
                    .post::<_, crate::migration::MigrationSummary>(&endpoint, &payload)
                    .await
                {
                    Ok(s) => Ok(s),
                    Err(crate::remote_client::RemoteClientError::HttpError {
                        status: 404, ..
                    }) => {
                        // Fall back to the single-repo endpoint.
                        client
                            .post::<_, crate::migration::MigrationSummary>(
                                "/api/migrate",
                                &payload,
                            )
                            .await
                    }
                    Err(e) => Err(e),
                }
            })
            .map_err(|e| CliError::Other(format!("migration failed: {e}")))?;

            // ── Source file upload (PRD 12.3) ──────────────────────────
            let source_files = repo::list_migration_files(&root);
            let total_files = source_files.len();

            if !json && total_files > 0 {
                println!("Uploading {} source files…", total_files);
            }

            // Serializable types for the batch upload request/response.
            #[derive(serde::Serialize)]
            struct FileEntry {
                path: String,
                content_base64: String,
            }
            #[derive(serde::Serialize)]
            struct FileBatch {
                files: Vec<FileEntry>,
            }
            #[derive(serde::Deserialize)]
            struct FileBatchResponse {
                uploaded: usize,
            }

            const BATCH_SIZE: usize = 50;
            let mut files_uploaded = 0usize;
            let files_endpoint = format!("/api/repos/{repo_name}/files");

            for chunk in source_files.chunks(BATCH_SIZE) {
                let entries: Vec<FileEntry> = chunk
                    .iter()
                    .filter_map(|path| {
                        let content = std::fs::read(path).ok()?;
                        let rel = path
                            .strip_prefix(&root)
                            .ok()?
                            .to_string_lossy()
                            .replace('\\', "/");
                        let encoded =
                            base64::engine::general_purpose::STANDARD.encode(&content);
                        Some(FileEntry {
                            path: rel,
                            content_base64: encoded,
                        })
                    })
                    .collect();

                if entries.is_empty() {
                    continue;
                }

                let batch = FileBatch { files: entries };

                let resp: FileBatchResponse = rt
                    .block_on(async {
                        match client
                            .post::<_, FileBatchResponse>(&files_endpoint, &batch)
                            .await
                        {
                            Ok(r) => Ok(r),
                            Err(crate::remote_client::RemoteClientError::HttpError {
                                status: 404,
                                ..
                            }) => {
                                client
                                    .post::<_, FileBatchResponse>("/api/files", &batch)
                                    .await
                            }
                            Err(e) => Err(e),
                        }
                    })
                    .map_err(|e| {
                        CliError::Other(format!("source file upload failed: {e}"))
                    })?;

                files_uploaded += resp.uploaded;
                if !json {
                    println!(
                        "  Uploaded {}/{} files…",
                        files_uploaded, total_files
                    );
                }
            }

            // ── Graph rebuild (PRD 12.4) ────────────────────────────────
            if files_uploaded > 0 {
                if !json {
                    println!("Triggering server-side graph rebuild…");
                }

                #[derive(serde::Deserialize)]
                struct GraphRefreshResp {
                    files_scanned: usize,
                }

                let graph_endpoint =
                    format!("/api/repos/{repo_name}/graph/refresh");
                let refresh: GraphRefreshResp = rt
                    .block_on(async {
                        match client
                            .post::<_, GraphRefreshResp>(
                                &graph_endpoint,
                                &serde_json::json!({}),
                            )
                            .await
                        {
                            Ok(r) => Ok(r),
                            Err(crate::remote_client::RemoteClientError::HttpError {
                                status: 404,
                                ..
                            }) => {
                                client
                                    .post::<_, GraphRefreshResp>(
                                        "/api/graph/refresh",
                                        &serde_json::json!({}),
                                    )
                                    .await
                            }
                            Err(e) => Err(e),
                        }
                    })
                    .map_err(|e| {
                        CliError::Other(format!("graph refresh failed: {e}"))
                    })?;

                if !json {
                    println!(
                        "  Graph rebuilt — {} files scanned.",
                        refresh.files_scanned
                    );
                }
            }

            // Write the migration marker (TOML, includes counts for verification).
            let marker = crate::migration::MigrationMarker {
                migrated_at: summary.migrated_at,
                remote_url: remote.url.clone(),
                events_migrated: summary.events_migrated,
                issues_migrated: summary.issues_migrated,
                versions_migrated: summary.versions_migrated,
                escalations_migrated: summary.escalations_migrated,
                head_version: summary.head_version.clone(),
            };
            marker
                .write(&vai_dir)
                .map_err(|e| CliError::Other(format!("failed to write migrated_at: {e}")))?;

            if json {
                println!("{}", serde_json::to_string_pretty(&summary).unwrap());
            } else {
                println!("Migration complete.");
                println!(
                    "  Events:      {}",
                    summary.events_migrated
                );
                println!("  Issues:      {}", summary.issues_migrated);
                println!("  Versions:    {}", summary.versions_migrated);
                println!("  Escalations: {}", summary.escalations_migrated);
                if let Some(ref head) = summary.head_version {
                    println!("  HEAD:        {}", head);
                }
                println!(
                    "\nAll future commands will proxy to {}",
                    remote.url.cyan()
                );
                println!(
                    "Local .vai/ kept as backup (remove with `vai remote remove` to revert)."
                );
            }
        }
    }
    Ok(())
}

/// Handle `vai clone`.
pub(super) fn handle_clone(url: String, dest: Option<String>, key: String, json: bool) -> Result<(), CliError> {
    // Derive destination directory from repo name if not specified.
    let dest_path = if let Some(d) = dest {
        std::path::PathBuf::from(d)
    } else {
        // Use the last path component of the URL as the directory name.
        let name = url
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or("vai-repo")
            .to_string();
        std::path::PathBuf::from(name)
    };

    let result = tokio::runtime::Runtime::new()
        .map_err(|e| CliError::Other(format!("cannot create async runtime: {e}")))?
        .block_on(crate::clone::clone(&url, &dest_path, &key))?;

    if json {
        println!("{}", serde_json::to_string_pretty(&result).unwrap());
    } else {
        crate::clone::print_clone_result(&result);
    }
    Ok(())
}

/// Handle `vai pull`.
pub(super) fn handle_pull(
    from: Option<String>,
    key: Option<String>,
    repo: Option<String>,
    force: bool,
    json: bool,
) -> Result<(), CliError> {
    let cwd = std::env::current_dir()
        .map_err(|e| CliError::Other(format!("cannot determine working directory: {e}")))?;
    let root = repo::find_root(&cwd)
        .ok_or_else(|| CliError::Other("not inside a vai repository".to_string()))?;
    let vai_dir = root.join(".vai");

    // Build PullConfig from explicit flags or the configured remote.
    let pull_config = if let Some(server_url) = from {
        let api_key = key.ok_or(remote_pull::PullError::MissingKey)?;
        let repo_name = repo.ok_or(remote_pull::PullError::MissingRepo)?;
        remote_pull::PullConfig { server_url, api_key, repo_name }
    } else {
        let config = repo::read_config(&vai_dir)?;
        let remote = config.remote.ok_or(remote_pull::PullError::NoRemote)?;
        let api_key = remote.resolve_api_key()
            .map_err(|e| CliError::Other(format!("API key error: {e}")))?;
        remote_pull::PullConfig {
            server_url: remote.url,
            api_key,
            repo_name: config.name,
        }
    };

    let result = if force {
        make_rt()?.block_on(remote_pull::pull_force(&root, pull_config))?
    } else {
        make_rt()?.block_on(remote_pull::pull(&root, pull_config))?
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&result).unwrap());
    } else {
        remote_pull::print_pull_result(&result);
    }
    Ok(())
}

/// Handle `vai push`.
pub(super) fn handle_push(
    message: Option<String>,
    to: Option<String>,
    key: Option<String>,
    repo: Option<String>,
    dry_run: bool,
    json: bool,
) -> Result<(), CliError> {
    let cwd = std::env::current_dir()
        .map_err(|e| CliError::Other(format!("cannot determine working directory: {e}")))?;
    let root = repo::find_root(&cwd)
        .ok_or_else(|| CliError::Other("not inside a vai repository".to_string()))?;
    let vai_dir = root.join(".vai");

    let msg = message.ok_or(remote_push::PushError::MissingMessage)?;

    // Build PushConfig from explicit flags or the configured remote.
    let push_config = if let Some(server_url) = to {
        let api_key = key.ok_or(remote_push::PushError::MissingKey)?;
        let repo_name = repo.ok_or(remote_push::PushError::MissingRepo)?;
        remote_push::PushConfig { server_url, api_key, repo_name }
    } else {
        let config = repo::read_config(&vai_dir)?;
        let remote = config.remote.ok_or(remote_push::PushError::NoRemote)?;
        let api_key = remote.resolve_api_key()
            .map_err(|e| CliError::Other(format!("API key error: {e}")))?;
        remote_push::PushConfig {
            server_url: remote.url,
            api_key,
            repo_name: config.name,
        }
    };

    let result = make_rt()?.block_on(remote_push::push(&root, push_config, &msg, dry_run))?;

    if json {
        println!("{}", serde_json::to_string_pretty(&result).unwrap());
    } else {
        remote_push::print_push_result(&result, dry_run);
    }
    Ok(())
}

/// Handle `vai sync`.
pub(super) fn handle_sync(json: bool) -> Result<(), CliError> {
    let cwd = std::env::current_dir()
        .map_err(|e| CliError::Other(format!("cannot determine working directory: {e}")))?;
    let root = repo::find_root(&cwd)
        .ok_or_else(|| CliError::Other("not inside a vai repository".to_string()))?;

    let result = tokio::runtime::Runtime::new()
        .map_err(|e| CliError::Other(format!("cannot create async runtime: {e}")))?
        .block_on(remote_sync::sync(&root))?;

    if json {
        println!("{}", serde_json::to_string_pretty(&result).unwrap());
    } else {
        remote_sync::print_sync_result(&result);
    }
    Ok(())
}

