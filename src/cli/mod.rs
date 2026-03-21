//! CLI command definitions and dispatch.
//!
//! Uses `clap` derive API to define all vai subcommands.
//! Each command handler lives in its own submodule.

use clap::{Parser, Subcommand};
use colored::Colorize;
use thiserror::Error;

use crate::graph::GraphSnapshot;
use crate::repo;

/// Errors that can occur during CLI execution.
#[derive(Debug, Error)]
pub enum CliError {
    #[error("Repository error: {0}")]
    Repo(#[from] repo::RepoError),

    #[error("Graph error: {0}")]
    Graph(#[from] crate::graph::GraphError),

    #[error("{0}")]
    Other(String),
}

/// vai — version control for AI agents.
#[derive(Debug, Parser)]
#[command(name = "vai", version, about = "Version control for AI agents")]
pub struct Cli {
    /// Output machine-readable JSON instead of human-readable text.
    #[arg(long, global = true)]
    pub json: bool,

    /// Suppress non-essential output.
    #[arg(long, short = 'q', global = true)]
    pub quiet: bool,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

/// Top-level vai subcommands.
#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Initialize a new vai repository in the current directory.
    Init,
    /// Show repository status, active workspaces, and graph stats.
    Status,
    /// Manage workspaces.
    #[command(subcommand)]
    Workspace(WorkspaceCommands),
    /// Manage merges and resolve conflicts.
    #[command(subcommand)]
    Merge(MergeCommands),
    /// Show version history.
    Log {
        /// Limit the number of versions shown.
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Show details of a specific version.
    Show {
        /// Version identifier (e.g., v2).
        version: String,
    },
    /// Roll back to a previous version.
    Rollback {
        /// Version identifier to roll back to.
        version: String,
        /// Skip confirmation prompt.
        #[arg(long)]
        force: bool,
        /// Roll back only a specific entity.
        #[arg(long)]
        entity: Option<String>,
    },
    /// Show semantic diff between two versions.
    Diff {
        /// First version.
        version_a: String,
        /// Second version.
        version_b: String,
    },
    /// Query and inspect the semantic graph.
    #[command(subcommand)]
    Graph(GraphCommands),
}

/// Workspace subcommands.
#[derive(Debug, Subcommand)]
pub enum WorkspaceCommands {
    /// Create a new workspace with a stated intent.
    Create {
        /// The intent describing what this workspace is for.
        #[arg(long)]
        intent: String,
    },
    /// List all active workspaces.
    List,
    /// Switch to a workspace.
    Switch {
        /// Workspace ID.
        id: String,
    },
    /// Show changes in the current workspace.
    Diff {
        /// Show only entity-level changes.
        #[arg(long)]
        entities_only: bool,
    },
    /// Submit the current workspace for merging.
    Submit,
    /// Discard a workspace and remove its files.
    Discard {
        /// Workspace ID.
        id: String,
    },
}

/// Merge subcommands.
#[derive(Debug, Subcommand)]
pub enum MergeCommands {
    /// Show pending merges and conflicts.
    Status,
    /// Mark a conflict as resolved.
    Resolve {
        /// Conflict ID.
        conflict_id: String,
    },
}

/// Graph subcommands.
#[derive(Debug, Subcommand)]
pub enum GraphCommands {
    /// Display graph statistics.
    Show,
    /// Search for entities by name.
    Query {
        /// Entity name to search for.
        name: String,
    },
}

/// Execute a parsed CLI command.
pub fn execute(cli: Cli) -> Result<(), CliError> {
    match cli.command {
        None => {
            println!("vai — version control for AI agents");
            println!("Run `vai --help` for usage.");
        }
        Some(Commands::Init) => {
            let cwd = std::env::current_dir()
                .map_err(|e| CliError::Other(format!("cannot determine working directory: {e}")))?;
            let result = repo::init(&cwd)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&result).unwrap());
            } else {
                repo::print_init_result(&result);
            }
        }
        Some(Commands::Graph(graph_cmd)) => {
            let cwd = std::env::current_dir()
                .map_err(|e| CliError::Other(format!("cannot determine working directory: {e}")))?;
            let root = repo::find_root(&cwd)
                .ok_or_else(|| CliError::Other("not inside a vai repository".to_string()))?;
            let snapshot_path = root.join(".vai").join("graph").join("snapshot.db");
            let snapshot = GraphSnapshot::open(&snapshot_path)?;

            match graph_cmd {
                GraphCommands::Show => {
                    let stats = snapshot.stats()?;
                    if cli.json {
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
                GraphCommands::Query { name } => {
                    let entities = snapshot.search_entities_by_name(&name)?;
                    if cli.json {
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
            }
        }
        Some(cmd) => {
            eprintln!("Command not yet implemented: {cmd:?}");
        }
    }
    Ok(())
}
