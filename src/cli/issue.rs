//! Issue command handlers.

use colored::Colorize;

use crate::event_log::EventLog;
use crate::issue::{IssueFilter, IssueStore, IssuePriority, IssueStatus};
use crate::repo;

use super::{CliError, IssueCommands};
use super::{colorize_priority, colorize_status, format_age, json_id_short, make_rt,
            print_issue_summary, resolve_issue, truncate, try_remote, whoami};

/// Handle all `vai issue` subcommands.
pub(super) fn handle(issue_cmd: IssueCommands, json: bool, local: bool) -> Result<(), CliError> {
    let cwd = std::env::current_dir()
        .map_err(|e| CliError::Other(format!("cannot determine working directory: {e}")))?;
    let root = repo::find_root(&cwd)
        .ok_or_else(|| CliError::Other("not inside a vai repository".to_string()))?;
    let vai_dir = root.join(".vai");

    // ── Remote dispatch ────────────────────────────────────────────────
    if let Some(client) = try_remote(&vai_dir, local)? {
        let rt = make_rt()?;
        let repo_name = repo::read_config(&vai_dir)
            .map(|c| c.name)
            .unwrap_or_default();
        match issue_cmd {
            IssueCommands::List { status, priority, label, created_by, blocked_by } => {
                let mut params: Vec<String> = vec![];
                if let Some(s) = status { params.push(format!("status={s}")); }
                if let Some(p) = priority { params.push(format!("priority={p}")); }
                if let Some(l) = label { params.push(format!("label={l}")); }
                if let Some(c) = created_by { params.push(format!("created_by={c}")); }
                if let Some(b) = blocked_by { params.push(format!("blocked_by={b}")); }
                let path = if params.is_empty() {
                    format!("/api/repos/{repo_name}/issues")
                } else {
                    format!("/api/repos/{repo_name}/issues?{}", params.join("&"))
                };
                let issues: serde_json::Value = rt.block_on(client.get(&path))?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&issues).unwrap());
                } else {
                    let arr = issues.as_array().cloned().unwrap_or_default();
                    if arr.is_empty() {
                        println!("No issues found.");
                    } else {
                        println!("{:<10}  {:<11}  {:<8}  {:<30}  CREATED", "ID", "STATUS", "PRIORITY", "TITLE");
                        println!("{}", "-".repeat(85));
                        for issue in &arr {
                            let id_short = json_id_short(issue, "id");
                            let age = issue["created_at"].as_str()
                                .and_then(|s| s.parse::<chrono::DateTime<chrono::Utc>>().ok())
                                .map(format_age)
                                .unwrap_or_else(|| "?".to_string());
                            println!(
                                "{:<10}  {:<11}  {:<8}  {:<30}  {}",
                                id_short,
                                issue["status"].as_str().unwrap_or("?"),
                                issue["priority"].as_str().unwrap_or("?"),
                                truncate(issue["title"].as_str().unwrap_or(""), 30),
                                age,
                            );
                        }
                    }
                }
            }
            IssueCommands::Create { title, body, priority, label, blocked_by } => {
                let labels: Vec<String> = label.iter()
                    .flat_map(|s| s.split(','))
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                let req = serde_json::json!({
                    "title": title,
                    "description": body,
                    "priority": priority,
                    "labels": labels,
                    "blocked_by": blocked_by,
                });
                let issue: serde_json::Value = rt.block_on(client.post(&format!("/api/repos/{repo_name}/issues"), &req))?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&issue).unwrap());
                } else {
                    println!("{} Created issue {}", "✓".green(), json_id_short(&issue, "id").cyan());
                    println!("  Title    : {}", issue["title"].as_str().unwrap_or(""));
                    println!("  Status   : {}", issue["status"].as_str().unwrap_or(""));
                    println!("  Priority : {}", issue["priority"].as_str().unwrap_or(""));
                }
            }
            IssueCommands::Show { id } => {
                let path = format!("/api/repos/{repo_name}/issues/{id}");
                let issue: serde_json::Value = rt.block_on(client.get(&path))?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&issue).unwrap());
                } else {
                    println!("{} {}", "Issue".bold(), issue["id"].as_str().unwrap_or("").cyan());
                    println!("  Title       : {}", issue["title"].as_str().unwrap_or("").bold());
                    println!("  Status      : {}", issue["status"].as_str().unwrap_or(""));
                    println!("  Priority    : {}", issue["priority"].as_str().unwrap_or(""));
                    let labels: Vec<&str> = issue["labels"].as_array()
                        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                        .unwrap_or_default();
                    if !labels.is_empty() {
                        println!("  Labels      : {}", labels.join(", "));
                    }
                    println!("  Creator     : {}", issue["creator"].as_str().unwrap_or(""));
                    if let Some(res) = issue["resolution"].as_str() {
                        println!("  Resolution  : {res}");
                    }
                    println!("  Created     : {}", issue["created_at"].as_str().unwrap_or(""));
                    println!("  Updated     : {}", issue["updated_at"].as_str().unwrap_or(""));
                    let desc = issue["description"].as_str().unwrap_or("");
                    if !desc.is_empty() {
                        println!();
                        println!("{desc}");
                    }
                    let ws_ids: Vec<&str> = issue["linked_workspace_ids"].as_array()
                        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                        .unwrap_or_default();
                    if !ws_ids.is_empty() {
                        println!();
                        println!("Linked workspaces:");
                        for ws_id in ws_ids {
                            println!("  {ws_id}");
                        }
                    }
                }
            }
            IssueCommands::Update { id, priority, label, title, body, blocked_by } => {
                let new_labels: Option<Vec<String>> = if label.is_empty() {
                    None
                } else {
                    Some(label.iter()
                        .flat_map(|s| s.split(','))
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect())
                };
                let blocked_by_opt: Option<Vec<String>> = if blocked_by.is_empty() {
                    None
                } else {
                    Some(blocked_by)
                };
                let req = serde_json::json!({
                    "priority": priority,
                    "title": title,
                    "description": body,
                    "labels": new_labels,
                    "blocked_by": blocked_by_opt,
                });
                let path = format!("/api/repos/{repo_name}/issues/{id}");
                let issue: serde_json::Value = rt.block_on(client.patch(&path, &req))?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&issue).unwrap());
                } else {
                    println!("{} Updated issue {}", "✓".green(), json_id_short(&issue, "id").cyan());
                    println!("  Title    : {}", issue["title"].as_str().unwrap_or(""));
                    println!("  Status   : {}", issue["status"].as_str().unwrap_or(""));
                    println!("  Priority : {}", issue["priority"].as_str().unwrap_or(""));
                }
            }
            IssueCommands::Close { id, resolution } => {
                let req = serde_json::json!({ "resolution": resolution });
                let path = format!("/api/repos/{repo_name}/issues/{id}/close");
                let issue: serde_json::Value = rt.block_on(client.post(&path, &req))?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&issue).unwrap());
                } else {
                    println!(
                        "{} Closed issue {} ({})",
                        "✓".green(),
                        json_id_short(&issue, "id").cyan(),
                        issue["resolution"].as_str().unwrap_or(""),
                    );
                }
            }
        }
        return Ok(());
    }
    // ── Local dispatch ─────────────────────────────────────────────────

    match issue_cmd {
        IssueCommands::Create { title, body, priority, label, blocked_by } => {
            let prio = IssuePriority::from_db_str(&priority)
                .ok_or_else(|| CliError::Other(format!("unknown priority: {priority}")))?;
            // Expand comma-separated label values (e.g. --label "a,b" or --label a --label b).
            let labels: Vec<String> = label
                .iter()
                .flat_map(|s| s.split(','))
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            let store = IssueStore::open(&vai_dir)?;
            let mut event_log = EventLog::open(&vai_dir)
                .map_err(|e| CliError::Other(format!("cannot open event log: {e}")))?;
            let issue = store.create(
                title,
                body,
                prio,
                labels,
                whoami(),
                &mut event_log,
            )?;
            for blocker_str in &blocked_by {
                let blocker = resolve_issue(&store, blocker_str)?;
                store.add_dependency(issue.id, blocker.id)?;
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&issue).unwrap());
            } else {
                println!(
                    "{} Created issue {}",
                    "✓".green(),
                    issue.id.to_string()[..8].cyan()
                );
                print_issue_summary(&issue);
            }
        }
        IssueCommands::List { status, priority, label, created_by, blocked_by } => {
            let status_filter = if let Some(s) = status {
                Some(IssueStatus::from_db_str(&s)
                    .ok_or_else(|| CliError::Other(format!("unknown status: {s}")))?)
            } else {
                None
            };
            let priority_filter = if let Some(p) = priority {
                Some(IssuePriority::from_db_str(&p)
                    .ok_or_else(|| CliError::Other(format!("unknown priority: {p}")))?)
            } else {
                None
            };
            let store = IssueStore::open(&vai_dir)?;
            let blocked_by_id = if let Some(ref b) = blocked_by {
                let blocker = resolve_issue(&store, b)?;
                Some(blocker.id)
            } else {
                None
            };
            let filter = IssueFilter {
                status: status_filter,
                priority: priority_filter,
                label,
                creator: created_by,
                blocked_by: blocked_by_id,
            };
            let issues = store.list(&filter)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&issues).unwrap());
            } else if issues.is_empty() {
                println!("No issues found.");
            } else {
                println!(
                    "{:<10}  {:<8}  {:<8}  {:<30}  CREATED",
                    "ID", "STATUS", "PRIORITY", "TITLE"
                );
                println!("{}", "-".repeat(80));
                for issue in &issues {
                    let age = format_age(issue.created_at);
                    println!(
                        "{:<10}  {:<8}  {:<8}  {:<30}  {}",
                        &issue.id.to_string()[..8],
                        issue.status.as_str(),
                        issue.priority.as_str(),
                        truncate(&issue.title, 30),
                        age,
                    );
                }
            }
        }
        IssueCommands::Show { id } => {
            let store = IssueStore::open(&vai_dir)?;
            let issue = resolve_issue(&store, &id)?;
            if json {
                let workspaces = store.linked_workspaces(issue.id)?;
                let out = serde_json::json!({
                    "issue": &issue,
                    "linked_workspaces": workspaces,
                });
                println!("{}", serde_json::to_string_pretty(&out).unwrap());
            } else {
                println!("{} {}", "Issue".bold(), issue.id.to_string().cyan());
                println!("  Title       : {}", issue.title.bold());
                println!("  Status      : {}", colorize_status(&issue.status));
                println!("  Priority    : {}", colorize_priority(&issue.priority));
                if !issue.labels.is_empty() {
                    println!("  Labels      : {}", issue.labels.join(", "));
                }
                println!("  Creator     : {}", issue.creator);
                if let Some(res) = &issue.resolution {
                    println!("  Resolution  : {}", res);
                }
                println!("  Created     : {}", issue.created_at.format("%Y-%m-%d %H:%M UTC"));
                println!("  Updated     : {}", issue.updated_at.format("%Y-%m-%d %H:%M UTC"));
                if !issue.description.is_empty() {
                    println!();
                    println!("{}", issue.description);
                }
                let workspaces = store.linked_workspaces(issue.id)?;
                if !workspaces.is_empty() {
                    println!();
                    println!("Linked workspaces:");
                    for ws_id in &workspaces {
                        println!("  {}", ws_id);
                    }
                }
            }
        }
        IssueCommands::Update { id, priority, label, title, body, blocked_by } => {
            let prio = if let Some(p) = priority {
                Some(IssuePriority::from_db_str(&p)
                    .ok_or_else(|| CliError::Other(format!("unknown priority: {p}")))?)
            } else {
                None
            };
            let new_labels = if label.is_empty() { None } else { Some(label) };
            let store = IssueStore::open(&vai_dir)?;
            let mut event_log = EventLog::open(&vai_dir)
                .map_err(|e| CliError::Other(format!("cannot open event log: {e}")))?;
            let resolved = resolve_issue(&store, &id)?;
            let updated = store.update(
                resolved.id,
                title,
                body,
                prio,
                new_labels,
                &mut event_log,
            )?;
            for blocker_str in &blocked_by {
                let blocker = resolve_issue(&store, blocker_str)?;
                store.add_dependency(updated.id, blocker.id)?;
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&updated).unwrap());
            } else {
                println!(
                    "{} Updated issue {}",
                    "✓".green(),
                    &updated.id.to_string()[..8].cyan()
                );
                print_issue_summary(&updated);
            }
        }
        IssueCommands::Close { id, resolution } => {
            let store = IssueStore::open(&vai_dir)?;
            let mut event_log = EventLog::open(&vai_dir)
                .map_err(|e| CliError::Other(format!("cannot open event log: {e}")))?;
            let resolved_issue = resolve_issue(&store, &id)?;
            let closed = store.close(resolved_issue.id, &resolution, &mut event_log)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&closed).unwrap());
            } else {
                println!(
                    "{} Closed issue {} ({})",
                    "✓".green(),
                    &closed.id.to_string()[..8].cyan(),
                    closed.resolution.as_deref().unwrap_or(""),
                );
            }
        }
    }
    Ok(())
}
