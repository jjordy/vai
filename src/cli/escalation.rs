//! Escalation command handlers.

use colored::Colorize;

use crate::escalation::{EscalationStatus, EscalationStore, ResolutionOption};
use crate::event_log::EventLog;
use crate::repo;

use super::{CliError, EscalationCommands};
use super::{colorize_severity, format_age, resolve_escalation, truncate};

/// Handle all `vai escalations` subcommands.
pub(super) fn handle(esc_cmd: EscalationCommands, json: bool) -> Result<(), CliError> {
    let cwd = std::env::current_dir()
        .map_err(|e| CliError::Other(format!("cannot determine working directory: {e}")))?;
    let root = repo::find_root(&cwd)
        .ok_or_else(|| CliError::Other("not inside a vai repository".to_string()))?;
    let vai_dir = root.join(".vai");

    match esc_cmd {
        EscalationCommands::List { all } => {
            let store = EscalationStore::open(&vai_dir)?;
            let status_filter = if all {
                None
            } else {
                Some(EscalationStatus::Pending)
            };
            let escalations = store.list(status_filter.as_ref())?;
            if json {
                println!("{}", serde_json::to_string_pretty(&escalations).unwrap());
            } else if escalations.is_empty() {
                if all {
                    println!("No escalations.");
                } else {
                    println!("No pending escalations.");
                }
            } else {
                println!(
                    "{:<10}  {:<10}  {:<8}  {:<30}  CREATED",
                    "ID", "TYPE", "SEVERITY", "SUMMARY"
                );
                println!("{}", "-".repeat(80));
                for e in &escalations {
                    let age = format_age(e.created_at);
                    let status_marker = if e.is_pending() {
                        "●".yellow()
                    } else {
                        "✓".green()
                    };
                    println!(
                        "{} {:<10}  {:<10}  {:<8}  {:<30}  {}",
                        status_marker,
                        &e.id.to_string()[..8],
                        e.escalation_type.as_str(),
                        e.severity.as_str(),
                        truncate(&e.summary, 30),
                        age,
                    );
                }
                let pending = escalations.iter().filter(|e| e.is_pending()).count();
                if pending > 0 {
                    println!();
                    println!(
                        "{} escalation(s) need attention. Use {} to resolve.",
                        pending,
                        "vai escalations resolve <id> --option <opt>".bold()
                    );
                }
            }
        }
        EscalationCommands::Show { id } => {
            let store = EscalationStore::open(&vai_dir)?;
            let e = resolve_escalation(&store, &id)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&e).unwrap());
            } else {
                let status_color = if e.is_pending() {
                    e.status.as_str().yellow()
                } else {
                    e.status.as_str().green()
                };
                println!("{} {}", "Escalation".bold(), e.id.to_string().cyan());
                println!("  Type      : {}", e.escalation_type.as_str());
                println!("  Severity  : {}", colorize_severity(&e.severity));
                println!("  Status    : {}", status_color);
                println!("  Created   : {}", e.created_at.format("%Y-%m-%d %H:%M UTC"));
                println!();
                println!("Summary: {}", e.summary.bold());
                if !e.intents.is_empty() {
                    println!();
                    println!("Intents involved:");
                    for (i, intent) in e.intents.iter().enumerate() {
                        println!("  {}. {}", i + 1, intent);
                    }
                }
                if !e.agents.is_empty() {
                    println!();
                    println!("Agents: {}", e.agents.join(", "));
                }
                if !e.workspace_ids.is_empty() {
                    println!();
                    println!("Workspaces:");
                    for ws in &e.workspace_ids {
                        println!("  {}", ws);
                    }
                }
                if !e.affected_entities.is_empty() {
                    println!();
                    println!("Affected entities:");
                    for ent in &e.affected_entities {
                        println!("  {}", ent);
                    }
                }
                if e.is_pending() && !e.resolution_options.is_empty() {
                    println!();
                    println!("Resolution options:");
                    for (i, opt) in e.resolution_options.iter().enumerate() {
                        println!("  {}. {} ({})", i + 1, opt.label(), opt.as_str());
                    }
                    println!();
                    println!(
                        "Resolve with: {}",
                        format!(
                            "vai escalations resolve {} --option <option>",
                            &e.id.to_string()[..8]
                        )
                        .bold()
                    );
                } else if let Some(res) = &e.resolution {
                    println!();
                    println!("Resolution : {}", res.label());
                    if let Some(by) = &e.resolved_by {
                        println!("Resolved by: {}", by);
                    }
                    if let Some(at) = e.resolved_at {
                        println!("Resolved at: {}", at.format("%Y-%m-%d %H:%M UTC"));
                    }
                }
            }
        }
        EscalationCommands::Resolve { id, resolution, by } => {
            let opt = ResolutionOption::from_db_str(&resolution).ok_or_else(|| {
                CliError::Other(format!(
                    "unknown resolution `{resolution}`; valid values: keep_agent_a, \
                     keep_agent_b, send_back_to_agent_a, send_back_to_agent_b, \
                     pause_both"
                ))
            })?;
            let store = EscalationStore::open(&vai_dir)?;
            let e = resolve_escalation(&store, &id)?;
            let mut event_log = EventLog::open(&vai_dir)
                .map_err(|e| CliError::Other(format!("cannot open event log: {e}")))?;
            let resolved = store.resolve(e.id, opt, by.clone(), &mut event_log)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&resolved).unwrap());
            } else {
                println!(
                    "{} Resolved escalation {} — {}",
                    "✓".green(),
                    &resolved.id.to_string()[..8].cyan(),
                    resolved
                        .resolution
                        .as_ref()
                        .map(|r| r.label())
                        .unwrap_or(""),
                );
            }
        }
    }
    Ok(())
}
