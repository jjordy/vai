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

use vai::server::{start_for_testing_pg_multi_repo, start_for_testing_pg_with_mem_fs};

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
    let diffs = diff_resp["files"].as_array().unwrap();
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

// ── Version diff via storage ───────────────────────────────────────────────────

/// Verifies that `GET /api/repos/:repo/versions/:id/diff` returns correct diff
/// content sourced from the content-addressable blob store (not the local
/// `.vai/` filesystem snapshot directories).
///
/// This is a regression guard for the filesystem fallback described in
/// issue #154: previously snapshot directories written by
/// `prepare_workspace_for_submit` were required for the diff to be non-empty.
/// After the fix, diffs are built from `blobs/{hash}` keys stored in S3/storage
/// during file upload — no local snapshot needed.
#[tokio::test(flavor = "multi_thread")]
async fn test_version_diff_via_storage() {
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
        .json(&serde_json::json!({ "name": "e2e-version-diff" }))
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
            "title": "add greeter module",
            "description": "add greeter.rs with a greet function",
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
    let file_content = b"pub fn greet(name: &str) -> String {\n    format!(\"Hello, {name}!\")\n}\n";
    let resp = client
        .post(format!("{rp}/workspaces/{ws_id}/files"))
        .bearer_auth(admin)
        .json(&serde_json::json!({
            "files": [{
                "path": "src/greeter.rs",
                "content_base64": b64(file_content)
            }]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "upload files: {}", resp.text().await.unwrap_or_default());

    // Submit the workspace to create a version.
    let resp = client
        .post(format!("{rp}/workspaces/{ws_id}/submit"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "summary": "add greeter module" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "submit: {}", resp.text().await.unwrap_or_default());
    let submit_resp: serde_json::Value = resp.json().await.unwrap();
    let version_label = submit_resp["version"].as_str().unwrap().to_string();
    assert!(!version_label.is_empty(), "submit must return a version label");

    // Look up the version UUID from the version list.
    let resp = client.get(format!("{rp}/versions")).bearer_auth(admin).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let versions: serde_json::Value = resp.json().await.unwrap();
    let version_id = versions
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["label"].as_str() == Some(&version_label))
        .and_then(|v| v["id"].as_str())
        .expect("submitted version must appear in version list")
        .to_string();

    // GET /versions/:id/diff — must return a non-empty diff for src/greeter.rs,
    // sourced from the content-addressable blob store (not snapshot directories).
    let resp = client
        .get(format!("{rp}/versions/{version_id}/diff"))
        .bearer_auth(admin)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        200,
        "version diff failed: {}",
        resp.text().await.unwrap_or_default()
    );
    let diff_resp: serde_json::Value = resp.json().await.unwrap();
    let diffs = diff_resp["files"].as_array().expect("files must be an array");
    assert!(!diffs.is_empty(), "version diff must be non-empty");

    let greeter_diff = diffs
        .iter()
        .find(|d| d["path"].as_str() == Some("src/greeter.rs"))
        .expect("diff must include src/greeter.rs");

    assert_eq!(
        greeter_diff["change_type"].as_str().unwrap_or(""),
        "added",
        "src/greeter.rs must be an added file"
    );
    let diff_text = greeter_diff["diff"].as_str().unwrap_or("");
    assert!(!diff_text.is_empty(), "diff text for src/greeter.rs must be non-empty");
    assert!(
        diff_text.contains("greet"),
        "diff must contain content from the uploaded file"
    );

    shutdown_tx.send(()).ok();
}

// ── submit → current/ update (MemFs backend) ─────────────────────────────────

/// Verifies that after a successful submit the `current/` prefix in the file
/// store is updated to reflect the submitted overlay — i.e. a subsequent file
/// download returns exactly the content that was uploaded.
///
/// Uses the `ServerWithMemFs` backend so the `S3MergeFs` submit path is
/// exercised without real S3.
#[tokio::test(flavor = "multi_thread")]
async fn test_submit_updates_current_files_match() {
    let Some(url) = db_url() else { return };

    let tmp = TempDir::new().unwrap();
    let (addr, shutdown_tx) = start_for_testing_pg_with_mem_fs(tmp.path(), &url)
        .await
        .expect("start_for_testing_pg_with_mem_fs failed");

    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let admin = "vai_admin_test";

    // Create repo.
    let resp = client
        .post(format!("{base}/api/repos"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "name": "e2e-submit-files-match" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create repo: {}", resp.text().await.unwrap_or_default());
    let repo: serde_json::Value = resp.json().await.unwrap();
    let rp = format!("{base}/api/repos/{}", repo["name"].as_str().unwrap());

    // Create and claim an issue.
    let resp = client
        .post(format!("{rp}/issues"))
        .bearer_auth(admin)
        .json(&serde_json::json!({
            "title": "add greeter",
            "description": "add greeter.rs",
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

    // Upload a file.
    let file_content = b"pub fn greet() -> &'static str { \"hello\" }\n";
    let resp = client
        .post(format!("{rp}/workspaces/{ws_id}/files"))
        .bearer_auth(admin)
        .json(&serde_json::json!({
            "files": [{ "path": "src/greeter.rs", "content_base64": b64(file_content) }]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "upload: {}", resp.text().await.unwrap_or_default());

    // Submit.
    let resp = client
        .post(format!("{rp}/workspaces/{ws_id}/submit"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "summary": "add greeter" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "submit: {}", resp.text().await.unwrap_or_default());

    // Download the full repo tarball — must include the submitted file.
    let resp = client
        .get(format!("{rp}/files/download"))
        .bearer_auth(admin)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        200,
        "files/download after submit: {}",
        resp.text().await.unwrap_or_default()
    );
    let tarball = resp.bytes().await.unwrap();
    assert!(!tarball.is_empty(), "download tarball must not be empty");

    // Extract the tarball and verify the file content matches.
    let decoder = flate2::read::GzDecoder::new(std::io::Cursor::new(&tarball[..]));
    let mut archive = tar::Archive::new(decoder);
    let mut found = false;
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        let path = entry.path().unwrap().to_string_lossy().into_owned();
        if path.contains("greeter.rs") {
            let mut content = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut content).unwrap();
            assert_eq!(
                content, file_content as &[u8],
                "downloaded greeter.rs content must match uploaded content"
            );
            found = true;
            break;
        }
    }
    assert!(found, "greeter.rs must be present in the download tarball");

    shutdown_tx.send(()).ok();
}

/// Verifies that after a submit with deleted files, the deleted files are
/// absent from a subsequent `current/` download.
///
/// Flow: seed `current/` with a file via upload+submit, then submit a second
/// workspace that deletes it.  The final download must not contain the deleted file.
#[tokio::test(flavor = "multi_thread")]
async fn test_submit_with_deletions_files_absent() {
    let Some(url) = db_url() else { return };

    let tmp = TempDir::new().unwrap();
    let (addr, shutdown_tx) = start_for_testing_pg_with_mem_fs(tmp.path(), &url)
        .await
        .expect("start_for_testing_pg_with_mem_fs failed");

    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let admin = "vai_admin_test";

    // Create repo.
    let resp = client
        .post(format!("{base}/api/repos"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "name": "e2e-submit-deletions" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let repo: serde_json::Value = resp.json().await.unwrap();
    let rp = format!("{base}/api/repos/{}", repo["name"].as_str().unwrap());

    // ── Step 1: upload + submit a file to seed current/ ───────────────────
    let resp = client
        .post(format!("{rp}/issues"))
        .bearer_auth(admin)
        .json(&serde_json::json!({
            "title": "add temp file",
            "description": "add a file that will be deleted",
            "priority": "low"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let issue1: serde_json::Value = resp.json().await.unwrap();
    let issue1_id = issue1["id"].as_str().unwrap().to_string();

    let resp = client
        .post(format!("{rp}/work-queue/claim"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "issue_id": issue1_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "claim1: {}", resp.text().await.unwrap_or_default());
    let claim1: serde_json::Value = resp.json().await.unwrap();
    let ws1_id = claim1["workspace_id"].as_str().unwrap().to_string();

    let to_delete_content = b"fn to_be_deleted() {}\n";
    client
        .post(format!("{rp}/workspaces/{ws1_id}/files"))
        .bearer_auth(admin)
        .json(&serde_json::json!({
            "files": [{ "path": "src/temp.rs", "content_base64": b64(to_delete_content) }]
        }))
        .send()
        .await
        .unwrap();

    let resp = client
        .post(format!("{rp}/workspaces/{ws1_id}/submit"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "summary": "add temp file" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "submit1: {}", resp.text().await.unwrap_or_default());

    // ── Step 2: upload-snapshot (with deletion) + submit ──────────────────
    let resp = client
        .post(format!("{rp}/issues"))
        .bearer_auth(admin)
        .json(&serde_json::json!({
            "title": "delete temp file",
            "description": "remove the temp.rs file",
            "priority": "low"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let issue2: serde_json::Value = resp.json().await.unwrap();
    let issue2_id = issue2["id"].as_str().unwrap().to_string();

    let resp = client
        .post(format!("{rp}/work-queue/claim"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "issue_id": issue2_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "claim2: {}", resp.text().await.unwrap_or_default());
    let claim2: serde_json::Value = resp.json().await.unwrap();
    let ws2_id = claim2["workspace_id"].as_str().unwrap().to_string();

    // Upload a snapshot containing only the files to KEEP (not temp.rs).
    // The upload-snapshot handler diffs against current/ and records temp.rs
    // as deleted in the workspace's deleted_paths column.
    {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        let keep_content = b"fn keep() {}\n";
        let mut enc = GzEncoder::new(Vec::new(), Compression::default());
        {
            let mut ar = tar::Builder::new(&mut enc);
            let mut header = tar::Header::new_gnu();
            header.set_size(keep_content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            ar.append_data(&mut header, "src/keep.rs", keep_content.as_slice()).unwrap();
            ar.finish().unwrap();
        }
        let tarball_bytes = enc.finish().unwrap();

        let resp = client
            .post(format!("{rp}/workspaces/{ws2_id}/upload-snapshot"))
            .bearer_auth(admin)
            .header("content-type", "application/gzip")
            .body(tarball_bytes)
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            200,
            "upload-snapshot: {}",
            resp.text().await.unwrap_or_default()
        );
    }

    let resp = client
        .post(format!("{rp}/workspaces/{ws2_id}/submit"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "summary": "delete temp file" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "submit2: {}", resp.text().await.unwrap_or_default());

    // ── Verify: download must NOT contain the deleted file ────────────────
    let resp = client
        .get(format!("{rp}/files/download"))
        .bearer_auth(admin)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        200,
        "files/download after deletion submit: {}",
        resp.text().await.unwrap_or_default()
    );
    let tarball = resp.bytes().await.unwrap();
    let decoder = flate2::read::GzDecoder::new(std::io::Cursor::new(&tarball[..]));
    let mut archive = tar::Archive::new(decoder);
    for entry in archive.entries().unwrap() {
        let entry = entry.unwrap();
        let path = entry.path().unwrap().to_string_lossy().into_owned();
        assert!(
            !path.contains("temp.rs"),
            "src/temp.rs must be absent from current/ after deletion submit; found: {path}"
        );
    }

    shutdown_tx.send(()).ok();
}

/// Verifies that two sequential submits both update `current/` so the final
/// download reflects the combined state of both workspaces.
#[tokio::test(flavor = "multi_thread")]
async fn test_sequential_submits_current_reflects_both() {
    let Some(url) = db_url() else { return };

    let tmp = TempDir::new().unwrap();
    let (addr, shutdown_tx) = start_for_testing_pg_with_mem_fs(tmp.path(), &url)
        .await
        .expect("start_for_testing_pg_with_mem_fs failed");

    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let admin = "vai_admin_test";

    // Create repo.
    let resp = client
        .post(format!("{base}/api/repos"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "name": "e2e-sequential-submits" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let repo: serde_json::Value = resp.json().await.unwrap();
    let rp = format!("{base}/api/repos/{}", repo["name"].as_str().unwrap());

    // Submit file A, then file B sequentially.
    for (title, path, content) in [
        ("add alpha", "src/alpha.rs", b"pub fn alpha() {}\n" as &[u8]),
        ("add beta", "src/beta.rs", b"pub fn beta() {}\n" as &[u8]),
    ] {
        let resp = client
            .post(format!("{rp}/issues"))
            .bearer_auth(admin)
            .json(&serde_json::json!({
                "title": title,
                "description": title,
                "priority": "medium"
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
        assert_eq!(resp.status(), 201, "claim {title}: {}", resp.text().await.unwrap_or_default());
        let claim: serde_json::Value = resp.json().await.unwrap();
        let ws_id = claim["workspace_id"].as_str().unwrap().to_string();

        let resp = client
            .post(format!("{rp}/workspaces/{ws_id}/files"))
            .bearer_auth(admin)
            .json(&serde_json::json!({
                "files": [{ "path": path, "content_base64": b64(content) }]
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200, "upload {path}: {}", resp.text().await.unwrap_or_default());

        let resp = client
            .post(format!("{rp}/workspaces/{ws_id}/submit"))
            .bearer_auth(admin)
            .json(&serde_json::json!({ "summary": title }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200, "submit {title}: {}", resp.text().await.unwrap_or_default());
    }

    // Download — both files must be present with correct content.
    let resp = client
        .get(format!("{rp}/files/download"))
        .bearer_auth(admin)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        200,
        "files/download after 2 submits: {}",
        resp.text().await.unwrap_or_default()
    );
    let tarball = resp.bytes().await.unwrap();

    let decoder = flate2::read::GzDecoder::new(std::io::Cursor::new(&tarball[..]));
    let mut archive = tar::Archive::new(decoder);
    let mut found_alpha = false;
    let mut found_beta = false;
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        let path = entry.path().unwrap().to_string_lossy().into_owned();
        if path.contains("alpha.rs") {
            let mut content = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut content).unwrap();
            assert_eq!(content, b"pub fn alpha() {}\n", "alpha.rs content must match");
            found_alpha = true;
        } else if path.contains("beta.rs") {
            let mut content = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut content).unwrap();
            assert_eq!(content, b"pub fn beta() {}\n", "beta.rs content must match");
            found_beta = true;
        }
    }
    assert!(found_alpha, "src/alpha.rs must be present after sequential submits");
    assert!(found_beta, "src/beta.rs must be present after sequential submits");

    shutdown_tx.send(()).ok();
}

// ── Work queue blocking via issue links ───────────────────────────────────────

/// Verifies the link-based blocking chain: A blocks B blocks C.
///
/// Flow:
/// 1. Create three issues A, B, C
/// 2. Create link: A blocks B  (via POST /issues/:b/links)
/// 3. Create link: B blocks C
/// 4. GET /work-queue → only A is available; B and C are blocked
/// 5. Close A
/// 6. GET /work-queue → A gone, B available, C still blocked
/// 7. Close B
/// 8. GET /work-queue → C now available
#[tokio::test(flavor = "multi_thread")]
async fn test_work_queue_link_blocking() {
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
        .json(&serde_json::json!({ "name": "link-blocking-test" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let repo: serde_json::Value = resp.json().await.unwrap();
    let rp = format!("{base}/api/repos/{}", repo["name"].as_str().unwrap());

    // Create issue A.
    let resp = client
        .post(format!("{rp}/issues"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "title": "Issue A", "priority": "high" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let issue_a: serde_json::Value = resp.json().await.unwrap();
    let id_a = issue_a["id"].as_str().unwrap().to_string();

    // Create issue B (blocked_by A using the new field).
    let resp = client
        .post(format!("{rp}/issues"))
        .bearer_auth(admin)
        .json(&serde_json::json!({
            "title": "Issue B",
            "priority": "medium",
            "blocked_by": [id_a]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create B: {}", resp.text().await.unwrap_or_default());
    let issue_b: serde_json::Value = resp.json().await.unwrap();
    let id_b = issue_b["id"].as_str().unwrap().to_string();
    // Verify blocked_by field in response.
    assert!(
        issue_b["blocked_by"].as_array().unwrap().iter().any(|v| v.as_str() == Some(&id_a)),
        "B.blocked_by must contain A"
    );

    // Create issue C (blocked_by B).
    let resp = client
        .post(format!("{rp}/issues"))
        .bearer_auth(admin)
        .json(&serde_json::json!({
            "title": "Issue C",
            "priority": "low",
            "blocked_by": [id_b]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create C: {}", resp.text().await.unwrap_or_default());
    let issue_c: serde_json::Value = resp.json().await.unwrap();
    let id_c = issue_c["id"].as_str().unwrap().to_string();

    // Work queue: only A available, B and C blocked.
    let resp = client
        .get(format!("{rp}/work-queue"))
        .bearer_auth(admin)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let wq: serde_json::Value = resp.json().await.unwrap();
    let available: Vec<&str> = wq["available_work"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["issue_id"].as_str().unwrap())
        .collect();
    let blocked_ids: Vec<&str> = wq["blocked_work"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["issue_id"].as_str().unwrap())
        .collect();
    assert!(available.contains(&id_a.as_str()), "A must be available");
    assert!(blocked_ids.contains(&id_b.as_str()), "B must be blocked");
    assert!(blocked_ids.contains(&id_c.as_str()), "C must be blocked");

    // Close A → B should become available.
    let resp = client
        .post(format!("{rp}/issues/{id_a}/close"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "resolution": "resolved" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "close A: {}", resp.text().await.unwrap_or_default());

    let resp = client
        .get(format!("{rp}/work-queue"))
        .bearer_auth(admin)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let wq: serde_json::Value = resp.json().await.unwrap();
    let available: Vec<&str> = wq["available_work"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["issue_id"].as_str().unwrap())
        .collect();
    let blocked_ids: Vec<&str> = wq["blocked_work"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["issue_id"].as_str().unwrap())
        .collect();
    assert!(!available.contains(&id_a.as_str()), "A must not be in available (it's closed)");
    assert!(available.contains(&id_b.as_str()), "B must be available after A is closed");
    assert!(blocked_ids.contains(&id_c.as_str()), "C must still be blocked (B not closed yet)");

    // Close B → C should become available.
    let resp = client
        .post(format!("{rp}/issues/{id_b}/close"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "resolution": "resolved" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "close B: {}", resp.text().await.unwrap_or_default());

    let resp = client
        .get(format!("{rp}/work-queue"))
        .bearer_auth(admin)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let wq: serde_json::Value = resp.json().await.unwrap();
    let available: Vec<&str> = wq["available_work"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["issue_id"].as_str().unwrap())
        .collect();
    assert!(available.contains(&id_c.as_str()), "C must be available after B is closed");

    // Verify IssueResponse blocking/blocked_by fields via GET /issues/:id.
    let resp = client
        .get(format!("{rp}/issues/{id_c}"))
        .bearer_auth(admin)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let c_detail: serde_json::Value = resp.json().await.unwrap();
    // B is closed but still linked — blocked_by should still list B as the blocker.
    assert!(
        c_detail["blocked_by"].as_array().unwrap().iter().any(|v| v.as_str() == Some(&id_b)),
        "C.blocked_by must contain B"
    );

    shutdown_tx.send(()).ok();
}

// ── Deletion round-trip ───────────────────────────────────────────────────────

/// Builds a gzip-compressed tar archive containing the provided files.
///
/// `files` is a slice of `(path, content)` pairs.  All entries use mode 0o644.
fn make_tarball(files: &[(&str, &[u8])]) -> Vec<u8> {
    use flate2::Compression;
    use flate2::write::GzEncoder;

    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
    {
        let mut ar = tar::Builder::new(&mut enc);
        for (path, content) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            ar.append_data(&mut header, path, *content).unwrap();
        }
        ar.finish().unwrap();
    }
    enc.finish().unwrap()
}

/// Collects all file paths from a gzip-compressed tar archive.
fn tarball_paths(bytes: &[u8]) -> Vec<String> {
    use flate2::read::GzDecoder;
    let decoder = GzDecoder::new(std::io::Cursor::new(bytes));
    let mut archive = tar::Archive::new(decoder);
    archive
        .entries()
        .unwrap()
        .filter_map(|e| {
            let e = e.ok()?;
            Some(e.path().ok()?.to_string_lossy().into_owned())
        })
        .collect()
}

/// Full deletion round-trip: seed A, B, C → delete B + modify A → re-add B + delete C.
///
/// Verifies that deleted files are absent from downloads and that version diffs
/// record the correct change types at each step.
///
/// Flow:
/// 1.  Seed repo: upload src/a.rs, src/b.rs, src/c.rs via workspace → v2
/// 2.  Download → verify A, B, C present
/// 3.  upload-snapshot with modified A + unchanged C (B absent) → B detected as deleted
/// 4.  Submit → v3
/// 5.  Download → A (modified), C present, B absent
/// 6.  GET /versions/:v3_id/diff → A modified, B removed
/// 7.  upload-snapshot with unchanged A + B re-added (C absent) → C detected as deleted
/// 8.  Submit → v4
/// 9.  Download → A, B present, C absent
/// 10. GET /versions/:v4_id/diff → B added, C removed
#[tokio::test(flavor = "multi_thread")]
async fn test_deletion_round_trip() {
    let Some(url) = db_url() else { return };

    let tmp = TempDir::new().unwrap();
    let (addr, shutdown_tx) = start_for_testing_pg_with_mem_fs(tmp.path(), &url)
        .await
        .expect("start_for_testing_pg_with_mem_fs failed");

    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let admin = "vai_admin_test";

    // ── Create repo ───────────────────────────────────────────────────────────
    let resp = client
        .post(format!("{base}/api/repos"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "name": "e2e-deletion-roundtrip" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create repo: {}", resp.text().await.unwrap_or_default());
    let repo: serde_json::Value = resp.json().await.unwrap();
    let rp = format!("{base}/api/repos/{}", repo["name"].as_str().unwrap());

    // ── Step 1: seed A, B, C via workspace upload + submit ────────────────────
    let resp = client
        .post(format!("{rp}/issues"))
        .bearer_auth(admin)
        .json(&serde_json::json!({
            "title": "seed files A B C",
            "description": "initial file set",
            "priority": "low"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let issue1: serde_json::Value = resp.json().await.unwrap();
    let issue1_id = issue1["id"].as_str().unwrap().to_string();

    let resp = client
        .post(format!("{rp}/work-queue/claim"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "issue_id": issue1_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "claim1: {}", resp.text().await.unwrap_or_default());
    let claim1: serde_json::Value = resp.json().await.unwrap();
    let ws1_id = claim1["workspace_id"].as_str().unwrap().to_string();

    let a_v1 = b"fn a_v1() {}\n";
    let b_content = b"fn b() {}\n";
    let c_content = b"fn c() {}\n";

    let resp = client
        .post(format!("{rp}/workspaces/{ws1_id}/files"))
        .bearer_auth(admin)
        .json(&serde_json::json!({
            "files": [
                { "path": "src/a.rs", "content_base64": b64(a_v1) },
                { "path": "src/b.rs", "content_base64": b64(b_content) },
                { "path": "src/c.rs", "content_base64": b64(c_content) }
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "upload seed files: {}", resp.text().await.unwrap_or_default());

    let resp = client
        .post(format!("{rp}/workspaces/{ws1_id}/submit"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "summary": "seed A B C" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "submit seed: {}", resp.text().await.unwrap_or_default());

    // ── Step 2: download → verify A, B, C present ────────────────────────────
    let resp = client
        .get(format!("{rp}/files/download"))
        .bearer_auth(admin)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "download after seed: {}", resp.text().await.unwrap_or_default());
    let tarball = resp.bytes().await.unwrap();
    let paths = tarball_paths(&tarball);
    assert!(paths.iter().any(|p| p.contains("src/a.rs")), "A must be present after seed; got: {paths:?}");
    assert!(paths.iter().any(|p| p.contains("src/b.rs")), "B must be present after seed; got: {paths:?}");
    assert!(paths.iter().any(|p| p.contains("src/c.rs")), "C must be present after seed; got: {paths:?}");

    // ── Step 3: workspace 2 — modify A, delete B (C unchanged) ───────────────
    let resp = client
        .post(format!("{rp}/issues"))
        .bearer_auth(admin)
        .json(&serde_json::json!({
            "title": "modify A, delete B",
            "description": "second iteration",
            "priority": "low"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let issue2: serde_json::Value = resp.json().await.unwrap();
    let issue2_id = issue2["id"].as_str().unwrap().to_string();

    let resp = client
        .post(format!("{rp}/work-queue/claim"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "issue_id": issue2_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "claim2: {}", resp.text().await.unwrap_or_default());
    let claim2: serde_json::Value = resp.json().await.unwrap();
    let ws2_id = claim2["workspace_id"].as_str().unwrap().to_string();

    // Snapshot contains A (modified) and C (unchanged); B is absent → detected as deleted.
    let a_v2 = b"fn a_v2() { /* modified */ }\n";
    let tarball2 = make_tarball(&[("src/a.rs", a_v2), ("src/c.rs", c_content)]);

    let resp = client
        .post(format!("{rp}/workspaces/{ws2_id}/upload-snapshot"))
        .bearer_auth(admin)
        .header("content-type", "application/gzip")
        .body(tarball2)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "upload-snapshot2: {}", resp.text().await.unwrap_or_default());

    let resp = client
        .post(format!("{rp}/workspaces/{ws2_id}/submit"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "summary": "modify A, delete B" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "submit2: {}", resp.text().await.unwrap_or_default());
    let submit2: serde_json::Value = resp.json().await.unwrap();
    let v2_label = submit2["version"].as_str().unwrap().to_string();

    // ── Step 4: download → verify A (modified), C present, B absent ──────────
    let resp = client
        .get(format!("{rp}/files/download"))
        .bearer_auth(admin)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let tarball = resp.bytes().await.unwrap();
    let paths = tarball_paths(&tarball);
    assert!(paths.iter().any(|p| p.contains("src/a.rs")), "A must be present after step 2; got: {paths:?}");
    assert!(!paths.iter().any(|p| p.contains("src/b.rs")), "B must be absent after deletion; got: {paths:?}");
    assert!(paths.iter().any(|p| p.contains("src/c.rs")), "C must be unchanged after step 2; got: {paths:?}");

    // ── Step 5: version diff for the step-2 version → A modified, B removed ──
    let resp = client.get(format!("{rp}/versions")).bearer_auth(admin).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let versions: serde_json::Value = resp.json().await.unwrap();
    let v2_id = versions
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["label"].as_str() == Some(&v2_label))
        .and_then(|v| v["id"].as_str())
        .expect("step-2 version must appear in version list")
        .to_string();

    let resp = client
        .get(format!("{rp}/versions/{v2_id}/diff"))
        .bearer_auth(admin)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "step-2 diff: {}", resp.text().await.unwrap_or_default());
    let diff2: serde_json::Value = resp.json().await.unwrap();
    let files2 = diff2["files"].as_array().expect("files must be present in step-2 diff");

    let a_diff2 = files2.iter().find(|f| f["path"].as_str() == Some("src/a.rs"));
    assert!(a_diff2.is_some(), "step-2 diff must include src/a.rs");
    assert_eq!(
        a_diff2.unwrap()["change_type"].as_str().unwrap_or(""),
        "modified",
        "src/a.rs must be modified in step-2 version"
    );

    let b_diff2 = files2.iter().find(|f| f["path"].as_str() == Some("src/b.rs"));
    assert!(b_diff2.is_some(), "step-2 diff must include src/b.rs");
    assert_eq!(
        b_diff2.unwrap()["change_type"].as_str().unwrap_or(""),
        "removed",
        "src/b.rs must be removed in step-2 version"
    );

    // ── Step 6: workspace 3 — re-add B, delete C ─────────────────────────────
    let resp = client
        .post(format!("{rp}/issues"))
        .bearer_auth(admin)
        .json(&serde_json::json!({
            "title": "re-add B, delete C",
            "description": "third iteration",
            "priority": "low"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let issue3: serde_json::Value = resp.json().await.unwrap();
    let issue3_id = issue3["id"].as_str().unwrap().to_string();

    let resp = client
        .post(format!("{rp}/work-queue/claim"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "issue_id": issue3_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "claim3: {}", resp.text().await.unwrap_or_default());
    let claim3: serde_json::Value = resp.json().await.unwrap();
    let ws3_id = claim3["workspace_id"].as_str().unwrap().to_string();

    // Snapshot contains A (unchanged from step 2) and B (re-added); C absent → detected as deleted.
    let tarball3 = make_tarball(&[("src/a.rs", a_v2), ("src/b.rs", b_content)]);

    let resp = client
        .post(format!("{rp}/workspaces/{ws3_id}/upload-snapshot"))
        .bearer_auth(admin)
        .header("content-type", "application/gzip")
        .body(tarball3)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "upload-snapshot3: {}", resp.text().await.unwrap_or_default());

    let resp = client
        .post(format!("{rp}/workspaces/{ws3_id}/submit"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "summary": "re-add B, delete C" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "submit3: {}", resp.text().await.unwrap_or_default());
    let submit3: serde_json::Value = resp.json().await.unwrap();
    let v3_label = submit3["version"].as_str().unwrap().to_string();

    // ── Step 7: download → verify A, B present, C absent ─────────────────────
    let resp = client
        .get(format!("{rp}/files/download"))
        .bearer_auth(admin)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let tarball = resp.bytes().await.unwrap();
    let paths = tarball_paths(&tarball);
    assert!(paths.iter().any(|p| p.contains("src/a.rs")), "A must be present after step 3; got: {paths:?}");
    assert!(paths.iter().any(|p| p.contains("src/b.rs")), "B must be re-added after step 3; got: {paths:?}");
    assert!(!paths.iter().any(|p| p.contains("src/c.rs")), "C must be absent after step 3; got: {paths:?}");

    // ── Step 8: version diff for step-3 version → B added, C removed ─────────
    let resp = client.get(format!("{rp}/versions")).bearer_auth(admin).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let versions: serde_json::Value = resp.json().await.unwrap();
    let v3_id = versions
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["label"].as_str() == Some(&v3_label))
        .and_then(|v| v["id"].as_str())
        .expect("step-3 version must appear in version list")
        .to_string();

    let resp = client
        .get(format!("{rp}/versions/{v3_id}/diff"))
        .bearer_auth(admin)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "step-3 diff: {}", resp.text().await.unwrap_or_default());
    let diff3: serde_json::Value = resp.json().await.unwrap();
    let files3 = diff3["files"].as_array().expect("files must be present in step-3 diff");

    let b_diff3 = files3.iter().find(|f| f["path"].as_str() == Some("src/b.rs"));
    assert!(b_diff3.is_some(), "step-3 diff must include src/b.rs");
    assert_eq!(
        b_diff3.unwrap()["change_type"].as_str().unwrap_or(""),
        "added",
        "src/b.rs must be added (re-created) in step-3 version"
    );

    let c_diff3 = files3.iter().find(|f| f["path"].as_str() == Some("src/c.rs"));
    assert!(c_diff3.is_some(), "step-3 diff must include src/c.rs");
    assert_eq!(
        c_diff3.unwrap()["change_type"].as_str().unwrap_or(""),
        "removed",
        "src/c.rs must be removed in step-3 version"
    );

    shutdown_tx.send(()).ok();
}

// ── Read-only repo_root enforcement ───────────────────────────────────────────

/// Recursively removes the write bit from every file and directory under
/// `path`.  Used to simulate a server-mode `repo_root` that has only
/// `.vai/config.toml` written and is then locked to prevent any further
/// filesystem writes.
#[cfg(unix)]
fn make_dir_readonly(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;

    if path.is_dir() {
        // Descend before changing the dir itself so we can still list it.
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                make_dir_readonly(&entry.path());
            }
        }
    }

    if let Ok(meta) = std::fs::metadata(path) {
        let mut perms = meta.permissions();
        // Clear the write bits for owner, group, and others (mask off 0o222).
        perms.set_mode(perms.mode() & !0o222);
        let _ = std::fs::set_permissions(path, perms);
    }
}

/// Recursively restores write permissions to `path` so that `TempDir` can
/// delete the tree after the test.
#[cfg(unix)]
fn restore_dir_write(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;

    if path.is_dir() {
        // Restore the dir first so we can descend into it.
        if let Ok(meta) = std::fs::metadata(path) {
            let mut perms = meta.permissions();
            perms.set_mode(0o755);
            let _ = std::fs::set_permissions(path, perms);
        }
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                restore_dir_write(&entry.path());
            }
        }
    } else if let Ok(meta) = std::fs::metadata(path) {
        let mut perms = meta.permissions();
        perms.set_mode(0o644);
        let _ = std::fs::set_permissions(path, perms);
    }
}

/// Verifies the full agent workflow — create workspace, upload files, submit,
/// download — succeeds even when the on-disk `repo_root` directory is
/// **read-only** after the initial `POST /api/repos` call writes
/// `.vai/config.toml`.
///
/// This is the regression guard for issue #165: any server handler that still
/// attempts a `std::fs::write` (or create-file) against `repo_root` after
/// initial creation will receive an `EACCES` error from the OS and fail the
/// test, proving the filesystem leak exists.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn test_complete_agent_workflow_readonly_repo_root() {
    let Some(url) = db_url() else { return };

    let tmp = TempDir::new().unwrap();
    // Use the in-memory file store variant so workspace overlays and current/
    // are stored in memory rather than on disk.  After locking the repo
    // directory all subsequent operations must succeed via Postgres + MemFs.
    let (addr, shutdown_tx) = start_for_testing_pg_with_mem_fs(tmp.path(), &url)
        .await
        .expect("start_for_testing_pg_with_mem_fs failed");

    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let admin = "vai_admin_test";

    // ── 1. Create repo ────────────────────────────────────────────────────────
    // This is the ONLY step allowed to write to disk (.vai/config.toml).
    let resp = client
        .post(format!("{base}/api/repos"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "name": "e2e-readonly-root" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create repo: {}", resp.text().await.unwrap_or_default());
    let repo: serde_json::Value = resp.json().await.unwrap();
    let repo_name = repo["name"].as_str().unwrap().to_string();
    let rp = format!("{base}/api/repos/{repo_name}");

    // Lock the repo directory to read-only.  Any handler that tries to write
    // source files to disk from this point on will get EACCES.
    let repo_dir = tmp.path().join(&repo_name);
    make_dir_readonly(&repo_dir);

    // ── 2. Create issue ───────────────────────────────────────────────────────
    let resp = client
        .post(format!("{rp}/issues"))
        .bearer_auth(admin)
        .json(&serde_json::json!({
            "title": "readonly-root test issue",
            "description": "Must work without filesystem writes",
            "priority": "high"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create issue: {}", resp.text().await.unwrap_or_default());
    let issue: serde_json::Value = resp.json().await.unwrap();
    let issue_id = issue["id"].as_str().unwrap().to_string();

    // ── 3. Claim work ─────────────────────────────────────────────────────────
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

    // ── 4. Upload files ───────────────────────────────────────────────────────
    let file_content = b"pub fn readonly_test() -> &'static str { \"ok\" }\n";
    let resp = client
        .post(format!("{rp}/workspaces/{ws_id}/files"))
        .bearer_auth(admin)
        .json(&serde_json::json!({
            "files": [{
                "path": "src/readonly_test.rs",
                "content_base64": b64(file_content)
            }]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "upload files: {}", resp.text().await.unwrap_or_default());
    let upload_resp: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(upload_resp["uploaded"].as_u64().unwrap(), 1);

    // ── 5. Submit workspace ───────────────────────────────────────────────────
    let resp = client
        .post(format!("{rp}/workspaces/{ws_id}/submit"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "summary": "add readonly_test function" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "submit workspace: {}", resp.text().await.unwrap_or_default());
    let submit_resp: serde_json::Value = resp.json().await.unwrap();
    let new_version = submit_resp["version"].as_str().unwrap().to_string();
    assert!(!new_version.is_empty(), "submit must return the new version label");

    // ── 6. Version appears in list ────────────────────────────────────────────
    let resp = client.get(format!("{rp}/versions")).bearer_auth(admin).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let versions: serde_json::Value = resp.json().await.unwrap();
    let version_arr = versions.as_array().unwrap();
    assert!(
        version_arr.iter().any(|v| v["label"].as_str() == Some(&new_version)),
        "submitted version must appear in version list"
    );

    let version_id = version_arr
        .iter()
        .find(|v| v["label"].as_str() == Some(&new_version))
        .and_then(|v| v["id"].as_str())
        .unwrap()
        .to_string();

    // ── 7. Version detail has file_changes ────────────────────────────────────
    let resp = client
        .get(format!("{rp}/versions/{version_id}"))
        .bearer_auth(admin)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let version_detail: serde_json::Value = resp.json().await.unwrap();
    let file_changes = version_detail["file_changes"].as_array().unwrap();
    assert!(!file_changes.is_empty(), "version detail must have file_changes");
    assert!(
        file_changes
            .iter()
            .any(|fc| fc["path"].as_str() == Some("src/readonly_test.rs")),
        "file_changes must include src/readonly_test.rs"
    );

    // ── 8. Download tarball succeeds (reads from MemFs, not disk) ─────────────
    let resp = client
        .get(format!("{rp}/files/download"))
        .bearer_auth(admin)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "files download: {}", resp.text().await.unwrap_or_default());
    let download_body = resp.bytes().await.unwrap();
    assert!(!download_body.is_empty(), "download tarball must be non-empty");

    // ── 9. Graph refresh passes with read-only repo_root ─────────────────────
    // In server mode, graph refresh reads from S3/MemFs current/ prefix, not
    // from the local filesystem.
    let resp = client
        .post(format!("{rp}/graph/refresh"))
        .bearer_auth(admin)
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "graph refresh must succeed with read-only repo_root: status={}",
        resp.status()
    );

    // ── 10. Close issue ───────────────────────────────────────────────────────
    let resp = client
        .post(format!("{rp}/issues/{issue_id}/close"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "resolution": "verified read-only disk safety" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "close issue: {}", resp.text().await.unwrap_or_default());

    // Restore write permissions so TempDir can delete the directory tree.
    restore_dir_write(&repo_dir);
    shutdown_tx.send(()).ok();
}

// ── Comment author_type / author_id ──────────────────────────────────────────

/// Verify that `author_type` and `author_id` are stored and returned correctly.
#[tokio::test]
async fn test_comment_author_type() {
    let Some(url) = db_url() else { return };

    let tmp = TempDir::new().unwrap();
    let (addr, shutdown_tx) = start_for_testing_pg_multi_repo(tmp.path(), &url)
        .await
        .expect("start server");

    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let admin = "vai_admin_test";

    // ── Create repo ───────────────────────────────────────────────────────────
    let resp = client
        .post(format!("{base}/api/repos"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "name": "comment-author-type-test" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let repo: serde_json::Value = resp.json().await.unwrap();
    let rp = format!("{base}/api/repos/{}", repo["name"].as_str().unwrap());

    // ── Create issue ──────────────────────────────────────────────────────────
    let resp = client
        .post(format!("{rp}/issues"))
        .bearer_auth(admin)
        .json(&serde_json::json!({
            "title": "comment author type test",
            "description": "test issue",
            "priority": "low"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let issue: serde_json::Value = resp.json().await.unwrap();
    let issue_id = issue["id"].as_str().unwrap();

    // ── Post human comment (no author_type → defaults to "human") ─────────────
    let resp = client
        .post(format!("{rp}/issues/{issue_id}/comments"))
        .bearer_auth(admin)
        .json(&serde_json::json!({
            "author": "alice",
            "body": "Human comment here."
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create human comment: {}", resp.text().await.unwrap_or_default());
    let c: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(c["author_type"].as_str().unwrap(), "human");
    assert!(c["author_id"].is_null(), "author_id should be null");

    // ── Post agent comment with author_id ─────────────────────────────────────
    let resp = client
        .post(format!("{rp}/issues/{issue_id}/comments"))
        .bearer_auth(admin)
        .json(&serde_json::json!({
            "author": "ralph",
            "body": "Agent comment with structured ID.",
            "author_type": "agent",
            "author_id": "ralph-instance-42"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create agent comment: {}", resp.text().await.unwrap_or_default());
    let c: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(c["author_type"].as_str().unwrap(), "agent");
    assert_eq!(c["author_id"].as_str().unwrap(), "ralph-instance-42");

    // ── List comments → verify both present with correct author_type ──────────
    let resp = client
        .get(format!("{rp}/issues/{issue_id}/comments"))
        .bearer_auth(admin)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let comments: serde_json::Value = resp.json().await.unwrap();
    let arr = comments.as_array().unwrap();
    assert_eq!(arr.len(), 2, "expected 2 comments");

    let human = arr.iter().find(|c| c["author"].as_str() == Some("alice")).expect("human comment");
    assert_eq!(human["author_type"].as_str().unwrap(), "human");
    assert!(human["author_id"].is_null());

    let agent = arr.iter().find(|c| c["author"].as_str() == Some("ralph")).expect("agent comment");
    assert_eq!(agent["author_type"].as_str().unwrap(), "agent");
    assert_eq!(agent["author_id"].as_str().unwrap(), "ralph-instance-42");

    shutdown_tx.send(()).ok();
}

// ── Issue attachment CRUD ─────────────────────────────────────────────────────

#[tokio::test]
async fn test_issue_attachments() {
    let Some(url) = db_url() else { return };

    let tmp = TempDir::new().unwrap();
    let (addr, shutdown_tx) = start_for_testing_pg_multi_repo(tmp.path(), &url)
        .await
        .expect("start server");

    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let admin = "vai_admin_test";

    // ── Create repo ───────────────────────────────────────────────────────────
    let resp = client
        .post(format!("{base}/api/repos"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "name": "attachment-test-repo" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let repo: serde_json::Value = resp.json().await.unwrap();
    let rp = format!("{base}/api/repos/{}", repo["name"].as_str().unwrap());

    // ── Create issue ──────────────────────────────────────────────────────────
    let resp = client
        .post(format!("{rp}/issues"))
        .bearer_auth(admin)
        .json(&serde_json::json!({
            "title": "attachment test issue",
            "description": "testing file attachments",
            "priority": "low"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let issue: serde_json::Value = resp.json().await.unwrap();
    let issue_id = issue["id"].as_str().unwrap();

    // ── List attachments → empty ──────────────────────────────────────────────
    let resp = client
        .get(format!("{rp}/issues/{issue_id}/attachments"))
        .bearer_auth(admin)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let list: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(list.as_array().unwrap().len(), 0, "initially no attachments");

    // ── Upload attachment (base64 JSON) ────────────────────────────────────────
    let file_content = b"hello, world! this is a test file.";
    let encoded = BASE64.encode(file_content);
    let resp = client
        .post(format!("{rp}/issues/{issue_id}/attachments"))
        .bearer_auth(admin)
        .json(&serde_json::json!({
            "filename": "hello.txt",
            "content_type": "text/plain",
            "content": encoded,
            "uploaded_by": "test-agent"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        201,
        "upload failed: {}",
        resp.text().await.unwrap_or_default()
    );
    let att: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(att["filename"].as_str().unwrap(), "hello.txt");
    assert_eq!(att["content_type"].as_str().unwrap(), "text/plain");
    assert_eq!(att["size_bytes"].as_i64().unwrap(), file_content.len() as i64);
    assert_eq!(att["uploaded_by"].as_str().unwrap(), "test-agent");
    assert_eq!(att["issue_id"].as_str().unwrap(), issue_id);

    // ── Upload second attachment ───────────────────────────────────────────────
    let img_content = b"\x89PNG\r\n\x1a\nfake-png-data";
    let encoded2 = BASE64.encode(img_content);
    let resp = client
        .post(format!("{rp}/issues/{issue_id}/attachments"))
        .bearer_auth(admin)
        .json(&serde_json::json!({
            "filename": "screenshot.png",
            "content_type": "image/png",
            "content": encoded2,
            "uploaded_by": "test-agent"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);

    // ── List attachments → two items ──────────────────────────────────────────
    let resp = client
        .get(format!("{rp}/issues/{issue_id}/attachments"))
        .bearer_auth(admin)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let list: serde_json::Value = resp.json().await.unwrap();
    let arr = list.as_array().unwrap();
    assert_eq!(arr.len(), 2, "expected 2 attachments");
    let filenames: Vec<&str> = arr
        .iter()
        .map(|a| a["filename"].as_str().unwrap())
        .collect();
    assert!(filenames.contains(&"hello.txt"));
    assert!(filenames.contains(&"screenshot.png"));

    // ── Download first attachment → verify content matches ────────────────────
    let resp = client
        .get(format!("{rp}/issues/{issue_id}/attachments/hello.txt"))
        .bearer_auth(admin)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(ct.contains("text/plain"), "content-type was: {ct}");
    let body = resp.bytes().await.unwrap();
    assert_eq!(body.as_ref(), file_content, "downloaded content mismatch");

    // ── Duplicate filename → 409 ──────────────────────────────────────────────
    let resp = client
        .post(format!("{rp}/issues/{issue_id}/attachments"))
        .bearer_auth(admin)
        .json(&serde_json::json!({
            "filename": "hello.txt",
            "content_type": "text/plain",
            "content": BASE64.encode(b"duplicate"),
            "uploaded_by": "test-agent"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        409,
        "expected conflict for duplicate filename"
    );

    // ── Delete first attachment ────────────────────────────────────────────────
    let resp = client
        .delete(format!("{rp}/issues/{issue_id}/attachments/hello.txt"))
        .bearer_auth(admin)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);

    // ── List attachments → one item remains ───────────────────────────────────
    let resp = client
        .get(format!("{rp}/issues/{issue_id}/attachments"))
        .bearer_auth(admin)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let list: serde_json::Value = resp.json().await.unwrap();
    let arr = list.as_array().unwrap();
    assert_eq!(arr.len(), 1, "expected 1 attachment after delete");
    assert_eq!(arr[0]["filename"].as_str().unwrap(), "screenshot.png");

    // ── Download deleted attachment → 404 ─────────────────────────────────────
    let resp = client
        .get(format!("{rp}/issues/{issue_id}/attachments/hello.txt"))
        .bearer_auth(admin)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404, "deleted attachment should return 404");

    shutdown_tx.send(()).ok();
}

// ── Delta tarball mode ────────────────────────────────────────────────────────

/// Verifies that upload-snapshot processes `.vai-delta.json` correctly.
///
/// Flow:
/// 1.  Seed repo with three files (a.rs, b.rs, c.rs) via workspace → v2.
/// 2.  Create a second workspace and upload a **delta** tarball that:
///     - contains only a modified `a.rs`
///     - lists `b.rs` in `deleted_paths`
///     - does NOT mention `c.rs` (so c.rs should remain unchanged)
/// 3.  Verify the response has `is_delta: true`, `modified: 1`, `deleted: 1`.
/// 4.  Submit the workspace → v3.
/// 5.  Download repo → verify a.rs (modified), c.rs present, b.rs absent.
#[tokio::test(flavor = "multi_thread")]
async fn test_delta_tarball_upload() {
    let Some(url) = db_url() else { return };

    let tmp = TempDir::new().unwrap();
    let (addr, shutdown_tx) = start_for_testing_pg_multi_repo(tmp.path(), &url)
        .await
        .expect("start server");

    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let admin = "vai_admin_test";

    // ── Create repo ───────────────────────────────────────────────────────────
    let resp = client
        .post(format!("{base}/api/repos"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "name": "delta-test" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let repo: serde_json::Value = resp.json().await.unwrap();
    let rp = format!("{base}/api/repos/{}", repo["name"].as_str().unwrap());

    // ── Create & claim issue 1 → seed repo ───────────────────────────────────
    let resp = client
        .post(format!("{rp}/issues"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "title": "seed", "description": "", "priority": "low" }))
        .send()
        .await
        .unwrap();
    let issue1: serde_json::Value = resp.json().await.unwrap();

    let resp = client
        .post(format!("{rp}/work-queue/claim"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "issue_id": issue1["id"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let claim1: serde_json::Value = resp.json().await.unwrap();
    let ws1_id = claim1["workspace_id"].as_str().unwrap().to_string();

    // Upload three files via the files endpoint.
    client
        .post(format!("{rp}/workspaces/{ws1_id}/files"))
        .bearer_auth(admin)
        .json(&serde_json::json!({
            "files": [
                { "path": "src/a.rs", "content_base64": b64(b"fn a_v1() {}\n") },
                { "path": "src/b.rs", "content_base64": b64(b"fn b() {}\n") },
                { "path": "src/c.rs", "content_base64": b64(b"fn c() {}\n") },
            ]
        }))
        .send()
        .await
        .unwrap();

    let resp = client
        .post(format!("{rp}/workspaces/{ws1_id}/submit"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "summary": "seed" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "submit seed: {}", resp.text().await.unwrap_or_default());

    // ── Create & claim issue 2 → delta upload ─────────────────────────────────
    let resp = client
        .post(format!("{rp}/issues"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "title": "delta", "description": "", "priority": "low" }))
        .send()
        .await
        .unwrap();
    let issue2: serde_json::Value = resp.json().await.unwrap();

    let resp = client
        .post(format!("{rp}/work-queue/claim"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "issue_id": issue2["id"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let claim2: serde_json::Value = resp.json().await.unwrap();
    let ws2_id = claim2["workspace_id"].as_str().unwrap().to_string();

    // Build a delta tarball: modified a.rs + manifest (b.rs deleted, c.rs untouched).
    let manifest = serde_json::json!({
        "base_version": "v2",
        "deleted_paths": ["src/b.rs"]
    })
    .to_string();
    let delta_tb = make_tarball(&[
        (".vai-delta.json", manifest.as_bytes()),
        ("src/a.rs", b"fn a_v2() {}\n"),
    ]);

    let resp = client
        .post(format!("{rp}/workspaces/{ws2_id}/upload-snapshot"))
        .bearer_auth(admin)
        .header("content-type", "application/gzip")
        .body(delta_tb)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "upload-snapshot delta: {}", resp.text().await.unwrap_or_default());
    let snap: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(snap["is_delta"], serde_json::json!(true), "is_delta must be true");
    assert_eq!(snap["modified"], serde_json::json!(1), "a.rs should be modified");
    assert_eq!(snap["deleted"], serde_json::json!(1), "b.rs should be deleted via manifest");
    assert_eq!(snap["added"], serde_json::json!(0), "no new files");

    // ── Submit → v3 ──────────────────────────────────────────────────────────
    let resp = client
        .post(format!("{rp}/workspaces/{ws2_id}/submit"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "summary": "delta apply" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "submit delta: {}", resp.text().await.unwrap_or_default());

    // ── Download and verify final state ──────────────────────────────────────
    let resp = client
        .get(format!("{rp}/files/download"))
        .bearer_auth(admin)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "download");
    let tarball = resp.bytes().await.unwrap();
    let paths = tarball_paths(&tarball);

    let has_a = paths.iter().any(|p| p.contains("src/a.rs"));
    let has_b = paths.iter().any(|p| p.contains("src/b.rs"));
    let has_c = paths.iter().any(|p| p.contains("src/c.rs"));
    assert!(has_a, "a.rs must be present after delta apply");
    assert!(!has_b, "b.rs must be absent after delta deletion");
    assert!(has_c, "c.rs must be unchanged by delta upload");

    // Verify a.rs has the new content.
    let a_content = extract_file_from_tarball(&tarball, "src/a.rs");
    assert_eq!(a_content.as_deref(), Some(b"fn a_v2() {}\n".as_ref()), "a.rs must have updated content");

    shutdown_tx.send(()).ok();
}

/// Extracts the content of a file from a gzip tarball by path suffix.
fn extract_file_from_tarball<'a>(bytes: &'a [u8], path_suffix: &str) -> Option<Vec<u8>> {
    use flate2::read::GzDecoder;
    use std::io::Read;
    let decoder = GzDecoder::new(std::io::Cursor::new(bytes));
    let mut archive = tar::Archive::new(decoder);
    for entry in archive.entries().ok()? {
        let mut e = entry.ok()?;
        let p = e.path().ok()?.to_string_lossy().into_owned();
        if p.contains(path_suffix) {
            let mut content = Vec::new();
            e.read_to_end(&mut content).ok()?;
            return Some(content);
        }
    }
    None
}
