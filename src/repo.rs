//! Repository management — initialization and discovery of `.vai/` repositories.
//!
//! This module handles `vai init` (creating the `.vai/` directory structure,
//! recording the first event, and building the initial semantic graph) and
//! `find_root` (walking up the directory tree to locate an existing repository).

use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::event_log::{EventKind, EventLog};
use crate::graph::{GraphSnapshot, GraphStats};
pub use crate::version::VersionMeta;

/// Errors that can occur during repository operations.
#[derive(Debug, Error)]
pub enum RepoError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("TOML serialization error: {0}")]
    TomlSer(#[from] toml::ser::Error),

    #[error("TOML deserialization error: {0}")]
    TomlDe(#[from] toml::de::Error),

    #[error("Event log error: {0}")]
    EventLog(#[from] crate::event_log::EventLogError),

    #[error("Graph error: {0}")]
    Graph(#[from] crate::graph::GraphError),

    #[error("vai repository already initialized at {0}")]
    AlreadyInitialized(PathBuf),

    #[error("not inside a vai repository (no .vai/ directory found)")]
    NotARepo,

    #[error("{0}")]
    Other(String),
}

// ── On-disk config types ──────────────────────────────────────────────────────

/// Remote server configuration stored in `.vai/config.toml` under `[remote]`.
///
/// When present, CLI commands proxy to this server instead of operating on the
/// local `.vai/` directory directly.
///
/// Exactly one of `api_key`, `api_key_env`, or `api_key_cmd` should be set.
/// Resolution order: env var → command → direct value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteServerConfig {
    /// Base HTTP URL of the remote vai server, e.g. `https://vai.example.com`.
    pub url: String,
    /// Literal API key value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Name of an environment variable that holds the API key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    /// Shell command whose stdout is the API key (e.g. `pass show vai/api-key`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key_cmd: Option<String>,
}

impl RemoteServerConfig {
    /// Resolves the API key using the configured storage method.
    ///
    /// Evaluated in order: `api_key_env` (environment variable), `api_key_cmd`
    /// (command stdout), then `api_key` (literal value).
    ///
    /// Returns an error if no key is configured or resolution fails.
    pub fn resolve_api_key(&self) -> Result<String, ApiKeyError> {
        // 1. Environment variable reference.
        if let Some(var_name) = &self.api_key_env {
            return std::env::var(var_name)
                .map(|v| v.trim().to_string())
                .map_err(|_| ApiKeyError::EnvVarNotSet(var_name.clone()));
        }

        // 2. Command output.
        if let Some(cmd) = &self.api_key_cmd {
            let output = std::process::Command::new("sh")
                .arg("-c")
                .arg(cmd)
                .output()
                .map_err(|e| ApiKeyError::CommandFailed(format!("{cmd}: {e}")))?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                return Err(ApiKeyError::CommandFailed(format!("{cmd}: {stderr}")));
            }
            let key = String::from_utf8(output.stdout)
                .map_err(|e| ApiKeyError::CommandFailed(format!("non-UTF-8 output: {e}")))?;
            return Ok(key.trim().to_string());
        }

        // 3. Literal value.
        self.api_key
            .clone()
            .ok_or(ApiKeyError::NotConfigured)
    }
}

/// Errors that can occur when resolving the API key.
#[derive(Debug, thiserror::Error)]
pub enum ApiKeyError {
    #[error("environment variable `{0}` is not set")]
    EnvVarNotSet(String),

    #[error("api_key_cmd failed: {0}")]
    CommandFailed(String),

    #[error("no API key configured — set api_key, api_key_env, or api_key_cmd in [remote]")]
    NotConfigured,
}

/// Server bind settings stored in `.vai/config.toml` under `[server]`.
///
/// When present, these values are used as defaults for `vai server start`.
/// CLI flags (`--host`, `--port`) override these values.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalServerConfig {
    /// IP address to bind to (e.g. `0.0.0.0` or `127.0.0.1`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// TCP port to listen on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
}

/// Global server configuration stored in `~/.vai/server.toml` under `[server]`.
///
/// Applies to all repositories hosted by this server instance.  Per-repo
/// settings in `.vai/config.toml` take precedence, and CLI flags override
/// everything.
#[cfg(feature = "server")]
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GlobalServerToml {
    /// Top-level `[server]` table.
    #[serde(default)]
    pub server: GlobalServerSection,
    /// Optional `[s3]` table for S3-compatible file storage.
    #[cfg(feature = "s3")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub s3: Option<crate::storage::s3::S3Config>,
}

/// Fields within the `[server]` table of `~/.vai/server.toml`.
#[cfg(feature = "server")]
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GlobalServerSection {
    /// IP address to bind to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// TCP port to listen on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// Root directory where multi-repo storage lives (e.g. `/var/vai/repos`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage_root: Option<PathBuf>,
    /// Postgres connection URL for server-mode storage.
    ///
    /// When set the server uses `PostgresStorage`; otherwise the SQLite/filesystem
    /// backend is used. Can also be supplied via the `VAI_DATABASE_URL` environment
    /// variable or the `--database-url` CLI flag.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub database_url: Option<String>,
    /// Maximum number of Postgres connections in the pool (default: 25).
    ///
    /// Increase this if you see `pool timed out` errors under load.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub db_pool_size: Option<u32>,
    /// S3-compatible file store config from the `[s3]` table in `server.toml`.
    ///
    /// When present and `database_url` is also set, the server uses
    /// `StorageBackend::ServerWithS3` for durable file storage.
    #[cfg(feature = "s3")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub s3: Option<crate::storage::s3::S3Config>,
    /// Allowed CORS origins.
    ///
    /// Comma-separated list of allowed origins.  When absent the server
    /// defaults to `*` (all origins).  Set this to your dashboard domain in
    /// production.  Can be overridden by `VAI_CORS_ORIGINS` at runtime.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cors_origins: Option<Vec<String>>,
}

/// Reads the global server config from `~/.vai/server.toml`.
///
/// Returns `Default` (all fields `None`) if the file does not exist; propagates
/// I/O and parse errors.
#[cfg(feature = "server")]
pub fn read_global_server_config() -> Result<GlobalServerSection, RepoError> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map_err(|_| RepoError::Other("cannot determine home directory".to_string()))?;
    let path = PathBuf::from(home).join(".vai").join("server.toml");

    if !path.exists() {
        return Ok(GlobalServerSection::default());
    }

    let raw = fs::read_to_string(&path)?;
    let parsed: GlobalServerToml = toml::from_str(&raw)?;
    let mut section = parsed.server;
    // Promote the top-level `[s3]` table into the section so callers see it.
    #[cfg(feature = "s3")]
    if section.s3.is_none() {
        section.s3 = parsed.s3;
    }
    Ok(section)
}

/// Contents of `.vai/config.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoConfig {
    /// Unique repository identifier.
    pub repo_id: Uuid,
    /// Human-readable name (defaults to directory name).
    pub name: String,
    /// When the repository was initialized.
    pub created_at: DateTime<Utc>,
    /// vai version that created this repository.
    pub vai_version: String,
    /// Optional remote server configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote: Option<RemoteServerConfig>,
    /// Optional local server bind configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server: Option<LocalServerConfig>,
}

/// Contents of `vai.toml` at the project root.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaiToml {
    /// Languages to parse. Use `["auto"]` for auto-detection.
    pub languages: Vec<String>,
    /// Glob patterns to exclude from parsing.
    pub ignore: Vec<String>,
}

impl Default for VaiToml {
    fn default() -> Self {
        VaiToml {
            languages: vec!["auto".to_string()],
            ignore: vec![
                ".vai/".to_string(),
                ".git/".to_string(),
                "target/".to_string(),
                "node_modules/".to_string(),
                "*.o".to_string(),
                "*.class".to_string(),
            ],
        }
    }
}

// ── Result type ───────────────────────────────────────────────────────────────

/// Output produced by `init`.
#[derive(Debug, Serialize)]
pub struct InitResult {
    /// Root directory of the new repository.
    pub root: PathBuf,
    /// Repository configuration.
    pub config: RepoConfig,
    /// Initial version metadata.
    pub version: VersionMeta,
    /// Number of source files parsed during initialization.
    pub files_parsed: usize,
    /// Semantic graph statistics after initial parse.
    pub graph_stats: GraphStats,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Initializes a new vai repository at `root`.
///
/// Creates the `.vai/` directory structure, writes `vai.toml`, records the
/// `RepoInitialized` event, and creates the initial version (`v1`).
///
/// Returns an error if `root/.vai/` already exists.
pub fn init(root: &Path) -> Result<InitResult, RepoError> {
    let vai_dir = root.join(".vai");

    if vai_dir.exists() {
        return Err(RepoError::AlreadyInitialized(vai_dir));
    }

    // ── Directory structure ───────────────────────────────────────────────────
    fs::create_dir_all(vai_dir.join("event_log"))?;
    fs::create_dir_all(vai_dir.join("graph").join("entities"))?;
    fs::create_dir_all(vai_dir.join("workspaces"))?;
    fs::create_dir_all(vai_dir.join("versions"))?;
    fs::create_dir_all(vai_dir.join("cache").join("treesitter"))?;

    // ── vai.toml at project root ──────────────────────────────────────────────
    let vai_toml_path = root.join("vai.toml");
    let vai_toml: VaiToml = if vai_toml_path.exists() {
        let raw = fs::read_to_string(&vai_toml_path)?;
        toml::from_str(&raw)?
    } else {
        let default = VaiToml::default();
        fs::write(&vai_toml_path, toml::to_string_pretty(&default)?)?;
        default
    };

    // ── .vai/config.toml ─────────────────────────────────────────────────────
    let repo_name = root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unnamed")
        .to_string();

    let config = RepoConfig {
        repo_id: Uuid::new_v4(),
        name: repo_name,
        created_at: Utc::now(),
        vai_version: env!("CARGO_PKG_VERSION").to_string(),
        remote: None,
        server: None,
    };
    fs::write(
        vai_dir.join("config.toml"),
        toml::to_string_pretty(&config)?,
    )?;

    // ── Event log: RepoInitialized ────────────────────────────────────────────
    let mut log = EventLog::open(&vai_dir.join("event_log"))?;
    log.append(EventKind::RepoInitialized {
        repo_id: config.repo_id,
        name: config.name.clone(),
    })?;

    // ── Initial version (v1) ─────────────────────────────────────────────────
    let version = VersionMeta {
        id: None,
        version_id: "v1".to_string(),
        parent_version_id: None,
        intent: "initial repository".to_string(),
        created_by: "system".to_string(),
        created_at: config.created_at,
        merge_event_id: None,
    };
    fs::write(
        vai_dir.join("versions").join("v1.toml"),
        toml::to_string_pretty(&version)?,
    )?;

    // ── HEAD pointer ──────────────────────────────────────────────────────────
    fs::write(vai_dir.join("head"), "v1\n")?;

    // ── Semantic graph: parse all source files ────────────────────────────────
    let source_files = collect_source_files(root, &vai_toml.ignore);
    let snapshot_path = vai_dir.join("graph").join("snapshot.db");
    let snapshot = GraphSnapshot::open(&snapshot_path)?;

    let pb = ProgressBar::new(source_files.len() as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{prefix:.bold} [{bar:40}] {pos}/{len} {msg}")
            .unwrap_or_else(|_| ProgressStyle::default_bar())
            .progress_chars("=> "),
    );
    pb.set_prefix("Parsing");

    let mut files_parsed = 0usize;
    for file_path in &source_files {
        let rel = file_path
            .strip_prefix(root)
            .unwrap_or(file_path)
            .to_string_lossy()
            .into_owned();
        pb.set_message(rel.clone());
        if let Ok(source) = fs::read(file_path) {
            // Errors on individual files are skipped — best-effort parse.
            let _ = snapshot.update_file(&rel, &source);
            files_parsed += 1;
        }
        pb.inc(1);
    }
    pb.finish_and_clear();

    let graph_stats = snapshot.stats()?;

    Ok(InitResult {
        root: root.to_owned(),
        config,
        version,
        files_parsed,
        graph_stats,
    })
}

/// Result of a graph refresh operation.
#[derive(Debug, Serialize)]
pub struct RefreshResult {
    /// Number of source files scanned.
    pub files_scanned: usize,
    /// Graph statistics after the refresh.
    pub graph_stats: GraphStats,
}

/// Re-scans all source files and rebuilds the semantic graph.
///
/// Reads ignore patterns from `vai.toml`, collects all matching source files,
/// and re-parses them into the graph snapshot. Existing graph data is replaced.
pub fn refresh_graph(root: &Path) -> Result<RefreshResult, RepoError> {
    let vai_dir = root.join(".vai");
    if !vai_dir.exists() {
        return Err(RepoError::NotARepo);
    }

    // Read ignore patterns from vai.toml.
    let vai_toml_path = root.join("vai.toml");
    let vai_toml: VaiToml = if vai_toml_path.exists() {
        let raw = fs::read_to_string(&vai_toml_path)?;
        toml::from_str(&raw)?
    } else {
        VaiToml::default()
    };

    let source_files = collect_source_files(root, &vai_toml.ignore);
    let snapshot_path = vai_dir.join("graph").join("snapshot.db");
    let snapshot = GraphSnapshot::open(&snapshot_path)?;

    let mut files_scanned = 0usize;
    for file_path in &source_files {
        let rel = file_path
            .strip_prefix(root)
            .unwrap_or(file_path)
            .to_string_lossy()
            .into_owned();
        if let Ok(source) = fs::read(file_path) {
            let _ = snapshot.update_file(&rel, &source);
            files_scanned += 1;
        }
    }

    let graph_stats = snapshot.stats()?;

    Ok(RefreshResult {
        files_scanned,
        graph_stats,
    })
}

/// Walks up the directory tree from `start` to find the root of a vai repository.
///
/// Returns the path containing `.vai/`, or `None` if not found.
pub fn find_root(start: &Path) -> Option<PathBuf> {
    let mut current = start.to_owned();
    loop {
        if current.join(".vai").is_dir() {
            return Some(current);
        }
        match current.parent() {
            Some(p) => current = p.to_owned(),
            None => return None,
        }
    }
}

/// Reads the current HEAD version string from `.vai/head`.
pub fn read_head(vai_dir: &Path) -> Result<String, RepoError> {
    let head = fs::read_to_string(vai_dir.join("head"))?;
    Ok(head.trim().to_string())
}

/// Reads the repository configuration from `.vai/config.toml`.
pub fn read_config(vai_dir: &Path) -> Result<RepoConfig, RepoError> {
    let raw = fs::read_to_string(vai_dir.join("config.toml"))?;
    Ok(toml::from_str(&raw)?)
}

/// Writes the repository configuration to `.vai/config.toml`.
pub fn write_config(vai_dir: &Path, config: &RepoConfig) -> Result<(), RepoError> {
    fs::write(vai_dir.join("config.toml"), toml::to_string_pretty(config)?)?;
    Ok(())
}

// ── Source file collection ─────────────────────────────────────────────────────

/// Recursively collects all supported source files under `root`, respecting ignore patterns.
///
/// Supported extensions: `.rs`, `.ts`, `.tsx`, `.js`, `.jsx`.
/// Respects `.gitignore`, `.vaignore`, and `ignore` patterns from `vai.toml`.
pub(crate) fn collect_source_files(root: &Path, ignore: &[String]) -> Vec<PathBuf> {
    crate::ignore_rules::collect_source_files(root, ignore)
}

/// Collects all files under `root` for migration, respecting ignore rules.
///
/// Returns every regular file regardless of extension — suitable for a full
/// project upload via `vai remote migrate` (PRD 12.3).
/// Respects `.gitignore`, `.vaignore`, and `vai.toml` ignore patterns.
pub fn list_migration_files(root: &Path) -> Vec<PathBuf> {
    let vai_toml_path = root.join("vai.toml");
    let vai_toml: VaiToml = if vai_toml_path.exists() {
        let raw = fs::read_to_string(&vai_toml_path).unwrap_or_default();
        toml::from_str(&raw).unwrap_or_default()
    } else {
        VaiToml::default()
    };
    crate::ignore_rules::collect_all_files(root, &vai_toml.ignore)
}

/// Prints the init result to stdout in human-readable format.
pub fn print_init_result(result: &InitResult) {
    println!(
        "{} Initialized vai repository {}",
        "✓".green().bold(),
        result.config.name.bold()
    );
    println!(
        "  Repository ID  : {}",
        result.config.repo_id.to_string().dimmed()
    );
    println!("  Initial version: v1 \"initial repository\"");
    println!("  Directory      : .vai/");
    println!();
    println!(
        "  Semantic graph : {} entities, {} relationships ({} files parsed)",
        result.graph_stats.entity_count,
        result.graph_stats.relationship_count,
        result.files_parsed,
    );
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn init_creates_directory_structure() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let result = init(root).unwrap();

        // Check .vai/ subdirectories exist.
        let vai = root.join(".vai");
        assert!(vai.join("event_log").is_dir(), "event_log/ missing");
        assert!(vai.join("graph").is_dir(), "graph/ missing");
        assert!(vai.join("graph").join("entities").is_dir(), "graph/entities/ missing");
        assert!(vai.join("workspaces").is_dir(), "workspaces/ missing");
        assert!(vai.join("versions").is_dir(), "versions/ missing");
        assert!(vai.join("cache").join("treesitter").is_dir(), "cache/treesitter/ missing");

        // Check config files.
        assert!(vai.join("config.toml").exists(), "config.toml missing");
        assert!(vai.join("head").exists(), "head missing");
        assert!(vai.join("versions").join("v1.toml").exists(), "v1.toml missing");
        assert!(root.join("vai.toml").exists(), "vai.toml missing");

        // HEAD points to v1.
        let head = read_head(&vai).unwrap();
        assert_eq!(head, "v1");

        // Result contains the right name.
        let dir_name = root.file_name().unwrap().to_string_lossy();
        assert_eq!(result.config.name, dir_name.as_ref());
    }

    #[test]
    fn init_records_repo_initialized_event() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        init(root).unwrap();

        let log = EventLog::open(&root.join(".vai").join("event_log")).unwrap();
        let events = log.query_by_type("RepoInitialized").unwrap();
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn init_twice_returns_error() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        init(root).unwrap();
        let err = init(root).unwrap_err();
        assert!(matches!(err, RepoError::AlreadyInitialized(_)));
    }

    #[test]
    fn find_root_walks_up() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        init(root).unwrap();

        // Create a nested directory and start search from there.
        let nested = root.join("src").join("deep");
        fs::create_dir_all(&nested).unwrap();

        let found = find_root(&nested).unwrap();
        assert_eq!(found, root);
    }

    #[test]
    fn find_root_returns_none_outside_repo() {
        let tmp = TempDir::new().unwrap();
        let result = find_root(tmp.path());
        assert!(result.is_none());
    }

    #[test]
    fn init_builds_graph_for_rust_sources() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Write a sample Rust file before initializing.
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("src").join("lib.rs"),
            b"pub fn hello() -> &'static str { \"hello\" }\npub struct Greeter;\n",
        )
        .unwrap();

        let result = init(root).unwrap();

        assert_eq!(result.files_parsed, 1, "expected 1 file parsed");
        assert!(result.graph_stats.entity_count >= 2, "expected at least 2 entities");
        assert_eq!(result.graph_stats.file_count, 1);

        // The snapshot should be queryable.
        use crate::graph::GraphSnapshot;
        let snapshot =
            GraphSnapshot::open(&root.join(".vai").join("graph").join("snapshot.db")).unwrap();
        let entities = snapshot.search_entities_by_name("hello").unwrap();
        assert!(!entities.is_empty(), "expected entity 'hello' in graph");
    }

    #[cfg(feature = "server")]
    #[test]
    fn global_server_config_returns_defaults_when_missing() {
        // Point HOME at a temp dir with no server.toml.
        let tmp = TempDir::new().unwrap();
        std::env::set_var("HOME", tmp.path());
        let cfg = read_global_server_config().unwrap();
        assert!(cfg.host.is_none());
        assert!(cfg.port.is_none());
        assert!(cfg.storage_root.is_none());
    }

    #[cfg(feature = "server")]
    #[test]
    fn global_server_config_parses_server_toml() {
        let tmp = TempDir::new().unwrap();
        let vai_dir = tmp.path().join(".vai");
        fs::create_dir_all(&vai_dir).unwrap();
        fs::write(
            vai_dir.join("server.toml"),
            b"[server]\nhost = \"0.0.0.0\"\nport = 9000\nstorage_root = \"/var/vai/repos\"\n",
        )
        .unwrap();
        std::env::set_var("HOME", tmp.path());
        let cfg = read_global_server_config().unwrap();
        assert_eq!(cfg.host.as_deref(), Some("0.0.0.0"));
        assert_eq!(cfg.port, Some(9000));
        assert_eq!(cfg.storage_root, Some(PathBuf::from("/var/vai/repos")));
    }

}
