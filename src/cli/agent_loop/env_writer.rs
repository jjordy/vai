//! `.env` block writer for `vai agent loop init`.
//!
//! Appends a labeled block containing missing vai-related environment variables.
//! Idempotent: if the sentinel `# --- vai loop (added ` is already present,
//! the file is left untouched.

use std::path::Path;

/// Sentinel prefix used to detect an existing vai loop block.
///
/// Idempotency check: if this substring is found anywhere in the file, the
/// append is skipped.
const SENTINEL: &str = "# --- vai loop (added ";

// ── Provider-specific token lines ─────────────────────────────────────────────

/// Agent type discriminant used to select the provider token block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentKind {
    /// Claude Code (uses `CLAUDE_CODE_OAUTH_TOKEN`).
    ClaudeCode,
    /// OpenAI Codex (uses `OPENAI_API_KEY`).
    Codex,
    /// Custom agent — only a comment is emitted.
    Custom,
}

impl AgentKind {
    /// Infer from a raw agent name string such as `"claude-code"` or `"codex"`.
    pub fn from_str(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "claude-code" | "claude" => AgentKind::ClaudeCode,
            "codex" => AgentKind::Codex,
            _ => AgentKind::Custom,
        }
    }
}

/// Returns the provider-specific lines to append after `VAI_API_KEY=…`.
fn provider_lines(kind: AgentKind) -> &'static [&'static str] {
    match kind {
        AgentKind::ClaudeCode => &[
            "# Claude Code OAuth token — run `claude setup-token` and paste the token here:",
            "CLAUDE_CODE_OAUTH_TOKEN=",
        ],
        AgentKind::Codex => &[
            "# OpenAI API key — create one at https://platform.openai.com/api-keys",
            "OPENAI_API_KEY=",
        ],
        AgentKind::Custom => &["# Provider token (configure for your agent)"],
    }
}

// ── Block builder ─────────────────────────────────────────────────────────────

/// Returns `true` if the file content already contains a vai loop block.
pub fn has_vai_block(content: &str) -> bool {
    content.contains(SENTINEL)
}

/// Appends the vai loop block to `existing` and returns the new content.
///
/// A blank line is inserted before the block when the file is non-empty and
/// does not already end with a blank line.
///
/// # Arguments
/// - `existing` — current file content (may be empty for new files).
/// - `vai_api_key` — the plaintext key to embed in `VAI_API_KEY=…`.
/// - `agent` — agent kind; determines the provider token comment block.
/// - `date` — ISO-8601 date string used in the block header (YYYY-MM-DD).
pub fn build_block(existing: &str, vai_api_key: &str, agent: AgentKind, date: &str) -> String {
    let mut out = existing.to_string();

    // Ensure the block is preceded by at least one blank line.
    if !out.is_empty() {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        if !out.ends_with("\n\n") {
            out.push('\n');
        }
    }

    out.push_str(&format!("{SENTINEL}{date} ---\n"));
    out.push_str(&format!("VAI_API_KEY={vai_api_key}\n"));

    for &line in provider_lines(agent) {
        out.push_str(line);
        out.push('\n');
    }

    out
}

// ── File I/O ──────────────────────────────────────────────────────────────────

/// Write or update the `.env` file at `path`, appending the vai block.
///
/// - If the file does not exist, it is created.
/// - If it already contains a vai block (idempotency sentinel), the function
///   returns `Ok(false)` and leaves the file unchanged.
/// - Otherwise the block is appended and `Ok(true)` is returned.
pub fn write_env(
    path: &Path,
    vai_api_key: &str,
    agent: AgentKind,
    date: &str,
) -> std::io::Result<bool> {
    let existing = if path.exists() {
        std::fs::read_to_string(path)?
    } else {
        String::new()
    };

    if has_vai_block(&existing) {
        return Ok(false);
    }

    let new_content = build_block(&existing, vai_api_key, agent, date);
    std::fs::write(path, new_content)?;
    Ok(true)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_tmp() -> TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    // ── has_vai_block ─────────────────────────────────────────────────────────

    #[test]
    fn sentinel_detected() {
        let content = "FOO=bar\n# --- vai loop (added 2026-01-01 ---\nVAI_API_KEY=vk_x\n";
        assert!(has_vai_block(content));
    }

    #[test]
    fn no_sentinel_in_empty_file() {
        assert!(!has_vai_block(""));
    }

    #[test]
    fn no_sentinel_in_regular_env() {
        assert!(!has_vai_block("SECRET=abc\n# comment\n"));
    }

    // ── build_block ───────────────────────────────────────────────────────────

    #[test]
    fn new_file_creation() {
        let block = build_block("", "vk_test_key", AgentKind::ClaudeCode, "2026-04-19");
        assert!(block.contains("VAI_API_KEY=vk_test_key"));
        assert!(block.contains("CLAUDE_CODE_OAUTH_TOKEN="));
        assert!(block.contains("2026-04-19"));
        // Should start with the sentinel (no leading blank when file was empty).
        assert!(block.starts_with(SENTINEL));
    }

    #[test]
    fn append_to_existing() {
        let existing = "FOO=bar\n";
        let block = build_block(existing, "vk_abc", AgentKind::Codex, "2026-04-19");
        assert!(block.starts_with("FOO=bar\n"));
        assert!(block.contains("OPENAI_API_KEY="));
        // Should have a blank separator line between old content and block.
        assert!(block.contains("\n\n"));
    }

    #[test]
    fn codex_agent_emits_openai_key() {
        let block = build_block("", "k", AgentKind::Codex, "2026-04-19");
        assert!(block.contains("OPENAI_API_KEY="));
        assert!(!block.contains("CLAUDE_CODE_OAUTH_TOKEN"));
    }

    #[test]
    fn custom_agent_emits_comment_only() {
        let block = build_block("", "k", AgentKind::Custom, "2026-04-19");
        assert!(block.contains("# Provider token (configure for your agent)"));
        // No placeholder key line.
        let key_lines: Vec<&str> = block
            .lines()
            .filter(|l| !l.starts_with('#') && l.contains('=') && !l.contains("VAI_API_KEY"))
            .collect();
        assert!(key_lines.is_empty(), "unexpected key lines: {key_lines:?}");
    }

    // ── write_env ─────────────────────────────────────────────────────────────

    #[test]
    fn write_env_creates_new_file() {
        let dir = make_tmp();
        let path = dir.path().join(".env");
        let written = write_env(&path, "vk_new_key", AgentKind::ClaudeCode, "2026-04-19").unwrap();
        assert!(written, "should have written");
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("VAI_API_KEY=vk_new_key"));
    }

    #[test]
    fn write_env_appends_to_existing() {
        let dir = make_tmp();
        let path = dir.path().join(".env");
        std::fs::write(&path, "EXISTING=1\n").unwrap();
        write_env(&path, "vk_key", AgentKind::ClaudeCode, "2026-04-19").unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.starts_with("EXISTING=1\n"));
        assert!(content.contains("VAI_API_KEY=vk_key"));
    }

    #[test]
    fn write_env_idempotent_second_run() {
        let dir = make_tmp();
        let path = dir.path().join(".env");
        // First run writes the block.
        write_env(&path, "vk_key1", AgentKind::ClaudeCode, "2026-04-19").unwrap();
        let content_after_first = std::fs::read_to_string(&path).unwrap();
        // Second run should be a no-op.
        let written = write_env(&path, "vk_key2", AgentKind::ClaudeCode, "2026-04-19").unwrap();
        assert!(!written, "second run should be skipped");
        let content_after_second = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content_after_first, content_after_second, "file must not change on second run");
        // Key1 should be present; key2 must not appear.
        assert!(content_after_second.contains("vk_key1"));
        assert!(!content_after_second.contains("vk_key2"));
    }
}
