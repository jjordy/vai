//! `vai agent loop list` — print a table of configured agent loops.
//!
//! For each subdirectory under `.vai/agents/`, prints:
//! - `name` — directory name
//! - `mode` — `docker` if a `Dockerfile` exists, else `bare`
//! - `last_run` — relative age of `.vai/agents/<name>/.last-run` (or `never`)
//! - `active` — whether this is the `default_agent` from `agent.toml`

use std::path::Path;

use serde::Serialize;

use crate::cli::CliError;

// ── Data ──────────────────────────────────────────────────────────────────────

/// Metadata for a single configured agent loop.
#[derive(Debug, Serialize)]
pub struct LoopEntry {
    /// Agent name (subdirectory name under `.vai/agents/`).
    pub name: String,
    /// `docker` if a `Dockerfile` is present, else `bare`.
    pub mode: String,
    /// Human-readable relative time since `.last-run` was written, or `null`.
    pub last_run: Option<String>,
    /// True if this is the `default_agent` in `agent.toml`.
    pub active: bool,
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Format a duration as a short relative-time string (`2h ago`, `never`, …).
///
/// Returns `None` if the `.last-run` file does not exist.
fn format_relative(secs: u64) -> String {
    if secs < 60 {
        return format!("{secs}s ago");
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m ago");
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{hours}h ago");
    }
    let days = hours / 24;
    if days < 7 {
        return format!("{days}d ago");
    }
    let weeks = days / 7;
    format!("{weeks}w ago")
}

/// Read the `.last-run` file and return a relative-time string.
///
/// The file contains an ISO-8601 timestamp written by `vai agent loop run`.
/// Returns `None` if the file does not exist or cannot be parsed.
fn read_last_run(agent_dir: &Path) -> Option<String> {
    let path = agent_dir.join(".last-run");
    let contents = std::fs::read_to_string(&path).ok()?;
    let ts: chrono::DateTime<chrono::Utc> = contents.trim().parse().ok()?;
    let now = chrono::Utc::now();
    let elapsed = now.signed_duration_since(ts);
    let secs = elapsed.num_seconds().max(0) as u64;
    Some(format_relative(secs))
}

/// Collect all configured loops from `.vai/agents/`.
fn collect_entries(vai_dir: &Path, default_agent: Option<&str>) -> Vec<LoopEntry> {
    let agents_dir = vai_dir.join("agents");
    let Ok(read_dir) = std::fs::read_dir(&agents_dir) else {
        return Vec::new();
    };

    let mut entries: Vec<LoopEntry> = read_dir
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter_map(|e| {
            let name = e.file_name().to_str()?.to_string();
            let dir = e.path();
            let mode = if dir.join("Dockerfile").exists() {
                "docker".to_string()
            } else {
                "bare".to_string()
            };
            let last_run = read_last_run(&dir);
            let active = default_agent.map(|d| d == name).unwrap_or(false);
            Some(LoopEntry { name, mode, last_run, active })
        })
        .collect();

    entries.sort_by(|a, b| a.name.cmp(&b.name));
    entries
}

// ── Table printing ────────────────────────────────────────────────────────────

/// Print entries as an aligned text table.
fn print_table(entries: &[LoopEntry]) {
    // Column headers.
    const H_NAME: &str = "NAME";
    const H_MODE: &str = "MODE";
    const H_LAST: &str = "LAST RUN";
    const H_STATUS: &str = "STATUS";

    // Compute column widths.
    let w_name = entries
        .iter()
        .map(|e| e.name.len())
        .max()
        .unwrap_or(0)
        .max(H_NAME.len());
    let w_mode = entries
        .iter()
        .map(|e| e.mode.len())
        .max()
        .unwrap_or(0)
        .max(H_MODE.len());
    let w_last = entries
        .iter()
        .map(|e| e.last_run.as_deref().unwrap_or("never").len())
        .max()
        .unwrap_or(0)
        .max(H_LAST.len());

    // Header row.
    println!(
        "{:<w_name$}  {:<w_mode$}  {:<w_last$}  {}",
        H_NAME, H_MODE, H_LAST, H_STATUS
    );

    for entry in entries {
        let last = entry.last_run.as_deref().unwrap_or("never");
        let status = if entry.active { "(active)" } else { "" };
        println!(
            "{:<w_name$}  {:<w_mode$}  {:<w_last$}  {}",
            entry.name, entry.mode, last, status
        );
    }
}

// ── Handler ───────────────────────────────────────────────────────────────────

/// Handle `vai agent loop list`.
pub(super) fn handle(json: bool) -> Result<(), CliError> {
    let cwd = std::env::current_dir()
        .map_err(|e| CliError::Other(format!("cannot determine current directory: {e}")))?;
    let vai_dir = cwd.join(".vai");

    // Resolve the default agent from agent.toml (best-effort).
    let default_agent: Option<String> = crate::agent::load_config(&cwd)
        .ok()
        .and_then(|c| c.default_agent);

    let entries = collect_entries(&vai_dir, default_agent.as_deref());

    if json {
        let out = serde_json::to_string_pretty(&entries)
            .map_err(|e| CliError::Other(format!("JSON serialization failed: {e}")))?;
        println!("{out}");
        return Ok(());
    }

    if entries.is_empty() {
        println!("No loops configured. Run 'vai agent loop init' to create one.");
        return Ok(());
    }

    print_table(&entries);
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_tmp() -> TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    /// Create `.vai/agents/<name>/loop.sh` and return the agent dir.
    fn scaffold_agent(vai_dir: &Path, name: &str) -> std::path::PathBuf {
        let agent_dir = vai_dir.join("agents").join(name);
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(agent_dir.join("loop.sh"), "#!/bin/sh\n").unwrap();
        agent_dir
    }

    // ── format_relative ───────────────────────────────────────────────────────

    #[test]
    fn format_seconds() {
        assert_eq!(format_relative(45), "45s ago");
    }

    #[test]
    fn format_minutes() {
        assert_eq!(format_relative(90), "1m ago");
        assert_eq!(format_relative(3599), "59m ago");
    }

    #[test]
    fn format_hours() {
        assert_eq!(format_relative(3600), "1h ago");
        assert_eq!(format_relative(7199), "1h ago");
        assert_eq!(format_relative(7200), "2h ago");
    }

    #[test]
    fn format_days() {
        assert_eq!(format_relative(86400), "1d ago");
        assert_eq!(format_relative(86400 * 6), "6d ago");
    }

    #[test]
    fn format_weeks() {
        assert_eq!(format_relative(86400 * 7), "1w ago");
        assert_eq!(format_relative(86400 * 14), "2w ago");
    }

    // ── collect_entries ───────────────────────────────────────────────────────

    #[test]
    fn empty_when_no_agents_dir() {
        let dir = make_tmp();
        let vai = dir.path().join(".vai");
        let entries = collect_entries(&vai, None);
        assert!(entries.is_empty());
    }

    #[test]
    fn single_bare_agent_no_last_run() {
        let dir = make_tmp();
        let vai = dir.path().join(".vai");
        scaffold_agent(&vai, "claude-code");
        let entries = collect_entries(&vai, None);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "claude-code");
        assert_eq!(entries[0].mode, "bare");
        assert!(entries[0].last_run.is_none());
        assert!(!entries[0].active);
    }

    #[test]
    fn docker_mode_when_dockerfile_present() {
        let dir = make_tmp();
        let vai = dir.path().join(".vai");
        let agent_dir = scaffold_agent(&vai, "claude-code");
        std::fs::write(agent_dir.join("Dockerfile"), "FROM ubuntu\n").unwrap();
        let entries = collect_entries(&vai, None);
        assert_eq!(entries[0].mode, "docker");
    }

    #[test]
    fn active_flag_matches_default_agent() {
        let dir = make_tmp();
        let vai = dir.path().join(".vai");
        scaffold_agent(&vai, "claude-code");
        scaffold_agent(&vai, "codex");
        let entries = collect_entries(&vai, Some("claude-code"));
        let cc = entries.iter().find(|e| e.name == "claude-code").unwrap();
        let cx = entries.iter().find(|e| e.name == "codex").unwrap();
        assert!(cc.active);
        assert!(!cx.active);
    }

    #[test]
    fn entries_sorted_by_name() {
        let dir = make_tmp();
        let vai = dir.path().join(".vai");
        scaffold_agent(&vai, "zzz");
        scaffold_agent(&vai, "aaa");
        scaffold_agent(&vai, "mmm");
        let entries = collect_entries(&vai, None);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["aaa", "mmm", "zzz"]);
    }

    #[test]
    fn last_run_parsed_from_file() {
        let dir = make_tmp();
        let vai = dir.path().join(".vai");
        let agent_dir = scaffold_agent(&vai, "codex");
        // Write a timestamp 2 hours ago.
        let ts = chrono::Utc::now() - chrono::Duration::hours(2);
        std::fs::write(agent_dir.join(".last-run"), format!("{}\n", ts.to_rfc3339())).unwrap();
        let entries = collect_entries(&vai, None);
        assert_eq!(entries[0].last_run.as_deref(), Some("2h ago"));
    }

    #[test]
    fn json_output_serializes_correctly() {
        let dir = make_tmp();
        let vai = dir.path().join(".vai");
        scaffold_agent(&vai, "claude-code");
        let entries = collect_entries(&vai, Some("claude-code"));
        let json = serde_json::to_string(&entries).unwrap();
        assert!(json.contains("\"name\""));
        assert!(json.contains("claude-code"));
        assert!(json.contains("\"active\":true"));
    }
}
