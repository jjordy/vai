//! End-to-end integration test for the single-agent workspace and graph workflow.
//!
//! Exercises the full lifecycle:
//! `vai init` → workspace create → diff → submit → log → show → rollback

use std::fs;
use tempfile::TempDir;
use vai::diff;
use vai::merge;
use vai::repo;
use vai::version;
use vai::workspace;

// ── Sample Rust source files ──────────────────────────────────────────────────

const AUTH_RS: &str = r#"/// Authentication service
pub struct AuthService {
    pub secret: String,
}

impl AuthService {
    /// Validates a token against the stored secret.
    pub fn validate_token(&self, token: &str) -> bool {
        token == self.secret
    }
}
"#;

const CONFIG_RS: &str = r#"/// Repository configuration
pub struct Config {
    pub name: String,
    pub max_agents: usize,
}

impl Config {
    /// Returns a default configuration.
    pub fn default_config() -> Self {
        Config {
            name: "unnamed".to_string(),
            max_agents: 8,
        }
    }
}
"#;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Sets up a temporary repository with sample Rust source files and runs
/// `vai init`, returning the temp dir (kept alive for the test duration).
fn setup_repo() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("auth.rs"), AUTH_RS).unwrap();
    fs::write(src.join("config.rs"), CONFIG_RS).unwrap();

    let vai_dir = root.join(".vai");
    (tmp, root, vai_dir)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Step 1–3: init and graph sanity check.
#[test]
fn test_init_and_graph() {
    let (_tmp, root, vai_dir) = setup_repo();

    let result = repo::init(&root).expect("vai init failed");

    assert!(vai_dir.exists(), ".vai directory not created");
    assert_eq!(result.version.version_id, "v1");
    assert!(
        result.graph_stats.entity_count > 0,
        "graph should have entities after init"
    );
    assert!(result.files_parsed > 0, "at least one Rust file should be parsed");

    // HEAD should be v1
    let head = repo::read_head(&vai_dir).unwrap();
    assert_eq!(head, "v1");
}

/// Steps 4–9: create workspace, modify files, diff, submit, verify v2.
#[test]
fn test_workspace_create_diff_submit() {
    let (_tmp, root, vai_dir) = setup_repo();
    repo::init(&root).unwrap();

    // Step 4: create workspace
    let head = repo::read_head(&vai_dir).unwrap();
    let create_result = workspace::create(&vai_dir, "add login function", &head)
        .expect("workspace create failed");
    let ws = &create_result.workspace;
    assert_eq!(ws.status, workspace::WorkspaceStatus::Created);
    assert_eq!(ws.base_version, "v1");

    // Step 5: write a modified file to the overlay (add a new function)
    let overlay_src = workspace::overlay_dir(&vai_dir, &ws.id.to_string()).join("src");
    fs::create_dir_all(&overlay_src).unwrap();
    let modified_auth = format!(
        "{}\n/// Generates a new token.\npub fn generate_token(secret: &str) -> String {{\n    secret.to_uppercase()\n}}\n",
        AUTH_RS
    );
    fs::write(overlay_src.join("auth.rs"), &modified_auth).unwrap();

    // Step 6: compute diff and verify it shows the change
    let diff_result = diff::compute(&vai_dir, &root).expect("diff compute failed");
    assert_eq!(diff_result.file_diffs.len(), 1, "one file should be changed");
    assert_eq!(diff_result.file_diffs[0].path, "src/auth.rs");

    let added: Vec<_> = diff_result
        .entity_changes
        .iter()
        .filter(|c| c.change_type == vai::diff::EntityChangeType::Added)
        .collect();
    assert!(!added.is_empty(), "at least one entity should be added");
    assert!(
        added.iter().any(|c| c.qualified_name.contains("generate_token")),
        "generate_token should appear in added entities"
    );

    // Step 7: submit → creates v2 (fast-forward since HEAD == base_version)
    let submit_result = merge::submit(&vai_dir, &root).expect("submit failed");
    assert_eq!(submit_result.version.version_id, "v2");
    assert!(submit_result.files_applied > 0);

    // Step 8: HEAD should now be v2
    let head = repo::read_head(&vai_dir).unwrap();
    assert_eq!(head, "v2");

    // Verify the file was applied to the repo root
    let applied = fs::read_to_string(root.join("src").join("auth.rs")).unwrap();
    assert!(applied.contains("generate_token"));

    // Step 9: vai log — should have two versions
    let versions = version::list_versions(&vai_dir).expect("list_versions failed");
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0].version_id, "v1");
    assert_eq!(versions[1].version_id, "v2");
    assert_eq!(versions[1].intent, "add login function");

    // vai show v2 — entity changes present
    let changes = version::get_version_changes(&vai_dir, "v2").expect("get_version_changes failed");
    assert!(!changes.entity_changes.is_empty(), "v2 should have entity changes");
    assert!(!changes.file_changes.is_empty(), "v2 should have file changes");
}

/// Steps 10–11: two non-conflicting workspaces, auto-merge succeeds.
#[test]
fn test_two_non_conflicting_workspaces() {
    let (_tmp, root, vai_dir) = setup_repo();
    repo::init(&root).unwrap();

    // Workspace 1: modify auth.rs
    let head = repo::read_head(&vai_dir).unwrap();
    let ws1 = workspace::create(&vai_dir, "add token validation helper", &head).unwrap();
    let overlay1 = workspace::overlay_dir(&vai_dir, &ws1.workspace.id.to_string()).join("src");
    fs::create_dir_all(&overlay1).unwrap();
    let auth_v2 = format!(
        "{}\npub fn is_strong_token(token: &str) -> bool {{\n    token.len() >= 16\n}}\n",
        AUTH_RS
    );
    fs::write(overlay1.join("auth.rs"), &auth_v2).unwrap();
    let r1 = merge::submit(&vai_dir, &root).expect("first submit failed");
    assert_eq!(r1.version.version_id, "v2");

    // Workspace 2: modify config.rs (different file, no conflict)
    let head = repo::read_head(&vai_dir).unwrap();
    let ws2 = workspace::create(&vai_dir, "add config timeout field", &head).unwrap();
    let overlay2 = workspace::overlay_dir(&vai_dir, &ws2.workspace.id.to_string()).join("src");
    fs::create_dir_all(&overlay2).unwrap();
    let config_v2 = format!(
        "{}\n/// Validates the configuration.\npub fn validate(cfg: &Config) -> bool {{\n    cfg.max_agents > 0\n}}\n",
        CONFIG_RS
    );
    fs::write(overlay2.join("config.rs"), &config_v2).unwrap();
    let r2 = merge::submit(&vai_dir, &root).expect("second submit failed");
    assert_eq!(r2.version.version_id, "v3");

    let versions = version::list_versions(&vai_dir).unwrap();
    assert_eq!(versions.len(), 3);
}

/// Steps 12–13: rollback creates a new append-only version.
#[test]
fn test_rollback_creates_new_version() {
    let (_tmp, root, vai_dir) = setup_repo();
    repo::init(&root).unwrap();

    // Create v2 by adding a function to auth.rs
    let head = repo::read_head(&vai_dir).unwrap();
    let ws = workspace::create(&vai_dir, "add helper function", &head).unwrap();
    let overlay = workspace::overlay_dir(&vai_dir, &ws.workspace.id.to_string()).join("src");
    fs::create_dir_all(&overlay).unwrap();
    let auth_modified = format!(
        "{}\npub fn helper() -> u32 {{\n    42\n}}\n",
        AUTH_RS
    );
    fs::write(overlay.join("auth.rs"), &auth_modified).unwrap();
    merge::submit(&vai_dir, &root).expect("submit failed");
    assert_eq!(repo::read_head(&vai_dir).unwrap(), "v2");

    // Verify helper is present in root
    let content_after = fs::read_to_string(root.join("src").join("auth.rs")).unwrap();
    assert!(content_after.contains("helper"));

    // Step 12: analyze rollback impact for v2
    let impact = version::analyze_rollback_impact(&vai_dir, "v2")
        .expect("impact analysis failed");
    assert_eq!(impact.target_version.version_id, "v2");
    // HEAD == v2 so no downstream impacts
    assert!(impact.downstream_impacts.is_empty(), "no downstream impacts expected");

    // Step 13: rollback v2 — creates v3 that restores the pre-v2 state
    let rb = version::rollback(&vai_dir, &root, "v2", None)
        .expect("rollback failed");
    assert_eq!(rb.new_version.version_id, "v3");
    assert!(rb.files_restored > 0 || rb.files_deleted > 0);

    // HEAD should now be v3
    let head = repo::read_head(&vai_dir).unwrap();
    assert_eq!(head, "v3");

    // The file should no longer contain the helper function
    let content_after_rollback = fs::read_to_string(root.join("src").join("auth.rs")).unwrap();
    assert!(
        !content_after_rollback.contains("helper"),
        "helper function should be rolled back"
    );

    // History is append-only: v1, v2, v3 all exist
    let versions = version::list_versions(&vai_dir).unwrap();
    assert_eq!(versions.len(), 3);
    assert_eq!(versions[2].version_id, "v3");
}

/// Full lifecycle: init → workspace → diff → submit v2 → submit v3 → rollback v2 → v4.
#[test]
fn test_full_workspace_and_graph_lifecycle() {
    let (_tmp, root, vai_dir) = setup_repo();

    // 1. Init
    let init = repo::init(&root).unwrap();
    assert_eq!(init.version.version_id, "v1");
    assert!(init.graph_stats.entity_count > 0);

    // 2. Workspace 1: add a function to auth.rs
    let ws1 = workspace::create(&vai_dir, "add session support", "v1").unwrap();
    let ov1 = workspace::overlay_dir(&vai_dir, &ws1.workspace.id.to_string()).join("src");
    fs::create_dir_all(&ov1).unwrap();
    let auth_v2 = format!(
        "{}\npub fn create_session(user: &str) -> String {{\n    format!(\"session-{{}}\", user)\n}}\n",
        AUTH_RS
    );
    fs::write(ov1.join("auth.rs"), &auth_v2).unwrap();

    // 3. Diff — should detect auth.rs changed and create_session added
    let d1 = diff::compute(&vai_dir, &root).unwrap();
    assert_eq!(d1.file_diffs.len(), 1);
    assert!(d1.entity_changes.iter().any(|c| c.qualified_name.contains("create_session")));

    // 4. Submit → v2 (fast-forward)
    let s1 = merge::submit(&vai_dir, &root).unwrap();
    assert_eq!(s1.version.version_id, "v2");

    // 5. Workspace 2: modify config.rs (different file, no conflict with v2)
    let ws2 = workspace::create(&vai_dir, "add config validation", "v2").unwrap();
    let ov2 = workspace::overlay_dir(&vai_dir, &ws2.workspace.id.to_string()).join("src");
    fs::create_dir_all(&ov2).unwrap();
    let config_v2 = format!(
        "{}\npub fn is_valid(cfg: &Config) -> bool {{\n    !cfg.name.is_empty()\n}}\n",
        CONFIG_RS
    );
    fs::write(ov2.join("config.rs"), &config_v2).unwrap();
    let s2 = merge::submit(&vai_dir, &root).unwrap();
    assert_eq!(s2.version.version_id, "v3");

    // 6. Log — three versions
    let versions = version::list_versions(&vai_dir).unwrap();
    assert_eq!(versions.len(), 3);

    // 7. Show v2 — entity changes
    let v2_changes = version::get_version_changes(&vai_dir, "v2").unwrap();
    assert!(!v2_changes.entity_changes.is_empty());

    // 8. Impact analysis for v2 — v3 modified config.rs (different file) so low/no risk
    let impact = version::analyze_rollback_impact(&vai_dir, "v2").unwrap();
    assert_eq!(impact.target_version.version_id, "v2");

    // 9. Rollback v2 — creates v4, auth.rs reverts to original
    let rb = version::rollback(&vai_dir, &root, "v2", None).unwrap();
    assert_eq!(rb.new_version.version_id, "v4");
    assert_eq!(repo::read_head(&vai_dir).unwrap(), "v4");

    let auth_final = fs::read_to_string(root.join("src").join("auth.rs")).unwrap();
    assert!(
        !auth_final.contains("create_session"),
        "create_session should be rolled back in v4"
    );

    // History is append-only — v4 is a new version, not a rewrite
    let final_versions = version::list_versions(&vai_dir).unwrap();
    assert_eq!(final_versions.len(), 4);
    assert!(final_versions.iter().any(|v| v.version_id == "v4"));
}
