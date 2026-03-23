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
}

// ── On-disk config types ──────────────────────────────────────────────────────

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

// ── Source file collection ─────────────────────────────────────────────────────

/// Recursively collects all supported source files under `root`, respecting ignore patterns.
///
/// Supported extensions: `.rs`, `.ts`, `.tsx`, `.js`, `.jsx`.
fn collect_source_files(root: &Path, ignore: &[String]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_recursive(root, ignore, &mut files);
    files
}

fn collect_recursive(dir: &Path, ignore: &[String], files: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        if path_should_ignore(&name, ignore) {
            continue;
        }
        if path.is_dir() {
            collect_recursive(&path, ignore, files);
        } else if matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("rs" | "ts" | "tsx" | "js" | "jsx")
        ) {
            files.push(path);
        }
    }
}

/// Returns `true` if a file or directory `name` matches any ignore pattern.
fn path_should_ignore(name: &str, ignore: &[String]) -> bool {
    for pattern in ignore {
        let p = pattern.trim_end_matches('/');
        if let Some(ext) = p.strip_prefix("*.") {
            if name.ends_with(&format!(".{ext}")) {
                return true;
            }
        } else if name == p {
            return true;
        }
    }
    false
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

    #[test]
    fn path_should_ignore_matches_dir_and_glob() {
        let ignore = vec![".vai/".to_string(), "target/".to_string(), "*.o".to_string()];
        assert!(path_should_ignore(".vai", &ignore));
        assert!(path_should_ignore("target", &ignore));
        assert!(path_should_ignore("foo.o", &ignore));
        assert!(!path_should_ignore("src", &ignore));
        assert!(!path_should_ignore("main.rs", &ignore));
    }
}
