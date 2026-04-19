//! `.env` file parser for `vai agent loop init`.
//!
//! Line-based parser that classifies each line and extracts key=value pairs.
//! Pure functions — no filesystem I/O. Preserves the original line list so
//! writers can reconstruct the file with formatting intact.

use std::collections::HashMap;

// ── Line type ─────────────────────────────────────────────────────────────────

/// A single parsed line from a `.env` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvLine {
    /// Blank or whitespace-only line.
    Blank,
    /// Comment line starting with `#`.
    Comment(String),
    /// A `KEY=VALUE` assignment.  `raw` preserves the original text.
    KeyValue {
        /// The key (left-hand side of `=`, trimmed).
        key: String,
        /// The value (right-hand side of `=`, preserved as-is including quotes).
        value: String,
        /// The original raw line, used to reconstruct the file.
        raw: String,
    },
}

// ── Parse result ──────────────────────────────────────────────────────────────

/// Result of parsing a `.env` file.
#[derive(Debug, Default)]
pub struct ParsedEnv {
    /// All lines in the original file order.
    pub lines: Vec<EnvLine>,
    /// Map of key → value for all `KEY=VALUE` lines.
    pub keys: HashMap<String, String>,
}

impl ParsedEnv {
    /// Returns `true` if `key` is present with a non-empty value.
    pub fn has_key_with_value(&self, key: &str) -> bool {
        self.keys
            .get(key)
            .map(|v| !v.trim_matches(|c: char| c == '"' || c == '\'').trim().is_empty())
            .unwrap_or(false)
    }
}

// ── Parser ────────────────────────────────────────────────────────────────────

/// Parse a `.env` file `content` string into a [`ParsedEnv`].
///
/// Rules:
/// - Blank / whitespace-only → [`EnvLine::Blank`].
/// - Lines starting with `#` → [`EnvLine::Comment`].
/// - Lines containing `=` → [`EnvLine::KeyValue`]; the key is the part before
///   the first `=` (trimmed), the value is everything after (preserved as-is).
///   An empty key is treated as a comment (graceful degradation).
/// - Anything else is treated as a comment.
pub fn parse(content: &str) -> ParsedEnv {
    let mut result = ParsedEnv::default();

    for raw_line in content.lines() {
        let trimmed = raw_line.trim();

        let line = if trimmed.is_empty() {
            EnvLine::Blank
        } else if trimmed.starts_with('#') {
            EnvLine::Comment(raw_line.to_string())
        } else if let Some(eq_pos) = raw_line.find('=') {
            let key = raw_line[..eq_pos].trim().to_string();
            let value = raw_line[eq_pos + 1..].to_string();
            if key.is_empty() {
                // Not a valid KEY=VALUE — treat as a comment-like passthrough.
                EnvLine::Comment(raw_line.to_string())
            } else {
                result.keys.insert(key.clone(), value.clone());
                EnvLine::KeyValue { key, value, raw: raw_line.to_string() }
            }
        } else {
            // No `=` and not a comment — treat as passthrough comment.
            EnvLine::Comment(raw_line.to_string())
        };

        result.lines.push(line);
    }

    result
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_file() {
        let p = parse("");
        assert!(p.lines.is_empty());
        assert!(p.keys.is_empty());
    }

    #[test]
    fn comment_only_file() {
        let p = parse("# this is a comment\n# another comment");
        assert_eq!(p.lines.len(), 2);
        assert!(p.keys.is_empty());
        assert!(matches!(&p.lines[0], EnvLine::Comment(_)));
    }

    #[test]
    fn mixed_content() {
        let content = "# header\nFOO=bar\n\nBAZ=qux";
        let p = parse(content);
        assert_eq!(p.lines.len(), 4);
        assert_eq!(p.keys.get("FOO").map(String::as_str), Some("bar"));
        assert_eq!(p.keys.get("BAZ").map(String::as_str), Some("qux"));
    }

    #[test]
    fn existing_vai_api_key_non_empty() {
        let p = parse("VAI_API_KEY=vk_live_abc123");
        assert!(p.has_key_with_value("VAI_API_KEY"));
    }

    #[test]
    fn existing_vai_api_key_empty_value() {
        let p = parse("VAI_API_KEY=");
        assert!(!p.has_key_with_value("VAI_API_KEY"));
    }

    #[test]
    fn existing_provider_token_detected() {
        let p = parse("CLAUDE_CODE_OAUTH_TOKEN=tok_abc\n");
        assert!(p.has_key_with_value("CLAUDE_CODE_OAUTH_TOKEN"));
    }

    #[test]
    fn quoted_values_preserved() {
        let p = parse(r#"SECRET="hello world""#);
        assert_eq!(p.keys.get("SECRET").map(String::as_str), Some("\"hello world\""));
    }

    #[test]
    fn equals_sign_in_value() {
        // Value may contain `=` — only the first `=` is the delimiter.
        let p = parse("TOKEN=abc=def=ghi");
        assert_eq!(p.keys.get("TOKEN").map(String::as_str), Some("abc=def=ghi"));
    }

    #[test]
    fn blank_lines_are_classified() {
        let p = parse("\n\n");
        assert!(p.lines.iter().all(|l| *l == EnvLine::Blank));
    }

    #[test]
    fn empty_key_treated_as_comment() {
        let p = parse("=bad_line");
        assert!(p.keys.is_empty());
        assert!(matches!(&p.lines[0], EnvLine::Comment(_)));
    }
}
