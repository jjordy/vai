//! CLI command definitions and dispatch.
//!
//! Uses `clap` derive API to define all vai subcommands.
//! Each command handler lives in its own submodule.

use base64::Engine as _;
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use colored::Colorize;
use serde::Serialize;
use thiserror::Error;

use crate::auth;
use crate::clone as remote_clone;
use crate::conflict;
use crate::diff;
use crate::event_log::EventLog;
use crate::graph::{GraphSnapshot, GraphStats};
use crate::scope_inference;
use crate::scope_history::ScopeHistoryStore;
use crate::escalation::{EscalationStatus, EscalationStore, ResolutionOption};
use crate::issue::{IssueFilter, IssueStore, IssuePriority, IssueStatus};
use crate::merge;
use crate::merge_patterns::MergePatternStore;
use crate::remote_workspace;
use crate::repo;
use crate::server;
use crate::sync as remote_sync;
use crate::version::VersionMeta;
use crate::version;
use crate::work_queue;
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

    #[error("Escalation error: {0}")]
    Escalation(#[from] crate::escalation::EscalationError),

    #[error("Scope inference error: {0}")]
    ScopeInference(#[from] scope_inference::ScopeInferenceError),

    #[error("Merge pattern error: {0}")]
    MergePattern(#[from] crate::merge_patterns::MergePatternError),

    #[error("Scope history error: {0}")]
    ScopeHistory(#[from] crate::scope_history::ScopeHistoryError),

    #[error("Work queue error: {0}")]
    WorkQueue(#[from] crate::work_queue::WorkQueueError),

    #[error("Remote client error: {0}")]
    Remote(#[from] crate::remote_client::RemoteClientError),

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

    /// Force local operation, ignoring any configured remote.
    ///
    /// When set, all commands read and write the local `.vai/` directory even
    /// if a `[remote]` section is present in the config.
    #[arg(long, global = true)]
    pub local: bool,

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
    /// Manage escalations requiring human attention.
    #[command(subcommand)]
    Escalations(EscalationCommands),
    /// Inspect and claim work from the prioritized work queue.
    #[command(subcommand)]
    WorkQueue(WorkQueueCommands),
    /// Launch the TUI dashboard for real-time agent oversight.
    ///
    /// Without `--server`, polls the local `.vai/` directory.
    /// With `--server vai://host:port`, connects via WebSocket for live updates.
    Dashboard {
        /// Remote server URL for live updates (e.g. `vai://localhost:7865`).
        #[arg(long)]
        server: Option<String>,
        /// API key for authenticating with the remote server.
        #[arg(long)]
        key: Option<String>,
    },
    /// Manage the remote server configuration.
    #[command(subcommand)]
    Remote(RemoteCommands),
}

/// Issue management subcommands.
#[derive(Debug, Subcommand)]
pub enum IssueCommands {
    /// Create a new issue.
    Create {
        /// Short summary of the issue.
        #[arg(long)]
        title: String,
        /// Full description / body text (optional, Markdown supported).
        #[arg(long, default_value = "")]
        body: String,
        /// Priority level: critical, high, medium, low.
        #[arg(long, default_value = "medium")]
        priority: String,
        /// Label to apply (can be repeated, or use comma-separated values).
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
        /// Add a label (can be repeated, or use comma-separated values).
        #[arg(long)]
        label: Vec<String>,
        /// New title.
        #[arg(long)]
        title: Option<String>,
        /// New body / description text (Markdown supported).
        #[arg(long)]
        body: Option<String>,
    },
    /// Close an issue with a resolution.
    Close {
        /// Issue ID.
        id: String,
        /// Resolution: resolved, wontfix, duplicate (default: resolved).
        #[arg(long, default_value = "resolved")]
        resolution: String,
    },
}

/// Escalation management subcommands.
#[derive(Debug, Subcommand)]
pub enum EscalationCommands {
    /// List escalations (default: pending only).
    List {
        /// Show all escalations including resolved ones.
        #[arg(long)]
        all: bool,
    },
    /// Show full details of an escalation.
    Show {
        /// Escalation ID (full UUID or 8-char prefix).
        id: String,
    },
    /// Resolve an escalation by choosing a resolution option.
    Resolve {
        /// Escalation ID (full UUID or 8-char prefix).
        id: String,
        /// Resolution: keep_agent_a, keep_agent_b,
        /// send_back_to_agent_a, send_back_to_agent_b, pause_both.
        #[arg(long)]
        resolution: String,
        /// Identifier of the human resolving this escalation.
        #[arg(long, default_value = "human")]
        by: String,
    },
}

/// Work queue subcommands.
#[derive(Debug, Subcommand)]
pub enum WorkQueueCommands {
    /// List available and blocked work ranked by priority.
    ///
    /// Predicts which entities each open issue will touch and checks for
    /// overlap with active workspace scopes.
    List,
    /// Atomically claim an issue: mark it in-progress and create a workspace.
    ///
    /// Re-checks for conflicts at claim time; returns an error if the issue
    /// is no longer open or now conflicts with an active workspace.
    Claim {
        /// Issue ID (full UUID or 8-char prefix).
        issue_id: String,
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
    /// List learned merge conflict patterns with success rates.
    Patterns,
    /// Disable auto-resolution for a specific pattern (human override).
    PatternsDisable {
        /// Numeric pattern ID (from `vai merge patterns`).
        pattern_id: i64,
    },
    /// Re-enable auto-resolution for a previously disabled pattern.
    PatternsEnable {
        /// Numeric pattern ID (from `vai merge patterns`).
        pattern_id: i64,
    },
}

/// Server management subcommands.
#[derive(Debug, Subcommand)]
pub enum ServerCommands {
    /// Start the vai HTTP server for this repository.
    Start {
        /// TCP port to listen on. Overrides `[server].port` in `.vai/config.toml`.
        #[arg(long)]
        port: Option<u16>,
        /// IP address to bind to. Overrides `[server].host` in `.vai/config.toml`.
        #[arg(long)]
        host: Option<String>,
        /// Write the server PID to this file on startup; removed on clean shutdown.
        #[arg(long)]
        pid_file: Option<std::path::PathBuf>,
        /// Postgres connection URL (e.g. `postgres://vai:secret@localhost/vai`).
        ///
        /// When set the server uses the Postgres backend instead of the default
        /// SQLite/filesystem storage. Overrides `[server].database_url` in
        /// `~/.vai/server.toml`. Also read from the `VAI_DATABASE_URL` environment
        /// variable when the `env` clap feature is enabled.
        #[arg(long)]
        database_url: Option<String>,
        /// Maximum number of Postgres connections in the pool (default: 25).
        ///
        /// Increase this value if the server returns `pool timed out` errors
        /// under concurrent load (e.g. many agents + dashboard polling).
        #[arg(long)]
        db_pool_size: Option<u32>,
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

/// Remote server configuration subcommands.
#[derive(Debug, Subcommand)]
pub enum RemoteCommands {
    /// Set the remote server URL and API key.
    Add {
        /// Base URL of the remote vai server (e.g. `https://vai.example.com`).
        url: String,
        /// Literal API key value.
        #[arg(long)]
        key: Option<String>,
        /// Name of an environment variable that holds the API key.
        #[arg(long, conflicts_with_all = ["key", "key_cmd"])]
        key_env: Option<String>,
        /// Shell command whose stdout is the API key (e.g. `pass show vai/api-key`).
        #[arg(long, conflicts_with_all = ["key", "key_env"])]
        key_cmd: Option<String>,
    },
    /// Remove the remote server configuration.
    Remove,
    /// Show current remote config and test connectivity.
    Status,
    /// Migrate all local data to the configured remote server.
    ///
    /// Reads all local SQLite data (events, issues, versions, escalations),
    /// streams it to the remote server, and writes a `.vai/migrated_at` marker
    /// on success.  All subsequent CLI commands will proxy to the remote.
    Migrate,
}

/// Graph subcommands.
#[derive(Debug, Subcommand)]
pub enum GraphCommands {
    /// Display graph statistics.
    Show,
    /// Re-scan all source files and rebuild the semantic graph.
    Refresh,
    /// Search for entities by name.
    Query {
        /// Entity name to search for.
        name: String,
    },
    /// Infer which entities are likely to be affected by a natural-language intent.
    Infer {
        /// Free-text description of the planned change.
        intent: String,
        /// Number of relationship hops to traverse from direct matches (default: 2).
        #[arg(long, default_value_t = 2)]
        hops: usize,
        /// Weight predictions using historical intent data.
        #[arg(long)]
        history: bool,
    },
    /// Show scope prediction accuracy over the last N completed intents.
    Accuracy {
        /// Number of past intents to evaluate (default: 20).
        #[arg(long, default_value_t = 20)]
        limit: usize,
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
                GraphCommands::Refresh => {
                    let result = repo::refresh_graph(&root)?;
                    if cli.json {
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
                GraphCommands::Infer { intent, hops, history } => {
                    let history_path = root.join(".vai").join("graph").join("history.db");
                    let result = if history {
                        let hist = ScopeHistoryStore::open(&history_path)?;
                        scope_inference::infer_with_history(&snapshot, &hist, &intent, hops)?
                    } else {
                        scope_inference::infer(&snapshot, &intent, hops)?
                    };
                    if cli.json {
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
                                if !cli.quiet {
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
                    if cli.json {
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
                    // Proxy to remote if configured.
                    if let Some(client) = try_remote(&vai_dir, cli.local)? {
                        let rt = make_rt()?;
                        let workspaces: serde_json::Value = rt.block_on(client.get("/api/workspaces"))?;
                        if cli.json {
                            println!("{}", serde_json::to_string_pretty(&workspaces).unwrap());
                        } else {
                            let arr = workspaces.as_array().cloned().unwrap_or_default();
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
                        let intent_text = active_ws_meta.intent.clone();
                        let workspace_id = active_ws_meta.id.to_string();

                        let result = merge::submit(&vai_dir, &root)?;

                        // Record intent → actual entities in history store.
                        let history_path = vai_dir.join("graph").join("history.db");
                        if let Ok(hist) = ScopeHistoryStore::open(&history_path) {
                            let terms = scope_inference::extract_terms(&intent_text);
                            let _ = hist.record(
                                &intent_text,
                                &terms,
                                &[], // predicted IDs not tracked at submit time
                                &result.entity_ids,
                                Some(&workspace_id),
                            );
                        }

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

                    // ── Remote dispatch (skip when --others or --local) ───
                    if !others && !cli.local {
                        if let Some(client) = try_remote(&vai_dir, cli.local)? {
                            let rt = make_rt()?;
                            let status: serde_json::Value = rt.block_on(client.get("/api/status"))?;
                            if cli.json {
                                println!("{}", serde_json::to_string_pretty(&status).unwrap());
                            } else {
                                let repo_name = status["repo_name"].as_str().unwrap_or("?");
                                let head = status["head_version"].as_str().unwrap_or("?");
                                let ws_count = status["workspace_count"].as_u64().unwrap_or(0);
                                let issue_count = status["issue_count"].as_u64().unwrap_or(0);
                                let entity_count = status["entity_count"].as_u64().unwrap_or(0);
                                let uptime = status["uptime_secs"].as_u64().unwrap_or(0);
                                println!("{} repository: {}", "vai".bold(), repo_name.bold());
                                println!("Current version: {}", head.bold());
                                println!("Active workspaces: {ws_count}");
                                println!("Open issues: {issue_count}");
                                println!("Graph entities: {entity_count}");
                                println!("Server uptime: {}s", uptime);
                            }
                            return Ok(());
                        }
                    }

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

                    // Query pending escalation count (best-effort).
                    let pending_escalations = crate::escalation::EscalationStore::open(&vai_dir)
                        .ok()
                        .and_then(|s| s.count_pending().ok())
                        .unwrap_or(0);

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
            // Load the global server config first so we can detect multi-repo
            // mode before deciding whether `find_root` is required.
            let global_cfg = repo::read_global_server_config().unwrap_or_default();
            let is_multi_repo = global_cfg.storage_root.is_some();

            // In multi-repo mode the server is not tied to any single
            // repository, so `find_root` would spuriously fail when the
            // process is started from an unrelated directory.  Use `~/.vai/`
            // as the server-level store for API keys instead.
            let vai_dir = if is_multi_repo {
                let home = std::env::var("HOME")
                    .or_else(|_| std::env::var("USERPROFILE"))
                    .map_err(|_| CliError::Other("cannot determine home directory".to_string()))?;
                std::path::PathBuf::from(home).join(".vai")
            } else {
                let cwd = std::env::current_dir()
                    .map_err(|e| CliError::Other(format!("cannot determine working directory: {e}")))?;
                let root = repo::find_root(&cwd)
                    .ok_or_else(|| CliError::Other("not inside a vai repository".to_string()))?;
                root.join(".vai")
            };

            match server_cmd {
                ServerCommands::Start { port, host, pid_file, database_url, db_pool_size } => {
                    // Config layering (lowest → highest priority):
                    //   1. Built-in defaults (127.0.0.1:7865, no storage_root)
                    //   2. ~/.vai/server.toml [server] section (global, optional)
                    //   3. .vai/config.toml [server] section (per-repo, single-repo mode only)
                    //   4. CLI flags / VAI_DATABASE_URL env var (--host, --port, --database-url)
                    let mut config = server::ServerConfig::default();

                    // Layer 2: global server config (already loaded above).
                    if let Some(h) = global_cfg.host { config.host = h; }
                    if let Some(p) = global_cfg.port { config.port = p; }
                    if let Some(r) = global_cfg.storage_root { config.storage_root = Some(r); }
                    if let Some(u) = global_cfg.database_url { config.database_url = Some(u); }
                    if let Some(s) = global_cfg.db_pool_size { config.db_pool_size = Some(s); }

                    // Layer 3: per-repo config (single-repo mode only).
                    if !is_multi_repo {
                        if let Ok(repo_cfg) = repo::read_config(&vai_dir) {
                            if let Some(srv) = repo_cfg.server {
                                if let Some(h) = srv.host { config.host = h; }
                                if let Some(p) = srv.port { config.port = p; }
                            }
                        }
                    }

                    // Layer 4: CLI flags / env var
                    if let Some(h) = host { config.host = h; }
                    if let Some(p) = port { config.port = p; }
                    if let Some(pf) = pid_file { config.pid_file = Some(pf); }
                    if let Some(u) = database_url { config.database_url = Some(u); }
                    if let Some(s) = db_pool_size { config.db_pool_size = Some(s); }

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

            // ── Remote dispatch ────────────────────────────────────────────────
            if let Some(client) = try_remote(&vai_dir, cli.local)? {
                let rt = make_rt()?;
                match issue_cmd {
                    IssueCommands::List { status, priority, label, created_by } => {
                        let mut params: Vec<String> = vec![];
                        if let Some(s) = status { params.push(format!("status={s}")); }
                        if let Some(p) = priority { params.push(format!("priority={p}")); }
                        if let Some(l) = label { params.push(format!("label={l}")); }
                        if let Some(c) = created_by { params.push(format!("created_by={c}")); }
                        let path = if params.is_empty() {
                            "/api/issues".to_string()
                        } else {
                            format!("/api/issues?{}", params.join("&"))
                        };
                        let issues: serde_json::Value = rt.block_on(client.get(&path))?;
                        if cli.json {
                            println!("{}", serde_json::to_string_pretty(&issues).unwrap());
                        } else {
                            let arr = issues.as_array().cloned().unwrap_or_default();
                            if arr.is_empty() {
                                println!("No issues found.");
                            } else {
                                println!("{:<10}  {:<11}  {:<8}  {:<30}  {}", "ID", "STATUS", "PRIORITY", "TITLE", "CREATED");
                                println!("{}", "-".repeat(85));
                                for issue in &arr {
                                    let id_short = json_id_short(issue, "id");
                                    let age = issue["created_at"].as_str()
                                        .and_then(|s| s.parse::<DateTime<Utc>>().ok())
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
                    IssueCommands::Create { title, body, priority, label } => {
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
                        });
                        let issue: serde_json::Value = rt.block_on(client.post("/api/issues", &req))?;
                        if cli.json {
                            println!("{}", serde_json::to_string_pretty(&issue).unwrap());
                        } else {
                            println!("{} Created issue {}", "✓".green(), json_id_short(&issue, "id").cyan());
                            println!("  Title    : {}", issue["title"].as_str().unwrap_or(""));
                            println!("  Status   : {}", issue["status"].as_str().unwrap_or(""));
                            println!("  Priority : {}", issue["priority"].as_str().unwrap_or(""));
                        }
                    }
                    IssueCommands::Show { id } => {
                        let path = format!("/api/issues/{id}");
                        let issue: serde_json::Value = rt.block_on(client.get(&path))?;
                        if cli.json {
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
                    IssueCommands::Update { id, priority, label, title, body } => {
                        let new_labels: Option<Vec<String>> = if label.is_empty() {
                            None
                        } else {
                            Some(label.iter()
                                .flat_map(|s| s.split(','))
                                .map(|s| s.trim().to_string())
                                .filter(|s| !s.is_empty())
                                .collect())
                        };
                        let req = serde_json::json!({
                            "priority": priority,
                            "title": title,
                            "description": body,
                            "labels": new_labels,
                        });
                        let path = format!("/api/issues/{id}");
                        let issue: serde_json::Value = rt.block_on(client.patch(&path, &req))?;
                        if cli.json {
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
                        let path = format!("/api/issues/{id}/close");
                        let issue: serde_json::Value = rt.block_on(client.post(&path, &req))?;
                        if cli.json {
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
                IssueCommands::Create { title, body, priority, label } => {
                    let prio = IssuePriority::from_str(&priority)
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
                IssueCommands::Update { id, priority, label, title, body } => {
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
                        body,
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
                    let store = IssueStore::open(&vai_dir)?;
                    let mut event_log = EventLog::open(&vai_dir)
                        .map_err(|e| CliError::Other(format!("cannot open event log: {e}")))?;
                    let resolved_issue = resolve_issue(&store, &id)?;
                    let closed = store.close(resolved_issue.id, &resolution, &mut event_log)?;
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
        Some(Commands::Escalations(esc_cmd)) => {
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
                    if cli.json {
                        println!("{}", serde_json::to_string_pretty(&escalations).unwrap());
                    } else if escalations.is_empty() {
                        if all {
                            println!("No escalations.");
                        } else {
                            println!("No pending escalations.");
                        }
                    } else {
                        println!(
                            "{:<10}  {:<10}  {:<8}  {:<30}  {}",
                            "ID", "TYPE", "SEVERITY", "SUMMARY", "CREATED"
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
                    if cli.json {
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
                    let opt = ResolutionOption::from_str(&resolution).ok_or_else(|| {
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
                    if cli.json {
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
        }
        Some(Commands::WorkQueue(wq_cmd)) => {
            let cwd = std::env::current_dir()
                .map_err(|e| CliError::Other(format!("cannot determine working directory: {e}")))?;
            let root = repo::find_root(&cwd)
                .ok_or_else(|| CliError::Other("not inside a vai repository".to_string()))?;
            let vai_dir = root.join(".vai");

            // ── Remote dispatch ────────────────────────────────────────────────
            if let Some(client) = try_remote(&vai_dir, cli.local)? {
                let rt = make_rt()?;
                match wq_cmd {
                    WorkQueueCommands::List => {
                        let queue: serde_json::Value = rt.block_on(client.get("/api/work-queue"))?;
                        if cli.json {
                            println!("{}", serde_json::to_string_pretty(&queue).unwrap());
                        } else {
                            let available = queue["available_work"].as_array().cloned().unwrap_or_default();
                            let blocked = queue["blocked_work"].as_array().cloned().unwrap_or_default();
                            if available.is_empty() && blocked.is_empty() {
                                println!("No open issues in the work queue.");
                            } else {
                                if !available.is_empty() {
                                    println!("{}", "Available work:".green().bold());
                                    println!("  {:<10}  {:<8}  {}", "ISSUE", "PRIORITY", "TITLE");
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
                                    println!("  {:<10}  {:<8}  {}", "ISSUE", "PRIORITY", "TITLE");
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
                        if cli.json {
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
                    if cli.json {
                        println!("{}", serde_json::to_string_pretty(&queue).unwrap());
                    } else if queue.available_work.is_empty() && queue.blocked_work.is_empty() {
                        println!("No open issues in the work queue.");
                    } else {
                        if !queue.available_work.is_empty() {
                            println!("{}", "Available work:".green().bold());
                            println!(
                                "  {:<10}  {:<8}  {}",
                                "ISSUE", "PRIORITY", "TITLE"
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
                                "  {:<10}  {:<8}  {}",
                                "ISSUE", "PRIORITY", "TITLE"
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
                    if cli.json {
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
                MergeCommands::Patterns => {
                    let store = MergePatternStore::open(&vai_dir)?;
                    let patterns = store.list_patterns()?;
                    if cli.json {
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
                    if cli.json {
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
                    if cli.json {
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
        }
        Some(Commands::Dashboard { server, key }) => {
            let cwd = std::env::current_dir()
                .map_err(|e| CliError::Other(format!("cannot determine working directory: {e}")))?;
            let root = repo::find_root(&cwd)
                .ok_or_else(|| CliError::Other("not inside a vai repository".to_string()))?;
            let vai_dir = root.join(".vai");
            if let Some(server_url) = server {
                let api_key = key.unwrap_or_default();
                crate::dashboard::run_server(&vai_dir, &server_url, &api_key)
                    .map_err(|e| CliError::Other(e.to_string()))?;
            } else {
                crate::dashboard::run(&vai_dir)
                    .map_err(|e| CliError::Other(e.to_string()))?;
            }
        }
        Some(Commands::Remote(remote_cmd)) => {
            let cwd = std::env::current_dir()
                .map_err(|e| CliError::Other(format!("cannot determine working directory: {e}")))?;
            let root = repo::find_root(&cwd)
                .ok_or_else(|| CliError::Other("not inside a vai repository".to_string()))?;
            let vai_dir = root.join(".vai");
            match remote_cmd {
                RemoteCommands::Add { url, key, key_env, key_cmd } => {
                    if key.is_none() && key_env.is_none() && key_cmd.is_none() {
                        return Err(CliError::Other(
                            "one of --key, --key-env, or --key-cmd is required".to_string(),
                        ));
                    }
                    let mut config = repo::read_config(&vai_dir)?;
                    config.remote = Some(repo::RemoteServerConfig {
                        url: url.clone(),
                        api_key: key,
                        api_key_env: key_env,
                        api_key_cmd: key_cmd,
                    });
                    repo::write_config(&vai_dir, &config)?;
                    if cli.json {
                        println!("{}", serde_json::json!({"status": "ok", "url": url}));
                    } else {
                        println!("Remote set to {}", url.cyan());
                    }
                }
                RemoteCommands::Remove => {
                    let mut config = repo::read_config(&vai_dir)?;
                    if config.remote.is_none() {
                        return Err(CliError::Other("no remote configured".to_string()));
                    }
                    config.remote = None;
                    repo::write_config(&vai_dir, &config)?;
                    if cli.json {
                        println!("{}", serde_json::json!({"status": "ok"}));
                    } else {
                        println!("Remote configuration removed.");
                    }
                }
                RemoteCommands::Status => {
                    let config = repo::read_config(&vai_dir)?;
                    // Check for a migration marker written by `vai remote migrate`.
                    let marker = crate::migration::MigrationMarker::read(&vai_dir);
                    match &config.remote {
                        None => {
                            if cli.json {
                                println!("{}", serde_json::json!({"configured": false}));
                            } else {
                                println!("No remote configured.");
                                println!("Run `vai remote add <url> --key <api-key>` to set one.");
                            }
                        }
                        Some(remote) => {
                            let rt = tokio::runtime::Runtime::new()
                                .map_err(|e| CliError::Other(format!("cannot create async runtime: {e}")))?;
                            let client = crate::remote_client::RemoteClient::new(remote)
                                .map_err(|e| CliError::Other(format!("API key error: {e}")))?;

                            // Ping connectivity.
                            let reachable = rt.block_on(async {
                                client
                                    .get::<serde_json::Value>("/api/status")
                                    .await
                                    .is_ok()
                            });

                            // If migrated, fetch remote stats for verification.
                            let repo_name = &config.name;
                            let stats: Option<crate::server::MigrationStatsResponse> = if marker.is_some() {
                                rt.block_on(async {
                                    let repo_endpoint =
                                        format!("/api/repos/{repo_name}/migration-stats");
                                    match client
                                        .get::<crate::server::MigrationStatsResponse>(
                                            &repo_endpoint,
                                        )
                                        .await
                                    {
                                        Ok(s) => Some(s),
                                        Err(_) => {
                                            // Fall back to single-repo endpoint.
                                            client
                                                .get::<crate::server::MigrationStatsResponse>(
                                                    "/api/migration-stats",
                                                )
                                                .await
                                                .ok()
                                        }
                                    }
                                })
                            } else {
                                None
                            };

                            if cli.json {
                                let mut obj = serde_json::json!({
                                    "configured": true,
                                    "url": remote.url,
                                    "reachable": reachable,
                                });
                                if let Some(ref m) = marker {
                                    obj["migrated_at"] = serde_json::json!(m.migrated_at.to_rfc3339());
                                    obj["migration"] = serde_json::json!({
                                        "events_migrated": m.events_migrated,
                                        "issues_migrated": m.issues_migrated,
                                        "versions_migrated": m.versions_migrated,
                                        "escalations_migrated": m.escalations_migrated,
                                        "head_version": m.head_version,
                                    });
                                }
                                if let Some(ref s) = stats {
                                    obj["remote_counts"] = serde_json::json!({
                                        "events": s.events,
                                        "issues": s.issues,
                                        "versions": s.versions,
                                        "escalations": s.escalations,
                                        "head_version": s.head_version,
                                    });
                                }
                                println!("{}", obj);
                            } else {
                                if reachable {
                                    println!("Remote: {} {}", remote.url.cyan(), "(reachable)".green());
                                } else {
                                    println!("Remote: {} {}", remote.url.cyan(), "(unreachable)".red());
                                }
                                if let Some(ref m) = marker {
                                    println!();
                                    println!(
                                        "Migrated: {}",
                                        m.migrated_at.format("%Y-%m-%d %H:%M:%S UTC")
                                    );
                                    println!("Data transferred:");
                                    println!("  Events:      {}", m.events_migrated);
                                    println!("  Issues:      {}", m.issues_migrated);
                                    println!("  Versions:    {}", m.versions_migrated);
                                    println!("  Escalations: {}", m.escalations_migrated);
                                    if let Some(ref head) = m.head_version {
                                        println!("  HEAD:        {}", head);
                                    }

                                    if let Some(ref s) = stats {
                                        println!();
                                        println!("Verification (remote counts):");
                                        let ev_ok = s.events == m.events_migrated as i64;
                                        let is_ok = s.issues == m.issues_migrated as i64;
                                        let ve_ok = s.versions == m.versions_migrated as i64;
                                        let es_ok = s.escalations == m.escalations_migrated as i64;
                                        let tick = |ok: bool| {
                                            if ok { "OK".green().to_string() } else { "MISMATCH".red().to_string() }
                                        };
                                        println!(
                                            "  Events:      {} (expected {}) [{}]",
                                            s.events, m.events_migrated, tick(ev_ok)
                                        );
                                        println!(
                                            "  Issues:      {} (expected {}) [{}]",
                                            s.issues, m.issues_migrated, tick(is_ok)
                                        );
                                        println!(
                                            "  Versions:    {} (expected {}) [{}]",
                                            s.versions, m.versions_migrated, tick(ve_ok)
                                        );
                                        println!(
                                            "  Escalations: {} (expected {}) [{}]",
                                            s.escalations, m.escalations_migrated, tick(es_ok)
                                        );
                                        if ev_ok && is_ok && ve_ok && es_ok {
                                            println!("\nMigration verified successfully.");
                                        } else {
                                            println!("\nWarning: some counts do not match.");
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                RemoteCommands::Migrate => {
                    let config = repo::read_config(&vai_dir)?;
                    let remote = config.remote.as_ref().ok_or_else(|| {
                        CliError::Other(
                            "no remote configured; run `vai remote add <url> --key <key>` first"
                                .to_string(),
                        )
                    })?;

                    if !cli.json {
                        println!("Gathering local data…");
                    }

                    let payload = crate::migration::gather_local_data(&vai_dir)
                        .map_err(|e| CliError::Other(format!("failed to read local data: {e}")))?;

                    let event_count = payload.events.len();
                    let issue_count = payload.issues.len();
                    let version_count = payload.versions.len();
                    let esc_count = payload.escalations.len();

                    if !cli.json {
                        println!(
                            "Migrating {} events, {} issues, {} versions, {} escalations…",
                            event_count, issue_count, version_count, esc_count
                        );
                    }

                    let rt = tokio::runtime::Runtime::new()
                        .map_err(|e| CliError::Other(format!("cannot create async runtime: {e}")))?;
                    let client = crate::remote_client::RemoteClient::new(remote)
                        .map_err(|e| CliError::Other(format!("API key error: {e}")))?;

                    // Try repo-scoped endpoint first; fall back to single-repo.
                    let repo_name = &config.name;
                    let endpoint = format!("/api/repos/{repo_name}/migrate");

                    let summary: crate::migration::MigrationSummary = rt.block_on(async {
                        // Try the repo-scoped endpoint.
                        match client
                            .post::<_, crate::migration::MigrationSummary>(&endpoint, &payload)
                            .await
                        {
                            Ok(s) => Ok(s),
                            Err(crate::remote_client::RemoteClientError::HttpError {
                                status: 404, ..
                            }) => {
                                // Fall back to the single-repo endpoint.
                                client
                                    .post::<_, crate::migration::MigrationSummary>(
                                        "/api/migrate",
                                        &payload,
                                    )
                                    .await
                            }
                            Err(e) => Err(e),
                        }
                    })
                    .map_err(|e| CliError::Other(format!("migration failed: {e}")))?;

                    // ── Source file upload (PRD 12.3) ──────────────────────────
                    let source_files = repo::list_migration_files(&root);
                    let total_files = source_files.len();

                    if !cli.json && total_files > 0 {
                        println!("Uploading {} source files…", total_files);
                    }

                    // Serializable types for the batch upload request/response.
                    #[derive(serde::Serialize)]
                    struct FileEntry {
                        path: String,
                        content_base64: String,
                    }
                    #[derive(serde::Serialize)]
                    struct FileBatch {
                        files: Vec<FileEntry>,
                    }
                    #[derive(serde::Deserialize)]
                    struct FileBatchResponse {
                        uploaded: usize,
                    }

                    const BATCH_SIZE: usize = 50;
                    let mut files_uploaded = 0usize;
                    let files_endpoint = format!("/api/repos/{repo_name}/files");

                    for chunk in source_files.chunks(BATCH_SIZE) {
                        let entries: Vec<FileEntry> = chunk
                            .iter()
                            .filter_map(|path| {
                                let content = std::fs::read(path).ok()?;
                                let rel = path
                                    .strip_prefix(&root)
                                    .ok()?
                                    .to_string_lossy()
                                    .replace('\\', "/");
                                let encoded =
                                    base64::engine::general_purpose::STANDARD.encode(&content);
                                Some(FileEntry {
                                    path: rel,
                                    content_base64: encoded,
                                })
                            })
                            .collect();

                        if entries.is_empty() {
                            continue;
                        }

                        let batch = FileBatch { files: entries };

                        let resp: FileBatchResponse = rt
                            .block_on(async {
                                match client
                                    .post::<_, FileBatchResponse>(&files_endpoint, &batch)
                                    .await
                                {
                                    Ok(r) => Ok(r),
                                    Err(crate::remote_client::RemoteClientError::HttpError {
                                        status: 404,
                                        ..
                                    }) => {
                                        client
                                            .post::<_, FileBatchResponse>("/api/files", &batch)
                                            .await
                                    }
                                    Err(e) => Err(e),
                                }
                            })
                            .map_err(|e| {
                                CliError::Other(format!("source file upload failed: {e}"))
                            })?;

                        files_uploaded += resp.uploaded;
                        if !cli.json {
                            println!(
                                "  Uploaded {}/{} files…",
                                files_uploaded, total_files
                            );
                        }
                    }

                    // ── Graph rebuild (PRD 12.4) ────────────────────────────────
                    if files_uploaded > 0 {
                        if !cli.json {
                            println!("Triggering server-side graph rebuild…");
                        }

                        #[derive(serde::Deserialize)]
                        struct GraphRefreshResp {
                            files_scanned: usize,
                        }

                        let graph_endpoint =
                            format!("/api/repos/{repo_name}/graph/refresh");
                        let refresh: GraphRefreshResp = rt
                            .block_on(async {
                                match client
                                    .post::<_, GraphRefreshResp>(
                                        &graph_endpoint,
                                        &serde_json::json!({}),
                                    )
                                    .await
                                {
                                    Ok(r) => Ok(r),
                                    Err(crate::remote_client::RemoteClientError::HttpError {
                                        status: 404,
                                        ..
                                    }) => {
                                        client
                                            .post::<_, GraphRefreshResp>(
                                                "/api/graph/refresh",
                                                &serde_json::json!({}),
                                            )
                                            .await
                                    }
                                    Err(e) => Err(e),
                                }
                            })
                            .map_err(|e| {
                                CliError::Other(format!("graph refresh failed: {e}"))
                            })?;

                        if !cli.json {
                            println!(
                                "  Graph rebuilt — {} files scanned.",
                                refresh.files_scanned
                            );
                        }
                    }

                    // Write the migration marker (TOML, includes counts for verification).
                    let marker = crate::migration::MigrationMarker {
                        migrated_at: summary.migrated_at,
                        remote_url: remote.url.clone(),
                        events_migrated: summary.events_migrated,
                        issues_migrated: summary.issues_migrated,
                        versions_migrated: summary.versions_migrated,
                        escalations_migrated: summary.escalations_migrated,
                        head_version: summary.head_version.clone(),
                    };
                    marker
                        .write(&vai_dir)
                        .map_err(|e| CliError::Other(format!("failed to write migrated_at: {e}")))?;

                    if cli.json {
                        println!("{}", serde_json::to_string_pretty(&summary).unwrap());
                    } else {
                        println!("Migration complete.");
                        println!(
                            "  Events:      {}",
                            summary.events_migrated
                        );
                        println!("  Issues:      {}", summary.issues_migrated);
                        println!("  Versions:    {}", summary.versions_migrated);
                        println!("  Escalations: {}", summary.escalations_migrated);
                        if let Some(ref head) = summary.head_version {
                            println!("  HEAD:        {}", head);
                        }
                        println!(
                            "\nAll future commands will proxy to {}",
                            remote.url.cyan()
                        );
                        println!(
                            "Local .vai/ kept as backup (remove with `vai remote remove` to revert)."
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

/// Truncates a string to `max` *characters*, appending `…` if needed.
fn truncate(s: &str, max: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max - 1).collect();
        format!("{truncated}…")
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

/// Resolve an escalation by full UUID or 8-character prefix.
fn resolve_escalation(
    store: &EscalationStore,
    id_str: &str,
) -> Result<crate::escalation::Escalation, CliError> {
    // Try full UUID first.
    if let Ok(uuid) = uuid::Uuid::parse_str(id_str) {
        return store.get(uuid).map_err(CliError::Escalation);
    }
    // Fall back to prefix match.
    let all = store.list(None)?;
    let matches: Vec<_> = all
        .into_iter()
        .filter(|e| e.id.to_string().starts_with(id_str))
        .collect();
    match matches.len() {
        0 => Err(CliError::Other(format!("no escalation with ID prefix `{id_str}`"))),
        1 => Ok(matches.into_iter().next().unwrap()),
        _ => Err(CliError::Other(format!(
            "ambiguous prefix `{id_str}` — matches {} escalations",
            matches.len()
        ))),
    }
}

/// Colorize an `EscalationSeverity` for terminal output.
fn colorize_severity(
    severity: &crate::escalation::EscalationSeverity,
) -> colored::ColoredString {
    match severity {
        crate::escalation::EscalationSeverity::Critical => severity.as_str().red().bold(),
        crate::escalation::EscalationSeverity::High => severity.as_str().red(),
    }
}

// ── Remote helpers ─────────────────────────────────────────────────────────────

/// Returns a `RemoteClient` if a remote server is configured and `--local` was
/// not passed.  Returns `None` if local-only mode should be used.
fn try_remote(
    vai_dir: &std::path::Path,
    local: bool,
) -> Result<Option<crate::remote_client::RemoteClient>, CliError> {
    if local {
        return Ok(None);
    }
    let config = repo::read_config(vai_dir)?;
    match config.remote {
        Some(remote_cfg) => {
            let client = crate::remote_client::RemoteClient::new(&remote_cfg)?;
            Ok(Some(client))
        }
        None => Ok(None),
    }
}

/// Creates a single-threaded blocking Tokio runtime for async remote calls.
fn make_rt() -> Result<tokio::runtime::Runtime, CliError> {
    tokio::runtime::Runtime::new()
        .map_err(|e| CliError::Other(format!("cannot create async runtime: {e}")))
}

/// Extracts the 8-character prefix from a UUID string field in a JSON value.
fn json_id_short(val: &serde_json::Value, field: &str) -> String {
    let id = val[field].as_str().unwrap_or("????????");
    if id.len() >= 8 { id[..8].to_string() } else { id.to_string() }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::fs;
    use super::truncate;
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

    /// `truncate` must not panic on multi-byte unicode characters.
    #[test]
    fn truncate_unicode_safe() {
        // ASCII: no truncation needed
        assert_eq!(truncate("hello", 10), "hello");
        // ASCII: truncation
        assert_eq!(truncate("hello world", 8), "hello w…");
        // Em-dash is 3 bytes — byte-slicing at byte 29 would panic
        let em = "title with em\u{2014}dash right here!";
        let result = truncate(em, 20);
        assert_eq!(result.chars().count(), 20);
        assert!(result.ends_with('…'));
        // String shorter than max: returned as-is
        let short = "café";
        assert_eq!(truncate(short, 10), short);
    }

    /// `read_config` should fail gracefully on a non-repo directory.
    #[test]
    fn read_config_fails_outside_repo() {
        let tmp = TempDir::new().unwrap();
        let result = repo::read_config(&tmp.path().join(".vai"));
        assert!(result.is_err());
    }
}
