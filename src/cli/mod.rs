//! CLI command definitions and dispatch.
//!
//! Uses `clap` derive API to define all vai subcommands.
//! Each command handler lives in its own submodule.

use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use colored::Colorize;
use thiserror::Error;

use crate::diff;
use crate::graph::GraphSnapshot;
use crate::merge;
use crate::repo;
use crate::version;
use crate::workspace;

/// Errors that can occur during CLI execution.
#[derive(Debug, Error)]
pub enum CliError {
    #[error("Repository error: {0}")]
    Repo(#[from] repo::RepoError),

    #[error("Graph error: {0}")]
    Graph(#[from] crate::graph::GraphError),

    #[error("Workspace error: {0}")]
    Workspace(#[from] workspace::WorkspaceError),

    #[error("Diff error: {0}")]
    Diff(#[from] diff::DiffError),

    #[error("Merge error: {0}")]
    Merge(#[from] merge::MergeError),

    #[error("Version error: {0}")]
    Version(#[from] version::VersionError),

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
        Some(Commands::Workspace(ws_cmd)) => {
            let cwd = std::env::current_dir()
                .map_err(|e| CliError::Other(format!("cannot determine working directory: {e}")))?;
            let root = repo::find_root(&cwd)
                .ok_or_else(|| CliError::Other("not inside a vai repository".to_string()))?;
            let vai_dir = root.join(".vai");
            let head = repo::read_head(&vai_dir)
                .map_err(|e| CliError::Other(format!("cannot read HEAD: {e}")))?;

            match ws_cmd {
                WorkspaceCommands::Create { intent } => {
                    let result = workspace::create(&vai_dir, &intent, &head)?;
                    if cli.json {
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
                    }
                }
                WorkspaceCommands::List => {
                    let workspaces = workspace::list(&vai_dir)?;
                    let active_id = workspace::active_id(&vai_dir);
                    if cli.json {
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
                    if cli.json {
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
                    if cli.json {
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

                    if cli.json {
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
                    let result = merge::submit(&vai_dir, &root)?;
                    if cli.json {
                        println!("{}", serde_json::to_string_pretty(&result).unwrap());
                    } else {
                        println!(
                            "{} Merged workspace → {}",
                            "✓".green().bold(),
                            result.version.version_id.bold()
                        );
                        println!("  Intent   : {}", result.version.intent);
                        println!("  Files    : {}", result.files_applied);
                        println!("  Entities : {}", result.entities_changed);
                    }
                }
            }
        }
        Some(Commands::Log { limit }) => {
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

            if cli.json {
                println!("{}", serde_json::to_string_pretty(&versions).unwrap());
            } else if versions.is_empty() {
                println!("No versions yet.");
            } else {
                for v in &versions {
                    let age = format_age(v.created_at);
                    println!("{:<4}  {:<50}  {}", v.version_id.bold(), v.intent, age);
                }
            }
        }
        Some(Commands::Show { version: version_id }) => {
            let cwd = std::env::current_dir()
                .map_err(|e| CliError::Other(format!("cannot determine working directory: {e}")))?;
            let root = repo::find_root(&cwd)
                .ok_or_else(|| CliError::Other("not inside a vai repository".to_string()))?;
            let vai_dir = root.join(".vai");

            let changes = version::get_version_changes(&vai_dir, &version_id)?;
            let v = &changes.version;

            if cli.json {
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
        }
        Some(Commands::Diff {
            version_a,
            version_b,
        }) => {
            let cwd = std::env::current_dir()
                .map_err(|e| CliError::Other(format!("cannot determine working directory: {e}")))?;
            let root = repo::find_root(&cwd)
                .ok_or_else(|| CliError::Other("not inside a vai repository".to_string()))?;
            let vai_dir = root.join(".vai");

            let all_changes = version::get_versions_diff(&vai_dir, &version_a, &version_b)?;

            if cli.json {
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
        Some(cmd) => {
            eprintln!("Command not yet implemented: {cmd:?}");
        }
    }
    Ok(())
}

/// Truncates a string to `max` characters, appending `…` if needed.
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max - 1])
    }
}

/// Returns a human-readable age string (e.g. "5m ago", "2h ago").
fn format_age(dt: DateTime<Utc>) -> String {
    let secs = (Utc::now() - dt).num_seconds().max(0);
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    }
}
