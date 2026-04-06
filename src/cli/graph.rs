//! Graph command handlers.

use colored::Colorize;

use crate::graph::GraphSnapshot;
use crate::scope_history::ScopeHistoryStore;
use crate::scope_inference;
use crate::repo;

use super::{CliError, GraphCommands};

/// Handle all `vai graph` subcommands.
pub(super) fn handle(graph_cmd: GraphCommands, json: bool, quiet: bool) -> Result<(), CliError> {
    let cwd = std::env::current_dir()
        .map_err(|e| CliError::Other(format!("cannot determine working directory: {e}")))?;
    let root = repo::find_root(&cwd)
        .ok_or_else(|| CliError::Other("not inside a vai repository".to_string()))?;
    let snapshot_path = root.join(".vai").join("graph").join("snapshot.db");
    let snapshot = GraphSnapshot::open(&snapshot_path)?;

    match graph_cmd {
        GraphCommands::Show => {
            let stats = snapshot.stats()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&stats).unwrap());
            } else {
                println!("{}", "Semantic graph".bold());
                println!("  Entities      : {}", stats.entity_count);
                for (kind, count) in &stats.by_kind {
                    println!("    {kind:<15} {count}");
                }
                println!("  Relationships : {}", stats.relationship_count);
                println!("  Files         : {}", stats.file_count);
            }
        }
        GraphCommands::Refresh => {
            let result = repo::refresh_graph(&root)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&result).unwrap());
            } else {
                println!(
                    "{} Graph refreshed — {} files scanned",
                    "✓".green().bold(),
                    result.files_scanned
                );
                println!("  Entities      : {}", result.graph_stats.entity_count);
                for (kind, count) in &result.graph_stats.by_kind {
                    println!("    {kind:<15} {count}");
                }
                println!("  Relationships : {}", result.graph_stats.relationship_count);
                println!("  Files         : {}", result.graph_stats.file_count);
            }
        }
        GraphCommands::Query { name } => {
            let entities = snapshot.search_entities_by_name(&name)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&entities).unwrap());
            } else if entities.is_empty() {
                println!("No entities found matching {:?}", name);
            } else {
                println!("{} entities matching {:?}:", entities.len(), name);
                for e in &entities {
                    println!(
                        "  {} {} {}:{}",
                        e.kind.as_str().cyan(),
                        e.qualified_name.bold(),
                        e.file_path,
                        e.line_range.0,
                    );
                }
            }
        }
        GraphCommands::Infer { intent, hops, history } => {
            let history_path = root.join(".vai").join("graph").join("history.db");
            let result = if history {
                let hist = ScopeHistoryStore::open(&history_path)?;
                scope_inference::infer_with_history(&snapshot, &hist, &intent, hops)?
            } else {
                scope_inference::infer(&snapshot, &intent, hops)?
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&result).unwrap());
            } else {
                println!("{}", "Scope inference".bold());
                println!("  Intent : {}", result.intent);
                println!("  Terms  : {}", result.terms.join(", "));
                println!(
                    "  Found  : {} entities predicted",
                    result.predicted_scope.len()
                );
                if result.predicted_scope.is_empty() {
                    println!("\n  No matching entities found in graph.");
                } else {
                    println!();
                    for scoped in &result.predicted_scope {
                        println!(
                            "  [{conf}] {kind} {name}  {file}:{line}",
                            conf = scoped.confidence.label().cyan(),
                            kind = scoped.entity.kind.as_str(),
                            name = scoped.entity.qualified_name.bold(),
                            file = scoped.entity.file_path,
                            line = scoped.entity.line_range.0,
                        );
                        if !quiet {
                            println!("         ↳ {}", scoped.reason);
                        }
                    }
                }
                if history && !result.history_influences.is_empty() {
                    println!("\n  {}", "Historical influences:".bold());
                    for inf in &result.history_influences {
                        println!(
                            "    ({} term overlap) \"{}\"  → {} entities",
                            inf.term_overlap,
                            inf.past_intent,
                            inf.entity_ids.len(),
                        );
                    }
                }
            }
        }
        GraphCommands::Accuracy { limit } => {
            let history_path = root.join(".vai").join("graph").join("history.db");
            let hist = ScopeHistoryStore::open(&history_path)?;
            let metrics = hist.accuracy(limit)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&metrics).unwrap());
            } else {
                println!("{}", "Scope prediction accuracy".bold());
                println!("  Intents evaluated : {}", metrics.sample_count);
                if metrics.sample_count == 0 {
                    println!("  No data yet — submit some workspaces to build history.");
                } else {
                    println!("  Avg recall        : {:.1}%  (target ≥ 70%)",
                        metrics.avg_recall * 100.0);
                    println!("  Avg precision     : {:.1}%  (target ≥ 70%)",
                        metrics.avg_precision * 100.0);
                    println!("  F1 score          : {:.3}", metrics.f1_score);
                }
            }
        }
    }
    Ok(())
}
