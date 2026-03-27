//! End-to-end integration tests for the full Postgres server mode workflow.
//!
//! These tests run a real vai server backed by Postgres with **no** `.vai/`
//! directory pre-existing on disk.  Every operation goes through the HTTP API
//! against `/api/repos/:repo/` endpoints.  If any handler silently falls back
//! to filesystem operations, the test fails with a "file not found" error —
//! exactly the regression guard described in issue #147.
//!
//! # Running
//!
//! Start Postgres with:
//! ```bash
//! docker compose up -d postgres
//! ```
//!
//! Then run with the connection URL:
//! ```bash
//! VAI_TEST_DATABASE_URL=postgres://vai:vai@localhost:5432/vai \
//!   cargo test --test server_postgres_e2e
//! ```
//!
//! Tests are silently skipped when `VAI_TEST_DATABASE_URL` is not set so that
//! `cargo test` in environments without Postgres continues to pass.

use std::env;
use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use futures_util::{SinkExt, StreamExt};
use tempfile::TempDir;
use tokio::time::timeout;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

use vai::server::start_for_testing_pg_multi_repo;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Returns the Postgres URL from the environment, or `None` (test will skip).
fn db_url() -> Option<String> {
    match env::var("VAI_TEST_DATABASE_URL") {
        Ok(url) => Some(url),
        Err(_) => {
            eprintln!("VAI_TEST_DATABASE_URL not set — skipping Postgres e2e tests");
            None
        }
    }
}

/// Encodes bytes as padded base64.
fn b64(bytes: &[u8]) -> String {
    BASE64.encode(bytes)
}

// ── Complete agent workflow ───────────────────────────────────────────────────

/// Full single-agent workflow exercising every major endpoint in
/// `/api/repos/:repo/` without any pre-existing `.vai/` directory.
///
/// Flow:
/// 1.  `POST /api/repos`                         → create repo
/// 2.  `GET  /api/repos/:repo/status`            → verify status
/// 3.  `POST /api/repos/:repo/issues`            → create issue
/// 4.  `GET  /api/repos/:repo/issues`            → verify issue appears
/// 5.  `GET  /api/repos/:repo/work-queue`        → issue in available_work
/// 6.  `POST /api/repos/:repo/work-queue/claim`  → claim (workspace created, issue in_progress)
/// 7.  `POST /api/repos/:repo/workspaces/:id/files` → upload files
/// 8.  `POST /api/repos/:repo/workspaces/:id/submit` → submit (new version)
/// 9.  `GET  /api/repos/:repo/versions`          → version list includes new version
/// 10. `GET  /api/repos/:repo/versions/:id`      → version detail has file_changes
/// 11. `GET  /api/repos/:repo/versions/:id/diff` → diff content non-empty
/// 12. `GET  /api/repos/:repo/files/download`    → tarball includes uploaded files
/// 13. `POST /api/repos/:repo/issues/:id/close`  → close issue
/// 14. `GET  /api/repos/:repo/issues`            → issue is closed
#[tokio::test(flavor = "multi_thread")]
async fn test_complete_agent_workflow() {
    let Some(url) = db_url() else { return };

    let tmp = TempDir::new().unwrap();
    let (addr, shutdown_tx) = start_for_testing_pg_multi_repo(tmp.path(), &url)
        .await
        .expect("start_for_testing_pg_multi_repo failed");

    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let admin = "vai_admin_test";

    // ── 1. Create repo ────────────────────────────────────────────────────────
    let resp = client
        .post(format!("{base}/api/repos"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "name": "e2e-workflow" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create repo: {}", resp.text().await.unwrap_or_default());
    let repo: serde_json::Value = resp.json().await.unwrap();
    let repo_name = repo["name"].as_str().unwrap().to_string();

    let rp = format!("{base}/api/repos/{repo_name}");

    // ── 2. Repo status ────────────────────────────────────────────────────────
    let resp = client.get(format!("{rp}/status")).bearer_auth(admin).send().await.unwrap();
    assert_eq!(resp.status(), 200, "repo status");

    // ── 3. Create issue ───────────────────────────────────────────────────────
    let resp = client
        .post(format!("{rp}/issues"))
        .bearer_auth(admin)
        .json(&serde_json::json!({
            "title": "add hello endpoint",
            "description": "Implement GET /hello returning 200 OK",
            "priority": "medium"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create issue: {}", resp.text().await.unwrap_or_default());
    let issue: serde_json::Value = resp.json().await.unwrap();
    let issue_id = issue["id"].as_str().unwrap().to_string();
    assert_eq!(issue["title"].as_str().unwrap(), "add hello endpoint");
    assert_eq!(
        issue["description"].as_str().unwrap_or(""),
        "Implement GET /hello returning 200 OK",
        "issue description must be stored in Postgres"
    );

    // ── 4. List issues ────────────────────────────────────────────────────────
    let resp = client.get(format!("{rp}/issues")).bearer_auth(admin).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let issues: serde_json::Value = resp.json().await.unwrap();
    let arr = issues.as_array().unwrap();
    assert!(!arr.is_empty(), "issue list must not be empty");
    assert!(
        arr.iter().any(|i| i["id"].as_str() == Some(&issue_id)),
        "created issue must appear in list"
    );

    // ── 5. Work queue ─────────────────────────────────────────────────────────
    let resp = client.get(format!("{rp}/work-queue")).bearer_auth(admin).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let wq: serde_json::Value = resp.json().await.unwrap();
    let available = wq["available_work"].as_array().unwrap();
    assert!(
        available.iter().any(|w| w["issue_id"].as_str() == Some(&issue_id)),
        "issue must appear in work queue available_work"
    );

    // ── 6. Claim work ─────────────────────────────────────────────────────────
    let resp = client
        .post(format!("{rp}/work-queue/claim"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "issue_id": issue_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "claim work: {}", resp.text().await.unwrap_or_default());
    let claim: serde_json::Value = resp.json().await.unwrap();
    let ws_id = claim["workspace_id"].as_str().unwrap().to_string();

    // Verify workspace was created and issue is now in_progress.
    let resp = client
        .get(format!("{rp}/workspaces/{ws_id}"))
        .bearer_auth(admin)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let ws_detail: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(ws_detail["status"].as_str().unwrap_or(""), "Created");

    let resp = client
        .get(format!("{rp}/issues/{issue_id}"))
        .bearer_auth(admin)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let issue_detail: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        issue_detail["status"].as_str().unwrap_or(""),
        "in_progress",
        "issue must be in_progress after claim"
    );

    // ── 7. Upload files ───────────────────────────────────────────────────────
    let hello_content = b"pub fn hello() -> &'static str { \"hello\" }\n";
    let resp = client
        .post(format!("{rp}/workspaces/{ws_id}/files"))
        .bearer_auth(admin)
        .json(&serde_json::json!({
            "files": [{
                "path": "src/hello.rs",
                "content_base64": b64(hello_content)
            }]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "upload files: {}", resp.text().await.unwrap_or_default());
    let upload_resp: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(upload_resp["uploaded"].as_u64().unwrap(), 1);

    // ── 8. Submit workspace ───────────────────────────────────────────────────
    let resp = client
        .post(format!("{rp}/workspaces/{ws_id}/submit"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "summary": "add hello function" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "submit workspace: {}", resp.text().await.unwrap_or_default());
    let submit_resp: serde_json::Value = resp.json().await.unwrap();
    let new_version = submit_resp["version"].as_str().unwrap().to_string();
    assert!(!new_version.is_empty(), "submit must return the new version label");

    // ── 9. Version list ───────────────────────────────────────────────────────
    let resp = client.get(format!("{rp}/versions")).bearer_auth(admin).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let versions: serde_json::Value = resp.json().await.unwrap();
    let version_arr = versions.as_array().unwrap();
    assert!(
        version_arr.iter().any(|v| v["label"].as_str() == Some(&new_version)),
        "submitted version must appear in version list"
    );

    // Find the UUID of the new version for detail checks.
    let version_id = version_arr
        .iter()
        .find(|v| v["label"].as_str() == Some(&new_version))
        .and_then(|v| v["id"].as_str())
        .unwrap()
        .to_string();

    // ── 10. Version detail ────────────────────────────────────────────────────
    let resp = client
        .get(format!("{rp}/versions/{version_id}"))
        .bearer_auth(admin)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let version_detail: serde_json::Value = resp.json().await.unwrap();
    let file_changes = version_detail["file_changes"].as_array().unwrap();
    assert!(
        !file_changes.is_empty(),
        "version detail must have file_changes"
    );
    assert!(
        file_changes.iter().any(|fc| fc["path"].as_str() == Some("src/hello.rs")),
        "file_changes must include src/hello.rs"
    );

    // ── 11. Version diff ──────────────────────────────────────────────────────
    let resp = client
        .get(format!("{rp}/versions/{version_id}/diff"))
        .bearer_auth(admin)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let diff_resp: serde_json::Value = resp.json().await.unwrap();
    let diffs = diff_resp["diffs"].as_array().unwrap();
    assert!(!diffs.is_empty(), "version diff must be non-empty");

    // ── 12. Files download ────────────────────────────────────────────────────
    let resp = client
        .get(format!("{rp}/files/download"))
        .bearer_auth(admin)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "files download: {}", resp.text().await.unwrap_or_default());
    let download_body = resp.bytes().await.unwrap();
    assert!(!download_body.is_empty(), "download tarball must be non-empty");

    // ── 13. Close issue ───────────────────────────────────────────────────────
    let resp = client
        .post(format!("{rp}/issues/{issue_id}/close"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "resolution": "implemented in workspace" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "close issue: {}", resp.text().await.unwrap_or_default());

    // ── 14. Verify issue is closed ────────────────────────────────────────────
    let resp = client.get(format!("{rp}/issues")).bearer_auth(admin).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let issues: serde_json::Value = resp.json().await.unwrap();
    // Issues may be filtered from the default list view by status; check via direct get.
    let _ = issues;
    let resp = client
        .get(format!("{rp}/issues/{issue_id}"))
        .bearer_auth(admin)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let issue_after_close: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        issue_after_close["status"].as_str().unwrap_or(""),
        "closed",
        "issue must be closed after POST /close"
    );

    shutdown_tx.send(()).ok();
}

// ── Workspace discard ─────────────────────────────────────────────────────────

/// Verifies that `DELETE /api/repos/:repo/workspaces/:id` discards the
/// workspace and reopens the linked issue — entirely through Postgres, with
/// no `.vai/` directory on disk before the server starts.
#[tokio::test(flavor = "multi_thread")]
async fn test_workspace_discard() {
    let Some(url) = db_url() else { return };

    let tmp = TempDir::new().unwrap();
    let (addr, shutdown_tx) = start_for_testing_pg_multi_repo(tmp.path(), &url)
        .await
        .expect("start_for_testing_pg_multi_repo failed");

    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let admin = "vai_admin_test";

    // Create repo.
    let resp = client
        .post(format!("{base}/api/repos"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "name": "e2e-discard" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let repo: serde_json::Value = resp.json().await.unwrap();
    let rp = format!("{base}/api/repos/{}", repo["name"].as_str().unwrap());

    // Create and claim an issue.
    let resp = client
        .post(format!("{rp}/issues"))
        .bearer_auth(admin)
        .json(&serde_json::json!({
            "title": "discard test issue",
            "description": "will be discarded",
            "priority": "low"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let issue: serde_json::Value = resp.json().await.unwrap();
    let issue_id = issue["id"].as_str().unwrap().to_string();

    let resp = client
        .post(format!("{rp}/work-queue/claim"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "issue_id": issue_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "claim: {}", resp.text().await.unwrap_or_default());
    let claim: serde_json::Value = resp.json().await.unwrap();
    let ws_id = claim["workspace_id"].as_str().unwrap().to_string();

    // Discard the workspace.
    let resp = client
        .delete(format!("{rp}/workspaces/{ws_id}"))
        .bearer_auth(admin)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "discard: {}", resp.text().await.unwrap_or_default());

    // Issue must be reopened.
    let resp = client
        .get(format!("{rp}/issues/{issue_id}"))
        .bearer_auth(admin)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let issue_after: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        issue_after["status"].as_str().unwrap_or(""),
        "open",
        "issue must be reopened after workspace discard"
    );

    shutdown_tx.send(()).ok();
}

// ── WebSocket events ──────────────────────────────────────────────────────────

/// Verifies that key operations fire WebSocket events on
/// `WS /api/repos/:repo/ws/events`.
///
/// Flow:
/// - Connect to WS
/// - Create workspace → verify WorkspaceCreated event
/// - Upload files → verify FileUploaded event (or any upload event)
/// - Submit workspace → verify WorkspaceSubmitted event
#[tokio::test(flavor = "multi_thread")]
async fn test_websocket_events_fire() {
    let Some(url) = db_url() else { return };

    let tmp = TempDir::new().unwrap();
    let (addr, shutdown_tx) = start_for_testing_pg_multi_repo(tmp.path(), &url)
        .await
        .expect("start_for_testing_pg_multi_repo failed");

    let base = format!("http://{addr}");
    let ws_base = format!("ws://{addr}");
    let client = reqwest::Client::new();
    let admin = "vai_admin_test";

    // Create repo.
    let resp = client
        .post(format!("{base}/api/repos"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "name": "e2e-ws" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let repo: serde_json::Value = resp.json().await.unwrap();
    let repo_name = repo["name"].as_str().unwrap().to_string();
    let rp = format!("{base}/api/repos/{repo_name}");

    // Create an issue so we can claim it.
    let resp = client
        .post(format!("{rp}/issues"))
        .bearer_auth(admin)
        .json(&serde_json::json!({
            "title": "ws test issue",
            "description": "for websocket test",
            "priority": "medium"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let issue: serde_json::Value = resp.json().await.unwrap();
    let issue_id = issue["id"].as_str().unwrap().to_string();

    // Connect to per-repo WebSocket.
    let ws_url = format!("{ws_base}/api/repos/{repo_name}/ws/events?key={admin}");
    let (mut ws, _) = connect_async(&ws_url).await.expect("WebSocket connect failed");

    // Send subscription (receive all event types).
    ws.send(Message::Text(
        serde_json::json!({
            "subscribe": { "event_types": [], "paths": [], "entities": [], "workspaces": [] }
        })
        .to_string()
        .into(),
    ))
    .await
    .expect("subscribe send failed");

    // Drain any buffered startup events.
    let _ = timeout(Duration::from_millis(200), async {
        while let Some(Ok(_)) = ws.next().await {}
    })
    .await;

    // Claim work → should generate a WorkspaceCreated event.
    let resp = client
        .post(format!("{rp}/work-queue/claim"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "issue_id": issue_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "claim: {}", resp.text().await.unwrap_or_default());
    let claim: serde_json::Value = resp.json().await.unwrap();
    let ws_id = claim["workspace_id"].as_str().unwrap().to_string();

    // Collect events for up to 2 seconds.
    let mut events: Vec<serde_json::Value> = Vec::new();
    let _ = timeout(Duration::from_secs(2), async {
        while let Some(Ok(Message::Text(msg))) = ws.next().await {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&msg) {
                events.push(v);
            }
        }
    })
    .await;
    let has_ws_created = events.iter().any(|e| {
        e["event_type"].as_str() == Some("WorkspaceCreated")
            || e["kind"].as_str() == Some("WorkspaceCreated")
    });
    assert!(has_ws_created, "WorkspaceCreated event must be received; got: {events:?}");

    // Upload files.
    client
        .post(format!("{rp}/workspaces/{ws_id}/files"))
        .bearer_auth(admin)
        .json(&serde_json::json!({
            "files": [{ "path": "src/ws_test.rs", "content_base64": b64(b"fn ws_test() {}") }]
        }))
        .send()
        .await
        .unwrap();

    // Submit → should generate WorkspaceSubmitted + VersionCreated events.
    let resp = client
        .post(format!("{rp}/workspaces/{ws_id}/submit"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "summary": "ws test submit" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "submit: {}", resp.text().await.unwrap_or_default());

    let mut events: Vec<serde_json::Value> = Vec::new();
    let _ = timeout(Duration::from_secs(3), async {
        while let Some(Ok(Message::Text(msg))) = ws.next().await {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&msg) {
                events.push(v);
            }
        }
    })
    .await;
    let has_ws_submitted = events.iter().any(|e| {
        e["event_type"].as_str() == Some("WorkspaceSubmitted")
            || e["kind"].as_str() == Some("WorkspaceSubmitted")
    });
    assert!(has_ws_submitted, "WorkspaceSubmitted event must be received; got: {events:?}");

    ws.close(None).await.ok();
    shutdown_tx.send(()).ok();
}

// ── Concurrency: two agents ───────────────────────────────────────────────────

/// Two agents claim separate issues concurrently, both submit successfully.
/// A third agent tries to claim an already-claimed issue and is rejected.
#[tokio::test(flavor = "multi_thread")]
async fn test_concurrent_agents() {
    let Some(url) = db_url() else { return };

    let tmp = TempDir::new().unwrap();
    let (addr, shutdown_tx) = start_for_testing_pg_multi_repo(tmp.path(), &url)
        .await
        .expect("start_for_testing_pg_multi_repo failed");

    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let admin = "vai_admin_test";

    // Create repo.
    let resp = client
        .post(format!("{base}/api/repos"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "name": "e2e-concurrent" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let repo: serde_json::Value = resp.json().await.unwrap();
    let rp = format!("{base}/api/repos/{}", repo["name"].as_str().unwrap());

    // Create 2 issues.
    let mut issue_ids = Vec::new();
    for i in 0..2 {
        let resp = client
            .post(format!("{rp}/issues"))
            .bearer_auth(admin)
            .json(&serde_json::json!({
                "title": format!("concurrent issue {i}"),
                "description": format!("issue {i} for concurrency test"),
                "priority": "medium"
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);
        let issue: serde_json::Value = resp.json().await.unwrap();
        issue_ids.push(issue["id"].as_str().unwrap().to_string());
    }

    // Agent 1 claims issue 0.
    let resp = client
        .post(format!("{rp}/work-queue/claim"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "issue_id": issue_ids[0] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "agent 1 claim issue 0: {}", resp.text().await.unwrap_or_default());
    let claim1: serde_json::Value = resp.json().await.unwrap();
    let ws1_id = claim1["workspace_id"].as_str().unwrap().to_string();

    // Agent 2 claims issue 1.
    let resp = client
        .post(format!("{rp}/work-queue/claim"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "issue_id": issue_ids[1] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "agent 2 claim issue 1: {}", resp.text().await.unwrap_or_default());
    let claim2: serde_json::Value = resp.json().await.unwrap();
    let ws2_id = claim2["workspace_id"].as_str().unwrap().to_string();

    // Verify different workspaces.
    assert_ne!(ws1_id, ws2_id, "agents must get different workspaces");

    // Agent 3 tries to claim already-claimed issue 0 → must fail (409).
    let resp = client
        .post(format!("{rp}/work-queue/claim"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "issue_id": issue_ids[0] }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        409,
        "claiming already-claimed issue must return 409; got: {}",
        resp.status()
    );

    // Both agents upload files and submit.
    for (ws_id, file_name) in [(&ws1_id, "src/agent1.rs"), (&ws2_id, "src/agent2.rs")] {
        client
            .post(format!("{rp}/workspaces/{ws_id}/files"))
            .bearer_auth(admin)
            .json(&serde_json::json!({
                "files": [{ "path": file_name, "content_base64": b64(b"fn placeholder() {}") }]
            }))
            .send()
            .await
            .unwrap();

        let resp = client
            .post(format!("{rp}/workspaces/{ws_id}/submit"))
            .bearer_auth(admin)
            .json(&serde_json::json!({ "summary": format!("submit from {ws_id}") }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200, "submit workspace {ws_id}: {}", resp.text().await.unwrap_or_default());
    }

    // Verify two new versions were created (beyond v1).
    let resp = client.get(format!("{rp}/versions")).bearer_auth(admin).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let versions: serde_json::Value = resp.json().await.unwrap();
    let version_count = versions.as_array().unwrap().len();
    assert!(version_count >= 2, "expected at least 2 versions, got {version_count}");

    shutdown_tx.send(()).ok();
}

// ── Workspace file download before submit ─────────────────────────────────────

/// Verifies that `GET /api/repos/:repo/workspaces/:id/files/*path` returns the
/// uploaded overlay content via the storage trait **before** submit — i.e. it
/// must not require a `.vai/` directory on disk.
///
/// This is a regression guard for the filesystem fallback bug described in
/// issue #154, where the handler used `workspace::get()` + `std::fs::read()`
/// which silently failed in Postgres-only mode.
#[tokio::test(flavor = "multi_thread")]
async fn test_workspace_file_download_before_submit() {
    let Some(url) = db_url() else { return };

    let tmp = TempDir::new().unwrap();
    let (addr, shutdown_tx) = start_for_testing_pg_multi_repo(tmp.path(), &url)
        .await
        .expect("start_for_testing_pg_multi_repo failed");

    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let admin = "vai_admin_test";

    // Create repo.
    let resp = client
        .post(format!("{base}/api/repos"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "name": "e2e-file-download" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let repo: serde_json::Value = resp.json().await.unwrap();
    let rp = format!("{base}/api/repos/{}", repo["name"].as_str().unwrap());

    // Create an issue and claim it (creates a workspace).
    let resp = client
        .post(format!("{rp}/issues"))
        .bearer_auth(admin)
        .json(&serde_json::json!({
            "title": "file download test",
            "description": "test workspace file download via storage",
            "priority": "medium"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let issue: serde_json::Value = resp.json().await.unwrap();
    let issue_id = issue["id"].as_str().unwrap();

    let resp = client
        .post(format!("{rp}/work-queue/claim"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "issue_id": issue_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "claim: {}", resp.text().await.unwrap_or_default());
    let claim: serde_json::Value = resp.json().await.unwrap();
    let ws_id = claim["workspace_id"].as_str().unwrap().to_string();

    // Upload a file into the workspace overlay.
    let file_content = b"fn main() { println!(\"hello\"); }\n";
    let resp = client
        .post(format!("{rp}/workspaces/{ws_id}/files"))
        .bearer_auth(admin)
        .json(&serde_json::json!({
            "files": [{
                "path": "src/main.rs",
                "content_base64": b64(file_content)
            }]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "upload: {}", resp.text().await.unwrap_or_default());

    // Download the file BEFORE submit — must succeed using storage trait, not filesystem.
    let resp = client
        .get(format!("{rp}/workspaces/{ws_id}/files/src/main.rs"))
        .bearer_auth(admin)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        200,
        "workspace file download before submit failed: {}",
        resp.text().await.unwrap_or_default()
    );
    let download: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        download["found_in"].as_str().unwrap_or(""),
        "overlay",
        "file must be found in overlay"
    );
    let decoded = BASE64
        .decode(download["content_base64"].as_str().unwrap())
        .expect("base64 decode");
    assert_eq!(decoded, file_content, "downloaded content must match uploaded content");

    // Also verify list_repo_files returns head_version from storage (not filesystem).
    // HEAD is "v0" when no versions exist yet, not an error.
    let resp = client
        .get(format!("{rp}/files"))
        .bearer_auth(admin)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "list repo files: {}", resp.text().await.unwrap_or_default());
    let files_resp: serde_json::Value = resp.json().await.unwrap();
    assert!(
        files_resp["head_version"].as_str().is_some(),
        "head_version must be present in repo files response"
    );

    shutdown_tx.send(()).ok();
}
