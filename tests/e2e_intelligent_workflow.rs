//! End-to-end integration test for the intelligent workflow.
//!
//! Exercises scope inference, merge patterns, and dashboard capabilities in sequence:
//! 1.  Init repo with sample Rust code, start embedded server.
//! 2.  Create workspace with intent — verify scope inference suggests relevant entities.
//! 3.  Complete workspace — verify predicted vs. actual scope recorded in history.
//! 4.  Create second intent with similar keywords — verify historical learning improves prediction.
//! 5.  Create conflicting workspaces that trigger a merge conflict.
//! 6.  Resolve conflict — verify pattern recorded in merge pattern library.
//! 7.  Record same conflict pattern 11 more times — verify auto-resolution promoted.
//! 8.  Register a watcher agent.
//! 9.  Submit a discovery event — verify issue auto-created with correct scope.
//! 10. Dashboard snapshot (headless) — verify all panels show correct data.

#![cfg(feature = "server")]

use std::fs;

use tempfile::TempDir;

use vai::auth;
use vai::dashboard;
use vai::graph::GraphSnapshot;
use vai::merge;
use vai::merge_patterns::MergePatternStore;
use vai::repo;
use vai::scope_history::ScopeHistoryStore;
use vai::scope_inference;
use vai::server;
use vai::workspace;

// ── Sample source file ────────────────────────────────────────────────────────

/// A simple Rust source file with an `AuthService` struct that will be the
/// target of scope inference and later modified in conflicting workspaces.
const AUTH_RS: &str = r#"/// Authentication service
pub struct AuthService {
    pub secret: String,
}

impl AuthService {
    /// Validates a token against the stored secret.
    pub fn validate_token(&self, token: &str) -> bool {
        token == self.secret
    }

    /// Returns the stored secret.
    pub fn secret(&self) -> &str {
        &self.secret
    }
}
"#;

// ── Setup helper ──────────────────────────────────────────────────────────────

fn setup() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("auth.rs"), AUTH_RS).unwrap();
    repo::init(&root).expect("vai init failed");
    let vai_dir = root.join(".vai");
    (tmp, root, vai_dir)
}

// ── Full E2E test ────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn test_full_intelligent_workflow() {
    let start = std::time::Instant::now();

    // ── Step 1: Init repo and start embedded server ───────────────────────────
    let (_tmp, root, vai_dir) = setup();

    let (_, api_key) = auth::create(&vai_dir, "test-agent").expect("create key");
    let (addr, shutdown_tx) = server::start_for_testing(&vai_dir)
        .await
        .expect("start_for_testing failed");
    let repo_config = repo::read_config(&vai_dir).expect("read config");
    let repo = &repo_config.name;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    // ── Step 2: Scope inference — verify relevant entities predicted ──────────
    let graph_db = vai_dir.join("graph").join("snapshot.db");
    let snapshot = GraphSnapshot::open(&graph_db).expect("open graph snapshot");

    let inference1 = scope_inference::infer(&snapshot, "fix auth token validation", 2)
        .expect("scope inference failed");

    // After parsing auth.rs, the graph has AuthService, validate_token, secret.
    // The intent "fix auth token validation" should match at least one entity.
    assert!(
        !inference1.predicted_scope.is_empty(),
        "scope inference should predict at least one entity; terms: {:?}",
        inference1.terms
    );
    assert!(
        !inference1.terms.is_empty(),
        "term extraction should produce non-empty terms"
    );

    // ── Step 3: Submit workspace A and record scope history ───────────────────
    // Create both WS A and WS B before submitting either, so they share v1 as base.
    let head_v1 = repo::read_head(&vai_dir).expect("read HEAD");

    // WS A: change validate_token body — add non-empty check.
    let ws_a_result =
        workspace::create(&vai_dir, "fix auth token validation", &head_v1).expect("create WS A");
    let ws_a_id = ws_a_result.workspace.id;

    let auth_ws_a = AUTH_RS.replace(
        "token == self.secret",
        "!token.is_empty() && token == self.secret",
    );
    let ws_a_overlay = workspace::overlay_dir(&vai_dir, &ws_a_id.to_string());
    fs::create_dir_all(ws_a_overlay.join("src")).unwrap();
    fs::write(ws_a_overlay.join("src").join("auth.rs"), &auth_ws_a).unwrap();

    // WS B: change validate_token body — add length check (same file, same line, different change).
    let ws_b_result = workspace::create(&vai_dir, "add length check for auth token", &head_v1)
        .expect("create WS B");
    let ws_b_id = ws_b_result.workspace.id;

    let auth_ws_b = AUTH_RS.replace(
        "token == self.secret",
        "token.len() > 4 && token == self.secret",
    );
    let ws_b_overlay = workspace::overlay_dir(&vai_dir, &ws_b_id.to_string());
    fs::create_dir_all(ws_b_overlay.join("src")).unwrap();
    fs::write(ws_b_overlay.join("src").join("auth.rs"), &auth_ws_b).unwrap();

    // Submit WS A (set it as active first — WS B was created last so it's currently active).
    fs::write(vai_dir.join("workspaces").join("active"), ws_a_id.to_string()).unwrap();
    let submit_a = merge::submit(&vai_dir, &root).expect("submit WS A failed");
    assert_eq!(submit_a.version.version_id, "v2", "WS A should create v2");

    // Record scope history: predicted scope from step 2 vs. actual entities from submission.
    let history_db = vai_dir.join("graph").join("history.db");
    let history_store = ScopeHistoryStore::open(&history_db).expect("open history store");
    let predicted_ids: Vec<String> = inference1
        .predicted_scope
        .iter()
        .map(|s| s.entity.id.clone())
        .collect();
    let record = history_store
        .record(
            "fix auth token validation",
            &inference1.terms,
            &predicted_ids,
            &submit_a.entity_ids,
            Some(&ws_a_id.to_string()),
        )
        .expect("record scope history");
    assert_eq!(record.intent_text, "fix auth token validation");

    // ── Step 4: History-enhanced inference for similar intent ─────────────────
    // Reload the graph snapshot (WS A's submission may have updated it).
    let snapshot2 = GraphSnapshot::open(&graph_db).expect("reload graph snapshot");
    let inference2 = scope_inference::infer_with_history(
        &snapshot2,
        &history_store,
        "add length check for auth token",
        2,
    )
    .expect("infer_with_history failed");

    // The history record for "fix auth token validation" shares terms
    // "auth", "token" with the new intent → at least one entity should be
    // boosted or the history_influences list should be non-empty.
    // We assert the inference completed and returned a non-empty scope.
    assert!(
        !inference2.predicted_scope.is_empty() || !inference2.history_influences.is_empty(),
        "history-enhanced inference should produce entities or history influences"
    );

    // ── Step 5: Create conflicting workspaces — trigger merge conflict ─────────
    // WS B is still based on v1 but HEAD is now v2 (after WS A's submit).
    // Both workspaces touched the same line of auth.rs → semantic conflict.
    fs::write(
        vai_dir.join("workspaces").join("active"),
        ws_b_id.to_string(),
    )
    .unwrap();

    let conflict_result = merge::submit(&vai_dir, &root);
    assert!(
        matches!(conflict_result, Err(merge::MergeError::SemanticConflicts { .. })),
        "WS B should produce a SemanticConflicts error; got: {conflict_result:?}"
    );

    // The conflict records should be persisted.
    let conflicts = merge::list_conflicts(&vai_dir).expect("list_conflicts failed");
    assert!(
        !conflicts.is_empty(),
        "at least one conflict record should exist after WS B's failed merge"
    );

    // ── Step 6: Resolve conflict — verify pattern recorded ────────────────────
    let conflict = &conflicts[0];
    merge::resolve_conflict(&vai_dir, &conflict.conflict_id.to_string())
        .expect("resolve_conflict failed");

    let pattern_store = MergePatternStore::open(&vai_dir).expect("open pattern store");
    let patterns = pattern_store.list_patterns().expect("list patterns");
    assert!(
        !patterns.is_empty(),
        "resolve_conflict should have recorded a merge pattern"
    );
    let recorded_pattern = &patterns[0];
    assert!(
        recorded_pattern.instance_count >= 1,
        "pattern should have at least 1 instance"
    );

    // ── Step 7: Promote pattern via repeated recording → auto-resolution ──────
    // Record the same conflict pattern 10 more times (11 total) with 100 % success.
    // This meets the promotion criteria: instance_count > 10 and success_rate ≥ 0.90.
    let mut pattern_store_mut = MergePatternStore::open(&vai_dir).expect("open pattern store mut");
    for _ in 0..10 {
        pattern_store_mut
            .record_resolution(conflict, "manual", false)
            .expect("record_resolution failed");
    }

    // Verify auto-resolution is now promoted.
    let updated_pattern = pattern_store_mut
        .check_auto_resolution(conflict)
        .expect("check_auto_resolution failed");
    assert!(
        updated_pattern.is_some(),
        "pattern should be eligible for auto-resolution after 11+ successful instances"
    );
    let promoted = updated_pattern.unwrap();
    assert!(
        promoted.auto_resolution_enabled,
        "auto_resolution_enabled should be true after promotion"
    );
    assert!(
        promoted.instance_count > 10,
        "instance_count should exceed 10 (got {})",
        promoted.instance_count
    );
    assert!(
        promoted.success_rate() >= 0.90,
        "success_rate should be ≥ 0.90 (got {:.2})",
        promoted.success_rate()
    );

    // ── Step 8: Register a watcher agent ─────────────────────────────────────
    let register_res = client
        .post(format!("{base}/api/repos/{repo}/watchers/register"))
        .bearer_auth(&api_key)
        .json(&serde_json::json!({
            "agent_id": "security-scanner",
            "watch_type": "security",
            "description": "Scans for authentication vulnerabilities",
            "issue_creation_policy": {
                "auto_create": true,
                "max_per_hour": 5,
                "require_approval_above": "critical"
            }
        }))
        .send()
        .await
        .expect("register watcher request failed");

    assert_eq!(register_res.status(), 201, "register watcher should return 201");
    let watcher: serde_json::Value = register_res.json().await.unwrap();
    assert_eq!(watcher["agent_id"], "security-scanner");
    assert_eq!(watcher["status"], "active");

    // ── Step 9: Submit discovery event → verify issue auto-created ────────────
    let discovery_res = client
        .post(format!("{base}/api/repos/{repo}/discoveries"))
        .bearer_auth(&api_key)
        .json(&serde_json::json!({
            "agent_id": "security-scanner",
            "event": {
                "type": "security_vulnerability_discovered",
                "source": "auth-scanner",
                "severity": "high",
                "affected_entities": ["AuthService", "validate_token"],
                "description": "Token validation does not check expiry"
            }
        }))
        .send()
        .await
        .expect("submit discovery request failed");

    assert_eq!(discovery_res.status(), 201, "new discovery should return 201");
    let discovery_outcome: serde_json::Value = discovery_res.json().await.unwrap();
    assert!(
        !discovery_outcome["suppressed"].as_bool().unwrap_or(true),
        "discovery should not be suppressed"
    );
    assert!(
        discovery_outcome["created_issue_id"].is_string(),
        "discovery should auto-create an issue; got: {discovery_outcome}"
    );

    // ── Step 10: Dashboard snapshot — verify all panels show correct data ──────
    let snap = dashboard::snapshot(&vai_dir).expect("dashboard snapshot failed");

    // Active Work panel: should include WS A (Merged) and WS B (Created, failed to submit).
    assert!(
        snap.workspaces.len() >= 2,
        "snapshot should show at least 2 workspaces (WS A + WS B); got {}",
        snap.workspaces.len()
    );

    // Issues panel: at least 1 issue created by the watcher.
    let total_issues =
        snap.open_issues + snap.in_progress_issues + snap.resolved_issues + snap.closed_issues;
    assert!(
        total_issues >= 1,
        "snapshot should show at least 1 issue from the watcher discovery"
    );

    // Recent Versions panel: v1 (init) + v2 (WS A submit).
    assert!(
        snap.recent_versions.len() >= 2,
        "snapshot should show at least 2 versions; got {}",
        snap.recent_versions.len()
    );
    let version_ids: Vec<&str> = snap.recent_versions.iter().map(|v| v.version_id.as_str()).collect();
    assert!(
        version_ids.contains(&"v2"),
        "recent versions should include v2; got {version_ids:?}"
    );

    // System Health panel: graph should have entities from auth.rs.
    if let Some(stats) = &snap.graph_stats {
        assert!(
            stats.entity_count > 0,
            "graph should have at least one entity after init + merge"
        );
    }

    // ── Cleanup ───────────────────────────────────────────────────────────────
    let _ = shutdown_tx.send(());

    // ── Timing assertion ──────────────────────────────────────────────────────
    let elapsed = start.elapsed();
    assert!(
        elapsed.as_secs() < 90,
        "intelligent workflow E2E test must complete in under 90 seconds (took {elapsed:?})"
    );
}
