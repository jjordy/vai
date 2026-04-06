//! Work queue command handlers.

use colored::Colorize;

use crate::conflict;
use crate::issue::IssueStore;
use crate::repo;
use crate::work_queue;

use super::{CliError, WorkQueueCommands};
use super::{json_id_short, make_rt, resolve_issue, try_remote};

/// Handle all `vai work-queue` subcommands.
pub(super) fn handle(wq_cmd: WorkQueueCommands, json: bool, local: bool) -> Result<(), CliError> {
    let cwd = std::env::current_dir()
        .map_err(|e| CliError::Other(format!("cannot determine working directory: {e}")))?;
    let root = repo::find_root(&cwd)
        .ok_or_else(|| CliError::Other("not inside a vai repository".to_string()))?;
    let vai_dir = root.join(".vai");

    // ── Remote dispatch ────────────────────────────────────────────────
    if let Some(client) = try_remote(&vai_dir, local)? {
        let rt = make_rt()?;
        match wq_cmd {
            WorkQueueCommands::List => {
                let queue: serde_json::Value = rt.block_on(client.get("/api/work-queue"))?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&queue).unwrap());
                } else {
                    let available = queue["available_work"].as_array().cloned().unwrap_or_default();
                    let blocked = queue["blocked_work"].as_array().cloned().unwrap_or_default();
                    if available.is_empty() && blocked.is_empty() {
                        println!("No open issues in the work queue.");
                    } else {
                        if !available.is_empty() {
                            println!("{}", "Available work:".green().bold());
                            println!("  {:<10}  {:<8}  TITLE", "ISSUE", "PRIORITY");
                            println!("  {}", "-".repeat(60));
                            for item in &available {
                                let id = item["issue_id"].as_str().unwrap_or("?");
                                let id_short = if id.len() >= 8 { &id[..8] } else { id };
                                println!(
                                    "  {:<10}  {:<8}  {}",
                                    id_short,
                                    item["priority"].as_str().unwrap_or(""),
                                    item["title"].as_str().unwrap_or(""),
                                );
                            }
                        }
                        if !blocked.is_empty() {
                            if !available.is_empty() { println!(); }
                            println!("{}", "Blocked work:".yellow().bold());
                            println!("  {:<10}  {:<8}  TITLE", "ISSUE", "PRIORITY");
                            println!("  {}", "-".repeat(60));
                            for item in &blocked {
                                let id = item["issue_id"].as_str().unwrap_or("?");
                                let id_short = if id.len() >= 8 { &id[..8] } else { id };
                                println!(
                                    "  {:<10}  {:<8}  {}",
                                    id_short,
                                    item["priority"].as_str().unwrap_or(""),
                                    item["title"].as_str().unwrap_or(""),
                                );
                                println!("    {}", item["reason"].as_str().unwrap_or("").dimmed());
                            }
                        }
                    }
                }
            }
            WorkQueueCommands::Claim { issue_id } => {
                let req = serde_json::json!({ "issue_id": issue_id });
                let result: serde_json::Value = rt.block_on(client.post("/api/work-queue/claim", &req))?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&result).unwrap());
                } else {
                    let issue_id_short = json_id_short(&result, "issue_id");
                    let ws_id_short = json_id_short(&result, "workspace_id");
                    println!(
                        "{} Claimed issue {} → workspace {}",
                        "✓".green(),
                        issue_id_short.cyan(),
                        ws_id_short.cyan(),
                    );
                    if let Some(intent) = result["intent"].as_str() {
                        println!("  Intent : {intent}");
                    }
                }
            }
        }
        return Ok(());
    }

    // For local CLI, create an empty conflict engine — workspace scope
    // tracking is a server-side concern when multiple agents run concurrently.
    let engine = conflict::ConflictEngine::new();

    match wq_cmd {
        WorkQueueCommands::List => {
            let queue = work_queue::compute(&vai_dir, &engine)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&queue).unwrap());
            } else if queue.available_work.is_empty() && queue.blocked_work.is_empty() {
                println!("No open issues in the work queue.");
            } else {
                if !queue.available_work.is_empty() {
                    println!("{}", "Available work:".green().bold());
                    println!(
                        "  {:<10}  {:<8}  TITLE",
                        "ISSUE", "PRIORITY"
                    );
                    println!("  {}", "-".repeat(60));
                    for item in &queue.available_work {
                        println!(
                            "  {:<10}  {:<8}  {}",
                            &item.issue_id[..8],
                            item.priority,
                            item.title,
                        );
                    }
                }
                if !queue.blocked_work.is_empty() {
                    if !queue.available_work.is_empty() {
                        println!();
                    }
                    println!("{}", "Blocked work:".yellow().bold());
                    println!(
                        "  {:<10}  {:<8}  TITLE",
                        "ISSUE", "PRIORITY"
                    );
                    println!("  {}", "-".repeat(60));
                    for item in &queue.blocked_work {
                        println!(
                            "  {:<10}  {:<8}  {}",
                            &item.issue_id[..8],
                            item.priority,
                            item.title,
                        );
                        println!("    {}", item.reason.dimmed());
                    }
                }
            }
        }
        WorkQueueCommands::Claim { issue_id } => {
            // Resolve prefix → full UUID via the issue store.
            let issue_store = IssueStore::open(&vai_dir)?;
            let issue = resolve_issue(&issue_store, &issue_id)?;
            let result = work_queue::claim(&vai_dir, issue.id, &engine)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&result).unwrap());
            } else {
                println!(
                    "{} Claimed issue {} → workspace {}",
                    "✓".green(),
                    result.issue_id[..8].cyan(),
                    result.workspace_id[..8].cyan(),
                );
                println!("  Intent : {}", result.intent);
                println!(
                    "  Scope  : {} entities across {} file(s)",
                    result.predicted_scope.blast_radius,
                    result.predicted_scope.files.len(),
                );
            }
        }
    }
    Ok(())
}
