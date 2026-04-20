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

#![cfg(all(feature = "postgres", feature = "s3"))]

use std::env;
use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use futures_util::{SinkExt, StreamExt};
use tempfile::TempDir;
use tokio::time::timeout;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

use vai::server::{
    start_for_testing_pg_multi_repo,
    start_for_testing_pg_multi_repo_with_quota,
    start_for_testing_pg_with_mem_fs,
};

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
        .json(&serde_json::json!({ "name": format!("e2e-workflow-{}", unique_suffix()) }))
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
    let arr = issues["data"].as_array().unwrap();
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
    let version_arr = versions["data"].as_array().unwrap();
    assert!(
        version_arr.iter().any(|v| v["version_id"].as_str() == Some(&new_version)),
        "submitted version must appear in version list"
    );

    // Find the UUID of the new version for detail checks.
    let version_id = version_arr
        .iter()
        .find(|v| v["version_id"].as_str() == Some(&new_version))
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
        .json(&serde_json::json!({ "name": format!("e2e-discard-{}", unique_suffix()) }))
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
    assert_eq!(resp.status(), 204, "discard: {}", resp.text().await.unwrap_or_default());

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
        .json(&serde_json::json!({ "name": format!("e2e-ws-{}", unique_suffix()) }))
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
        .to_string(),
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
        e["type"].as_str() == Some("WorkspaceCreated")
            || e["event_type"].as_str() == Some("WorkspaceCreated")
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
        e["type"].as_str() == Some("WorkspaceSubmitted")
            || e["event_type"].as_str() == Some("WorkspaceSubmitted")
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
        .json(&serde_json::json!({ "name": format!("e2e-concurrent-{}", unique_suffix()) }))
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
    let version_count = versions["data"].as_array().unwrap().len();
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
    let repo_name = format!("e2e-file-download-{}", unique_suffix());
    let resp = client
        .post(format!("{base}/api/repos"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "name": repo_name }))
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
    let repo_name = format!("e2e-version-diff-{}", unique_suffix());
    let resp = client
        .post(format!("{base}/api/repos"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "name": repo_name }))
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
    let version_id = versions["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["version_id"].as_str() == Some(&version_label))
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
        .json(&serde_json::json!({ "name": format!("e2e-submit-files-match-{}", unique_suffix()) }))
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
    let repo_name = format!("e2e-submit-deletions-{}", unique_suffix());
    let resp = client
        .post(format!("{base}/api/repos"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "name": repo_name }))
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
        .json(&serde_json::json!({ "name": format!("e2e-sequential-submits-{}", unique_suffix()) }))
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
        .json(&serde_json::json!({ "name": format!("link-blocking-test-{}", unique_suffix()) }))
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
        .json(&serde_json::json!({ "name": format!("e2e-deletion-roundtrip-{}", unique_suffix()) }))
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
    let v2_id = versions["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["version_id"].as_str() == Some(&v2_label))
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
    let v3_id = versions["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["version_id"].as_str() == Some(&v3_label))
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
        .json(&serde_json::json!({ "name": format!("e2e-readonly-root-{}", unique_suffix()) }))
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
    let version_arr = versions["data"].as_array().unwrap();
    assert!(
        version_arr.iter().any(|v| v["version_id"].as_str() == Some(&new_version)),
        "submitted version must appear in version list"
    );

    let version_id = version_arr
        .iter()
        .find(|v| v["version_id"].as_str() == Some(&new_version))
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
        .json(&serde_json::json!({ "name": format!("comment-author-type-test-{}", unique_suffix()) }))
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
        .json(&serde_json::json!({ "name": format!("attachment-test-repo-{}", unique_suffix()) }))
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
    let repo_name = format!("delta-test-{}", unique_suffix());
    let resp = client
        .post(format!("{base}/api/repos"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "name": repo_name }))
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

// ── Stateless server lifecycle ────────────────────────────────────────────────

/// Validates the full stateless server lifecycle — the server starts with only
/// a `DATABASE_URL` (no `.vai/` directory, no `server.toml`, no `registry.json`)
/// and all API operations succeed through Postgres + MemFs.
///
/// Verifies:
/// 1. Health check passes immediately after startup
/// 2. Repo created via `POST /api/repos` — no filesystem state required
/// 3. Repo appears in `GET /api/repos`
/// 4. Workspace created, files uploaded, workspace submitted → new version
/// 5. `GET /api/repos/:repo/status` shows the correct `head_version`
/// 6. No `.vai/` subdirectory is ever created under the storage root
#[tokio::test(flavor = "multi_thread")]
async fn test_stateless_server_lifecycle() {
    let Some(url) = db_url() else { return };

    let tmp = TempDir::new().unwrap();
    // Start with a completely empty temp directory — no pre-existing state.
    assert!(
        std::fs::read_dir(tmp.path()).unwrap().next().is_none(),
        "storage root must be empty before server starts"
    );

    let (addr, shutdown_tx) = start_for_testing_pg_multi_repo(tmp.path(), &url)
        .await
        .expect("start_for_testing_pg_multi_repo failed");

    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let admin = "vai_admin_test";

    // ── 1. Health check ───────────────────────────────────────────────────────
    let resp = client.get(format!("{base}/health")).send().await.unwrap();
    assert_eq!(resp.status(), 200, "health check must pass");

    // ── 2. Create repo — Postgres only, no filesystem writes ─────────────────
    let resp = client
        .post(format!("{base}/api/repos"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "name": format!("stateless-test-{}", unique_suffix()) }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create repo: {}", resp.text().await.unwrap_or_default());
    let repo: serde_json::Value = resp.json().await.unwrap();
    let repo_name = repo["name"].as_str().unwrap();
    let rp = format!("{base}/api/repos/{repo_name}");

    // ── 3. Repo appears in GET /api/repos ─────────────────────────────────────
    let resp = client.get(format!("{base}/api/repos")).bearer_auth(admin).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let repos_list: serde_json::Value = resp.json().await.unwrap();
    let repos_arr = repos_list.as_array().unwrap();
    assert!(
        repos_arr.iter().any(|r| r["name"].as_str() == Some(repo_name)),
        "created repo must appear in GET /api/repos"
    );

    // ── 4. Create issue, claim (workspace), upload files, submit ─────────────
    let resp = client
        .post(format!("{rp}/issues"))
        .bearer_auth(admin)
        .json(&serde_json::json!({
            "title": "stateless test issue",
            "description": "validates stateless server lifecycle",
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
    let ws_id = claim["workspace_id"].as_str().unwrap();

    let resp = client
        .post(format!("{rp}/workspaces/{ws_id}/files"))
        .bearer_auth(admin)
        .json(&serde_json::json!({
            "files": [{ "path": "src/hello.rs", "content_base64": b64(b"fn hello() {}\n") }]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "upload files: {}", resp.text().await.unwrap_or_default());

    let resp = client
        .post(format!("{rp}/workspaces/{ws_id}/submit"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "message": "stateless submit" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "submit: {}", resp.text().await.unwrap_or_default());
    let submit: serde_json::Value = resp.json().await.unwrap();
    let new_version = submit["version"].as_str().unwrap().to_string();
    assert!(!new_version.is_empty(), "submit must return a version");

    // ── 5. Status shows correct head_version ──────────────────────────────────
    let resp = client.get(format!("{rp}/status")).bearer_auth(admin).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let status: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        status["head_version"].as_str().unwrap_or(""),
        new_version,
        "status head_version must match the submitted version"
    );
    assert_eq!(
        status["repo_name"].as_str().unwrap_or(""),
        repo_name,
        "status repo_name must match"
    );

    // ── 6. No .vai/ subdirectory created under storage root ───────────────────
    // In server (Postgres) mode, all metadata lives in Postgres.  The only
    // entries under the storage root should be the top-level `.vai` placeholder
    // that AppState uses as a path (never written to disk) — not a real
    // per-repo `.vai/` directory.
    let vai_subdirs: Vec<_> = std::fs::read_dir(tmp.path())
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".vai"))
        .collect();
    assert!(
        vai_subdirs.is_empty(),
        "no .vai/ directories should exist under the storage root in server mode; found: {:?}",
        vai_subdirs.iter().map(|e| e.path()).collect::<Vec<_>>()
    );

    shutdown_tx.send(()).ok();
}

// ── Non-admin repo creation and quota enforcement ─────────────────────────────

/// Returns a short unique suffix derived from the current time (microseconds).
///
/// Used to give test entities unique names across repeated runs against the
/// same Postgres instance.
fn unique_suffix() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let us = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros();
    format!("{us:x}")
}

/// Verifies that a non-admin user with a valid API key can create a repo via
/// `POST /api/repos` and is automatically added as an admin collaborator.
///
/// Flow:
/// 1. Create a user with the admin key: `POST /api/users`
/// 2. Mint a key for the user: `POST /api/keys` with `for_user_id`
/// 3. Use the user key to create a repo: `POST /api/repos`
/// 4. Verify 201 and that the user is an admin collaborator via `GET /api/repos/:repo/me`
#[tokio::test(flavor = "multi_thread")]
async fn test_non_admin_repo_creation() {
    let Some(url) = db_url() else { return };

    let tmp = TempDir::new().unwrap();
    let (addr, shutdown_tx) = start_for_testing_pg_multi_repo(tmp.path(), &url)
        .await
        .expect("server start failed");

    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let admin = "vai_admin_test";
    let sfx = unique_suffix();

    // 1. Create a user.
    let resp = client
        .post(format!("{base}/api/users"))
        .bearer_auth(admin)
        .json(&serde_json::json!({
            "name": format!("Alice-{sfx}"),
            "email": format!("alice-{sfx}@example.com"),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create user: {}", resp.text().await.unwrap_or_default());
    let user: serde_json::Value = resp.json().await.unwrap();
    let user_id = user["id"].as_str().unwrap().to_string();

    // 2. Create an API key for the user via admin's for_user_id field.
    let resp = client
        .post(format!("{base}/api/keys"))
        .bearer_auth(admin)
        .json(&serde_json::json!({
            "name": format!("alice-key-{sfx}"),
            "for_user_id": user_id,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create key: {}", resp.text().await.unwrap_or_default());
    let key_resp: serde_json::Value = resp.json().await.unwrap();
    let user_token = key_resp["token"].as_str().unwrap().to_string();

    // 3. Use the user key to create a repo.
    let repo_name = format!("alice-repo-{sfx}");
    let resp = client
        .post(format!("{base}/api/repos"))
        .bearer_auth(&user_token)
        .json(&serde_json::json!({ "name": repo_name }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "non-admin create repo: {}", resp.text().await.unwrap_or_default());
    let repo: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(repo["name"].as_str(), Some(repo_name.as_str()));

    // 4. Verify the user has admin collaborator role via GET /api/repos/:repo/me.
    let resp = client
        .get(format!("{base}/api/repos/{repo_name}/me"))
        .bearer_auth(&user_token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "GET /me: {}", resp.text().await.unwrap_or_default());
    let me: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(me["role"].as_str(), Some("admin"), "creator must have admin role");
    assert_eq!(me["user_id"].as_str(), Some(user_id.as_str()));

    shutdown_tx.send(()).ok();
}

/// Verifies that a non-admin user is blocked at the quota limit and receives a
/// structured 403 response body with `limit` and `current` fields.
///
/// Uses a server configured with `max_repos_per_user = 2` so the test only
/// needs to create 2 repos before hitting the limit.
#[tokio::test(flavor = "multi_thread")]
async fn test_non_admin_repo_quota_exceeded() {
    let Some(url) = db_url() else { return };

    let tmp = TempDir::new().unwrap();
    // Set quota to 2 so the 3rd creation attempt is rejected.
    let (addr, shutdown_tx) = start_for_testing_pg_multi_repo_with_quota(tmp.path(), &url, 2)
        .await
        .expect("server start failed");

    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let admin = "vai_admin_test";
    let sfx = unique_suffix();

    // Create user + key.
    let resp = client
        .post(format!("{base}/api/users"))
        .bearer_auth(admin)
        .json(&serde_json::json!({
            "name": format!("Bob-{sfx}"),
            "email": format!("bob-{sfx}@example.com"),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create user: {}", resp.text().await.unwrap_or_default());
    let user: serde_json::Value = resp.json().await.unwrap();
    let user_id = user["id"].as_str().unwrap().to_string();

    let resp = client
        .post(format!("{base}/api/keys"))
        .bearer_auth(admin)
        .json(&serde_json::json!({
            "name": format!("bob-key-{sfx}"),
            "for_user_id": user_id,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create key: {}", resp.text().await.unwrap_or_default());
    let key_resp: serde_json::Value = resp.json().await.unwrap();
    let user_token = key_resp["token"].as_str().unwrap().to_string();

    // Create 2 repos — should succeed.
    for i in 0..2u32 {
        let resp = client
            .post(format!("{base}/api/repos"))
            .bearer_auth(&user_token)
            .json(&serde_json::json!({ "name": format!("bob-repo-{sfx}-{i}") }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201, "repo {i} creation should succeed");
    }

    // 3rd repo must be rejected with 403 and the quota body.
    let resp = client
        .post(format!("{base}/api/repos"))
        .bearer_auth(&user_token)
        .json(&serde_json::json!({ "name": format!("bob-repo-{sfx}-overflow") }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403, "3rd repo must be blocked by quota");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"].as_str(), Some("repo quota exceeded"));
    assert_eq!(body["limit"].as_u64(), Some(2));
    assert_eq!(body["current"].as_u64(), Some(2));

    // Admin is never blocked by the quota.
    let resp = client
        .post(format!("{base}/api/repos"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "name": format!("admin-repo-{sfx}-no-quota") }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "admin must bypass quota");

    shutdown_tx.send(()).ok();
}

// ── CLI device code flow ──────────────────────────────────────────────────────

/// Full device code flow:
///
/// 1. `POST /api/auth/cli-device` — create a pending code (unauthenticated).
/// 2. `GET /api/auth/cli-device/:code` — poll while pending.
/// 3. `POST /api/auth/cli-device/authorize` — authorize (authenticated user).
/// 4. `GET /api/auth/cli-device/:code` — poll returns authorized + api_key.
/// 5. `GET /api/auth/cli-device/:code` — second poll returns 404 (key consumed).
/// 6. Verify the returned api_key is usable against another endpoint.
#[tokio::test(flavor = "multi_thread")]
async fn test_cli_device_code_flow() {
    let Some(url) = db_url() else { return };

    let tmp = TempDir::new().unwrap();
    let (addr, shutdown_tx) = start_for_testing_pg_multi_repo(tmp.path(), &url)
        .await
        .expect("server start failed");

    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let admin = "vai_admin_test";
    let sfx = unique_suffix();

    // Create a user to act as the dashboard-side authorizer.
    let resp = client
        .post(format!("{base}/api/users"))
        .bearer_auth(admin)
        .json(&serde_json::json!({
            "name": format!("DevUser-{sfx}"),
            "email": format!("devuser-{sfx}@example.com"),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let user: serde_json::Value = resp.json().await.unwrap();
    let user_id = user["id"].as_str().unwrap().to_string();

    // Mint an API key for the user so they can call the authorize endpoint.
    let resp = client
        .post(format!("{base}/api/keys"))
        .bearer_auth(admin)
        .json(&serde_json::json!({
            "name": format!("devuser-key-{sfx}"),
            "for_user_id": user_id,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let key_resp: serde_json::Value = resp.json().await.unwrap();
    let user_token = key_resp["token"].as_str().unwrap().to_string();

    // 1. Create a device code (unauthenticated).
    let resp = client
        .post(format!("{base}/api/auth/cli-device"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "create device code: {}", resp.text().await.unwrap_or_default());
    let dc: serde_json::Value = resp.json().await.unwrap();
    let code = dc["code"].as_str().unwrap().to_string();
    assert!(
        dc["verification_url"].as_str().is_some(),
        "verification_url missing"
    );
    assert_eq!(dc["poll_interval"].as_u64(), Some(3));
    // Code format: XXXX-YYYY (4 uppercase alphanumeric, dash, 4 more)
    assert_eq!(code.len(), 9, "code should be 9 chars");
    assert_eq!(&code[4..5], "-", "code should have dash at position 4");

    // 2. Poll while pending — should return {"status":"pending"}.
    let resp = client
        .get(format!("{base}/api/auth/cli-device/{code}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let status: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(status["status"].as_str(), Some("pending"));
    assert!(status["api_key"].is_null(), "api_key should be absent when pending");

    // 3. Authorize the code as the user (simulates dashboard /cli page).
    let resp = client
        .post(format!("{base}/api/auth/cli-device/authorize"))
        .bearer_auth(&user_token)
        .json(&serde_json::json!({ "code": code }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "authorize: {}", resp.text().await.unwrap_or_default());

    // 4. Poll again — should now return authorized with an api_key.
    let resp = client
        .get(format!("{base}/api/auth/cli-device/{code}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let status: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(status["status"].as_str(), Some("authorized"));
    let minted_key = status["api_key"].as_str().expect("api_key should be present after auth");

    // 5. Second poll must return 404 — the key is consumed on first reveal.
    let resp = client
        .get(format!("{base}/api/auth/cli-device/{code}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404, "second poll should return 404 after key is consumed");

    // 6. Verify the returned API key works by calling /api/keys.
    let resp = client
        .get(format!("{base}/api/keys"))
        .bearer_auth(minted_key)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "minted key should be usable: {}", resp.text().await.unwrap_or_default());

    shutdown_tx.send(()).ok();
}

/// Device code returns 404 for unknown or expired codes.
#[tokio::test(flavor = "multi_thread")]
async fn test_cli_device_code_not_found() {
    let Some(url) = db_url() else { return };

    let tmp = TempDir::new().unwrap();
    let (addr, shutdown_tx) = start_for_testing_pg_multi_repo(tmp.path(), &url)
        .await
        .expect("server start failed");

    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    // Poll a code that was never created.
    let resp = client
        .get(format!("{base}/api/auth/cli-device/FAKE-CODE"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404, "non-existent code should return 404");

    // Authorize with a non-existent code must also return 404.
    let admin = "vai_admin_test";
    let sfx = unique_suffix();
    let resp_user = client
        .post(format!("{base}/api/users"))
        .bearer_auth(admin)
        .json(&serde_json::json!({
            "name": format!("Ghost-{sfx}"),
            "email": format!("ghost-{sfx}@example.com"),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp_user.status(), 201);
    let user: serde_json::Value = resp_user.json().await.unwrap();
    let user_id = user["id"].as_str().unwrap();
    let resp_key = client
        .post(format!("{base}/api/keys"))
        .bearer_auth(admin)
        .json(&serde_json::json!({
            "name": format!("ghost-key-{sfx}"),
            "for_user_id": user_id,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp_key.status(), 201);
    let key_resp: serde_json::Value = resp_key.json().await.unwrap();
    let user_token = key_resp["token"].as_str().unwrap();

    let resp = client
        .post(format!("{base}/api/auth/cli-device/authorize"))
        .bearer_auth(user_token)
        .json(&serde_json::json!({ "code": "FAKE-CODE" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404, "authorizing non-existent code should return 404");

    shutdown_tx.send(()).ok();
}

// ── Multi-tenancy isolation tests ────────────────────────────────────────────

/// Creates a test user and returns (user_id, api_token).
async fn create_test_user(
    client: &reqwest::Client,
    base: &str,
    admin: &str,
    sfx: &str,
    label: &str,
) -> (String, String) {
    let resp = client
        .post(format!("{base}/api/users"))
        .bearer_auth(admin)
        .json(&serde_json::json!({
            "name": format!("{label}-{sfx}"),
            "email": format!("{label}-{sfx}@example.com"),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create user {label}: {}", resp.text().await.unwrap_or_default());
    let user: serde_json::Value = resp.json().await.unwrap();
    let user_id = user["id"].as_str().unwrap().to_string();

    let resp = client
        .post(format!("{base}/api/keys"))
        .bearer_auth(admin)
        .json(&serde_json::json!({
            "name": format!("{label}-key-{sfx}"),
            "for_user_id": &user_id,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create key for {label}: {}", resp.text().await.unwrap_or_default());
    let key_resp: serde_json::Value = resp.json().await.unwrap();
    let token = key_resp["token"].as_str().unwrap().to_string();

    (user_id, token)
}

/// Verifies that `GET /api/repos` only returns repos the caller is a collaborator on.
///
/// Flow:
/// 1. Create two users (Alice, Bob) with distinct API keys.
/// 2. Alice creates `alice-repo-<sfx>`.
/// 3. Bob creates `bob-repo-<sfx>`.
/// 4. Alice's `GET /api/repos` returns only her repo.
/// 5. Bob's `GET /api/repos` returns only his repo.
/// 6. Admin's `GET /api/repos` returns both repos.
#[tokio::test(flavor = "multi_thread")]
async fn test_list_repos_filters_by_collaborator() {
    let Some(url) = db_url() else { return };

    let tmp = TempDir::new().unwrap();
    let (addr, shutdown_tx) = start_for_testing_pg_multi_repo(tmp.path(), &url)
        .await
        .expect("server start failed");

    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let admin = "vai_admin_test";
    let sfx = unique_suffix();

    let (_alice_id, alice_token) = create_test_user(&client, &base, admin, &sfx, "alice").await;
    let (_bob_id, bob_token) = create_test_user(&client, &base, admin, &sfx, "bob").await;

    let alice_repo = format!("alice-repo-{sfx}");
    let bob_repo = format!("bob-repo-{sfx}");

    // Alice creates her repo.
    let resp = client
        .post(format!("{base}/api/repos"))
        .bearer_auth(&alice_token)
        .json(&serde_json::json!({ "name": &alice_repo }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "Alice create repo: {}", resp.text().await.unwrap_or_default());

    // Bob creates his repo.
    let resp = client
        .post(format!("{base}/api/repos"))
        .bearer_auth(&bob_token)
        .json(&serde_json::json!({ "name": &bob_repo }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "Bob create repo: {}", resp.text().await.unwrap_or_default());

    // Alice sees only her repo.
    let resp = client
        .get(format!("{base}/api/repos"))
        .bearer_auth(&alice_token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let repos: serde_json::Value = resp.json().await.unwrap();
    let arr = repos.as_array().unwrap();
    assert!(
        arr.iter().any(|r| r["name"].as_str() == Some(&alice_repo)),
        "Alice must see her own repo"
    );
    assert!(
        !arr.iter().any(|r| r["name"].as_str() == Some(&bob_repo)),
        "Alice must NOT see Bob's repo"
    );
    // path field must not be present for non-admin.
    for r in arr {
        assert!(r.get("path").is_none(), "path must be absent for non-admin user");
    }

    // Bob sees only his repo.
    let resp = client
        .get(format!("{base}/api/repos"))
        .bearer_auth(&bob_token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let repos: serde_json::Value = resp.json().await.unwrap();
    let arr = repos.as_array().unwrap();
    assert!(
        arr.iter().any(|r| r["name"].as_str() == Some(&bob_repo)),
        "Bob must see his own repo"
    );
    assert!(
        !arr.iter().any(|r| r["name"].as_str() == Some(&alice_repo)),
        "Bob must NOT see Alice's repo"
    );

    shutdown_tx.send(()).ok();
}

/// Verifies that a non-collaborator receives 403 on per-repo endpoints.
///
/// Alice creates a repo; Bob (no collaborator row) calls `GET /api/repos/:repo/issues`
/// and must get 403.
#[tokio::test(flavor = "multi_thread")]
async fn test_repo_access_returns_403_for_non_collaborator() {
    let Some(url) = db_url() else { return };

    let tmp = TempDir::new().unwrap();
    let (addr, shutdown_tx) = start_for_testing_pg_multi_repo(tmp.path(), &url)
        .await
        .expect("server start failed");

    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let admin = "vai_admin_test";
    let sfx = unique_suffix();

    let (_alice_id, alice_token) = create_test_user(&client, &base, admin, &sfx, "alice2").await;
    let (_bob_id, bob_token) = create_test_user(&client, &base, admin, &sfx, "bob2").await;

    let alice_repo = format!("alice-private-{sfx}");

    // Alice creates a repo.
    let resp = client
        .post(format!("{base}/api/repos"))
        .bearer_auth(&alice_token)
        .json(&serde_json::json!({ "name": &alice_repo }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "Alice create repo: {}", resp.text().await.unwrap_or_default());

    // Bob tries to list Alice's issues — must get 403.
    let resp = client
        .get(format!("{base}/api/repos/{alice_repo}/issues"))
        .bearer_auth(&bob_token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403, "Bob must get 403 on Alice's issues");

    // Bob tries to list Alice's versions — must get 403.
    let resp = client
        .get(format!("{base}/api/repos/{alice_repo}/versions"))
        .bearer_auth(&bob_token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403, "Bob must get 403 on Alice's versions");

    // Bob tries to list Alice's workspaces — must get 403.
    let resp = client
        .get(format!("{base}/api/repos/{alice_repo}/workspaces"))
        .bearer_auth(&bob_token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403, "Bob must get 403 on Alice's workspaces");

    // Alice can still access her own repo.
    let resp = client
        .get(format!("{base}/api/repos/{alice_repo}/issues"))
        .bearer_auth(&alice_token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "Alice must access her own repo issues");

    shutdown_tx.send(()).ok();
}

/// Verifies that the bootstrap admin key sees all repos via `GET /api/repos`.
///
/// Creates two repos owned by two different users and asserts the admin response
/// contains both, plus that the `path` field is present for admin.
#[tokio::test(flavor = "multi_thread")]
async fn test_admin_sees_all_repos() {
    let Some(url) = db_url() else { return };

    let tmp = TempDir::new().unwrap();
    let (addr, shutdown_tx) = start_for_testing_pg_multi_repo(tmp.path(), &url)
        .await
        .expect("server start failed");

    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let admin = "vai_admin_test";
    let sfx = unique_suffix();

    let (_u1_id, u1_token) = create_test_user(&client, &base, admin, &sfx, "u1").await;
    let (_u2_id, u2_token) = create_test_user(&client, &base, admin, &sfx, "u2").await;

    let repo1 = format!("admin-vis-repo1-{sfx}");
    let repo2 = format!("admin-vis-repo2-{sfx}");

    for (token, name) in [(&u1_token, &repo1), (&u2_token, &repo2)] {
        let resp = client
            .post(format!("{base}/api/repos"))
            .bearer_auth(token)
            .json(&serde_json::json!({ "name": name }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201, "create repo {name}: {}", resp.text().await.unwrap_or_default());
    }

    // Admin sees all repos (at minimum both just created).
    let resp = client
        .get(format!("{base}/api/repos"))
        .bearer_auth(admin)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let repos: serde_json::Value = resp.json().await.unwrap();
    let arr = repos.as_array().unwrap();
    assert!(
        arr.iter().any(|r| r["name"].as_str() == Some(repo1.as_str())),
        "admin must see repo1"
    );
    assert!(
        arr.iter().any(|r| r["name"].as_str() == Some(repo2.as_str())),
        "admin must see repo2"
    );

    shutdown_tx.send(()).ok();
}

/// Verifies that the per-repo WebSocket endpoint enforces `RepoAccess`.
///
/// Alice creates a repo; Bob (no collaborator row) tries to upgrade a WebSocket
/// connection to `GET /api/repos/:repo/ws/events?key=<bob_key>` and must be
/// rejected with a non-101 response (403 Forbidden).
/// Alice herself must be able to connect successfully.
#[tokio::test(flavor = "multi_thread")]
async fn test_ws_events_rejects_non_collaborator() {
    let Some(url) = db_url() else { return };

    let tmp = TempDir::new().unwrap();
    let (addr, shutdown_tx) = start_for_testing_pg_multi_repo(tmp.path(), &url)
        .await
        .expect("server start failed");

    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let admin = "vai_admin_test";
    let sfx = unique_suffix();

    let (_alice_id, alice_token) = create_test_user(&client, &base, admin, &sfx, "ws-alice").await;
    let (_bob_id, bob_token) = create_test_user(&client, &base, admin, &sfx, "ws-bob").await;

    let alice_repo = format!("ws-alice-repo-{sfx}");

    // Alice creates a repo.
    let resp = client
        .post(format!("{base}/api/repos"))
        .bearer_auth(&alice_token)
        .json(&serde_json::json!({ "name": &alice_repo }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "Alice create repo");

    // Bob tries to connect to Alice's per-repo WS endpoint — must be rejected.
    let bob_ws_url = format!("ws://{addr}/api/repos/{alice_repo}/ws/events?key={bob_token}");
    let result = connect_async(&bob_ws_url).await;
    match result {
        Err(_) => {
            // Connection rejected outright (HTTP 403 before upgrade) — correct.
        }
        Ok((mut stream, resp)) => {
            // If the handshake "succeeded" the server must close quickly with
            // a non-101 status or an immediate close frame.
            assert_ne!(
                resp.status(),
                tokio_tungstenite::tungstenite::http::StatusCode::SWITCHING_PROTOCOLS,
                "Bob must not receive 101 Switching Protocols on Alice's repo WS"
            );
            // Drain any close frame.
            let _ = timeout(Duration::from_secs(1), stream.next()).await;
        }
    }

    // Alice can connect to her own repo WS endpoint.
    let alice_ws_url = format!("ws://{addr}/api/repos/{alice_repo}/ws/events?key={alice_token}");
    let (mut alice_stream, _) = connect_async(&alice_ws_url)
        .await
        .expect("Alice's WebSocket connection must succeed");

    // Send a subscribe message to confirm the connection is live.
    alice_stream
        .send(Message::Text(
            serde_json::json!({ "subscribe": { "event_types": [] } }).to_string(),
        ))
        .await
        .expect("Alice subscribe send failed");

    // Close cleanly.
    let _ = alice_stream.close(None).await;

    shutdown_tx.send(()).ok();
}

/// Tests the onboarding state endpoints: GET /api/me/onboarding and
/// POST /api/me/onboarding-complete.
///
/// Covers:
/// - Fresh user returns `{ completed_at: null }` on GET.
/// - POST flips the flag and returns a timestamp.
/// - Subsequent GET returns the same timestamp.
/// - POST is idempotent — second call returns the same timestamp.
/// - Unauthenticated requests get 401.
/// - Admin-key requests get 401 (no user identity).
#[tokio::test(flavor = "multi_thread")]
async fn test_user_onboarding_endpoints() {
    let Some(url) = db_url() else { return };

    let tmp = TempDir::new().unwrap();
    let (addr, shutdown_tx) = start_for_testing_pg_multi_repo(tmp.path(), &url)
        .await
        .expect("server start failed");

    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let admin = "vai_admin_test";
    let sfx = unique_suffix();

    // Create a user + API key.
    let (_user_id, user_token) =
        create_test_user(&client, &base, admin, &sfx, "onboard").await;

    // 1. Fresh user has no onboarding record — completed_at must be null.
    let resp = client
        .get(format!("{base}/api/me/onboarding"))
        .bearer_auth(&user_token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "GET onboarding: {}", resp.text().await.unwrap_or_default());
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["completed_at"].is_null(),
        "fresh user should have null completed_at, got: {body}"
    );

    // 2. POST marks onboarding complete and returns a timestamp.
    let resp = client
        .post(format!("{base}/api/me/onboarding-complete"))
        .bearer_auth(&user_token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "POST onboarding-complete: {}", resp.text().await.unwrap_or_default());
    let body1: serde_json::Value = resp.json().await.unwrap();
    let ts1 = body1["completed_at"].as_str().expect("completed_at must be a string");
    assert!(!ts1.is_empty(), "completed_at must not be empty");

    // 3. GET now returns the same timestamp.
    let resp = client
        .get(format!("{base}/api/me/onboarding"))
        .bearer_auth(&user_token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        body["completed_at"].as_str(),
        Some(ts1),
        "GET after POST should return same timestamp"
    );

    // 4. POST again is idempotent — same timestamp.
    let resp = client
        .post(format!("{base}/api/me/onboarding-complete"))
        .bearer_auth(&user_token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body2: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        body2["completed_at"].as_str(),
        Some(ts1),
        "second POST should return original timestamp"
    );

    // 5. Unauthenticated GET returns 401.
    let resp = client
        .get(format!("{base}/api/me/onboarding"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401, "unauthenticated GET must return 401");

    // 6. Unauthenticated POST returns 401.
    let resp = client
        .post(format!("{base}/api/me/onboarding-complete"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401, "unauthenticated POST must return 401");

    // 7. Admin key has no user identity — must return 401.
    let resp = client
        .get(format!("{base}/api/me/onboarding"))
        .bearer_auth(admin)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401, "admin key GET must return 401");

    let resp = client
        .post(format!("{base}/api/me/onboarding-complete"))
        .bearer_auth(admin)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401, "admin key POST must return 401");

    shutdown_tx.send(()).ok();
}

// ── upload_snapshot() auto-selection e2e tests ────────────────────────────────

/// Drives the real `upload_snapshot()` function from `remote_workspace.rs`
/// end-to-end against a live Postgres+S3 server.
///
/// Flow:
/// 1.  Seed repo with five files (a–e) via workspace → v2.
/// 2.  Create a second workspace; populate its overlay directory with:
///     - 3 modified files (a.rs, b.rs, c.rs)
///     - 1 deleted path (d.rs recorded in deleted_paths)
///     - e.rs intentionally absent from overlay (unchanged)
/// 3.  Create a > 500 KiB "repo dir" so the threshold logic selects delta mode.
/// 4.  Call `upload_snapshot()` directly and assert `is_delta: true`.
/// 5.  Submit and download; assert modified/deleted/unchanged match.
#[tokio::test(flavor = "multi_thread")]
async fn test_upload_snapshot_auto_selects_delta_mode() {
    let Some(url) = db_url() else { return };

    let tmp = TempDir::new().unwrap();
    let (addr, shutdown_tx) = start_for_testing_pg_multi_repo(tmp.path(), &url)
        .await
        .expect("server start");

    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let admin = "vai_admin_test";
    let sfx = unique_suffix();
    let repo_name = format!("upload-snap-{sfx}");

    // ── 1. Create repo ────────────────────────────────────────────────────────
    let resp = client
        .post(format!("{base}/api/repos"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "name": &repo_name }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let rp = format!("{base}/api/repos/{repo_name}");

    // ── 2. Seed repo with five files ──────────────────────────────────────────
    let seed_issue_resp = client
        .post(format!("{rp}/issues"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "title": "seed", "description": "", "priority": "low" }))
        .send()
        .await
        .unwrap();
    let seed_issue: serde_json::Value = seed_issue_resp.json().await.unwrap();

    let claim1_resp = client
        .post(format!("{rp}/work-queue/claim"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "issue_id": seed_issue["id"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(claim1_resp.status(), 201);
    let claim1: serde_json::Value = claim1_resp.json().await.unwrap();
    let ws1_id = claim1["workspace_id"].as_str().unwrap().to_string();

    client
        .post(format!("{rp}/workspaces/{ws1_id}/files"))
        .bearer_auth(admin)
        .json(&serde_json::json!({
            "files": [
                { "path": "src/a.rs", "content_base64": b64(b"fn a_v1() {}\n") },
                { "path": "src/b.rs", "content_base64": b64(b"fn b_v1() {}\n") },
                { "path": "src/c.rs", "content_base64": b64(b"fn c_v1() {}\n") },
                { "path": "src/d.rs", "content_base64": b64(b"fn d_v1() {}\n") },
                { "path": "src/e.rs", "content_base64": b64(b"fn e_v1() {}\n") },
            ]
        }))
        .bearer_auth(admin)
        .send()
        .await
        .unwrap();

    let submit1 = client
        .post(format!("{rp}/workspaces/{ws1_id}/submit"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "summary": "seed five files" }))
        .send()
        .await
        .unwrap();
    assert_eq!(submit1.status(), 200, "seed submit: {}", submit1.text().await.unwrap_or_default());

    // ── 3. Create second workspace ────────────────────────────────────────────
    let work_issue_resp = client
        .post(format!("{rp}/issues"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "title": "delta work", "description": "", "priority": "low" }))
        .send()
        .await
        .unwrap();
    let work_issue: serde_json::Value = work_issue_resp.json().await.unwrap();

    let claim2_resp = client
        .post(format!("{rp}/work-queue/claim"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "issue_id": work_issue["id"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(claim2_resp.status(), 201);
    let claim2: serde_json::Value = claim2_resp.json().await.unwrap();
    let ws2_id = claim2["workspace_id"].as_str().unwrap().to_string();

    // Fetch workspace to get base_version.
    let ws_meta_resp = client
        .get(format!("{rp}/workspaces/{ws2_id}"))
        .bearer_auth(admin)
        .send()
        .await
        .unwrap();
    assert_eq!(ws_meta_resp.status(), 200);
    let ws_meta: serde_json::Value = ws_meta_resp.json().await.unwrap();
    let base_version = ws_meta["base_version"].as_str().unwrap_or("v2").to_string();

    // ── 4. Build repo dir (> 500 KiB) and overlay dir ─────────────────────────
    // repo_dir: must exceed the 500 KiB threshold to trigger delta mode.
    let repo_dir = TempDir::new().unwrap();
    let src_dir = repo_dir.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    // Write 600 KiB of dummy content to push the repo above the threshold.
    std::fs::write(repo_dir.path().join("bulk.bin"), vec![0u8; 600 * 1024]).unwrap();
    // Overlay dir: files that were changed by the agent.
    let overlay_dir = TempDir::new().unwrap();
    let overlay_src = overlay_dir.path().join("src");
    std::fs::create_dir_all(&overlay_src).unwrap();
    std::fs::write(overlay_src.join("a.rs"), b"fn a_v2() {}\n").unwrap();
    std::fs::write(overlay_src.join("b.rs"), b"fn b_v2() {}\n").unwrap();
    std::fs::write(overlay_src.join("c.rs"), b"fn c_v2() {}\n").unwrap();
    // d.rs is deleted; e.rs is unchanged (neither in overlay nor deleted_paths).
    let deleted_paths = vec!["src/d.rs".to_string()];

    // ── 5. Call upload_snapshot() directly ────────────────────────────────────
    let remote = vai::clone::RemoteConfig {
        server_url: format!("http://{addr}"),
        api_key: admin.to_string(),
        repo_name: repo_name.clone(),
        cloned_at_version: "v1".to_string(),
    };
    let snap = vai::remote_workspace::upload_snapshot(
        &remote,
        &ws2_id,
        repo_dir.path(),
        overlay_dir.path(),
        &base_version,
        &deleted_paths,
    )
    .await
    .expect("upload_snapshot failed");

    assert!(snap.is_delta, "upload_snapshot must select delta mode for > 500 KiB repo");
    assert_eq!(snap.modified, 3, "a/b/c.rs should be modified");
    assert_eq!(snap.deleted, 1, "d.rs should be deleted via manifest");
    // e.rs is unchanged (not in overlay, not in deleted_paths) → untouched by delta.

    // ── 6. Submit and verify final state ──────────────────────────────────────
    let submit2 = client
        .post(format!("{rp}/workspaces/{ws2_id}/submit"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "summary": "delta modifications" }))
        .send()
        .await
        .unwrap();
    assert_eq!(submit2.status(), 200, "submit2: {}", submit2.text().await.unwrap_or_default());

    let dl = client
        .get(format!("{rp}/files/download"))
        .bearer_auth(admin)
        .send()
        .await
        .unwrap();
    assert_eq!(dl.status(), 200);
    let tarball = dl.bytes().await.unwrap();
    let paths = tarball_paths(&tarball);

    assert!(paths.iter().any(|p| p.contains("src/a.rs")), "a.rs must be present");
    assert!(paths.iter().any(|p| p.contains("src/b.rs")), "b.rs must be present");
    assert!(paths.iter().any(|p| p.contains("src/c.rs")), "c.rs must be present");
    assert!(!paths.iter().any(|p| p.contains("src/d.rs")), "d.rs must be deleted");
    assert!(paths.iter().any(|p| p.contains("src/e.rs")), "e.rs (unchanged) must remain");

    let a_content = extract_file_from_tarball(&tarball, "src/a.rs");
    assert_eq!(a_content.as_deref(), Some(b"fn a_v2() {}\n".as_ref()), "a.rs must have v2 content");

    shutdown_tx.send(()).ok();
}

/// Many-file delta upload: 10+ modified, 5+ deleted, 3+ new files across
/// multiple subdirectories. Verifies the server handles real-RALPH-scale
/// submit shapes without data loss.
///
/// Flow:
/// 1. Seed repo with 15 files (10 that will be modified, 5 that will be deleted)
///    + 1 unchanged sentinel.
/// 2. Build delta tarball with 10 modified + 3 new files; mark 5 as deleted.
/// 3. Upload, assert counts. Submit, download, verify final state.
#[tokio::test(flavor = "multi_thread")]
async fn test_delta_tarball_many_files() {
    let Some(url) = db_url() else { return };

    let tmp = TempDir::new().unwrap();
    let (addr, shutdown_tx) = start_for_testing_pg_multi_repo(tmp.path(), &url)
        .await
        .expect("server start");

    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let admin = "vai_admin_test";
    let sfx = unique_suffix();
    let repo_name = format!("many-delta-{sfx}");
    let rp = format!("{base}/api/repos/{repo_name}");

    // ── Create repo ───────────────────────────────────────────────────────────
    let resp = client
        .post(format!("{base}/api/repos"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "name": &repo_name }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);

    // ── Seed: 10 to-be-modified + 5 to-be-deleted + 1 unchanged ──────────────
    let seed_issue: serde_json::Value = client
        .post(format!("{rp}/issues"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "title": "seed many", "description": "", "priority": "low" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let ws1_id = client
        .post(format!("{rp}/work-queue/claim"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "issue_id": seed_issue["id"] }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap()["workspace_id"]
        .as_str()
        .unwrap()
        .to_string();

    // Build seed files: mod_N.rs (10), del_N.rs (5), unchanged.rs (1).
    let mut seed_files = Vec::new();
    for i in 0..10usize {
        seed_files.push((
            format!("src/mod_{i}.rs"),
            format!("fn mod_{i}_v1() {{}}\n"),
        ));
    }
    for i in 0..5usize {
        seed_files.push((
            format!("lib/del_{i}.rs"),
            format!("fn del_{i}() {{}}\n"),
        ));
    }
    seed_files.push(("sentinel.rs".to_string(), "fn sentinel() {}\n".to_string()));

    let files_json: Vec<serde_json::Value> = seed_files
        .iter()
        .map(|(p, c)| serde_json::json!({ "path": p, "content_base64": b64(c.as_bytes()) }))
        .collect();

    client
        .post(format!("{rp}/workspaces/{ws1_id}/files"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "files": files_json }))
        .send()
        .await
        .unwrap();

    let s = client
        .post(format!("{rp}/workspaces/{ws1_id}/submit"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "summary": "seed many files" }))
        .send()
        .await
        .unwrap();
    assert_eq!(s.status(), 200, "seed submit: {}", s.text().await.unwrap_or_default());

    // ── Second workspace: delta with 10 modified + 5 deleted + 3 new ──────────
    let work_issue: serde_json::Value = client
        .post(format!("{rp}/issues"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "title": "many-delta work", "description": "", "priority": "low" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let ws2_id = client
        .post(format!("{rp}/work-queue/claim"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "issue_id": work_issue["id"] }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap()["workspace_id"]
        .as_str()
        .unwrap()
        .to_string();

    // Build delta tarball: 10 modified (src/mod_N.rs), 3 new (extra/new_N.rs).
    let mut delta_files: Vec<(&str, Vec<u8>)> = Vec::new();

    let mod_contents: Vec<(String, Vec<u8>)> = (0..10usize)
        .map(|i| {
            (
                format!("src/mod_{i}.rs"),
                format!("fn mod_{i}_v2() {{}}\n").into_bytes(),
            )
        })
        .collect();
    for (p, c) in &mod_contents {
        delta_files.push((p.as_str(), c.clone()));
    }

    let new_contents: Vec<(String, Vec<u8>)> = (0..3usize)
        .map(|i| {
            (
                format!("extra/new_{i}.rs"),
                format!("fn new_{i}() {{}}\n").into_bytes(),
            )
        })
        .collect();
    for (p, c) in &new_contents {
        delta_files.push((p.as_str(), c.clone()));
    }

    // Deleted paths: lib/del_0.rs .. lib/del_4.rs
    let deleted: Vec<String> = (0..5usize).map(|i| format!("lib/del_{i}.rs")).collect();

    let manifest = serde_json::json!({
        "base_version": "v2",
        "deleted_paths": deleted,
    })
    .to_string();
    let manifest_bytes = manifest.as_bytes().to_vec();

    // Assemble tarball: manifest + delta files.
    let mut tb_entries: Vec<(&str, &[u8])> = vec![(".vai-delta.json", &manifest_bytes)];
    for (p, c) in &delta_files {
        tb_entries.push((p, c.as_slice()));
    }
    let delta_tb = make_tarball(&tb_entries);

    let snap_resp = client
        .post(format!("{rp}/workspaces/{ws2_id}/upload-snapshot"))
        .bearer_auth(admin)
        .header("content-type", "application/gzip")
        .body(delta_tb)
        .send()
        .await
        .unwrap();
    assert_eq!(
        snap_resp.status(),
        200,
        "upload-snapshot many-delta: {}",
        snap_resp.text().await.unwrap_or_default()
    );
    let snap: serde_json::Value = snap_resp.json().await.unwrap();
    assert_eq!(snap["is_delta"], serde_json::json!(true), "is_delta must be true");
    assert_eq!(snap["modified"], serde_json::json!(10), "10 files modified");
    assert_eq!(snap["deleted"], serde_json::json!(5), "5 files deleted via manifest");
    assert_eq!(snap["added"], serde_json::json!(3), "3 new files added");

    // ── Submit → final version ────────────────────────────────────────────────
    let submit2 = client
        .post(format!("{rp}/workspaces/{ws2_id}/submit"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "summary": "many-file delta apply" }))
        .send()
        .await
        .unwrap();
    assert_eq!(submit2.status(), 200, "submit many-delta: {}", submit2.text().await.unwrap_or_default());

    // ── Verify final state via download ───────────────────────────────────────
    let dl = client
        .get(format!("{rp}/files/download"))
        .bearer_auth(admin)
        .send()
        .await
        .unwrap();
    assert_eq!(dl.status(), 200);
    let tarball = dl.bytes().await.unwrap();
    let paths = tarball_paths(&tarball);

    // All 10 modified files present.
    for i in 0..10usize {
        assert!(
            paths.iter().any(|p| p.contains(&format!("src/mod_{i}.rs"))),
            "src/mod_{i}.rs must be present after delta apply"
        );
    }
    // 5 deleted files absent.
    for i in 0..5usize {
        assert!(
            !paths.iter().any(|p| p.contains(&format!("lib/del_{i}.rs"))),
            "lib/del_{i}.rs must be absent after deletion"
        );
    }
    // 3 new files present.
    for i in 0..3usize {
        assert!(
            paths.iter().any(|p| p.contains(&format!("extra/new_{i}.rs"))),
            "extra/new_{i}.rs must be present after delta apply"
        );
    }
    // sentinel.rs unchanged.
    assert!(
        paths.iter().any(|p| p.contains("sentinel.rs")),
        "sentinel.rs (unchanged) must remain"
    );

    // Spot-check: mod_0.rs has v2 content.
    let mod0 = extract_file_from_tarball(&tarball, "src/mod_0.rs");
    assert_eq!(
        mod0.as_deref(),
        Some(b"fn mod_0_v2() {}\n".as_ref()),
        "mod_0.rs must have v2 content"
    );

    shutdown_tx.send(()).ok();
}

/// Verifies that `POST /api/repos` returns the server-assigned `id` UUID in the
/// 201 response body (issue #302 regression guard).
///
/// The client (`vai init`) must persist this `id` as the local `repo_id` so
/// that subsequent API calls using `repo_id` in their request body (e.g.
/// `POST /api/keys`) use the correct server-side UUID.
#[tokio::test]
async fn test_create_repo_returns_server_id() {
    let Some(db_url) = db_url() else { return };
    let tmp = TempDir::new().unwrap();
    let (addr, shutdown_tx) = start_for_testing_pg_multi_repo(tmp.path(), &db_url)
        .await
        .expect("start_for_testing_pg_multi_repo failed");
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let admin = "vai_admin_test";

    let repo_name = format!("id-check-{}", unique_suffix());

    // 1. Create repo via POST /api/repos.
    let resp = client
        .post(format!("{base}/api/repos"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "name": repo_name }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create repo: {}", resp.text().await.unwrap_or_default());
    let create_body: serde_json::Value = resp.json().await.unwrap();

    // 2. The response must include the `id` field (a valid UUID string).
    let server_id_str = create_body["id"]
        .as_str()
        .expect("POST /api/repos 201 response must contain 'id' field");
    let server_id: uuid::Uuid = server_id_str
        .parse()
        .expect("'id' field must be a valid UUID");

    // 3. GET /api/repos list must contain the repo; extract its `id` and verify
    //    it matches the `id` returned by the create endpoint.
    let resp = client
        .get(format!("{base}/api/repos"))
        .bearer_auth(admin)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let repos: Vec<serde_json::Value> = resp.json().await.unwrap();
    let list_entry = repos
        .iter()
        .find(|r| r["name"].as_str() == Some(&repo_name))
        .expect("created repo must appear in GET /api/repos");
    let list_id_str = list_entry["id"]
        .as_str()
        .expect("GET /api/repos entries must contain 'id' field");
    let list_id: uuid::Uuid = list_id_str.parse().expect("list 'id' must be a valid UUID");

    assert_eq!(
        server_id, list_id,
        "id from POST /api/repos must match id from GET /api/repos for the same repo"
    );

    shutdown_tx.send(()).ok();
}

/// `vai init` must read credentials from `~/.vai/credentials.toml` when no env vars are set.
///
/// Acceptance criteria (PRD 26 V-5 / issue #301):
/// - With valid credentials.toml and NO `VAI_API_KEY`/`VAI_SERVER_URL` env vars,
///   `vai init` registers the repo on the server and writes a `[remote]` block to
///   `.vai/config.toml`.
/// - The repo is visible in `GET /api/repos`.
#[tokio::test(flavor = "multi_thread")]
async fn test_init_reads_credentials_from_file_not_env_vars() {
    let Some(db_url) = db_url() else { return };

    let tmp = TempDir::new().unwrap();
    let (addr, shutdown_tx) = start_for_testing_pg_multi_repo(tmp.path(), &db_url)
        .await
        .expect("server start failed");
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let admin = "vai_admin_test";
    let sfx = unique_suffix();

    // Create a user who will own the repo.
    let resp = client
        .post(format!("{base}/api/users"))
        .bearer_auth(admin)
        .json(&serde_json::json!({
            "name": format!("InitFileUser-{sfx}"),
            "email": format!("initfile-{sfx}@example.com"),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create user: {}", resp.text().await.unwrap_or_default());
    let user: serde_json::Value = resp.json().await.unwrap();
    let user_id = user["id"].as_str().unwrap().to_string();

    // Mint an API key for the user.
    let resp = client
        .post(format!("{base}/api/keys"))
        .bearer_auth(admin)
        .json(&serde_json::json!({
            "name": format!("init-file-key-{sfx}"),
            "for_user_id": user_id,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create key: {}", resp.text().await.unwrap_or_default());
    let key_body: serde_json::Value = resp.json().await.unwrap();
    let api_key = key_body["token"].as_str().unwrap().to_string();

    // Build the repo directory in a temp location.
    let repo_tmp = TempDir::new().unwrap();
    let repo_dir = repo_tmp.path().to_path_buf();

    // Set up credentials.toml in a separate temp HOME dir.
    let home_tmp = TempDir::new().unwrap();
    let vai_cfg_dir = home_tmp.path().join(".vai");
    std::fs::create_dir_all(&vai_cfg_dir).unwrap();
    let creds_content = format!(
        "[default]\nserver_url = \"{base}\"\napi_key = \"{api_key}\"\n"
    );
    std::fs::write(vai_cfg_dir.join("credentials.toml"), &creds_content).unwrap();

    // Capture current env state.
    let original_home = std::env::var_os("HOME");
    let original_key = std::env::var("VAI_API_KEY").ok();
    let original_url = std::env::var("VAI_SERVER_URL").ok();

    // Point HOME at our temp dir and clear the API-key env vars so that
    // run_init MUST read credentials from the file.
    std::env::set_var("HOME", home_tmp.path());
    std::env::remove_var("VAI_API_KEY");
    std::env::remove_var("VAI_SERVER_URL");

    // Derive a valid repo name (must match ^[a-zA-Z0-9][a-zA-Z0-9-_]*$).
    let repo_name = format!("init-creds-test-{sfx}");

    // run_init calls make_rt() internally which creates a new tokio Runtime.
    // That is not allowed from inside an async tokio worker, so we delegate
    // to spawn_blocking which runs on a separate thread pool.
    let result = tokio::task::spawn_blocking(move || {
        vai::cli::run_init(&repo_dir, false, true, Some(repo_name), false)
    })
    .await
    .expect("spawn_blocking panicked");

    // Restore env.
    match original_home {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
    match original_key {
        Some(v) => std::env::set_var("VAI_API_KEY", v),
        None => std::env::remove_var("VAI_API_KEY"),
    }
    match original_url {
        Some(v) => std::env::set_var("VAI_SERVER_URL", v),
        None => std::env::remove_var("VAI_SERVER_URL"),
    }

    result.expect("run_init should succeed with credentials.toml");

    // The config.toml must contain a [remote] block with the server URL.
    let config_path = repo_tmp.path().join(".vai/config.toml");
    assert!(config_path.exists(), ".vai/config.toml must exist after init");
    let config_text = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        config_text.contains("[remote]"),
        "config.toml must have [remote] block; got:\n{config_text}"
    );
    assert!(
        config_text.contains(&base),
        "config.toml must contain the server URL; got:\n{config_text}"
    );

    // The repo must be visible on the server.
    let repos_resp = client
        .get(format!("{base}/api/repos"))
        .bearer_auth(&api_key)
        .send()
        .await
        .unwrap();
    assert_eq!(repos_resp.status(), 200);
    let repos: Vec<serde_json::Value> = repos_resp.json().await.unwrap();
    assert!(
        !repos.is_empty(),
        "at least one repo should appear in GET /api/repos after init"
    );

    shutdown_tx.send(()).ok();
}

/// Verifies that a delta upload preserves files that were not in the overlay.
///
/// Scenario:
/// 1. Seed 100 files (file_000.txt … file_099.txt) into a repo via workspace submit.
/// 2. Open a new workspace and upload a delta tarball that touches only file_000.txt
///    (no deleted_paths).
/// 3. Submit → download the repo.
/// 4. Assert all 100 files are present in the downloaded tarball.
#[tokio::test(flavor = "multi_thread")]
async fn test_delta_preserves_unchanged_files() {
    let Some(url) = db_url() else { return };

    let tmp = TempDir::new().unwrap();
    let (addr, shutdown_tx) = start_for_testing_pg_multi_repo(tmp.path(), &url)
        .await
        .expect("start server");

    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let admin = "vai_admin_test";

    let repo_name = format!("delta-preserve-{}", unique_suffix());
    let resp = client
        .post(format!("{base}/api/repos"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "name": repo_name }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let repo: serde_json::Value = resp.json().await.unwrap();
    let rp = format!("{base}/api/repos/{}", repo["name"].as_str().unwrap());

    // ── Seed workspace ────────────────────────────────────────────────────────
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

    // Upload 100 files via the files endpoint.
    let files_json: Vec<serde_json::Value> = (0..100)
        .map(|i| {
            serde_json::json!({
                "path": format!("file_{i:03}.txt"),
                "content_base64": b64(format!("content of file {i}\n").as_bytes()),
            })
        })
        .collect();
    let resp = client
        .post(format!("{rp}/workspaces/{ws1_id}/files"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "files": files_json }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "upload seed files: {}", resp.text().await.unwrap_or_default());

    let resp = client
        .post(format!("{rp}/workspaces/{ws1_id}/submit"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "summary": "seed 100 files" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "submit seed: {}", resp.text().await.unwrap_or_default());
    let seed_submit: serde_json::Value = resp.json().await.unwrap();
    let seed_version = seed_submit["version"].as_str().unwrap_or("v2").to_string();

    // ── Delta workspace: modify only file_000.txt ─────────────────────────────
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

    let manifest = serde_json::json!({
        "base_version": seed_version,
        "deleted_paths": []
    })
    .to_string();
    let delta_tb = make_tarball(&[
        (".vai-delta.json", manifest.as_bytes()),
        ("file_000.txt", b"modified content\n"),
    ]);

    let resp = client
        .post(format!("{rp}/workspaces/{ws2_id}/upload-snapshot"))
        .bearer_auth(admin)
        .header("content-type", "application/gzip")
        .body(delta_tb)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "upload delta: {}", resp.text().await.unwrap_or_default());
    let snap: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(snap["is_delta"], serde_json::json!(true), "must be a delta upload");
    assert_eq!(snap["modified"], serde_json::json!(1), "only file_000 modified");
    assert_eq!(snap["deleted"], serde_json::json!(0), "no deletions");

    // ── Submit and download ───────────────────────────────────────────────────
    let resp = client
        .post(format!("{rp}/workspaces/{ws2_id}/submit"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "summary": "delta patch" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "submit delta: {}", resp.text().await.unwrap_or_default());

    let resp = client
        .get(format!("{rp}/files/download"))
        .bearer_auth(admin)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "download");
    let tarball = resp.bytes().await.unwrap();
    let paths = tarball_paths(&tarball);

    // All 100 files must be present.
    for i in 0..100 {
        let name = format!("file_{i:03}.txt");
        assert!(
            paths.iter().any(|p| p.contains(&name)),
            "{name} must be present after delta upload (got {} files)",
            paths.len()
        );
    }
    // file_000.txt must have the updated content.
    let f0 = extract_file_from_tarball(&tarball, "file_000.txt");
    assert_eq!(f0.as_deref(), Some(b"modified content\n".as_ref()), "file_000 must have new content");

    shutdown_tx.send(()).ok();
}

/// Verifies that the server-side safety rail rejects uploads that would delete
/// more than 50% of the current repository files (unless `?allow_destructive=true`).
///
/// Scenario:
/// 1. Seed 100 files into a repo.
/// 2. Upload a full-mode tarball (no `.vai-delta.json`) containing only 1 file —
///    the server would effectively delete 99 files.
/// 3. Assert the response is 409 Conflict.
/// 4. Repeat with `?allow_destructive=true` and assert 200.
#[tokio::test(flavor = "multi_thread")]
async fn test_delta_safety_rail_rejects_mass_delete() {
    let Some(url) = db_url() else { return };

    let tmp = TempDir::new().unwrap();
    let (addr, shutdown_tx) = start_for_testing_pg_multi_repo(tmp.path(), &url)
        .await
        .expect("start server");

    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let admin = "vai_admin_test";

    let repo_name = format!("delta-safety-{}", unique_suffix());
    let resp = client
        .post(format!("{base}/api/repos"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "name": repo_name }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let repo: serde_json::Value = resp.json().await.unwrap();
    let rp = format!("{base}/api/repos/{}", repo["name"].as_str().unwrap());

    // ── Seed 100 files ────────────────────────────────────────────────────────
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

    let files_json: Vec<serde_json::Value> = (0..100)
        .map(|i| {
            serde_json::json!({
                "path": format!("file_{i:03}.txt"),
                "content_base64": b64(format!("content {i}\n").as_bytes()),
            })
        })
        .collect();
    let resp = client
        .post(format!("{rp}/workspaces/{ws1_id}/files"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "files": files_json }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "upload seed files: {}", resp.text().await.unwrap_or_default());
    let resp = client
        .post(format!("{rp}/workspaces/{ws1_id}/submit"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "summary": "seed 100 files" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "submit seed: {}", resp.text().await.unwrap_or_default());

    // ── Attempt destructive upload (full-mode tarball with only 1 file) ───────
    let resp = client
        .post(format!("{rp}/issues"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "title": "wipe", "description": "", "priority": "low" }))
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

    // Full-mode tarball: only file_000.txt, no .vai-delta.json.
    // This would implicitly delete the other 99 files on the server (>50%).
    let full_tb = make_tarball(&[("file_000.txt", b"survivor\n")]);

    let resp = client
        .post(format!("{rp}/workspaces/{ws2_id}/upload-snapshot"))
        .bearer_auth(admin)
        .header("content-type", "application/gzip")
        .body(full_tb.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        409,
        "safety rail must reject mass delete; body: {}",
        resp.text().await.unwrap_or_default()
    );

    // ── Retry with allow_destructive=true → must succeed ─────────────────────
    let resp = client
        .post(format!("{rp}/workspaces/{ws2_id}/upload-snapshot?allow_destructive=true"))
        .bearer_auth(admin)
        .header("content-type", "application/gzip")
        .body(full_tb)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        200,
        "allow_destructive=true must bypass safety rail; body: {}",
        resp.text().await.unwrap_or_default()
    );

    shutdown_tx.send(()).ok();
}

/// Verifies that chained delta submits produce a coherent version history and
/// that `GET /files/download?version=<older>` reconstructs an earlier snapshot.
///
/// Scenario:
/// 1. Seed files a.txt, b.txt, c.txt → v2.
/// 2. Delta: modify a.txt → v3.
/// 3. Delta: modify b.txt, delete c.txt → v4.
/// 4. Download at latest — verify a.txt (v2 content modified), b.txt (modified), c.txt absent.
/// 5. Download at v3 — verify a.txt modified, b.txt original, c.txt present.
#[tokio::test(flavor = "multi_thread")]
async fn test_delta_preserves_chain_reconstruction() {
    let Some(url) = db_url() else { return };

    let tmp = TempDir::new().unwrap();
    let (addr, shutdown_tx) = start_for_testing_pg_multi_repo(tmp.path(), &url)
        .await
        .expect("start server");

    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let admin = "vai_admin_test";

    let repo_name = format!("delta-chain-{}", unique_suffix());
    let resp = client
        .post(format!("{base}/api/repos"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "name": repo_name }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let repo: serde_json::Value = resp.json().await.unwrap();
    let rp = format!("{base}/api/repos/{}", repo["name"].as_str().unwrap());

    // ── Seed: a.txt, b.txt, c.txt → v2 ──────────────────────────────────────
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
    let claim1: serde_json::Value = resp.json().await.unwrap();
    let ws1_id = claim1["workspace_id"].as_str().unwrap().to_string();

    let resp = client
        .post(format!("{rp}/workspaces/{ws1_id}/files"))
        .bearer_auth(admin)
        .json(&serde_json::json!({
            "files": [
                { "path": "a.txt", "content_base64": b64(b"a-v2\n") },
                { "path": "b.txt", "content_base64": b64(b"b-v2\n") },
                { "path": "c.txt", "content_base64": b64(b"c-v2\n") },
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let resp = client
        .post(format!("{rp}/workspaces/{ws1_id}/submit"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "summary": "seed" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "submit seed: {}", resp.text().await.unwrap_or_default());
    let v2_resp: serde_json::Value = resp.json().await.unwrap();
    let v2 = v2_resp["version"].as_str().unwrap_or("v2").to_string();

    // ── Delta 1: modify a.txt → v3 ────────────────────────────────────────────
    let resp = client
        .post(format!("{rp}/issues"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "title": "d1", "description": "", "priority": "low" }))
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
    let claim2: serde_json::Value = resp.json().await.unwrap();
    let ws2_id = claim2["workspace_id"].as_str().unwrap().to_string();

    let manifest2 = serde_json::json!({ "base_version": v2, "deleted_paths": [] }).to_string();
    let delta2_tb = make_tarball(&[
        (".vai-delta.json", manifest2.as_bytes()),
        ("a.txt", b"a-v3\n"),
    ]);
    let resp = client
        .post(format!("{rp}/workspaces/{ws2_id}/upload-snapshot"))
        .bearer_auth(admin)
        .header("content-type", "application/gzip")
        .body(delta2_tb)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "upload delta2: {}", resp.text().await.unwrap_or_default());

    let resp = client
        .post(format!("{rp}/workspaces/{ws2_id}/submit"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "summary": "delta1 a.txt" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "submit delta2: {}", resp.text().await.unwrap_or_default());
    let v3_resp: serde_json::Value = resp.json().await.unwrap();
    let v3 = v3_resp["version"].as_str().unwrap_or("v3").to_string();

    // ── Delta 2: modify b.txt, delete c.txt → v4 ─────────────────────────────
    let resp = client
        .post(format!("{rp}/issues"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "title": "d2", "description": "", "priority": "low" }))
        .send()
        .await
        .unwrap();
    let issue3: serde_json::Value = resp.json().await.unwrap();
    let resp = client
        .post(format!("{rp}/work-queue/claim"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "issue_id": issue3["id"] }))
        .send()
        .await
        .unwrap();
    let claim3: serde_json::Value = resp.json().await.unwrap();
    let ws3_id = claim3["workspace_id"].as_str().unwrap().to_string();

    let manifest3 = serde_json::json!({ "base_version": v3, "deleted_paths": ["c.txt"] }).to_string();
    let delta3_tb = make_tarball(&[
        (".vai-delta.json", manifest3.as_bytes()),
        ("b.txt", b"b-v4\n"),
    ]);
    let resp = client
        .post(format!("{rp}/workspaces/{ws3_id}/upload-snapshot"))
        .bearer_auth(admin)
        .header("content-type", "application/gzip")
        .body(delta3_tb)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "upload delta3: {}", resp.text().await.unwrap_or_default());

    let resp = client
        .post(format!("{rp}/workspaces/{ws3_id}/submit"))
        .bearer_auth(admin)
        .json(&serde_json::json!({ "summary": "delta2 b.txt+del c.txt" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "submit delta3: {}", resp.text().await.unwrap_or_default());

    // ── Verify latest (v4): a=v3, b=v4, c absent ─────────────────────────────
    let resp = client
        .get(format!("{rp}/files/download"))
        .bearer_auth(admin)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let latest_tb = resp.bytes().await.unwrap();
    let latest_paths = tarball_paths(&latest_tb);

    assert!(latest_paths.iter().any(|p| p.contains("a.txt")), "a.txt must be in latest");
    assert!(latest_paths.iter().any(|p| p.contains("b.txt")), "b.txt must be in latest");
    assert!(!latest_paths.iter().any(|p| p.contains("c.txt")), "c.txt must be deleted in latest");

    let a_latest = extract_file_from_tarball(&latest_tb, "a.txt");
    assert_eq!(a_latest.as_deref(), Some(b"a-v3\n".as_ref()), "a.txt must be a-v3 at latest");
    let b_latest = extract_file_from_tarball(&latest_tb, "b.txt");
    assert_eq!(b_latest.as_deref(), Some(b"b-v4\n".as_ref()), "b.txt must be b-v4 at latest");

    // ── Verify at v3: a=v3, b=v2, c present ──────────────────────────────────
    let resp = client
        .get(format!("{rp}/files/download?version={v3}"))
        .bearer_auth(admin)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "download at {v3}");
    let v3_tb = resp.bytes().await.unwrap();
    let v3_paths = tarball_paths(&v3_tb);

    assert!(v3_paths.iter().any(|p| p.contains("a.txt")), "a.txt must be in v3");
    assert!(v3_paths.iter().any(|p| p.contains("b.txt")), "b.txt must be in v3");
    assert!(v3_paths.iter().any(|p| p.contains("c.txt")), "c.txt must be in v3 (not yet deleted)");

    let a_v3 = extract_file_from_tarball(&v3_tb, "a.txt");
    assert_eq!(a_v3.as_deref(), Some(b"a-v3\n".as_ref()), "a.txt must be a-v3 at version v3");
    let b_v3 = extract_file_from_tarball(&v3_tb, "b.txt");
    assert_eq!(b_v3.as_deref(), Some(b"b-v2\n".as_ref()), "b.txt must be b-v2 at version v3");

    shutdown_tx.send(()).ok();
}

/// Extracts the content of a file from a gzip tarball by path suffix.
fn extract_file_from_tarball(bytes: &[u8], path_suffix: &str) -> Option<Vec<u8>> {
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
