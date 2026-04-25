//! CLI command definitions and dispatch.
//!
//! Uses `clap` derive API to define all vai subcommands.
//! Each command handler lives in its own submodule.

mod agent_cmd;
pub mod agent_loop;
mod dashboard;
mod escalation;
mod graph;
mod init;
mod issue;
mod login;
mod merge;
mod remote;
mod server_cmd;
mod version;
mod work_queue;
mod workspace;

pub use init::run_init;

use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use colored::Colorize;
use serde::Serialize;
use thiserror::Error;

use crate::agent;
use crate::auth;
use crate::clone as remote_clone;
use crate::diff;
use crate::graph::{GraphStats};
use crate::scope_inference;
use crate::escalation::{EscalationStore};
use crate::issue::{IssueFilter, IssueStore, IssuePriority, IssueStatus};
use crate::merge as crate_merge;
use crate::remote_workspace;
use crate::repo;
#[cfg(feature = "server")]
use crate::server;
use crate::remote as remote_ops;
use crate::remote_diff;
use crate::status as remote_status;
use crate::version::VersionMeta;
use crate::version as crate_version;
use crate::work_queue as crate_work_queue;
use crate::workspace as crate_workspace;
use crate::workspace::WorkspaceMeta;

/// Errors that can occur during CLI execution.
#[derive(Debug, Error)]
pub enum CliError {
    #[error("Repository error: {0}")]
    Repo(#[from] repo::RepoError),

    #[error("Graph error: {0}")]
    Graph(#[from] crate::graph::GraphError),

    #[error("Workspace error: {0}")]
    Workspace(#[from] crate_workspace::WorkspaceError),

    #[error("Diff error: {0}")]
    Diff(#[from] diff::DiffError),

    #[error("Merge error: {0}")]
    Merge(#[from] crate_merge::MergeError),

    #[error("Version error: {0}")]
    Version(#[from] crate_version::VersionError),

    #[cfg(feature = "server")]
    #[error("Server error: {0}")]
    Server(#[from] server::ServerError),

    #[error("Auth error: {0}")]
    Auth(#[from] auth::AuthError),

    #[error("Clone error: {0}")]
    Clone(#[from] remote_clone::CloneError),

    #[error("Remote error: {0}")]
    RemoteOps(#[from] remote_ops::RemoteError),

    #[error("Status error: {0}")]
    Status(#[from] remote_status::StatusError),

    #[error("Diff error (remote): {0}")]
    RemoteDiff(#[from] remote_diff::RemoteDiffError),

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
    WorkQueue(#[from] crate_work_queue::WorkQueueError),

    #[error("Remote client error: {0}")]
    Remote(#[from] crate::remote_client::RemoteClientError),

    #[error("Agent error: {0}")]
    Agent(#[from] agent::AgentError),

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
    Init {
        /// Initialize locally only — do not register on the server or push.
        #[arg(long)]
        local_only: bool,
        /// Register on the server but skip the initial push.
        #[arg(long)]
        no_push: bool,
        /// Override the inferred repository name (defaults to directory basename).
        #[arg(long)]
        remote_name: Option<String>,
    },
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
    /// Show differences between local files and the server, or between two versions.
    ///
    /// With no arguments, compares the local working directory against the
    /// server's current state (equivalent to `git diff` vs remote HEAD).
    ///
    /// With a single `<path>` argument, diffs that specific file against the
    /// server.
    ///
    /// With two version arguments (`<version_a> <version_b>`), shows the
    /// semantic diff between those two versions (local mode only).
    ///
    /// Uses the remote configured via `vai remote add`, or explicit
    /// --from/--key/--repo flags.
    Diff {
        /// File path to diff (local vs server), or first version ID for
        /// version-to-version semantic diff.
        #[arg()]
        arg1: Option<String>,
        /// Second version ID for version-to-version semantic diff.
        #[arg()]
        arg2: Option<String>,
        /// Remote server URL. Uses configured remote if omitted.
        #[arg(long)]
        from: Option<String>,
        /// API key for the remote server. Required when --from is set.
        #[arg(long)]
        key: Option<String>,
        /// Repository name on the server. Required when --from is set.
        #[arg(long)]
        repo: Option<String>,
    },
    /// Query and inspect the semantic graph.
    #[command(subcommand)]
    Graph(GraphCommands),
    /// Manage the vai HTTP server.
    #[cfg(feature = "server")]
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
    /// Pull the latest changes from the remote server into the local working directory.
    ///
    /// Uses the remote configured via `vai remote add`, or explicit --from/--key/--repo flags.
    ///
    /// With `--force`, downloads the full file tarball and replaces all tracked files,
    /// preserving ignored paths (`.vai/`, `.git/`, `node_modules/`, etc.).  Use this
    /// when local files may have diverged from the server (e.g. after `vai agent download`
    /// or external edits).
    Pull {
        /// Remote server URL (e.g. `http://localhost:7865`). Uses configured remote if omitted.
        #[arg(long)]
        from: Option<String>,
        /// API key for the remote server. Required when --from is set.
        #[arg(long)]
        key: Option<String>,
        /// Repository name on the server. Required when --from is set.
        #[arg(long)]
        repo: Option<String>,
        /// Force a full re-download, replacing all tracked files with the server's current state.
        #[arg(long)]
        force: bool,
    },
    /// Push local changes to the remote server as a new version.
    ///
    /// Compares the local working directory against the server, creates a
    /// temporary workspace, uploads all modified files, and submits them for
    /// merge. Updates `.vai/head` to the resulting version on success.
    ///
    /// Uses the remote configured via `vai remote add`, or explicit
    /// --to/--key/--repo flags.
    Push {
        /// Commit message / intent describing this push. Required.
        #[arg(short = 'm', long)]
        message: Option<String>,
        /// Remote server URL (e.g. `http://localhost:7865`). Uses configured remote if omitted.
        #[arg(long)]
        to: Option<String>,
        /// API key for the remote server. Required when --to is set.
        #[arg(long)]
        key: Option<String>,
        /// Repository name on the server. Required when --to is set.
        #[arg(long)]
        repo: Option<String>,
        /// Show what would be pushed without actually pushing.
        #[arg(long)]
        dry_run: bool,
        /// Allow the push to delete server-side files that are absent locally.
        ///
        /// By default, vai aborts and lists the files that would be deleted.
        /// Pass --force to confirm you intend the deletions and proceed.
        #[arg(long)]
        force: bool,
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
    /// Agent workflow commands for autonomous agent loops.
    #[command(subcommand)]
    Agent(AgentCommands),
    /// Authenticate with a vai server and save credentials to `~/.vai/credentials.toml`.
    ///
    /// Opens the default browser to the dashboard's `/cli-auth` page, which
    /// mints an API key and posts it back to an ephemeral localhost port.
    ///
    /// Falls back to device code mode (`--device`) automatically in headless
    /// environments (no `DISPLAY` / `WAYLAND_DISPLAY` on Linux, or when the
    /// browser cannot be opened).
    Login {
        /// Base URL of the vai server.
        ///
        /// Defaults to the `VAI_SERVER_URL` environment variable, or the
        /// compile-time `VAI_DEFAULT_SERVER_URL` constant
        /// (`https://vai-server-polished-feather-2668.fly.dev` in release builds).
        #[arg(long)]
        server_url: Option<String>,
        /// Base URL of the dashboard.
        ///
        /// Defaults to the `VAI_DASHBOARD_URL` environment variable, or the
        /// compile-time `VAI_DEFAULT_DASHBOARD_URL` constant (`http://localhost:3000`
        /// in release builds).
        #[arg(long)]
        dashboard_url: Option<String>,
        /// Use the device code flow instead of opening a browser.
        ///
        /// Prints a short code and a URL; the user enters the code in their
        /// browser.  Useful in headless / SSH environments.
        #[arg(long)]
        device: bool,
        /// Human-readable name for the minted API key (default: `CLI on <hostname>`).
        #[arg(long)]
        name: Option<String>,
    },
}

/// Agent workflow subcommands.
///
/// These commands handle all vai server interaction for autonomous agent loops.
/// They are designed to be composed into a minimal shell loop; see PRD 20.
#[derive(Debug, Subcommand)]
pub enum AgentCommands {
    /// Initialize agent configuration in the current directory.
    ///
    /// Creates `.vai/agent.toml` with server URL and repo name.
    /// Falls back to `VAI_SERVER_URL` / `VAI_REPO` environment variables.
    /// The API key is **never** written to disk — supply it via `VAI_API_KEY`.
    Init {
        /// Base URL of the vai server (e.g. `https://vai.example.com`).
        #[arg(long)]
        server: Option<String>,
        /// Repository name on the server.
        #[arg(long)]
        repo: Option<String>,
        /// Path to the prompt template file (default: `.vai/prompt.md`).
        #[arg(long)]
        prompt_template: Option<String>,
    },

    /// Query the work queue and atomically claim the highest-priority issue.
    ///
    /// On success, writes `.vai/agent-state.json` with the issue ID, workspace
    /// ID, and phase.  If state already exists from a previous (possibly
    /// crashed) iteration, prints the current issue and exits 0 without
    /// re-claiming.
    ///
    /// Exits 0 when work was claimed (or resumed).
    /// Exits 1 when no work is available — enabling the shell loop pattern:
    ///
    /// ```sh
    /// while vai agent claim; do
    ///   vai agent download ./work
    ///   vai agent submit ./work || vai agent reset
    ///   rm -rf ./work
    /// done
    /// ```
    Claim {
        /// Override the server URL from config / env.
        #[arg(long)]
        server: Option<String>,
        /// Override the repository name from config / env.
        #[arg(long)]
        repo: Option<String>,
    },

    /// Download the repo tarball into a local working directory.
    ///
    /// Reads agent state from `.vai/agent-state.json` (written by `vai agent
    /// claim`), fetches the current repository snapshot from the server, and
    /// extracts it into `<dir>`.  A file manifest is saved to
    /// `.vai/download-manifest.json` for use by `vai agent submit`.
    ///
    /// Advances the agent phase to `downloaded`.
    Download {
        /// Directory to extract repository files into (created if absent).
        dir: std::path::PathBuf,
    },

    /// Display the details of the currently claimed issue.
    ///
    /// Reads the issue ID from `.vai/agent-state.json` and fetches full
    /// details from `GET /api/issues/:id` on the configured server.
    ///
    /// By default prints a human-readable summary (title, status, priority,
    /// description snippet, acceptance criteria, recent comments).
    /// With `--json` prints the raw JSON response for piping to agents.
    ///
    /// Exits 1 if no issue is currently claimed (no agent state).
    Issue,

    /// Print the status of the current agent iteration.
    ///
    /// Reads `.vai/agent-state.json` and displays the current issue title,
    /// workspace ID, phase, and elapsed time since the issue was claimed.
    ///
    /// Exits 1 if no agent state exists (i.e. no issue is currently claimed).
    Status,

    /// Discard the current workspace, reopen the issue, and clear state.
    ///
    /// Calls `DELETE /api/workspaces/:id` on the server, which atomically:
    /// - marks the workspace as `Discarded`
    /// - transitions the linked issue back to `Open`
    ///
    /// Use this after a failed or aborted iteration to return the issue to
    /// the work queue so it can be claimed again.
    ///
    /// Exits 1 if no agent state exists or the server call fails.
    Reset,

    /// Build a prompt from a template and the current issue details.
    ///
    /// Reads the template from `.vai/prompt.md` (default) or the path
    /// configured as `prompt_template` in `.vai/agent.toml`.  Replaces
    /// `{{issue}}` in the template with the JSON issue details fetched from
    /// the server.  Prints the completed prompt to stdout.
    ///
    /// If no template file exists, prints a sensible built-in default prompt
    /// containing the issue details.
    ///
    /// Exits 1 if no issue is currently claimed (no agent state).
    Prompt {
        /// Override the template file path.
        #[arg(long)]
        template: Option<String>,
    },

    /// Upload work, submit the workspace, close the issue, and clear state.
    ///
    /// Steps performed in order:
    /// 1. Build a gzip tarball of `<dir>` (excluding `.vai/`, `.git/`, `target/`,
    ///    `node_modules/`, `dist/`, `__pycache__/`, and any patterns configured
    ///    under `[ignore]` in `.vai/agent.toml`).
    /// 2. `POST /api/workspaces/:id/upload-snapshot` — upload the tarball.
    /// 3. `POST /api/workspaces/:id/submit` — trigger server-side merge.
    /// 4. `POST /api/issues/:id/close` — close the issue as `resolved`.
    /// 5. Clear `.vai/agent-state.json`.
    ///
    /// Agent state is preserved if any step fails so you can retry.
    ///
    /// Exits 0 on success, 1 on any error (state preserved for retry).
    /// Exits 3 if the workspace is empty (issue already resolved) and
    /// `--close-if-empty` is not set.
    Submit {
        /// Directory containing the agent's modified working tree.
        dir: std::path::PathBuf,
        /// When the workspace is empty (no file changes), automatically close
        /// the issue as resolved and exit 0 instead of exiting 3.
        ///
        /// Use this in agent loop scripts to avoid an infinite claim/download
        /// cycle when the requested fix is already in place.  Without this
        /// flag the caller receives exit 3 and can decide independently.
        #[arg(long, default_value_t = false)]
        close_if_empty: bool,
    },

    /// Run quality checks configured in `.vai/agent.toml`.
    ///
    /// Reads `checks.commands` from the agent config and runs each command
    /// sequentially with `<dir>` as the working directory.
    ///
    /// Exits 0 if all checks pass (or if no checks are configured).
    /// Exits 1 if any check fails, printing a structured error summary to
    /// stderr formatted for consumption by AI agents:
    ///
    /// ```text
    /// === <command> ===
    /// <stdout + stderr output>
    /// ```
    ///
    /// Configure checks in `.vai/agent.toml`:
    /// ```toml
    /// [checks]
    /// commands = ["cargo build", "cargo test", "cargo clippy -- -D warnings"]
    /// ```
    Verify {
        /// Working directory in which to run checks (the agent's work tree).
        dir: std::path::PathBuf,
    },

    /// Run setup commands from `[agent].setup` in `<dir>/vai.toml`.
    ///
    /// Reads `setup` from the project-level `vai.toml` in `<dir>` and runs
    /// each command sequentially in that directory.  Used by agent loops to
    /// install dependencies (e.g. `pnpm install`) before invoking the agent.
    ///
    /// Exits 0 if all setup commands succeed or if no setup is configured.
    /// Exits 1 if any setup command fails.
    Setup {
        /// Directory containing the downloaded repo (and its `vai.toml`).
        dir: std::path::PathBuf,
    },

    /// Manage agent loop configurations.
    #[command(subcommand)]
    Loop(LoopCommands),
}

/// `vai agent loop` subcommands.
#[derive(Debug, Subcommand)]
pub enum LoopCommands {
    /// Generate a new agent loop configuration.
    Init {
        /// Agent type / name (`claude-code`, `codex`, `custom`). Prompts if omitted.
        #[arg(long)]
        agent: Option<String>,
        /// Project type (`frontend-react`, `backend-rust`, `backend-typescript`, `generic`).
        /// Auto-detected when omitted.
        #[arg(long)]
        project_type: Option<String>,
        /// Force Docker mode (run agent in an isolated container).
        #[arg(long, overrides_with = "no_docker")]
        docker: bool,
        /// Force bare-shell mode (run agent directly on the host).
        #[arg(long, overrides_with = "docker")]
        no_docker: bool,
        /// Overwrite an existing loop configuration if present.
        #[arg(long)]
        overwrite: bool,
        /// Name for this loop configuration (for multi-config repos).
        #[arg(long)]
        name: Option<String>,
    },

    /// Run a configured agent loop.
    Run {
        /// Name of the loop configuration to run.
        #[arg(long)]
        name: Option<String>,
    },

    /// List configured agent loops.
    List,
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
        /// Issue ID that blocks this issue (can be repeated).
        #[arg(long)]
        blocked_by: Vec<String>,
    },
    /// List issues with optional filters.
    List {
        /// Filter by status. `open` matches all non-closed statuses
        /// (open, in_progress, resolved). Use `in_progress`, `resolved`, or
        /// `closed` to match a single state exactly.
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
        /// Only show issues blocked by this issue ID.
        #[arg(long)]
        blocked_by: Option<String>,
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
        /// Mark this issue as blocked by another issue (can be repeated).
        #[arg(long)]
        blocked_by: Vec<String>,
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
#[cfg(feature = "server")]
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
        /// `~/.vai/server.toml`. Also read from the `DATABASE_URL` environment
        /// variable (standard PaaS convention; lower priority than this flag).
        #[arg(long)]
        database_url: Option<String>,
        /// Maximum number of Postgres connections in the pool (default: 25).
        ///
        /// Increase this value if the server returns `pool timed out` errors
        /// under concurrent load (e.g. many agents + dashboard polling).
        #[arg(long)]
        db_pool_size: Option<u32>,
        /// Comma-separated list of allowed CORS origins (e.g. `https://app.example.com`).
        ///
        /// When unset the server allows all origins (`*`), which is suitable for
        /// development.  In production set this to the exact origin(s) of your
        /// dashboard.  Overrides `[server].cors_origins` in `~/.vai/server.toml`.
        /// Can also be set via `VAI_CORS_ORIGINS`.
        #[arg(long)]
        cors_origins: Option<String>,
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
    /// Set the remote server URL.
    ///
    /// Authentication is provided by `~/.vai/credentials.toml` (run `vai login`
    /// to set up credentials) or the `VAI_API_KEY` environment variable.
    Add {
        /// Base URL of the remote vai server (e.g. `https://vai.example.com`).
        url: String,
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
    /// Re-link this repository to the server, correcting a stale `repo_id`.
    ///
    /// Fetches the server's canonical id for the configured repo name and
    /// updates `repo_id` in `.vai/config.toml`.  Use this when `vai remote
    /// status` warns about a repo_id mismatch (e.g. after the server was
    /// re-initialised).
    Link {
        /// Apply the update without asking for confirmation.
        #[arg(long)]
        force: bool,
    },
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
        Some(Commands::Init { local_only, no_push, remote_name }) => {
            init::handle(local_only, no_push, remote_name, cli.json)?;
        }
        Some(Commands::Graph(graph_cmd)) => {
            graph::handle(graph_cmd, cli.json, cli.quiet)?;
        }
        Some(Commands::Workspace(ws_cmd)) => {
            workspace::handle(ws_cmd, cli.json, cli.quiet, cli.local)?;
        }
        Some(Commands::Log { limit }) => {
            version::handle_log(limit, cli.json)?;
        }
        Some(Commands::Show { version: version_id }) => {
            version::handle_show(version_id, cli.json)?;
        }
        Some(Commands::Diff { arg1, arg2, from, key, repo }) => {
            version::handle_diff(arg1, arg2, from, key, repo, cli.json)?;
        }
        Some(Commands::Status { others }) => {
            version::handle_status(others, cli.json, cli.local)?;
        }
        Some(Commands::Rollback { version, force, entity }) => {
            version::handle_rollback(version, force, entity, cli.json)?;
        }
        #[cfg(feature = "server")]
        Some(Commands::Server(server_cmd)) => {
            server_cmd::handle(server_cmd, cli.json)?;
        }
        Some(Commands::Clone { url, dest, key }) => {
            remote::handle_clone(url, dest, key, cli.json)?;
        }
        Some(Commands::Pull { from, key, repo, force }) => {
            remote::handle_pull(from, key, repo, force, cli.json)?;
        }
        Some(Commands::Push { message, to, key, repo, dry_run, force }) => {
            remote::handle_push(message, to, key, repo, dry_run, force, cli.json)?;
        }
        Some(Commands::Sync) => {
            remote::handle_sync(cli.json)?;
        }
        Some(Commands::Issue(issue_cmd)) => {
            issue::handle(issue_cmd, cli.json, cli.local)?;
        }
        Some(Commands::Escalations(esc_cmd)) => {
            escalation::handle(esc_cmd, cli.json)?;
        }
        Some(Commands::WorkQueue(wq_cmd)) => {
            work_queue::handle(wq_cmd, cli.json, cli.local)?;
        }
        Some(Commands::Merge(merge_cmd)) => {
            merge::handle(merge_cmd, cli.json)?;
        }
        Some(Commands::Dashboard { server, key }) => {
            dashboard::handle(server, key)?;
        }
        Some(Commands::Remote(remote_cmd)) => {
            remote::handle_remote(remote_cmd, cli.json)?;
        }
        Some(Commands::Agent(agent_cmd)) => {
            agent_cmd::handle(agent_cmd, cli.json)?;
        }
        Some(Commands::Login { server_url, dashboard_url, device, name }) => {
            login::handle(server_url, dashboard_url, device, name)?;
        }
    }
    Ok(())
}

/// Machine-readable output for `vai status`.
#[derive(Debug, Serialize)]
pub(crate) struct StatusOutput {
    repo_name: String,
    head_version: VersionMeta,
    graph_stats: GraphStats,
    workspaces: Vec<WorkspaceMeta>,
    pending_conflicts: u32,
}

/// Prints graph statistics in the standard human-readable format.
pub(super) fn print_graph_stats(stats: &GraphStats) {
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
pub(super) fn truncate(s: &str, max: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max - 1).collect();
        format!("{truncated}…")
    }
}

/// Returns a human-readable age string (e.g. "5m ago", "2h ago").
pub(super) fn format_age(dt: DateTime<Utc>) -> String {
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
pub(super) fn whoami() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "human".to_string())
}

/// Resolve an issue by full UUID or 8-character prefix.
pub(super) fn resolve_issue(
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
pub(super) fn print_issue_summary(issue: &crate::issue::Issue) {
    println!("  Title    : {}", issue.title);
    println!("  Status   : {}", colorize_status(&issue.status));
    println!("  Priority : {}", colorize_priority(&issue.priority));
    if !issue.labels.is_empty() {
        println!("  Labels   : {}", issue.labels.join(", "));
    }
}

/// Colorize an `IssueStatus` for terminal output.
pub(super) fn colorize_status(status: &IssueStatus) -> colored::ColoredString {
    match status {
        IssueStatus::Open => status.as_str().green(),
        IssueStatus::InProgress => status.as_str().yellow(),
        IssueStatus::Resolved => status.as_str().cyan(),
        IssueStatus::Closed => status.as_str().dimmed(),
    }
}

/// Colorize an `IssuePriority` for terminal output.
pub(super) fn colorize_priority(priority: &IssuePriority) -> colored::ColoredString {
    match priority {
        IssuePriority::Critical => priority.as_str().red().bold(),
        IssuePriority::High => priority.as_str().red(),
        IssuePriority::Medium => priority.as_str().yellow(),
        IssuePriority::Low => priority.as_str().normal(),
    }
}

/// Resolve an escalation by full UUID or 8-character prefix.
pub(super) fn resolve_escalation(
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
pub(super) fn colorize_severity(
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
///
/// As a side effect, prints a warning to stderr when the local `repo_id` in
/// `.vai/config.toml` does not match the id the server has on record for the
/// same repo name (drift detection).
pub(super) fn try_remote(
    vai_dir: &std::path::Path,
    local: bool,
) -> Result<Option<crate::remote_client::RemoteClient>, CliError> {
    if local {
        return Ok(None);
    }
    let config = repo::read_config(vai_dir)?;
    match config.remote {
        Some(remote_cfg) => {
            let (api_key, _) = crate::credentials::load_api_key()
                .map_err(|e| CliError::Other(format!("credentials error: {e}")))?;
            let client = crate::remote_client::RemoteClient::new(&remote_cfg.url, &api_key);
            // Best-effort drift check — print a warning if the local repo_id has drifted
            // from the server's id for this repo name.  A small temporary runtime is used
            // so the check can be async without changing the sync signature of try_remote.
            let repo_name = remote_cfg.repo_name.as_deref().unwrap_or(&config.name).to_string();
            let local_repo_id = config.repo_id;
            if let Ok(rt) = tokio::runtime::Runtime::new() {
                rt.block_on(warn_if_repo_id_drifted(&client, &repo_name, local_repo_id));
            }
            Ok(Some(client))
        }
        None => Ok(None),
    }
}

/// Checks whether the local `repo_id` matches the server's id for the given repo name.
///
/// If they differ, prints a clear warning to stderr with a recovery command.
/// Best-effort — all errors are silently ignored so they never interrupt the
/// main command.  Returns `true` when drift was detected.
pub(super) async fn warn_if_repo_id_drifted(
    client: &crate::remote_client::RemoteClient,
    repo_name: &str,
    local_repo_id: uuid::Uuid,
) -> bool {
    let path = format!("/api/repos/{}", repo_name);
    let info: serde_json::Value = match client.get(&path).await {
        Ok(v) => v,
        Err(_) => return false,
    };
    let server_id_str = match info["id"].as_str() {
        Some(s) => s,
        None => return false,
    };
    let server_id = match server_id_str.parse::<uuid::Uuid>() {
        Ok(id) => id,
        Err(_) => return false,
    };
    if server_id != local_repo_id {
        eprintln!(
            "{} repo_id mismatch for '{}': local {} ≠ server {}",
            "warning:".yellow().bold(),
            repo_name,
            &local_repo_id.to_string()[..8],
            &server_id.to_string()[..8],
        );
        eprintln!(
            "  Your .vai/config.toml has a stale repo_id. Run `vai remote link` to fix."
        );
        return true;
    }
    false
}

/// Creates a single-threaded blocking Tokio runtime for async remote calls.
pub(super) fn make_rt() -> Result<tokio::runtime::Runtime, CliError> {
    tokio::runtime::Runtime::new()
        .map_err(|e| CliError::Other(format!("cannot create async runtime: {e}")))
}

/// Extracts the 8-character prefix from a UUID string field in a JSON value.
pub(super) fn json_id_short(val: &serde_json::Value, field: &str) -> String {
    let id = val[field].as_str().unwrap_or("????????");
    if id.len() >= 8 { id[..8].to_string() } else { id.to_string() }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::fs;
    use super::truncate;
    use tempfile::TempDir;

    use crate::{graph::GraphSnapshot, repo, version};

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
        let ws_list = crate::workspace::list(&vai_dir).unwrap();
        assert!(ws_list.is_empty());

        // Create two workspaces.
        crate::workspace::create(&vai_dir, "fix auth bug", &head).unwrap();
        crate::workspace::create(&vai_dir, "add logging", &head).unwrap();

        let ws_list = crate::workspace::list(&vai_dir).unwrap();
        assert_eq!(ws_list.len(), 2);

        // Active workspace should be the most recently created one.
        let active_id = crate::workspace::active_id(&vai_dir);
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
