//! CLI command definitions and dispatch.
//!
//! Uses `clap` derive API to define all vai subcommands.
//! Each command handler lives in its own submodule.

use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use colored::Colorize;
use serde::Serialize;
use thiserror::Error;

use crate::auth;
use crate::clone as remote_clone;
use crate::diff;
use crate::event_log::EventLog;
use crate::graph::{GraphSnapshot, GraphStats};
use crate::issue::{IssueFilter, IssueStore, IssuePriority, IssueResolution, IssueStatus};
use crate::merge;
use crate::remote_workspace;
use crate::repo;
use crate::server;
use crate::sync as remote_sync;
use crate::version::VersionMeta;
use crate::version;
use crate::workspace;
use crate::workspace::WorkspaceMeta;

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

    #[error("Server error: {0}")]
    Server(#[from] server::ServerError),

    #[error("Auth error: {0}")]
    Auth(#[from] auth::AuthError),

    #[error("Clone error: {0}")]
    Clone(#[from] remote_clone::CloneError),

    #[error("Sync error: {0}")]
    Sync(#[from] remote_sync::SyncError),

    #[error("Remote workspace error: {0}")]
    RemoteWorkspace(#[from] remote_workspace::RemoteWorkspaceError),

    #[error("Issue error: {0}")]
    Issue(#[from] crate::issue::IssueError),

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
    Status {
        /// Query the remote server for other agents' active workspaces.
        /// Only valid in a cloned repository.
        #[arg(long)]
        others: bool,
    },
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
    /// Manage the vai HTTP server.
    #[command(subcommand)]
    Server(ServerCommands),
    /// Clone a remote vai repository.
    Clone {
        /// Remote URL in the form `vai://<host>:<port>/<repo>`.
        url: String,
        /// Local directory to clone into (defaults to the repo name).
        dest: Option<String>,
        /// API key for authenticating with the remote server.
        #[arg(long)]
        key: String,
    },
    /// Pull the latest changes from the remote server (incremental).
    Sync,
    /// Manage issues.
    #[command(subcommand)]
    Issue(IssueCommands),
}

/// Issue management subcommands.
#[derive(Debug, Subcommand)]
pub enum IssueCommands {
    /// Create a new issue.
    Create {
        /// Short summary of the issue.
        #[arg(long)]
        title: String,
        /// Full description (optional).
        #[arg(long, default_value = "")]
        description: String,
        /// Priority level: critical, high, medium, low.
        #[arg(long, default_value = "medium")]
        priority: String,
        /// Comma-separated labels (optional).
        #[arg(long)]
        label: Vec<String>,
    },
    /// List issues with optional filters.
    List {
        /// Filter by status: open, in_progress, resolved, closed.
        #[arg(long)]
        status: Option<String>,
        /// Filter by priority: critical, high, medium, low.
        #[arg(long)]
        priority: Option<String>,
        /// Filter by label substring.
        #[arg(long)]
        label: Option<String>,
        /// Filter by creator (human username or agent ID).
        #[arg(long)]
        created_by: Option<String>,
    },
    /// Show details of a specific issue.
    Show {
        /// Issue ID (full UUID or prefix).
        id: String,
    },
    /// Update mutable fields of an issue.
    Update {
        /// Issue ID.
        id: String,
        /// New priority level.
        #[arg(long)]
        priority: Option<String>,
        /// Add a label (can be repeated).
        #[arg(long)]
        label: Vec<String>,
        /// New title.
        #[arg(long)]
        title: Option<String>,
    },
    /// Close an issue with a resolution.
    Close {
        /// Issue ID.
        id: String,
        /// Resolution: resolved, wontfix, duplicate.
        #[arg(long)]
        resolution: String,
    },
}

/// Workspace subcommands.
#[derive(Debug, Subcommand)]
pub enum WorkspaceCommands {
    /// Create a new workspace with a stated intent.
    Create {
        /// The intent describing what this workspace is for.
        #[arg(long)]
        intent: String,
        /// Optional issue ID (full UUID or 8-char prefix) this workspace addresses.
        #[arg(long)]
        issue: Option<String>,
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

/// Server management subcommands.
#[derive(Debug, Subcommand)]
pub enum ServerCommands {
    /// Start the vai HTTP server for this repository.
    Start {
        /// Port to listen on.
        #[arg(long, default_value = "7832")]
        port: u16,
        /// Address to bind to.
        #[arg(long, default_value = "127.0.0.1")]
        bind: String,
    },
    /// Manage API keys for server authentication.
    #[command(subcommand)]
    Keys(KeysCommands),
}

/// API key management subcommands.
#[derive(Debug, Subcommand)]
pub enum KeysCommands {
    /// Generate a new API key. The key is printed once and cannot be recovered.
    Create {
        /// Human-readable name for this key (must be unique).
        #[arg(long)]
        name: String,
    },
    /// List all API keys (active and revoked).
    List,
    /// Revoke an API key by name.
    Revoke {
        /// Name of the key to revoke.
        name: String,
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
                WorkspaceCommands::Create { intent, issue } => {
                    // In a cloned repo, register the workspace on the server
                    // first so both sides share the same UUID.
                    let mut result = if let Some(remote) = remote_clone::read_remote_config(&vai_dir) {
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
                        if let Some(issue_id) = result.workspace.issue_id {
                            println!("  Issue  : {}", issue_id.to_string()[..8].cyan());
                        }
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
                    // If the workspace was linked to an issue, transition it back to Open.
                    if let Some(issue_id) = meta.issue_id {
                        let store = crate::issue::IssueStore::open(&vai_dir)?;
                        let mut event_log = EventLog::open(&vai_dir.join("event_log"))
                            .map_err(|e| CliError::Other(format!("cannot open event log: {e}")))?;
                        // Best-effort: ignore if the issue is not in a state that allows reopening.
                        let _ = store.reopen(issue_id, &mut event_log);
                    }
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
                    if let Some(remote) = remote_clone::read_remote_config(&vai_dir) {
                        // ── Remote submit path ─────────────────────────────
                        // 1. Determine active workspace ID and capture issue link.
                        let active_id = workspace::active_id(&vai_dir)
                            .ok_or_else(|| CliError::Other("no active workspace".to_string()))?;
                        let active_ws_meta = workspace::get(&vai_dir, &active_id)?;
                        let linked_issue_id = active_ws_meta.issue_id;
                        let overlay = workspace::overlay_dir(&vai_dir, &active_id);

                        // 2. Upload overlay files to server workspace.
                        let rt = tokio::runtime::Runtime::new()
                            .map_err(|e| CliError::Other(format!("cannot create async runtime: {e}")))?;

                        let uploaded = rt.block_on(remote_workspace::upload_overlay_files(
                            &remote,
                            &active_id,
                            &overlay,
                        ))?;

                        if !cli.quiet && !cli.json {
                            if uploaded.is_empty() {
                                println!("No files in workspace overlay — nothing to upload.");
                            } else {
                                println!("  Uploaded : {} file(s)", uploaded.len());
                            }
                        }

                        // 3. Trigger server-side merge.
                        let submit_result = rt
                            .block_on(remote_workspace::submit_workspace(&remote, &active_id))
                            .map_err(|e| match e {
                                remote_workspace::RemoteWorkspaceError::MergeConflict(body) => {
                                    CliError::Other(format!("merge conflict on server: {body}"))
                                }
                                other => CliError::RemoteWorkspace(other),
                            })?;

                        // 4. Update local HEAD to match the new server version.
                        std::fs::write(
                            vai_dir.join("head"),
                            format!("{}\n", submit_result.version),
                        )
                        .map_err(|e| CliError::Other(format!("update local HEAD: {e}")))?;

                        // 5. Mark local workspace as submitted.
                        let mut meta = workspace::get(&vai_dir, &active_id)?;
                        meta.status = workspace::WorkspaceStatus::Submitted;
                        meta.updated_at = chrono::Utc::now();
                        workspace::update_meta(&vai_dir, &meta)?;

                        // 6. If linked to an issue, resolve it.
                        if let Some(issue_id) = linked_issue_id {
                            let store = crate::issue::IssueStore::open(&vai_dir)?;
                            let mut event_log = EventLog::open(&vai_dir.join("event_log"))
                                .map_err(|e| CliError::Other(format!("cannot open event log: {e}")))?;
                            let _ = store.resolve(issue_id, Some(submit_result.version.clone()), &mut event_log);
                        }

                        if cli.json {
                            println!("{}", serde_json::to_string_pretty(&submit_result).unwrap());
                        } else {
                            println!(
                                "{} Merged workspace → {} (server)",
                                "✓".green().bold(),
                                submit_result.version.bold()
                            );
                            println!("  Files    : {}", submit_result.files_applied);
                            println!("  Entities : {}", submit_result.entities_changed);
                        }
                    } else {
                        // ── Local submit path ──────────────────────────────
                        // Capture linked issue before submit (workspace meta still accessible after).
                        let active_ws_meta = workspace::active(&vai_dir)?;
                        let linked_issue_id = active_ws_meta.issue_id;

                        let result = merge::submit(&vai_dir, &root)?;

                        // If linked to an issue, resolve it now that the workspace merged.
                        if let Some(issue_id) = linked_issue_id {
                            let store = crate::issue::IssueStore::open(&vai_dir)?;
                            let mut event_log = EventLog::open(&vai_dir.join("event_log"))
                                .map_err(|e| CliError::Other(format!("cannot open event log: {e}")))?;
                            let _ = store.resolve(issue_id, Some(result.version.version_id.clone()), &mut event_log);
                        }

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
        Some(Commands::Status { others }) => {
            let cwd = std::env::current_dir()
                .map_err(|e| CliError::Other(format!("cannot determine working directory: {e}")))?;
            match repo::find_root(&cwd) {
                None => {
                    if cli.json {
                        println!("{{\"error\":\"not inside a vai repository\"}}");
                    } else {
                        eprintln!("{} Not inside a vai repository (no .vai/ directory found)", "✗".red());
                    }
                }
                Some(root) => {
                    let vai_dir = root.join(".vai");

                    // ── --others: list remote workspaces ──────────────────
                    if others {
                        let remote = remote_clone::read_remote_config(&vai_dir)
                            .ok_or_else(|| CliError::Other(
                                "--others requires a cloned repository with a remote server".to_string(),
                            ))?;
                        let remote_workspaces = tokio::runtime::Runtime::new()
                            .map_err(|e| CliError::Other(format!("cannot create async runtime: {e}")))?
                            .block_on(remote_workspace::list_workspaces(&remote))?;

                        if cli.json {
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
                            let open = store.list(&IssueFilter { status: Some(IssueStatus::Open), ..Default::default() }).unwrap_or_default().len();
                            let in_progress = store.list(&IssueFilter { status: Some(IssueStatus::InProgress), ..Default::default() }).unwrap_or_default().len();
                            Some((open, in_progress))
                        } else {
                            None
                        }
                    };

                    if cli.json {
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
                        if let Some(remote) = remote_clone::read_remote_config(&vai_dir) {
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
                        println!("Pending conflicts: 0");
                    }
                }
            }
        }
        Some(Commands::Rollback { version, force, entity }) => {
            let repo_root = repo::find_root(&std::env::current_dir().unwrap())
                .ok_or_else(|| CliError::Other("not inside a vai repository".into()))?;
            let vai_dir = repo_root.join(".vai");

            // 1. Analyze impact.
            let impact = version::analyze_rollback_impact(&vai_dir, &version)
                .map_err(CliError::Version)?;

            if cli.json {
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
        }
        Some(Commands::Server(server_cmd)) => {
            let cwd = std::env::current_dir()
                .map_err(|e| CliError::Other(format!("cannot determine working directory: {e}")))?;
            let root = repo::find_root(&cwd)
                .ok_or_else(|| CliError::Other("not inside a vai repository".to_string()))?;
            let vai_dir = root.join(".vai");

            match server_cmd {
                ServerCommands::Start { port, bind } => {
                    let config = server::ServerConfig { bind, port };
                    tokio::runtime::Runtime::new()
                        .map_err(|e| CliError::Other(format!("cannot create async runtime: {e}")))?
                        .block_on(server::start(&vai_dir, config))?;
                }
                ServerCommands::Keys(keys_cmd) => {
                    match keys_cmd {
                        KeysCommands::Create { name } => {
                            let (meta, plaintext) = auth::create(&vai_dir, &name)?;
                            if cli.json {
                                let out = serde_json::json!({
                                    "name": meta.name,
                                    "key": plaintext,
                                    "key_prefix": meta.key_prefix,
                                    "created_at": meta.created_at.to_rfc3339(),
                                });
                                println!("{}", serde_json::to_string_pretty(&out).unwrap());
                            } else {
                                println!("{} API key created", "✓".green().bold());
                                println!("  Name : {}", meta.name.bold());
                                println!("  Key  : {}", plaintext.yellow().bold());
                                println!();
                                println!(
                                    "  {} This key will not be shown again. Store it securely.",
                                    "!".yellow().bold()
                                );
                            }
                        }
                        KeysCommands::List => {
                            let keys = auth::list(&vai_dir)?;
                            if cli.json {
                                println!("{}", serde_json::to_string_pretty(&keys).unwrap());
                            } else if keys.is_empty() {
                                println!("No API keys. Use `vai server keys create --name <name>` to create one.");
                            } else {
                                println!("{:<24} {:<16} {:<28} STATUS", "NAME", "PREFIX", "CREATED");
                                println!("{}", "-".repeat(80));
                                for k in &keys {
                                    let status = if k.revoked {
                                        "revoked".red().to_string()
                                    } else {
                                        "active".green().to_string()
                                    };
                                    let last_used = k
                                        .last_used_at
                                        .map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string())
                                        .unwrap_or_else(|| "never".to_string());
                                    println!(
                                        "{:<24} {:<16} {:<28} {}  (last used: {})",
                                        k.name,
                                        k.key_prefix,
                                        k.created_at.format("%Y-%m-%d %H:%M UTC"),
                                        status,
                                        last_used,
                                    );
                                }
                            }
                        }
                        KeysCommands::Revoke { name } => {
                            auth::revoke(&vai_dir, &name)?;
                            if cli.json {
                                println!(
                                    "{}",
                                    serde_json::to_string_pretty(
                                        &serde_json::json!({ "revoked": name })
                                    )
                                    .unwrap()
                                );
                            } else {
                                println!(
                                    "{} API key '{}' revoked",
                                    "✓".green().bold(),
                                    name.bold()
                                );
                            }
                        }
                    }
                }
            }
        }
        Some(Commands::Clone { url, dest, key }) => {
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
                .block_on(remote_clone::clone(&url, &dest_path, &key))?;

            if cli.json {
                println!("{}", serde_json::to_string_pretty(&result).unwrap());
            } else {
                remote_clone::print_clone_result(&result);
            }
        }
        Some(Commands::Sync) => {
            let cwd = std::env::current_dir()
                .map_err(|e| CliError::Other(format!("cannot determine working directory: {e}")))?;
            let root = repo::find_root(&cwd)
                .ok_or_else(|| CliError::Other("not inside a vai repository".to_string()))?;

            let result = tokio::runtime::Runtime::new()
                .map_err(|e| CliError::Other(format!("cannot create async runtime: {e}")))?
                .block_on(remote_sync::sync(&root))?;

            if cli.json {
                println!("{}", serde_json::to_string_pretty(&result).unwrap());
            } else {
                remote_sync::print_sync_result(&result);
            }
        }
        Some(Commands::Issue(issue_cmd)) => {
            let cwd = std::env::current_dir()
                .map_err(|e| CliError::Other(format!("cannot determine working directory: {e}")))?;
            let root = repo::find_root(&cwd)
                .ok_or_else(|| CliError::Other("not inside a vai repository".to_string()))?;
            let vai_dir = root.join(".vai");

            match issue_cmd {
                IssueCommands::Create { title, description, priority, label } => {
                    let prio = IssuePriority::from_str(&priority)
                        .ok_or_else(|| CliError::Other(format!("unknown priority: {priority}")))?;
                    let store = IssueStore::open(&vai_dir)?;
                    let mut event_log = EventLog::open(&vai_dir)
                        .map_err(|e| CliError::Other(format!("cannot open event log: {e}")))?;
                    let issue = store.create(
                        title,
                        description,
                        prio,
                        label,
                        whoami(),
                        &mut event_log,
                    )?;
                    if cli.json {
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
                IssueCommands::List { status, priority, label, created_by } => {
                    let status_filter = if let Some(s) = status {
                        Some(IssueStatus::from_str(&s)
                            .ok_or_else(|| CliError::Other(format!("unknown status: {s}")))?)
                    } else {
                        None
                    };
                    let priority_filter = if let Some(p) = priority {
                        Some(IssuePriority::from_str(&p)
                            .ok_or_else(|| CliError::Other(format!("unknown priority: {p}")))?)
                    } else {
                        None
                    };
                    let filter = IssueFilter {
                        status: status_filter,
                        priority: priority_filter,
                        label,
                        creator: created_by,
                    };
                    let store = IssueStore::open(&vai_dir)?;
                    let issues = store.list(&filter)?;
                    if cli.json {
                        println!("{}", serde_json::to_string_pretty(&issues).unwrap());
                    } else if issues.is_empty() {
                        println!("No issues found.");
                    } else {
                        println!(
                            "{:<10}  {:<8}  {:<8}  {:<30}  {}",
                            "ID", "STATUS", "PRIORITY", "TITLE", "CREATED"
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
                    if cli.json {
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
                IssueCommands::Update { id, priority, label, title } => {
                    let prio = if let Some(p) = priority {
                        Some(IssuePriority::from_str(&p)
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
                        None,
                        prio,
                        new_labels,
                        &mut event_log,
                    )?;
                    if cli.json {
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
                    let res = IssueResolution::from_str(&resolution)
                        .ok_or_else(|| CliError::Other(format!("unknown resolution: {resolution}. Use: resolved, wontfix, duplicate")))?;
                    let store = IssueStore::open(&vai_dir)?;
                    let mut event_log = EventLog::open(&vai_dir)
                        .map_err(|e| CliError::Other(format!("cannot open event log: {e}")))?;
                    let resolved_issue = resolve_issue(&store, &id)?;
                    let closed = store.close(resolved_issue.id, res, &mut event_log)?;
                    if cli.json {
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
        }
        Some(Commands::Merge(merge_cmd)) => {
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

                    if cli.json {
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
                    if cli.json {
                        println!("{}", serde_json::to_string_pretty(&record).unwrap());
                    } else {
                        println!(
                            "{} Conflict {} marked as resolved.",
                            "✓".green().bold(),
                            &record.conflict_id.to_string()[..8].bold()
                        );
                    }
                }
            }
        }
    }
    Ok(())
}

/// Machine-readable output for `vai status`.
#[derive(Debug, Serialize)]
struct StatusOutput {
    repo_name: String,
    head_version: VersionMeta,
    graph_stats: GraphStats,
    workspaces: Vec<WorkspaceMeta>,
    pending_conflicts: u32,
}

/// Prints graph statistics in the standard human-readable format.
fn print_graph_stats(stats: &GraphStats) {
    let by_kind_summary: Vec<String> = stats
        .by_kind
        .iter()
        .map(|(k, v)| format!("{v} {k}s"))
        .collect();
    let summary = if by_kind_summary.is_empty() {
        String::new()
    } else {
        format!(" ({})", by_kind_summary.join(", "))
    };
    println!("Semantic graph:");
    println!(
        "  {} entities{}",
        stats.entity_count, summary
    );
    println!("  {} relationships", stats.relationship_count);
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

/// Return the current user's login name (falls back to "human").
fn whoami() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "human".to_string())
}

/// Resolve an issue by full UUID or 8-character prefix.
fn resolve_issue(
    store: &IssueStore,
    id_str: &str,
) -> Result<crate::issue::Issue, CliError> {
    // Try exact UUID first.
    if let Ok(uuid) = uuid::Uuid::parse_str(id_str) {
        return Ok(store.get(uuid)?);
    }
    // Fall back to prefix search.
    let all = store.list(&IssueFilter::default())?;
    let prefix = id_str.to_lowercase();
    let matches: Vec<_> = all
        .into_iter()
        .filter(|i| i.id.to_string().starts_with(&prefix))
        .collect();
    match matches.len() {
        0 => Err(CliError::Other(format!("no issue found with id prefix: {id_str}"))),
        1 => Ok(matches.into_iter().next().unwrap()),
        _ => Err(CliError::Other(format!(
            "ambiguous id prefix {id_str}: matches {} issues",
            matches.len()
        ))),
    }
}

/// Print a one-line summary of an issue.
fn print_issue_summary(issue: &crate::issue::Issue) {
    println!("  Title    : {}", issue.title);
    println!("  Status   : {}", colorize_status(&issue.status));
    println!("  Priority : {}", colorize_priority(&issue.priority));
    if !issue.labels.is_empty() {
        println!("  Labels   : {}", issue.labels.join(", "));
    }
}

/// Colorize an `IssueStatus` for terminal output.
fn colorize_status(status: &IssueStatus) -> colored::ColoredString {
    match status {
        IssueStatus::Open => status.as_str().green(),
        IssueStatus::InProgress => status.as_str().yellow(),
        IssueStatus::Resolved => status.as_str().cyan(),
        IssueStatus::Closed => status.as_str().dimmed(),
    }
}

/// Colorize an `IssuePriority` for terminal output.
fn colorize_priority(priority: &IssuePriority) -> colored::ColoredString {
    match priority {
        IssuePriority::Critical => priority.as_str().red().bold(),
        IssuePriority::High => priority.as_str().red(),
        IssuePriority::Medium => priority.as_str().yellow(),
        IssuePriority::Low => priority.as_str().normal(),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::fs;
    use tempfile::TempDir;

    use crate::{graph::GraphSnapshot, repo, version, workspace};

    /// Init a repo, create two workspaces, then verify the status data is correct.
    #[test]
    fn status_data_reflects_repo_state() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Write a small Rust source file so the graph has some entities.
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("src").join("lib.rs"),
            b"pub fn greet() {}\npub struct Config;\n",
        )
        .unwrap();

        repo::init(root).unwrap();
        let vai_dir = root.join(".vai");

        // HEAD should be v1 after init.
        let head = repo::read_head(&vai_dir).unwrap();
        assert_eq!(head, "v1");

        // Config should round-trip correctly.
        let config = repo::read_config(&vai_dir).unwrap();
        assert_eq!(config.name, root.file_name().unwrap().to_string_lossy().as_ref());

        // Head version metadata.
        let head_version = version::get_version(&vai_dir, &head).unwrap();
        assert_eq!(head_version.version_id, "v1");
        assert_eq!(head_version.intent, "initial repository");

        // Graph stats should reflect the parsed file.
        let snapshot = GraphSnapshot::open(&vai_dir.join("graph").join("snapshot.db")).unwrap();
        let stats = snapshot.stats().unwrap();
        assert!(stats.entity_count >= 2, "expected at least 2 entities");

        // No workspaces yet.
        let ws_list = workspace::list(&vai_dir).unwrap();
        assert!(ws_list.is_empty());

        // Create two workspaces.
        workspace::create(&vai_dir, "fix auth bug", &head).unwrap();
        workspace::create(&vai_dir, "add logging", &head).unwrap();

        let ws_list = workspace::list(&vai_dir).unwrap();
        assert_eq!(ws_list.len(), 2);

        // Active workspace should be the most recently created one.
        let active_id = workspace::active_id(&vai_dir);
        assert!(active_id.is_some());
        assert_eq!(active_id.unwrap(), ws_list[0].id.to_string());
    }

    /// `read_config` should fail gracefully on a non-repo directory.
    #[test]
    fn read_config_fails_outside_repo() {
        let tmp = TempDir::new().unwrap();
        let result = repo::read_config(&tmp.path().join(".vai"));
        assert!(result.is_err());
    }
}
