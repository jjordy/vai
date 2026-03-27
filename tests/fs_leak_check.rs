//! CI check: no untagged filesystem calls in server handler code.
//!
//! Every line in `src/server/mod.rs` that matches one of the forbidden
//! filesystem patterns must have a `// ALLOW_FS: <reason>` comment on the
//! **same line** or on the **immediately preceding line**.
//!
//! # Rationale
//!
//! The vai server operates in two modes: local (SQLite + disk) and server
//! (Postgres + S3).  Server-mode handlers must use the storage trait and
//! `S3MergeFs` — never raw `std::fs` or workspace/repo helper functions that
//! touch the disk.  Any untagged match is a potential leak that would silently
//! succeed in local-mode CI but fail in server-mode production.
//!
//! # Adding a new exception
//!
//! If you genuinely need a filesystem call in a server handler, add:
//! ```
//! // ALLOW_FS: <brief reason why this is acceptable>
//! std::fs::read(&path)?;
//! ```
//!
//! If the reason is "this is local-mode only", make sure the call is guarded
//! by a mode check (e.g. `if !using_s3_merge { ... }`).

/// Patterns that must not appear in `src/server/mod.rs` without an
/// accompanying `// ALLOW_FS:` tag.
const FORBIDDEN_PATTERNS: &[&str] = &[
    "std::fs::read",
    "std::fs::write",
    "std::fs::create_dir",
    "std::fs::remove",
    "EventLog::open",
    "workspace::get(",
    "workspace::overlay_dir(",
    "workspace::update_meta(",
    "repo::read_head(",
];

#[test]
fn server_mod_no_untagged_fs_calls() {
    let source_path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/server/mod.rs");
    let source = std::fs::read_to_string(source_path)
        .expect("could not read src/server/mod.rs — run from repo root");

    let lines: Vec<&str> = source.lines().collect();
    let mut violations: Vec<String> = Vec::new();

    for (i, line) in lines.iter().enumerate() {
        // Skip comment-only lines — they can't be leaks.
        let trimmed = line.trim();
        if trimmed.starts_with("//") {
            continue;
        }

        for pattern in FORBIDDEN_PATTERNS {
            if !line.contains(pattern) {
                continue;
            }

            // This line matches a forbidden pattern.  Accept it if either:
            //   (a) the same line carries `// ALLOW_FS:`, or
            //   (b) the immediately preceding non-empty line carries `// ALLOW_FS:`.
            let same_line_ok = line.contains("// ALLOW_FS:");
            let prev_line_ok = (1..=i).rev().find_map(|j| {
                let prev = lines[j - 1].trim();
                if prev.is_empty() {
                    None // skip blanks, keep looking
                } else {
                    Some(prev.contains("// ALLOW_FS:"))
                }
            }).unwrap_or(false);

            if !same_line_ok && !prev_line_ok {
                violations.push(format!(
                    "  src/server/mod.rs:{}: untagged `{}` call\n    {}",
                    i + 1,
                    pattern,
                    trimmed,
                ));
            }
        }
    }

    if !violations.is_empty() {
        panic!(
            "\n\nUntagged filesystem calls found in src/server/mod.rs.\n\
             Add `// ALLOW_FS: <reason>` on the same or preceding line for each:\n\n{}\n",
            violations.join("\n")
        );
    }
}
