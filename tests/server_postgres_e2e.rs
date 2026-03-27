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
    let diffs = diff_resp["diffs"].as_array().expect("diffs must be an array");
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
