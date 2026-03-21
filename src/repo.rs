//! Repository management — initialization and discovery of `.vai/` repositories.
//!
//! This module handles `vai init` (creating the `.vai/` directory structure and
//! recording the first event) and `find_root` (walking up the directory tree to
//! locate an existing repository).

use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use colored::Colorize;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::event_log::{EventKind, EventLog};

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

/// Metadata for a single version, stored in `.vai/versions/v<N>.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionMeta {
    /// Version identifier, e.g. `"v1"`.
    pub version_id: String,
    /// Parent version ID, if any.
    pub parent_version_id: Option<String>,
    /// Intent description for this version.
    pub intent: String,
    /// Agent or user who created this version.
    pub created_by: String,
    /// When this version was created.
    pub created_at: DateTime<Utc>,
    /// ID of the merge event in the event log that produced this version, if any.
    pub merge_event_id: Option<u64>,
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
    if !vai_toml_path.exists() {
        let vai_toml = VaiToml::default();
        fs::write(&vai_toml_path, toml::to_string_pretty(&vai_toml)?)?;
    }

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

    Ok(InitResult {
        root: root.to_owned(),
        config,
        version,
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

/// Prints the init result to stdout in human-readable format.
pub fn print_init_result(result: &InitResult) {
    println!(
        "{} Initialized vai repository {}",
        "✓".green().bold(),
        result.config.name.bold()
    );
    println!(
        "  Repository ID : {}",
        result.config.repo_id.to_string().dimmed()
    );
    println!("  Initial version: v1 \"initial repository\"");
    println!("  Directory      : .vai/");
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
}
