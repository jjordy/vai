//! Unified file-ignore logic for vai.
//!
//! Combines `.gitignore`, `.vaignore`, and `vai.toml` patterns to determine
//! which files to include in file collection operations.
//!
//! ## Precedence (highest to lowest)
//! 1. `.vaignore` — vai-specific overrides (optional)
//! 2. `.gitignore` — industry-standard, auto-detected by the `ignore` crate
//! 3. `extra_patterns` from `vai.toml` — backward-compatible glob patterns
//! 4. Built-in defaults — `.vai/` and `.git/` are always excluded

use std::path::{Path, PathBuf};

use ignore::WalkBuilder;

/// Collects all regular files under `root`, respecting ignore rules.
///
/// - Reads `.gitignore` and `.vaignore` files automatically.
/// - `extra_patterns` are simple patterns from `vai.toml` (`target/`, `*.o`, etc.).
/// - `.vai/` and `.git/` are always excluded regardless of ignore files.
pub fn collect_all_files(root: &Path, extra_patterns: &[String]) -> Vec<PathBuf> {
    collect_impl(root, extra_patterns, None)
}

/// Collects supported source files under `root`, respecting ignore rules.
///
/// Only files with extensions `.rs`, `.ts`, `.tsx`, `.js`, or `.jsx` are returned.
/// Applies the same ignore logic as [`collect_all_files`].
pub fn collect_source_files(root: &Path, extra_patterns: &[String]) -> Vec<PathBuf> {
    const SRC_EXTS: &[&str] = &["rs", "ts", "tsx", "js", "jsx"];
    collect_impl(root, extra_patterns, Some(SRC_EXTS))
}

/// Collects all regular files under `root` as relative path strings, respecting ignore rules.
///
/// Returns paths in forward-slash format (`src/lib.rs`), suitable for use in
/// API responses and archive construction.  Applies the same ignore logic as
/// [`collect_all_files`].
pub fn collect_all_files_relative(root: &Path, extra_patterns: &[String]) -> Vec<String> {
    collect_all_files(root, extra_patterns)
        .into_iter()
        .filter_map(|p| {
            p.strip_prefix(root)
                .ok()
                .map(|rel| rel.to_string_lossy().replace('\\', "/"))
        })
        .collect()
}

// ── Internal implementation ───────────────────────────────────────────────────

/// Core walk implementation.
///
/// Builds an `ignore::Walk` that respects `.gitignore` and `.vaignore`, then
/// post-filters with `extra_patterns` and optionally an extension allowlist.
fn collect_impl(
    root: &Path,
    extra_patterns: &[String],
    extensions: Option<&[&str]>,
) -> Vec<PathBuf> {
    let mut builder = WalkBuilder::new(root);
    builder
        // Respect .gitignore files in the project directory (and parents).
        .git_ignore(true)
        // Respect .vaignore files (vai-specific ignore file).
        .add_custom_ignore_filename(".vaignore")
        // Don't use the global ~/.gitignore — only project-local rules.
        .git_global(false)
        // Don't use .git/info/exclude.
        .git_exclude(false)
        // Apply gitignore rules even if the directory is not a git repository.
        // This ensures .gitignore is respected in freshly-cloned or non-git dirs.
        .require_git(false)
        // Walk hidden files/dirs; gitignore rules handle exclusion explicitly.
        // (Hidden(false) is required so dotfiles respected by .gitignore are
        // still reachable for reading, and so .vaignore itself is found.)
        .hidden(false);

    let mut files: Vec<PathBuf> = builder
        .build()
        .filter_map(|result| {
            let entry = result.ok()?;
            let path = entry.path();

            // Skip directories — we only collect regular files.
            if entry.file_type().is_none_or(|ft| ft.is_dir()) {
                return None;
            }

            // Always exclude .vai/ and .git/ contents regardless of ignore files.
            // (These are internal bookkeeping directories that should never appear
            // in file listings, downloads, or migration uploads.)
            if is_under_builtin_exclude(root, path) {
                return None;
            }

            // Skip built-in secret files regardless of ignore configuration.
            if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
                if is_builtin_secret_file(file_name) {
                    return None;
                }
            }

            // Apply extra vai.toml patterns — check every path component so that
            // directory-level patterns like `node_modules/` and `target/` work
            // correctly regardless of nesting depth.
            let rel = path.strip_prefix(root).unwrap_or(path);
            if any_component_matches(rel, extra_patterns) {
                return None;
            }

            // Filter by extension allowlist (for source-file collection).
            if let Some(exts) = extensions {
                let ext = path.extension()?.to_str()?;
                if !exts.contains(&ext) {
                    return None;
                }
            }

            Some(path.to_owned())
        })
        .collect();

    files.sort();
    files
}

/// Returns `true` if `name` is a built-in secret or credential file that vai
/// never tracks, regardless of ignore configuration.
///
/// Covers common dotenv variants and TLS/SSH key material.  Conservative by
/// design — only near-universally-secret filenames are included.
pub(crate) fn is_builtin_secret_file(name: &str) -> bool {
    // Exact matches: .env and .env.<variant>
    if name == ".env" || name.starts_with(".env.") {
        return true;
    }
    // Key material and certificates.
    if name.ends_with(".pem") || name.ends_with(".key") {
        return true;
    }
    // SSH private key default names.
    if name.starts_with("id_rsa") || name.starts_with("id_ed25519") {
        return true;
    }
    false
}

/// Returns `true` if `path` is inside `.vai/` or `.git/` relative to `root`.
fn is_under_builtin_exclude(root: &Path, path: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(root) else {
        return false;
    };
    // Check the first component of the relative path.
    if let Some(first) = rel.components().next() {
        let name = first.as_os_str().to_str().unwrap_or("");
        if name == ".vai" || name == ".git" {
            return true;
        }
    }
    false
}

/// Returns `true` if any component of `rel_path` matches a vai.toml ignore pattern.
///
/// Checks every directory/file component so that patterns like `node_modules/`
/// correctly exclude deeply nested files such as `node_modules/foo/index.js`.
fn any_component_matches(rel_path: &Path, patterns: &[String]) -> bool {
    rel_path.components().any(|c| {
        let name = c.as_os_str().to_str().unwrap_or("");
        matches_extra_pattern(name, patterns)
    })
}

/// Returns `true` if `name` (a single path component) matches any vai.toml ignore pattern.
///
/// Supports:
/// - `*.ext` — matches any name ending in `.ext`
/// - `dir/` or `dir` — matches a name exactly equal to `dir`
fn matches_extra_pattern(name: &str, patterns: &[String]) -> bool {
    for pattern in patterns {
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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use tempfile::TempDir;

    fn make_file(dir: &Path, rel: &str) {
        let dest = dir.join(rel);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(dest, b"content").unwrap();
    }

    #[test]
    fn collects_regular_files() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        make_file(root, "src/lib.rs");
        make_file(root, "src/main.rs");
        make_file(root, "README.md");

        let files = collect_all_files(root, &[]);
        let names: Vec<_> = files
            .iter()
            .filter_map(|p| p.file_name()?.to_str())
            .collect();
        assert!(names.contains(&"lib.rs"));
        assert!(names.contains(&"main.rs"));
        assert!(names.contains(&"README.md"));
    }

    #[test]
    fn excludes_vai_and_git_directories() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        make_file(root, "src/lib.rs");
        make_file(root, ".vai/head");
        make_file(root, ".git/config");

        let files = collect_all_files(root, &[]);
        assert!(!files.iter().any(|p| p.to_string_lossy().contains(".vai")));
        assert!(!files.iter().any(|p| p.to_string_lossy().contains(".git")));
    }

    #[test]
    fn respects_gitignore() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        make_file(root, "src/lib.rs");
        make_file(root, "target/debug/vai");
        fs::write(root.join(".gitignore"), b"target/\n").unwrap();

        let files = collect_all_files(root, &[]);
        assert!(!files.iter().any(|p| p.to_string_lossy().contains("target")));
        assert!(files.iter().any(|p| p.file_name().unwrap() == "lib.rs"));
    }

    #[test]
    fn respects_vaignore() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        make_file(root, "src/lib.rs");
        make_file(root, "scratch/notes.txt");
        fs::write(root.join(".vaignore"), b"scratch/\n").unwrap();

        let files = collect_all_files(root, &[]);
        assert!(!files.iter().any(|p| p.to_string_lossy().contains("scratch")));
        assert!(files.iter().any(|p| p.file_name().unwrap() == "lib.rs"));
    }

    #[test]
    fn respects_extra_patterns() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        make_file(root, "src/lib.rs");
        make_file(root, "src/lib.o");
        make_file(root, "node_modules/pkg/index.js");

        let patterns = vec!["*.o".to_string(), "node_modules/".to_string()];
        let files = collect_all_files(root, &patterns);
        assert!(!files.iter().any(|p| p.extension().and_then(|e| e.to_str()) == Some("o")));
        assert!(!files.iter().any(|p| p.to_string_lossy().contains("node_modules")));
    }

    #[test]
    fn source_files_filters_by_extension() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        make_file(root, "src/lib.rs");
        make_file(root, "src/app.ts");
        make_file(root, "README.md");

        let files = collect_source_files(root, &[]);
        assert!(files.iter().any(|p| p.file_name().unwrap() == "lib.rs"));
        assert!(files.iter().any(|p| p.file_name().unwrap() == "app.ts"));
        assert!(!files.iter().any(|p| p.file_name().unwrap() == "README.md"));
    }

    #[test]
    fn relative_paths_use_forward_slashes() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        make_file(root, "src/lib.rs");

        let files = collect_all_files_relative(root, &[]);
        assert!(files.iter().any(|p| p == "src/lib.rs"));
        assert!(!files.iter().any(|p| p.contains('\\')));
    }

    #[test]
    fn matches_extra_pattern_glob() {
        assert!(matches_extra_pattern("foo.o", &["*.o".to_string()]));
        assert!(matches_extra_pattern("target", &["target/".to_string()]));
        assert!(!matches_extra_pattern("main.rs", &["*.o".to_string()]));
    }

    #[test]
    fn any_component_matches_deep_path() {
        let path = std::path::Path::new("node_modules/pkg/index.js");
        assert!(any_component_matches(path, &["node_modules/".to_string()]));
        assert!(!any_component_matches(path, &["target/".to_string()]));
    }

    #[test]
    fn builtin_secret_file_excludes_env_and_keys() {
        // .env variants
        assert!(is_builtin_secret_file(".env"));
        assert!(is_builtin_secret_file(".env.local"));
        assert!(is_builtin_secret_file(".env.production"));
        assert!(is_builtin_secret_file(".env.development"));
        assert!(is_builtin_secret_file(".env.test"));
        // certificate / key material
        assert!(is_builtin_secret_file("server.key"));
        assert!(is_builtin_secret_file("cert.pem"));
        assert!(is_builtin_secret_file("id_rsa"));
        assert!(is_builtin_secret_file("id_rsa.pub"));
        assert!(is_builtin_secret_file("id_ed25519"));
        // safe files — must NOT be excluded
        assert!(!is_builtin_secret_file("main.rs"));
        assert!(!is_builtin_secret_file(".envrc"));  // direnv, not a secret file
        assert!(!is_builtin_secret_file("env.sh"));
        assert!(!is_builtin_secret_file("README.md"));
    }

    #[test]
    fn collect_all_files_excludes_secret_files() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        make_file(root, "src/lib.rs");
        make_file(root, ".env");
        make_file(root, ".env.local");
        make_file(root, "secrets/server.key");
        make_file(root, "cert.pem");

        let files = collect_all_files(root, &[]);
        assert!(files.iter().any(|p| p.file_name().unwrap() == "lib.rs"));
        assert!(!files.iter().any(|p| p.file_name().unwrap() == ".env"));
        assert!(!files.iter().any(|p| p.file_name().unwrap() == ".env.local"));
        assert!(!files.iter().any(|p| p.file_name().unwrap() == "server.key"));
        assert!(!files.iter().any(|p| p.file_name().unwrap() == "cert.pem"));
    }
}
