//! Workspace command handlers.

use chrono::{DateTime, Utc};
use colored::Colorize;

use crate::diff;
use crate::event_log::EventLog;
use crate::file_workspace::{self, FileWorkspace, OpenOptions};
use crate::remote_workspace;
use crate::repo;
use crate::workspace;

use super::{CliError, WorkspaceCommands};
use super::{format_age, make_rt, resolve_issue, truncate};

/// Handle all `vai workspace` subcommands.
pub(super) fn handle(
    ws_cmd: WorkspaceCommands,
    json: bool,
    _quiet: bool,
    local: bool,
) -> Result<(), CliError> {
    let cwd = std::env::current_dir()
        .map_err(|e| CliError::Other(format!("cannot determine working directory: {e}")))?;
    let root = repo::find_root(&cwd)
        .ok_or_else(|| CliError::Other("not inside a vai repository".to_string()))?;
    let vai_dir = root.join(".vai");
    let head = repo::read_head(&vai_dir)
        .map_err(|e| CliError::Other(format!("cannot read HEAD: {e}")))?;

    match ws_cmd {
        WorkspaceCommands::Create { intent, issue } => {
            // In a cloned repo, register the workspace on the server
            // first so both sides share the same UUID.
            let mut result = if let Some(remote) = crate::clone::read_remote_config(&vai_dir) {
                let server_meta = tokio::runtime::Runtime::new()
                    .map_err(|e| CliError::Other(format!("cannot create async runtime: {e}")))?
                    .block_on(remote_workspace::register_workspace(&remote, &intent))?;

                let server_id: uuid::Uuid = server_meta
                    .id
                    .parse()
                    .map_err(|e| CliError::Other(format!("invalid server workspace ID: {e}")))?;

                workspace::create_with_id(&vai_dir, &intent, &head, server_id)?
            } else {
                workspace::create(&vai_dir, &intent, &head)?
            };

            // If --issue provided, link the workspace to the issue and transition it to InProgress.
            if let Some(issue_id_str) = &issue {
                let store = crate::issue::IssueStore::open(&vai_dir)?;
                let linked_issue = resolve_issue(&store, issue_id_str)?;
                let mut event_log = EventLog::open(&vai_dir.join("event_log"))
                    .map_err(|e| CliError::Other(format!("cannot open event log: {e}")))?;
                store.set_in_progress(linked_issue.id, result.workspace.id, &mut event_log)?;
                // Persist issue_id in workspace meta for later discard/submit hooks.
                result.workspace.issue_id = Some(linked_issue.id);
                workspace::update_meta(&vai_dir, &result.workspace)?;
            }

            if json {
                println!("{}", serde_json::to_string_pretty(&result).unwrap());
            } else {
                println!(
                    "{} Created workspace {}",
                    "✓".green(),
                    result.workspace.id.to_string().cyan()
                );
                println!("  Intent : {}", result.workspace.intent);
                println!("  Base   : {}", result.workspace.base_version);
                println!("  Path   : {}", result.path.display());
                if let Some(issue_id) = result.workspace.issue_id {
                    println!("  Issue  : {}", issue_id.to_string()[..8].cyan());
                }
            }
        }
        WorkspaceCommands::List => {
            // Proxy to remote if configured.
            if let Some(client) = super::try_remote(&vai_dir, local)? {
                let rt = make_rt()?;
                let repo_name = repo::read_config(&vai_dir).map(|c| c.name).unwrap_or_default();
                let workspaces: serde_json::Value = rt.block_on(client.get(&format!("/api/repos/{repo_name}/workspaces")))?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&workspaces).unwrap());
                } else {
                    let arr = workspaces["data"].as_array().cloned().unwrap_or_default();
                    if arr.is_empty() {
                        println!("No active workspaces.");
                    } else {
                        println!("{:<38}  {:<8}  {:<30}  Created", "ID", "Status", "Intent");
                        println!("{}", "-".repeat(100));
                        for ws in &arr {
                            let age = ws["created_at"].as_str()
                                .and_then(|s| s.parse::<DateTime<Utc>>().ok())
                                .map(format_age)
                                .unwrap_or_else(|| "?".to_string());
                            println!(
                                " {:<38}  {:<8}  {:<30}  {}",
                                ws["id"].as_str().unwrap_or(""),
                                ws["status"].as_str().unwrap_or(""),
                                truncate(ws["intent"].as_str().unwrap_or(""), 30),
                                age,
                            );
                        }
                    }
                }
                return Ok(());
            }
            let workspaces = workspace::list(&vai_dir)?;
            let active_id = workspace::active_id(&vai_dir);
            if json {
                println!("{}", serde_json::to_string_pretty(&workspaces).unwrap());
            } else if workspaces.is_empty() {
                println!("No active workspaces.");
            } else {
                println!("{:<38}  {:<8}  {:<30}  Created", "ID", "Status", "Intent");
                println!("{}", "-".repeat(100));
                for ws in &workspaces {
                    let marker = if active_id.as_deref() == Some(&ws.id.to_string()) {
                        "*"
                    } else {
                        " "
                    };
                    let age = format_age(ws.created_at);
                    println!(
                        "{}{:<38}  {:<8}  {:<30}  {}",
                        marker,
                        ws.id,
                        ws.status.as_str(),
                        truncate(&ws.intent, 30),
                        age
                    );
                }
            }
        }
        WorkspaceCommands::Switch { id } => {
            let meta = workspace::switch(&vai_dir, &id)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&meta).unwrap());
            } else {
                println!(
                    "{} Switched to workspace {}",
                    "✓".green(),
                    meta.id.to_string().cyan()
                );
                println!("  Intent : {}", meta.intent);
            }
        }
        WorkspaceCommands::Discard { id } => {
            let meta = workspace::discard(&vai_dir, &id, None)?;
            // If the workspace was linked to an issue, transition it back to Open.
            if let Some(issue_id) = meta.issue_id {
                let store = crate::issue::IssueStore::open(&vai_dir)?;
                let mut event_log = EventLog::open(&vai_dir.join("event_log"))
                    .map_err(|e| CliError::Other(format!("cannot open event log: {e}")))?;
                // Best-effort: ignore if the issue is not in a state that allows reopening.
                let _ = store.reopen(issue_id, &mut event_log);
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&meta).unwrap());
            } else {
                println!(
                    "{} Discarded workspace {}",
                    "✓".green(),
                    meta.id.to_string().cyan()
                );
            }
        }
        WorkspaceCommands::Diff { entities_only } => {
            let workspace_diff = diff::compute(&vai_dir, &root)?;

            // Record events and transition workspace to Active on first diff.
            diff::record_events(&vai_dir, &workspace_diff)?;

            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&workspace_diff).unwrap()
                );
            } else if workspace_diff.is_empty() {
                println!("No changes in active workspace.");
            } else {
                println!(
                    "{} workspace diff (base: {})",
                    "●".cyan(),
                    workspace_diff.base_version.bold()
                );

                if !entities_only && !workspace_diff.file_diffs.is_empty() {
                    println!("\n{}", "Files changed:".bold());
                    for fd in &workspace_diff.file_diffs {
                        let sigil = match fd.change_type {
                            diff::FileChangeType::Added => "+".green(),
                            diff::FileChangeType::Modified => "M".yellow(),
                            diff::FileChangeType::Deleted => "-".red(),
                        };
                        println!("  {} {} ({} lines)", sigil, fd.path, fd.lines);
                    }
                }

                if !workspace_diff.entity_changes.is_empty() {
                    println!("\n{}", "Entities changed:".bold());
                    for ec in &workspace_diff.entity_changes {
                        let (sigil, label) = match ec.change_type {
                            diff::EntityChangeType::Added => ("+".green(), "added"),
                            diff::EntityChangeType::Modified => ("~".yellow(), "modified"),
                            diff::EntityChangeType::Removed => ("-".red(), "removed"),
                        };
                        let location = if let Some((start, end)) = ec.line_range {
                            format!("{}:{}-{}", ec.file_path, start, end)
                        } else {
                            ec.file_path.clone()
                        };
                        println!(
                            "  {} {} {}  {}  {}",
                            sigil,
                            ec.kind.as_str().cyan(),
                            ec.qualified_name.bold(),
                            location,
                            label
                        );
                    }
                }
            }
        }
        WorkspaceCommands::Submit => {
            let rt = make_rt()?;
            let mut fw = rt
                .block_on(FileWorkspace::open(OpenOptions::from_root(root.clone())))
                .map_err(|e| CliError::Other(format!("workspace: {e}")))?;

            let applied = match rt.block_on(fw.submit(file_workspace::Submit::Required(String::new()))) {
                Err(file_workspace::FwError::Surprises(plan)) => {
                    return Err(CliError::Other(format!(
                        "submit refused: {} surprise(s) — run `vai workspace diff` to inspect",
                        plan.surprises.len()
                    )));
                }
                other => other.map_err(|e| CliError::Other(format!("submit: {e}")))?,
            };

            if json {
                println!("{}", serde_json::to_string_pretty(&applied).unwrap());
            } else {
                let label = if applied.snapshot.is_some() { " (server)" } else { "" };
                println!(
                    "{} Merged workspace → {}{}",
                    "✓".green().bold(),
                    applied.version.bold(),
                    label,
                );
                if !applied.intent.is_empty() {
                    println!("  Intent   : {}", applied.intent);
                }
                println!("  Files    : {}", applied.files);
                println!("  Entities : {}", applied.entities);
                if let Some(snap) = &applied.snapshot {
                    let mode = if snap.is_delta { "delta" } else { "full" };
                    println!(
                        "  Snapshot : {} added, {} modified, {} deleted ({mode} mode)",
                        snap.added, snap.modified, snap.deleted,
                    );
                }
            }
        }
    }
    Ok(())
}
