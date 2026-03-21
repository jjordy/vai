//! Integration test for the Phase 4 watcher agent registration and discovery pipeline.
//!
//! Exercises:
//! 1. Register a watcher via `POST /api/watchers/register`.
//! 2. List watchers via `GET /api/watchers`.
//! 3. Submit a discovery event via `POST /api/discoveries` → verify issue auto-created.
//! 4. Submit duplicate discovery → verify suppressed.
//! 5. Pause watcher via `POST /api/watchers/:id/pause`.
//! 6. Attempt discovery on paused watcher → 404.
//! 7. Resume watcher → discovery succeeds again.

use std::fs;
use tempfile::TempDir;
use vai::auth;
use vai::repo;
use vai::server;

// ── Test repo setup ───────────────────────────────────────────────────────────

fn setup() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        src.join("lib.rs"),
        "pub fn hello() -> &'static str { \"hello\" }\n",
    )
    .unwrap();
    repo::init(&root).expect("vai init failed");
    let vai_dir = root.join(".vai");
    (tmp, root, vai_dir)
}

// ── Integration test ──────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn test_watcher_registration_and_discovery_pipeline() {
    let (_tmp, _root, vai_dir) = setup();

    // Create an API key.
    let (_, key) = auth::create(&vai_dir, "test-agent").expect("create key");

    // Start the embedded server.
    let (addr, shutdown_tx) = server::start_for_testing(&vai_dir)
        .await
        .expect("start_for_testing failed");

    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    // ── 1. Register a watcher ────────────────────────────────────────────────
    let register_res = client
        .post(format!("{base}/api/watchers/register"))
        .bearer_auth(&key)
        .json(&serde_json::json!({
            "agent_id": "ci-watcher",
            "watch_type": "test_suite",
            "description": "Monitors nightly CI",
            "issue_creation_policy": {
                "auto_create": true,
                "max_per_hour": 10,
                "require_approval_above": "high"
            }
        }))
        .send()
        .await
        .expect("register request failed");

    assert_eq!(register_res.status(), 201, "register should return 201");
    let watcher: serde_json::Value = register_res.json().await.unwrap();
    assert_eq!(watcher["agent_id"], "ci-watcher");
    assert_eq!(watcher["status"], "active");

    // ── 2. List watchers ─────────────────────────────────────────────────────
    let list_res = client
        .get(format!("{base}/api/watchers"))
        .bearer_auth(&key)
        .send()
        .await
        .expect("list request failed");

    assert_eq!(list_res.status(), 200);
    let watchers: Vec<serde_json::Value> = list_res.json().await.unwrap();
    assert_eq!(watchers.len(), 1);
    assert_eq!(watchers[0]["agent_id"], "ci-watcher");

    // Duplicate registration should fail.
    let dup_res = client
        .post(format!("{base}/api/watchers/register"))
        .bearer_auth(&key)
        .json(&serde_json::json!({
            "agent_id": "ci-watcher",
            "watch_type": "test_suite",
            "description": "duplicate"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(dup_res.status(), 409, "duplicate registration should return 409");

    // ── 3. Submit a discovery event → issue auto-created ─────────────────────
    let disc_res = client
        .post(format!("{base}/api/discoveries"))
        .bearer_auth(&key)
        .json(&serde_json::json!({
            "agent_id": "ci-watcher",
            "event": {
                "type": "test_failure_discovered",
                "suite": "unit",
                "test_name": "test_hello",
                "failure_output": "assertion `left == right` failed",
                "version": "v1"
            }
        }))
        .send()
        .await
        .expect("submit discovery failed");

    assert_eq!(disc_res.status(), 201, "new discovery should return 201");
    let outcome: serde_json::Value = disc_res.json().await.unwrap();
    assert!(!outcome["suppressed"].as_bool().unwrap_or(true));
    assert!(
        outcome["created_issue_id"].is_string(),
        "issue should have been auto-created, got: {outcome}"
    );

    // ── 4. Submit duplicate → suppressed ─────────────────────────────────────
    let dup_disc_res = client
        .post(format!("{base}/api/discoveries"))
        .bearer_auth(&key)
        .json(&serde_json::json!({
            "agent_id": "ci-watcher",
            "event": {
                "type": "test_failure_discovered",
                "suite": "unit",
                "test_name": "test_hello",
                "failure_output": "still failing",
                "version": "v1"
            }
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(dup_disc_res.status(), 200, "duplicate should return 200");
    let dup_outcome: serde_json::Value = dup_disc_res.json().await.unwrap();
    assert!(
        dup_outcome["suppressed"].as_bool().unwrap_or(false),
        "duplicate discovery should be suppressed"
    );

    // ── 5. Pause the watcher ──────────────────────────────────────────────────
    let pause_res = client
        .post(format!("{base}/api/watchers/ci-watcher/pause"))
        .bearer_auth(&key)
        .send()
        .await
        .unwrap();
    assert_eq!(pause_res.status(), 200);
    let paused: serde_json::Value = pause_res.json().await.unwrap();
    assert_eq!(paused["status"], "paused");

    // ── 6. Discovery on paused watcher → error ────────────────────────────────
    let paused_disc = client
        .post(format!("{base}/api/discoveries"))
        .bearer_auth(&key)
        .json(&serde_json::json!({
            "agent_id": "ci-watcher",
            "event": {
                "type": "test_failure_discovered",
                "suite": "unit",
                "test_name": "test_other",
                "failure_output": "error",
                "version": null
            }
        }))
        .send()
        .await
        .unwrap();
    assert!(
        paused_disc.status().is_client_error(),
        "paused watcher should reject discoveries"
    );

    // ── 7. Resume watcher → discovery succeeds ────────────────────────────────
    let resume_res = client
        .post(format!("{base}/api/watchers/ci-watcher/resume"))
        .bearer_auth(&key)
        .send()
        .await
        .unwrap();
    assert_eq!(resume_res.status(), 200);
    let resumed: serde_json::Value = resume_res.json().await.unwrap();
    assert_eq!(resumed["status"], "active");

    let new_disc = client
        .post(format!("{base}/api/discoveries"))
        .bearer_auth(&key)
        .json(&serde_json::json!({
            "agent_id": "ci-watcher",
            "event": {
                "type": "test_failure_discovered",
                "suite": "integration",
                "test_name": "test_new",
                "failure_output": "timeout",
                "version": null
            }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(new_disc.status(), 201, "resumed watcher should accept discoveries");

    // ── Cleanup ───────────────────────────────────────────────────────────────
    let _ = shutdown_tx.send(());
}
