//! Version command handlers (Log, Show, Diff, Status, Rollback).

use colored::Colorize;

use crate::credentials;
use crate::remote_diff;
use crate::status as remote_status;
use crate::remote_workspace;
use crate::repo;
use crate::version;
use crate::workspace;
use crate::issue::{IssueFilter, IssueStatus};

use super::{CliError, StatusOutput};
use super::{format_age, make_rt, print_graph_stats, truncate};
use crate::graph::GraphSnapshot;

/// Handle `vai log`.
pub(super) fn handle_log(limit: Option<usize>, json: bool) -> Result<(), CliError> {
    let cwd = std::env::current_dir()
        .map_err(|e| CliError::Other(format!("cannot determine working directory: {e}")))?;
    let root = repo::find_root(&cwd)
        .ok_or_else(|| CliError::Other("not inside a vai repository".to_string()))?;
    let vai_dir = root.join(".vai");

    let mut versions = version::list_versions(&vai_dir)?;
    versions.reverse(); // most recent first

    if let Some(n) = limit {
        versions.truncate(n);
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&versions).unwrap());
    } else if versions.is_empty() {
        println!("No versions yet.");
    } else {
        for v in &versions {
            let age = format_age(v.created_at);
            println!("{:<4}  {:<50}  {}", v.version_id.bold(), v.intent, age);
        }
    }
    Ok(())
}

/// Handle `vai show`.
pub(super) fn handle_show(version_id: String, json: bool) -> Result<(), CliError> {
    let cwd = std::env::current_dir()
        .map_err(|e| CliError::Other(format!("cannot determine working directory: {e}")))?;
    let root = repo::find_root(&cwd)
        .ok_or_else(|| CliError::Other("not inside a vai repository".to_string()))?;
    let vai_dir = root.join(".vai");

    let changes = version::get_version_changes(&vai_dir, &version_id)?;
    let v = &changes.version;

    if json {
        println!("{}", serde_json::to_string_pretty(&changes).unwrap());
    } else {
        println!(
            "{} {}  {}",
            v.version_id.bold(),
            format!("\"{}\"", v.intent).italic(),
            format_age(v.created_at)
        );
        println!("  Created by : {}", v.created_by);
        if let Some(parent) = &v.parent_version_id {
            println!("  Parent     : {}", parent);
        }

        if changes.file_changes.is_empty() && changes.entity_changes.is_empty() {
            println!("\n  (initial version — no changes)");
        } else {
            if !changes.file_changes.is_empty() {
                println!("\n{}", "Files changed:".bold());
                for fc in &changes.file_changes {
                    let sigil = match fc.change_type {
                        version::VersionFileChangeType::Added => "+".green(),
                        version::VersionFileChangeType::Modified => "M".yellow(),
                        version::VersionFileChangeType::Removed => "-".red(),
                    };
                    println!("  {} {}", sigil, fc.path);
                }
            }
            if !changes.entity_changes.is_empty() {
                println!("\n{}", "Entities changed:".bold());
                for ec in &changes.entity_changes {
                    let sigil = ec.change_type.sigil();
                    let colored_sigil = match ec.change_type {
                        version::VersionChangeType::Added => sigil.green(),
                        version::VersionChangeType::Modified => sigil.yellow(),
                        version::VersionChangeType::Removed => sigil.red(),
                    };
                    if let (Some(kind), Some(name), Some(path)) =
                        (&ec.kind, &ec.qualified_name, &ec.file_path)
                    {
                        println!(
                            "  {} {} {}  {}  {}",
                            colored_sigil,
                            kind.cyan(),
                            name.bold(),
                            path,
                            ec.change_type.label()
                        );
                    } else if let Some(desc) = &ec.change_description {
                        println!("  {} {}", colored_sigil, desc);
                    } else {
                        println!(
                            "  {} entity {} {}",
                            colored_sigil,
                            ec.entity_id,
                            ec.change_type.label()
                        );
                    }
                }
            }
        }
    }
    Ok(())
}

/// Handle `vai diff`.
pub(super) fn handle_diff(
    arg1: Option<String>,
    arg2: Option<String>,
    from: Option<String>,
    key: Option<String>,
    repo: Option<String>,
    json: bool,
) -> Result<(), CliError> {
    let cwd = std::env::current_dir()
        .map_err(|e| CliError::Other(format!("cannot determine working directory: {e}")))?;
    let root = repo::find_root(&cwd)
        .ok_or_else(|| CliError::Other("not inside a vai repository".to_string()))?;
    let vai_dir = root.join(".vai");

    match (arg1, arg2) {
        // ── Two version args → semantic diff (local mode) ─────────────
        (Some(version_a), Some(version_b)) => {
            let all_changes = version::get_versions_diff(&vai_dir, &version_a, &version_b)?;

            if json {
                println!("{}", serde_json::to_string_pretty(&all_changes).unwrap());
            } else if all_changes.is_empty() {
                println!("No changes between {} and {}.", version_a, version_b);
            } else {
                println!(
                    "{} Semantic diff: {} → {}",
                    "●".cyan(),
                    version_a.bold(),
                    version_b.bold()
                );
                for vc in &all_changes {
                    println!(
                        "\n{}  {}",
                        vc.version.version_id.bold(),
                        format!("\"{}\"", vc.version.intent).italic()
                    );
                    for fc in &vc.file_changes {
                        let sigil = match fc.change_type {
                            version::VersionFileChangeType::Added => "+".green(),
                            version::VersionFileChangeType::Modified => "M".yellow(),
                            version::VersionFileChangeType::Removed => "-".red(),
                        };
                        println!("  {} {}", sigil, fc.path);
                    }
                    for ec in &vc.entity_changes {
                        let sigil = ec.change_type.sigil();
                        let colored_sigil = match ec.change_type {
                            version::VersionChangeType::Added => sigil.green(),
                            version::VersionChangeType::Modified => sigil.yellow(),
                            version::VersionChangeType::Removed => sigil.red(),
                        };
                        if let (Some(kind), Some(name)) = (&ec.kind, &ec.qualified_name) {
                            println!("  {} {} {}", colored_sigil, kind.cyan(), name.bold());
                        } else if let Some(desc) = &ec.change_description {
                            println!("  {} {}", colored_sigil, desc);
                        } else {
                            println!(
                                "  {} entity {} {}",
                                colored_sigil,
                                ec.entity_id,
                                ec.change_type.label()
                            );
                        }
                    }
                }
            }
        }
        // ── Zero or one arg → local vs server diff ─────────────────────
        // Note: (None, Some(_)) is structurally impossible via clap since
        // arg2 requires arg1 to be present first.
        (path_filter, _) => {
            // Build DiffConfig from explicit flags or the configured remote.
            let diff_config = if let Some(server_url) = from {
                let api_key = key.ok_or(remote_diff::RemoteDiffError::MissingKey)?;
                let repo_name = repo.ok_or(remote_diff::RemoteDiffError::MissingRepo)?;
                remote_diff::DiffConfig {
                    server_url,
                    api_key,
                    repo_name,
                    path_filter,
                }
            } else {
                let config = repo::read_config(&vai_dir)?;
                let remote = config.remote.ok_or(remote_diff::RemoteDiffError::NoRemote)?;
                let (api_key, _) = credentials::load_api_key()
                    .map_err(|e| CliError::Other(format!("credentials error: {e}")))?;
                remote_diff::DiffConfig {
                    server_url: remote.url,
                    api_key,
                    repo_name: config.name,
                    path_filter,
                }
            };

            let result = make_rt()?.block_on(remote_diff::compute_diff(&root, diff_config))?;
            if json {
                println!("{}", serde_json::to_string_pretty(&result).unwrap());
            } else {
                remote_diff::print_diff_result(&result);
            }
        }
    }
    Ok(())
}

/// Handle `vai status`.
pub(super) fn handle_status(others: bool, json: bool, local: bool) -> Result<(), CliError> {
    let cwd = std::env::current_dir()
        .map_err(|e| CliError::Other(format!("cannot determine working directory: {e}")))?;
    match repo::find_root(&cwd) {
        None => {
            if json {
                println!("{{\"error\":\"not inside a vai repository\"}}");
            } else {
                eprintln!("{} Not inside a vai repository (no .vai/ directory found)", "✗".red());
            }
        }
        Some(root) => {
            let vai_dir = root.join(".vai");

            // ── Remote dispatch (skip when --others or --local) ───
            if !others && !local {
                // Try cloned-repo remote first (.vai/remote.toml), then
                // the [remote] section of config.toml.
                let maybe_status_config: Option<remote_status::StatusConfig> =
                    if let Some(remote) = crate::clone::read_remote_config(&vai_dir) {
                        Some(remote_status::StatusConfig {
                            server_url: remote.server_url,
                            api_key: remote.api_key,
                            repo_name: remote.repo_name,
                        })
                    } else {
                        let cfg = repo::read_config(&vai_dir)
                            .map_err(|e| CliError::Other(format!("cannot read config: {e}")))?;
                        if let Some(remote_cfg) = cfg.remote {
                            let (api_key, _) = credentials::load_api_key()
                                .map_err(|e| CliError::Other(format!("credentials error: {e}")))?;
                            Some(remote_status::StatusConfig {
                                server_url: remote_cfg.url,
                                api_key,
                                repo_name: cfg.name,
                            })
                        } else {
                            None
                        }
                    };

                if let Some(status_config) = maybe_status_config {
                    let rt = make_rt()?;
                    let result = rt.block_on(remote_status::check_status(&root, status_config))?;
                    if json {
                        println!("{}", serde_json::to_string_pretty(&result).unwrap());
                    } else {
                        remote_status::print_status_result(&result);
                    }
                    return Ok(());
                }
            }

            // ── --others: list remote workspaces ──────────────────
            if others {
                let remote = crate::clone::read_remote_config(&vai_dir)
                    .ok_or_else(|| CliError::Other(
                        "--others requires a cloned repository with a remote server".to_string(),
                    ))?;
                let remote_workspaces = tokio::runtime::Runtime::new()
                    .map_err(|e| CliError::Other(format!("cannot create async runtime: {e}")))?
                    .block_on(remote_workspace::list_workspaces(&remote))?;

                if json {
                    println!("{}", serde_json::to_string_pretty(&remote_workspaces).unwrap());
                } else if remote_workspaces.is_empty() {
                    println!("No active workspaces on server {}.", remote.server_url.cyan());
                } else {
                    println!(
                        "Active workspaces on {} ({}):",
                        remote.server_url.cyan(),
                        remote_workspaces.len()
                    );
                    println!("{:<38}  {:<8}  Intent", "ID", "Status");
                    println!("{}", "-".repeat(80));
                    for ws in &remote_workspaces {
                        println!(
                            "{:<38}  {:<8}  {}",
                            ws.id,
                            ws.status,
                            truncate(&ws.intent, 50),
                        );
                    }
                }
                return Ok(());
            }

            let config = repo::read_config(&vai_dir)
                .map_err(|e| CliError::Other(format!("cannot read config: {e}")))?;
            let head = repo::read_head(&vai_dir)
                .map_err(|e| CliError::Other(format!("cannot read HEAD: {e}")))?;
            let head_version = version::get_version(&vai_dir, &head)?;
            let snapshot_path = vai_dir.join("graph").join("snapshot.db");
            let snapshot = GraphSnapshot::open(&snapshot_path)?;
            let graph_stats = snapshot.stats()?;
            let workspaces = workspace::list(&vai_dir)?;
            let active_id = workspace::active_id(&vai_dir);

            // Query issue counts (best-effort; zero if store doesn't exist yet).
            let issue_counts = {
                if let Ok(store) = crate::issue::IssueStore::open(&vai_dir) {
                    let open = store.list(&IssueFilter { status: Some(vec![IssueStatus::Open]), ..Default::default() }).unwrap_or_default().len();
                    let in_progress = store.list(&IssueFilter { status: Some(vec![IssueStatus::InProgress]), ..Default::default() }).unwrap_or_default().len();
                    Some((open, in_progress))
                } else {
                    None
                }
            };

            // Query pending escalation count (best-effort).
            let pending_escalations = crate::escalation::EscalationStore::open(&vai_dir)
                .ok()
                .and_then(|s| s.count_pending().ok())
                .unwrap_or(0);

            if json {
                let out = StatusOutput {
                    repo_name: config.name.clone(),
                    head_version: head_version.clone(),
                    graph_stats,
                    workspaces: workspaces.clone(),
                    pending_conflicts: 0,
                };
                println!("{}", serde_json::to_string_pretty(&out).unwrap());
            } else {
                println!("{} repository: {}", "vai".bold(), config.name.bold());
                println!(
                    "Current version: {} \"{}\"",
                    head_version.version_id.bold(),
                    head_version.intent
                );
                // Show remote connection info for cloned repos.
                if let Some(remote) = crate::clone::read_remote_config(&vai_dir) {
                    println!("Remote: {}", remote.server_url.cyan());
                    println!(
                        "  Cloned at: {}  (current: {})",
                        remote.cloned_at_version.dimmed(),
                        head_version.version_id
                    );
                }
                println!();
                print_graph_stats(&graph_stats);
                println!();
                println!("Active workspaces: {}", workspaces.len());
                for ws in &workspaces {
                    let marker = if active_id.as_deref() == Some(&ws.id.to_string()) {
                        "*"
                    } else {
                        " "
                    };
                    let age = format_age(ws.created_at);
                    println!(
                        "  {}{}  {:<30}  {:<8}  {}",
                        marker,
                        ws.id.to_string()[..8].dimmed(),
                        truncate(&ws.intent, 30),
                        ws.status.as_str(),
                        age
                    );
                }
                println!();
                if let Some((open, in_progress)) = issue_counts {
                    println!("Issues: {} open, {} in progress", open, in_progress);
                }
                if pending_escalations > 0 {
                    println!(
                        "{} {} escalation(s) require attention — run {}",
                        "!".red().bold(),
                        pending_escalations,
                        "vai escalations list".bold()
                    );
                }
                println!("Pending conflicts: 0");
            }
        }
    }
    Ok(())
}

/// Handle `vai rollback`.
pub(super) fn handle_rollback(
    version: String,
    force: bool,
    entity: Option<String>,
    json: bool,
) -> Result<(), CliError> {
    let repo_root = repo::find_root(&std::env::current_dir().unwrap())
        .ok_or_else(|| CliError::Other("not inside a vai repository".into()))?;
    let vai_dir = repo_root.join(".vai");

    // 1. Analyze impact.
    let impact = version::analyze_rollback_impact(&vai_dir, &version)
        .map_err(CliError::Version)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&impact).unwrap());
        return Ok(());
    }

    // 2. Display impact analysis.
    println!(
        "Analyzing impact of rolling back \"{}\"...",
        impact.target_version.intent.bold()
    );
    println!();

    if impact.target_changes.file_changes.is_empty()
        && impact.target_changes.entity_changes.is_empty()
    {
        println!("  No changes recorded for {version} — nothing to roll back.");
        return Ok(());
    }

    println!("Direct changes:");
    for fc in &impact.target_changes.file_changes {
        println!("  - {} ({:?})", fc.path, fc.change_type);
    }
    for ec in &impact.target_changes.entity_changes {
        if let Some(qname) = &ec.qualified_name {
            println!("  - {} {:?}", qname, ec.change_type);
        }
    }
    println!();

    if impact.downstream_impacts.is_empty() {
        println!("  No downstream dependencies affected.");
    } else {
        println!("Downstream dependencies:");
        for item in &impact.downstream_impacts {
            for entity in &item.overlapping_entities {
                println!(
                    "  {} {} \"{}\" references {}",
                    item.risk.symbol(),
                    item.version_id,
                    item.intent,
                    entity
                );
                let risk_label = match item.risk {
                    version::RiskLevel::Low => "LOW",
                    version::RiskLevel::Medium => "MEDIUM",
                    version::RiskLevel::High => "HIGH",
                };
                println!("    Risk: {risk_label}");
            }
            for file in &item.overlapping_files {
                if item.overlapping_entities.is_empty() {
                    println!(
                        "  {} {} \"{}\" modifies {}",
                        item.risk.symbol(),
                        item.version_id,
                        item.intent,
                        file
                    );
                }
            }
        }
    }
    println!();

    // 3. Confirm (unless --force).
    if !force && !impact.downstream_impacts.is_empty() {
        use std::io::{self, Write};
        print!("Proceed with rollback? [y/N] ");
        io::stdout().flush().ok();
        let mut input = String::new();
        io::stdin().read_line(&mut input).ok();
        let confirmed = matches!(input.trim().to_lowercase().as_str(), "y" | "yes");
        if !confirmed {
            println!("Rollback cancelled.");
            return Ok(());
        }
    }

    // 4. Perform rollback.
    let result = version::rollback(
        &vai_dir,
        &repo_root,
        &version,
        entity.as_deref(),
    )
    .map_err(CliError::Version)?;

    println!(
        "{} Rolled back {} → {} created",
        "✓".green().bold(),
        version.bold(),
        result.new_version.version_id.bold()
    );
    println!(
        "  Files restored: {}, files deleted: {}",
        result.files_restored, result.files_deleted
    );
    Ok(())
}
