//! Merge command handlers.

use colored::Colorize;

use crate::merge;
use crate::merge_patterns::MergePatternStore;
use crate::repo;

use super::{CliError, MergeCommands};

/// Handle all `vai merge` subcommands.
pub(super) fn handle(merge_cmd: MergeCommands, json: bool) -> Result<(), CliError> {
    let cwd = std::env::current_dir()
        .map_err(|e| CliError::Other(format!("cannot determine working directory: {e}")))?;
    let root = repo::find_root(&cwd)
        .ok_or_else(|| CliError::Other("not inside a vai repository".to_string()))?;
    let vai_dir = root.join(".vai");

    match merge_cmd {
        MergeCommands::Status => {
            let conflicts = merge::list_conflicts(&vai_dir)?;
            let pending: Vec<_> = conflicts.iter().filter(|c| !c.resolved).collect();
            let resolved_count = conflicts.len() - pending.len();

            if json {
                println!("{}", serde_json::to_string_pretty(&conflicts).unwrap());
            } else if conflicts.is_empty() {
                println!("No merge conflicts.");
            } else {
                println!(
                    "{} conflict(s): {} pending, {} resolved",
                    conflicts.len(),
                    pending.len(),
                    resolved_count
                );
                println!();
                for c in &conflicts {
                    let status_label = if c.resolved {
                        "resolved".green()
                    } else {
                        "pending".yellow()
                    };
                    let severity_label = match c.severity {
                        merge::ConflictSeverity::Low => "low".normal(),
                        merge::ConflictSeverity::Medium => "medium".yellow(),
                        merge::ConflictSeverity::High => "high".red(),
                    };
                    println!(
                        "  {} [{}] [{}]",
                        c.conflict_id.to_string()[..8].bold(),
                        severity_label,
                        status_label
                    );
                    println!("    File : {}", c.file_path);
                    println!("    Level: {}", c.merge_level);
                    println!("    Desc : {}", c.description);
                    if !c.entity_ids.is_empty() {
                        println!(
                            "    Entities: {}",
                            c.entity_ids.join(", ")
                        );
                    }
                    println!();
                }
                if !pending.is_empty() {
                    println!(
                        "Resolve with: {}",
                        "vai merge resolve <conflict-id>".bold()
                    );
                }
            }
        }
        MergeCommands::Resolve { conflict_id } => {
            let record = merge::resolve_conflict(&vai_dir, &conflict_id)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&record).unwrap());
            } else {
                println!(
                    "{} Conflict {} marked as resolved.",
                    "✓".green().bold(),
                    &record.conflict_id.to_string()[..8].bold()
                );
            }
        }
        MergeCommands::Patterns => {
            let store = MergePatternStore::open(&vai_dir)?;
            let patterns = store.list_patterns()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&patterns).unwrap());
            } else if patterns.is_empty() {
                println!("No merge patterns recorded yet.");
                println!("Patterns are built up as conflicts are resolved.");
            } else {
                println!(
                    "{} merge pattern(s) in library:",
                    patterns.len()
                );
                println!();
                for p in &patterns {
                    let auto_label = if p.auto_resolution_enabled {
                        "auto".green().bold()
                    } else if p.disabled_by_human {
                        "disabled".red()
                    } else {
                        "manual".normal()
                    };
                    println!(
                        "  [{}] {} ({})",
                        p.id.to_string().bold(),
                        p.description,
                        auto_label
                    );
                    println!(
                        "       instances: {}  success rate: {:.0}%  {}",
                        p.instance_count,
                        p.success_rate() * 100.0,
                        if p.instance_count <= 10 {
                            format!("({} more needed for promotion)", 11 - p.instance_count)
                        } else {
                            String::new()
                        }
                    );
                }
                println!();
                println!(
                    "Disable auto-resolution with: {}",
                    "vai merge patterns-disable <id>".bold()
                );
            }
        }
        MergeCommands::PatternsDisable { pattern_id } => {
            let mut store = MergePatternStore::open(&vai_dir)?;
            let pattern = store.disable_pattern(pattern_id)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&pattern).unwrap());
            } else {
                println!(
                    "{} Pattern {} ({}) auto-resolution disabled.",
                    "✓".green().bold(),
                    pattern.id,
                    pattern.description
                );
            }
        }
        MergeCommands::PatternsEnable { pattern_id } => {
            let mut store = MergePatternStore::open(&vai_dir)?;
            let pattern = store.enable_pattern(pattern_id)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&pattern).unwrap());
            } else {
                let auto_label = if pattern.auto_resolution_enabled {
                    "now auto-resolving".green()
                } else {
                    "manual (does not yet meet promotion criteria)".normal()
                };
                println!(
                    "{} Pattern {} ({}) re-enabled — {}.",
                    "✓".green().bold(),
                    pattern.id,
                    pattern.description,
                    auto_label
                );
            }
        }
    }
    Ok(())
}
