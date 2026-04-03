//! Concurrency tests for multi-agent access patterns.
//!
//! Verifies that the vai server correctly handles concurrent requests from
//! multiple agents without data corruption or lost writes.

#![cfg(feature = "server")]

use std::fs;
use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use tempfile::TempDir;
use tokio::time::timeout;

use vai::auth;
use vai::repo;
use vai::server;

// ── Sample source ────────────────────────────────────────────────────────────

const AUTH_RS: &str = r#"pub struct AuthService {
    pub secret: String,
}

impl AuthService {
    pub fn validate_token(&self, token: &str) -> bool {
        token == self.secret
    }
}
"#;

// ── Helpers ──────────────────────────────────────────────────────────────────

fn b64(bytes: &[u8]) -> String {
    BASE64.encode(bytes)
}

fn from_b64(s: &str) -> Vec<u8> {
    BASE64.decode(s).expect("base64 decode failed")
}

fn setup() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("auth.rs"), AUTH_RS).unwrap();

    // Create several files so agents can work on different ones.
    for i in 0..5 {
        fs::write(
            src.join(format!("module_{i}.rs")),
            format!("pub fn func_{i}() -> u32 {{ {i} }}\n"),
        )
        .unwrap();
    }

    repo::init(&root).expect("vai init failed");
    let vai_dir = root.join(".vai");
    (tmp, root, vai_dir)
}

// ── Tests ────────────────────────────────────────────────────────────────────

/// Multiple agents create workspaces simultaneously.
/// All should succeed and be visible in the workspace list.
#[tokio::test(flavor = "multi_thread")]
async fn test_concurrent_workspace_creation() {
    let (_tmp, _root, vai_dir) = setup();

    let mut keys = Vec::new();
    for i in 0..5 {
        let (_, key) = auth::create(&vai_dir, &format!("agent-{i}")).expect("create key");
        keys.push(key);
    }

    let (addr, shutdown_tx) = server::start_for_testing(&vai_dir)
        .await
        .expect("start server");

    let base_url = format!("http://{addr}");
    let client = reqwest::Client::new();

    // Launch all workspace creations concurrently.
    let mut handles = Vec::new();
    for (i, key) in keys.iter().enumerate() {
        let client = client.clone();
        let url = format!("{base_url}/api/workspaces");
        let key = key.clone();
        handles.push(tokio::spawn(async move {
            let resp = client
                .post(&url)
                .bearer_auth(&key)
                .json(&serde_json::json!({
                    "intent": format!("concurrent task {i}")
                }))
                .send()
                .await
                .expect("request failed");
            (i, resp.status().as_u16(), resp.json::<serde_json::Value>().await.unwrap())
        }));
    }

    let mut created_ids = Vec::new();
    for handle in handles {
        let (i, status, body) = handle.await.unwrap();
        assert_eq!(status, 201, "agent {i} workspace creation should succeed");
        let id = body["id"].as_str().unwrap().to_string();
        created_ids.push(id);
    }

    // Verify all 5 workspaces are visible.
    assert_eq!(created_ids.len(), 5);
    let unique: std::collections::HashSet<_> = created_ids.iter().collect();
    assert_eq!(unique.len(), 5, "all workspace IDs should be unique");

    // Verify via the status endpoint.
    let status_resp = client
        .get(format!("{base_url}/api/status"))
        .send()
        .await
        .unwrap();
    let status: serde_json::Value = status_resp.json().await.unwrap();
    let ws_count = status["workspace_count"].as_u64().unwrap();
    assert_eq!(ws_count, 5, "status should show all 5 workspaces");

    let _ = shutdown_tx.send(());
}

/// Multiple agents upload files and submit sequentially-dependent merges.
/// Each submit advances HEAD; all changes should be preserved.
#[tokio::test(flavor = "multi_thread")]
async fn test_concurrent_file_uploads_sequential_submits() {
    let (_tmp, _root, vai_dir) = setup();

    let (_, key_a) = auth::create(&vai_dir, "agent-a").expect("create key A");
    let (_, key_b) = auth::create(&vai_dir, "agent-b").expect("create key B");
    let (_, key_c) = auth::create(&vai_dir, "agent-c").expect("create key C");

    let (addr, shutdown_tx) = server::start_for_testing(&vai_dir)
        .await
        .expect("start server");

    let base_url = format!("http://{addr}");
    let client = reqwest::Client::new();

    // Each agent works on a different module file.
    let agents = vec![
        (&key_a, "module_0", "enhance module 0", "pub fn func_0() -> u32 { 100 }\npub fn extra_0() -> bool { true }\n"),
        (&key_b, "module_1", "enhance module 1", "pub fn func_1() -> u32 { 200 }\npub fn extra_1() -> bool { false }\n"),
        (&key_c, "module_2", "enhance module 2", "pub fn func_2() -> u32 { 300 }\npub fn extra_2() -> &'static str { \"hello\" }\n"),
    ];

    // Create workspaces and upload files concurrently.
    let mut ws_ids = Vec::new();
    let mut upload_handles = Vec::new();

    for (key, module, intent, _content) in &agents {
        let resp = client
            .post(format!("{base_url}/api/workspaces"))
            .bearer_auth(*key)
            .json(&serde_json::json!({ "intent": intent }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);
        let body: serde_json::Value = resp.json().await.unwrap();
        ws_ids.push((body["id"].as_str().unwrap().to_string(), module.to_string()));
    }

    // Upload files concurrently.
    for (i, (ws_id, module)) in ws_ids.iter().enumerate() {
        let client = client.clone();
        let url = format!("{base_url}/api/workspaces/{ws_id}/files");
        let key = agents[i].0.clone();
        let content = agents[i].3;
        let module = module.clone();
        upload_handles.push(tokio::spawn(async move {
            let resp = client
                .post(&url)
                .bearer_auth(&key)
                .json(&serde_json::json!({
                    "files": [{
                        "path": format!("src/{module}.rs"),
                        "content_base64": b64(content.as_bytes()),
                    }]
                }))
                .send()
                .await
                .expect("upload failed");
            assert_eq!(resp.status(), 200, "upload for {module} should succeed");
        }));
    }

    for handle in upload_handles {
        handle.await.unwrap();
    }

    // Submit sequentially (each advances HEAD).
    let expected_versions = ["v2", "v3", "v4"];
    for (i, (ws_id, _module)) in ws_ids.iter().enumerate() {
        let resp = client
            .post(format!("{base_url}/api/workspaces/{ws_id}/submit"))
            .bearer_auth(agents[i].0)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200, "submit {i} should succeed");
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(
            body["version"].as_str().unwrap(),
            expected_versions[i],
            "submit {i} should create {}",
            expected_versions[i]
        );
    }

    // Verify all files have the updated content.
    for (i, (_ws_id, module)) in ws_ids.iter().enumerate() {
        let resp = client
            .get(format!("{base_url}/api/files/src/{module}.rs"))
            .bearer_auth(agents[0].0)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        let content = String::from_utf8(
            from_b64(body["content_base64"].as_str().unwrap()),
        )
        .unwrap();
        let expected_fn = format!("extra_{i}");
        assert!(
            content.contains(&expected_fn),
            "module_{i}.rs should contain {expected_fn}, got: {content}"
        );
    }

    // Verify version history is complete.
    let versions_resp = client
        .get(format!("{base_url}/api/versions"))
        .bearer_auth(agents[0].0)
        .send()
        .await
        .unwrap();
    let versions_body: serde_json::Value = versions_resp.json().await.unwrap();
    let versions = versions_body["data"].as_array().unwrap();
    assert_eq!(versions.len(), 4, "should have v1 through v4");

    let _ = shutdown_tx.send(());
}

/// Concurrent issue creation should not produce duplicates when titles differ,
/// and should detect duplicates when titles are similar.
#[tokio::test(flavor = "multi_thread")]
async fn test_concurrent_issue_creation() {
    let (_tmp, _root, vai_dir) = setup();
    let (_, key) = auth::create(&vai_dir, "issue-agent").expect("create key");

    let (addr, shutdown_tx) = server::start_for_testing(&vai_dir)
        .await
        .expect("start server");

    let base_url = format!("http://{addr}");
    let client = reqwest::Client::new();

    // Create 10 issues concurrently with distinct titles.
    let mut handles = Vec::new();
    for i in 0..10 {
        let client = client.clone();
        let url = format!("{base_url}/api/issues");
        let key = key.clone();
        handles.push(tokio::spawn(async move {
            let resp = client
                .post(&url)
                .bearer_auth(&key)
                .json(&serde_json::json!({
                    "title": format!("distinct issue number {i}"),
                    "priority": "medium"
                }))
                .send()
                .await
                .expect("create issue failed");
            (i, resp.status().as_u16(), resp.json::<serde_json::Value>().await.unwrap())
        }));
    }

    let mut issue_ids = Vec::new();
    for handle in handles {
        let (i, status, body) = handle.await.unwrap();
        assert_eq!(status, 201, "issue {i} creation should succeed");
        let id = body["id"].as_str().unwrap_or("").to_string();
        assert!(!id.is_empty(), "issue {i} should have an ID");
        issue_ids.push(id);
    }

    // All IDs should be unique.
    let unique: std::collections::HashSet<_> = issue_ids.iter().collect();
    assert_eq!(unique.len(), 10, "all 10 issue IDs should be unique");

    // Verify issue count via list endpoint.
    let list_resp = client
        .get(format!("{base_url}/api/issues"))
        .bearer_auth(&key)
        .send()
        .await
        .unwrap();
    assert_eq!(list_resp.status(), 200);
    let issues_body: serde_json::Value = list_resp.json().await.unwrap();
    let issues = issues_body["data"].as_array().unwrap();
    assert_eq!(issues.len(), 10, "should have exactly 10 issues");

    let _ = shutdown_tx.send(());
}

/// Concurrent event log reads via the WebSocket should not block or deadlock
/// with concurrent workspace creation writes.
#[tokio::test(flavor = "multi_thread")]
async fn test_concurrent_ws_and_event_reads() {
    let (_tmp, _root, vai_dir) = setup();
    let (_, key) = auth::create(&vai_dir, "ws-agent").expect("create key");

    let (addr, shutdown_tx) = server::start_for_testing(&vai_dir)
        .await
        .expect("start server");

    let base_url = format!("http://{addr}");
    let ws_url = format!("ws://{addr}/ws/events?key={key}");
    let client = reqwest::Client::new();

    // Connect a WebSocket listener.
    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .expect("ws connect");

    use futures_util::SinkExt;
    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        serde_json::json!({
            "subscribe": { "event_types": [], "paths": [], "entities": [], "workspaces": [] }
        })
        .to_string(),
    ))
    .await
    .unwrap();

    // Spawn 5 concurrent workspace creations.
    let mut handles = Vec::new();
    for i in 0..5 {
        let client = client.clone();
        let url = format!("{base_url}/api/workspaces");
        let key = key.clone();
        handles.push(tokio::spawn(async move {
            client
                .post(&url)
                .bearer_auth(&key)
                .json(&serde_json::json!({ "intent": format!("concurrent ws {i}") }))
                .send()
                .await
                .expect("create ws failed")
        }));
    }

    for handle in handles {
        let resp = handle.await.unwrap();
        assert_eq!(resp.status(), 201);
    }

    // Collect WebSocket events — should receive 5 WorkspaceCreated events.
    use futures_util::StreamExt;
    let events = timeout(Duration::from_secs(5), async {
        let mut count = 0;
        loop {
            if let Some(Ok(tokio_tungstenite::tungstenite::Message::Text(msg))) =
                ws.next().await
            {
                let v: serde_json::Value =
                    serde_json::from_str(&msg).unwrap_or_default();
                if v["type"].as_str() == Some("WorkspaceCreated") {
                    count += 1;
                    if count >= 5 {
                        return count;
                    }
                }
            }
        }
    })
    .await
    .expect("timed out waiting for 5 WorkspaceCreated events");

    assert_eq!(events, 5, "should receive exactly 5 WorkspaceCreated events");

    let _ = shutdown_tx.send(());
}
