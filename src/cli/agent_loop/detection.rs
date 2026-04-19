//! Project-type detection for `vai agent loop init`.
//!
//! Inspects the root of a repository to classify it into one of four categories
//! used to select the appropriate loop template and quality-check configuration.

#![allow(dead_code)]

use std::path::Path;

/// The detected project type for a repository root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum ProjectType {
    /// React, Vue, Svelte, Next.js, Remix, Nuxt, or Angular frontend.
    FrontendReact,
    /// Rust backend (Cargo.toml present, no package.json).
    BackendRust,
    /// Node/TypeScript backend (package.json without a frontend framework).
    BackendTypescript,
    /// No recognised project markers.
    Generic,
}

/// Detect the project type by inspecting files at `repo_root`.
///
/// Detection rules (first match wins):
/// 1. `Cargo.toml` present, `package.json` absent → [`ProjectType::BackendRust`].
/// 2. `package.json` present with a frontend-framework dependency → [`ProjectType::FrontendReact`].
/// 3. `package.json` present without a frontend-framework dependency → [`ProjectType::BackendTypescript`].
/// 4. Both `Cargo.toml` and a React `package.json` → [`ProjectType::FrontendReact`].
/// 5. Nothing matches → [`ProjectType::Generic`].
///
/// Only the repository root is inspected; subdirectories are not traversed.
/// A malformed `package.json` is treated as absent (graceful degradation).
pub fn detect_project_type(repo_root: &Path) -> ProjectType {
    let has_cargo = repo_root.join("Cargo.toml").exists();
    let pkg_path = repo_root.join("package.json");
    let has_package = pkg_path.exists();

    match (has_cargo, has_package) {
        (true, false) => ProjectType::BackendRust,
        (false, false) => ProjectType::Generic,
        (_, true) => {
            // Try to detect a frontend framework inside package.json.
            // Returns None if the file is missing or malformed (treat as absent).
            match parse_package_json_framework(&pkg_path) {
                Some(true) => ProjectType::FrontendReact,
                Some(false) => ProjectType::BackendTypescript,
                None => ProjectType::Generic,
            }
        }
    }
}

const FRONTEND_KEYS: &[&str] = &[
    "react",
    "vue",
    "svelte",
    "next",
    "remix",
    "nuxt",
    "@angular/core",
];

/// Parse `package.json` at `path` and check for frontend-framework dependencies.
///
/// Returns:
/// - `Some(true)` — parseable and a frontend-framework key was found.
/// - `Some(false)` — parseable but no frontend-framework key found.
/// - `None` — file is missing or unparseable (treat as absent; graceful degradation).
fn parse_package_json_framework(path: &Path) -> Option<bool> {
    let contents = std::fs::read_to_string(path).ok()?;
    let value = serde_json::from_str::<serde_json::Value>(&contents).ok()?;

    for section in &["dependencies", "devDependencies"] {
        if let Some(deps) = value.get(section).and_then(|v| v.as_object()) {
            for key in FRONTEND_KEYS {
                if deps.contains_key(*key) {
                    return Some(true);
                }
            }
        }
    }
    Some(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn make_tmp() -> TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[test]
    fn cargo_only_is_backend_rust() {
        let dir = make_tmp();
        fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"x\"").unwrap();
        assert_eq!(detect_project_type(dir.path()), ProjectType::BackendRust);
    }

    #[test]
    fn package_json_no_framework_is_backend_typescript() {
        let dir = make_tmp();
        fs::write(
            dir.path().join("package.json"),
            r#"{"dependencies": {"express": "4.0"}}"#,
        )
        .unwrap();
        assert_eq!(
            detect_project_type(dir.path()),
            ProjectType::BackendTypescript
        );
    }

    #[test]
    fn package_json_react_is_frontend_react() {
        let dir = make_tmp();
        fs::write(
            dir.path().join("package.json"),
            r#"{"dependencies": {"react": "18.0"}}"#,
        )
        .unwrap();
        assert_eq!(detect_project_type(dir.path()), ProjectType::FrontendReact);
    }

    #[test]
    fn cargo_and_react_package_json_is_frontend_react() {
        let dir = make_tmp();
        fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"x\"").unwrap();
        fs::write(
            dir.path().join("package.json"),
            r#"{"dependencies": {"react": "18.0"}}"#,
        )
        .unwrap();
        assert_eq!(detect_project_type(dir.path()), ProjectType::FrontendReact);
    }

    #[test]
    fn empty_directory_is_generic() {
        let dir = make_tmp();
        assert_eq!(detect_project_type(dir.path()), ProjectType::Generic);
    }

    #[test]
    fn malformed_package_json_is_generic() {
        let dir = make_tmp();
        fs::write(dir.path().join("package.json"), "NOT { valid JSON }}}").unwrap();
        assert_eq!(detect_project_type(dir.path()), ProjectType::Generic);
    }

    #[test]
    fn dev_dependencies_frontend_detected() {
        let dir = make_tmp();
        fs::write(
            dir.path().join("package.json"),
            r#"{"devDependencies": {"svelte": "4.0"}}"#,
        )
        .unwrap();
        assert_eq!(detect_project_type(dir.path()), ProjectType::FrontendReact);
    }
}
