//! End-to-end integration test for the multi-agent coordination workflow.
//!
//! Exercises the full multi-agent lifecycle:
//! `vai init` → server start → two agents create workspaces → overlap detected
//! → Agent A submits (v2) → Agent B submits with semantic merge (v3) → audit trail verified.

#![cfg(feature = "server")]

use std::fs;
use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use futures_util::{SinkExt, StreamExt};
use tempfile::TempDir;
use tokio::time::timeout;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

use vai::auth;
use vai::repo;
use vai::server;

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

// ── Helper ────────────────────────────────────────────────────────────────────

/// Encodes `bytes` as a base64 string.
fn b64(bytes: &[u8]) -> String {
    BASE64.encode(bytes)
}

/// Decodes a base64 string, panicking on error (test helper).
fn from_b64(s: &str) -> Vec<u8> {
    BASE64.decode(s).expect("base64 decode failed")
}

/// Sets up a temporary directory with sample Rust source files and runs `vai init`.
/// Returns `(TempDir, root_path, vai_dir_path)`.
fn setup_server_repo() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("auth.rs"), AUTH_RS).unwrap();
    fs::write(src.join("config.rs"), CONFIG_RS).unwrap();

    repo::init(&root).expect("vai init failed");
    let vai_dir = root.join(".vai");
    (tmp, root, vai_dir)
}

// ── Multi-agent coordination test ────────────────────────────────────────────

/// Full multi-agent coordination workflow:
///
/// 1. `vai init` on a repo with sample Rust files.
/// 2. Create API keys for two agents (A and B).
/// 3. Start an embedded server.
/// 4. Agent A subscribes to WebSocket events.
/// 5. Both agents create workspaces with different intents.
/// 6. Agent A uploads changes to `auth.rs` (adds `generate_token`).
/// 7. Agent B uploads overlapping changes to `auth.rs` (adds `audit_login`).
///    → Conflict engine detects file-level overlap and broadcasts `OverlapDetected`.
/// 8. Agent A receives the `OverlapDetected` event via WebSocket.
/// 9. Agent A submits → server fast-forwards to v2.
/// 10. Agent B submits → server performs semantic merge → v3.
/// 11. Verify v3's `auth.rs` contains both agents' additions.
/// 12. Verify version history contains all three versions (v1, v2, v3).
/// 13. Verify the event log audit trail via the `/api/versions` endpoint.
#[tokio::test(flavor = "multi_thread")]
async fn test_multi_agent_coordination() {
    // ── 1. Server repo setup ─────────────────────────────────────────────────
    let (_tmp, _root, vai_dir) = setup_server_repo();

    // ── 2. Create API keys ───────────────────────────────────────────────────
    let (_, key_a) = auth::create(&vai_dir, "agent-a").expect("create key A");
    let (_, key_b) = auth::create(&vai_dir, "agent-b").expect("create key B");

    // ── 3. Start embedded server ─────────────────────────────────────────────
    let (addr, shutdown_tx) = server::start_for_testing(&vai_dir)
        .await
        .expect("start_for_testing failed");

    let base_url = format!("http://{addr}");
    let ws_url = format!("ws://{addr}/ws/events");
    let client = reqwest::Client::new();

    // ── 4. Agent A subscribes to WebSocket events ────────────────────────────
    let (mut ws_a, _) = connect_async(format!("{ws_url}?key={key_a}"))
        .await
        .expect("WebSocket connect failed");

    // Send subscription filter — receive all event types.
    let filter = serde_json::json!({
        "subscribe": {
            "event_types": [],
            "paths": [],
            "entities": [],
            "workspaces": []
        }
    });
    ws_a.send(Message::Text(filter.to_string()))
        .await
        .expect("subscribe send failed");

    // ── 5. Both agents create workspaces ─────────────────────────────────────
    let resp_a = client
        .post(format!("{base_url}/api/workspaces"))
        .bearer_auth(&key_a)
        .json(&serde_json::json!({ "intent": "add token generation to auth module" }))
        .send()
        .await
        .expect("create workspace A");
    assert_eq!(resp_a.status(), 201, "create workspace A should succeed");
    let ws_a_meta: serde_json::Value = resp_a.json().await.unwrap();
    let ws_a_id = ws_a_meta["id"].as_str().unwrap().to_string();

    let resp_b = client
        .post(format!("{base_url}/api/workspaces"))
        .bearer_auth(&key_b)
        .json(&serde_json::json!({ "intent": "add audit logging to auth module" }))
        .send()
        .await
        .expect("create workspace B");
    assert_eq!(resp_b.status(), 201, "create workspace B should succeed");
    let ws_b_meta: serde_json::Value = resp_b.json().await.unwrap();
    let ws_b_id = ws_b_meta["id"].as_str().unwrap().to_string();

    // ── 6. Agent A uploads changes to auth.rs ────────────────────────────────
    // Adds `generate_token` function at the end of the file.
    let auth_a = format!(
        "{AUTH_RS}\n/// Generates a new token.\npub fn generate_token(secret: &str) -> String {{\n    secret.to_uppercase()\n}}\n"
    );
    let upload_a = client
        .post(format!("{base_url}/api/workspaces/{ws_a_id}/files"))
        .bearer_auth(&key_a)
        .json(&serde_json::json!({
            "files": [{ "path": "src/auth.rs", "content_base64": b64(auth_a.as_bytes()) }]
        }))
        .send()
        .await
        .expect("upload files A");
    assert_eq!(upload_a.status(), 200, "upload A should succeed");

    // ── 7. Agent B uploads overlapping changes to auth.rs ────────────────────
    // Adds `audit_login` function — same file, different entity.
    let auth_b = format!(
        "{AUTH_RS}\n/// Audits a login attempt.\npub fn audit_login(user: &str) {{\n    println!(\"Login attempt: {{}}\", user);\n}}\n"
    );
    let upload_b = client
        .post(format!("{base_url}/api/workspaces/{ws_b_id}/files"))
        .bearer_auth(&key_b)
        .json(&serde_json::json!({
            "files": [{ "path": "src/auth.rs", "content_base64": b64(auth_b.as_bytes()) }]
        }))
        .send()
        .await
        .expect("upload files B");
    assert_eq!(upload_b.status(), 200, "upload B should succeed");

    // ── 8. Agent A receives OverlapDetected via WebSocket ────────────────────
    // Both workspaces now touch `src/auth.rs` → conflict engine fires.
    let overlap_event = timeout(Duration::from_secs(10), async {
        loop {
            match ws_a.next().await {
                Some(Ok(Message::Text(msg))) => {
                    let v: serde_json::Value = serde_json::from_str(&msg).unwrap_or_default();
                    if v["type"].as_str() == Some("OverlapDetected") {
                        return v;
                    }
                    // Skip unrelated messages (e.g. WorkspaceCreated, FilesUploaded).
                }
                Some(Ok(_)) => {} // binary / ping frames — skip
                Some(Err(e)) => panic!("WebSocket error: {e}"),
                None => panic!("WebSocket stream ended before OverlapDetected"),
            }
        }
    })
    .await
    .expect("timed out waiting for OverlapDetected event");

    assert_eq!(
        overlap_event["type"].as_str(),
        Some("OverlapDetected"),
        "Agent A should receive OverlapDetected"
    );

    // The overlap should mention auth.rs.
    let overlapping_files = overlap_event["data"]["overlapping_files"]
        .as_array()
        .expect("overlapping_files should be an array");
    assert!(
        overlapping_files
            .iter()
            .any(|f| f.as_str().unwrap_or("").contains("auth.rs")),
        "overlapping_files should include src/auth.rs"
    );

    // ── 9. Agent A submits → v2 (fast-forward) ───────────────────────────────
    let submit_a = client
        .post(format!("{base_url}/api/workspaces/{ws_a_id}/submit"))
        .bearer_auth(&key_a)
        .send()
        .await
        .expect("submit workspace A");
    assert_eq!(submit_a.status(), 200, "Agent A submit should succeed");
    let result_a: serde_json::Value = submit_a.json().await.unwrap();
    assert_eq!(
        result_a["version"].as_str(),
        Some("v2"),
        "Agent A submit should create v2"
    );

    // Server HEAD is now v2.
    let status_resp = client
        .get(format!("{base_url}/api/status"))
        .send()
        .await
        .unwrap();
    let status: serde_json::Value = status_resp.json().await.unwrap();
    assert_eq!(status["head_version"], "v2");

    // ── 10. Agent B submits → v3 (semantic merge) ─────────────────────────────
    // Agent B's workspace is based on v1; HEAD is now v2.
    // The server's merge engine should auto-resolve: A added `generate_token`,
    // B added `audit_login` — different entities in the same file.
    let submit_b = client
        .post(format!("{base_url}/api/workspaces/{ws_b_id}/submit"))
        .bearer_auth(&key_b)
        .send()
        .await
        .expect("submit workspace B");
    assert_eq!(
        submit_b.status(),
        200,
        "Agent B submit should succeed with semantic merge"
    );
    let result_b: serde_json::Value = submit_b.json().await.unwrap();
    assert_eq!(
        result_b["version"].as_str(),
        Some("v3"),
        "Agent B submit should create v3"
    );

    // ── 11. Verify final auth.rs contains both agents' changes ────────────────
    let file_resp = client
        .get(format!("{base_url}/api/files/src/auth.rs"))
        .bearer_auth(&key_a)
        .send()
        .await
        .expect("get final auth.rs");
    assert_eq!(file_resp.status(), 200);
    let file_body: serde_json::Value = file_resp.json().await.unwrap();
    let content_b64 = file_body["content_base64"].as_str().expect("content_base64 missing");
    let final_content =
        String::from_utf8(from_b64(content_b64)).expect("auth.rs is not valid UTF-8");

    assert!(
        final_content.contains("generate_token"),
        "final auth.rs should contain Agent A's generate_token function"
    );
    assert!(
        final_content.contains("audit_login"),
        "final auth.rs should contain Agent B's audit_login function"
    );

    // ── 12. Verify version history ────────────────────────────────────────────
    let versions_resp = client
        .get(format!("{base_url}/api/versions"))
        .bearer_auth(&key_a)
        .send()
        .await
        .unwrap();
    let versions_body: serde_json::Value = versions_resp.json().await.unwrap();
    let version_list = versions_body["data"].as_array().unwrap();

    assert_eq!(version_list.len(), 3, "should have v1, v2, and v3");
    assert_eq!(version_list[0]["version_id"], "v1");
    assert_eq!(version_list[1]["version_id"], "v2");
    assert_eq!(version_list[1]["intent"], "add token generation to auth module");
    assert_eq!(version_list[2]["version_id"], "v3");
    assert_eq!(version_list[2]["intent"], "add audit logging to auth module");

    // ── 13. Audit trail: verify event log has full history ───────────────────
    // The v2 and v3 version detail endpoints should both report file changes.
    let v2_resp = client
        .get(format!("{base_url}/api/versions/v2"))
        .bearer_auth(&key_a)
        .send()
        .await
        .unwrap();
    assert_eq!(v2_resp.status(), 200);
    let v2_detail: serde_json::Value = v2_resp.json().await.unwrap();
    let v2_files = v2_detail["file_changes"].as_array()
        .expect("v2 should have file_changes array");
    assert!(
        v2_files.iter().any(|f| {
            let path = f["path"].as_str().unwrap_or("");
            path.contains("auth.rs")
        }),
        "v2 file changes should include auth.rs, got: {v2_files:?}"
    );

    let v3_resp = client
        .get(format!("{base_url}/api/versions/v3"))
        .bearer_auth(&key_a)
        .send()
        .await
        .unwrap();
    assert_eq!(v3_resp.status(), 200);
    let v3_detail: serde_json::Value = v3_resp.json().await.unwrap();
    let v3_files = v3_detail["file_changes"].as_array()
        .expect("v3 should have file_changes array");
    assert!(
        v3_files.iter().any(|f| {
            let path = f["path"].as_str().unwrap_or("");
            path.contains("auth.rs")
        }),
        "v3 file changes should include auth.rs, got: {v3_files:?}"
    );

    // ── Cleanup ───────────────────────────────────────────────────────────────
    ws_a.close(None).await.ok();
    shutdown_tx.send(()).ok();
}

// ── Event buffer replay test ─────────────────────────────────────────────────

/// Verifies that an agent that disconnects and reconnects with `?last_event_id=N`
/// receives the events it missed during the disconnect.
#[tokio::test(flavor = "multi_thread")]
async fn test_event_buffer_replay_on_reconnect() {
    let (_tmp, _root, vai_dir) = setup_server_repo();
    let (_, key) = auth::create(&vai_dir, "replay-agent").expect("create key");

    let (addr, shutdown_tx) = server::start_for_testing(&vai_dir)
        .await
        .expect("start_for_testing failed");

    let base_url = format!("http://{addr}");
    let ws_url = format!("ws://{addr}/ws/events");
    let client = reqwest::Client::new();

    // Connect, subscribe, and note the last event ID (from the WorkspaceCreated event).
    let (mut ws, _) = connect_async(format!("{ws_url}?key={key}")).await.unwrap();
    ws.send(Message::Text(
        serde_json::json!({ "subscribe": { "event_types": [], "paths": [], "entities": [], "workspaces": [] } })
            .to_string(),
    ))
    .await
    .unwrap();

    // Create a workspace to generate an event.
    let resp = client
        .post(format!("{base_url}/api/workspaces"))
        .bearer_auth(&key)
        .json(&serde_json::json!({ "intent": "test replay" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);

    // Receive the WorkspaceCreated event and record its event_id.
    let event_id = timeout(Duration::from_secs(5), async {
        loop {
            if let Some(Ok(Message::Text(msg))) = ws.next().await {
                let v: serde_json::Value = serde_json::from_str(&msg).unwrap_or_default();
                if v["type"].as_str() == Some("WorkspaceCreated") {
                    return v["event_id"].as_u64().unwrap_or(0);
                }
            }
        }
    })
    .await
    .expect("timed out waiting for WorkspaceCreated");

    assert!(event_id > 0, "event_id should be positive");

    // Disconnect.
    ws.close(None).await.ok();

    // Generate another event while disconnected.
    let resp2 = client
        .post(format!("{base_url}/api/workspaces"))
        .bearer_auth(&key)
        .json(&serde_json::json!({ "intent": "second workspace during disconnect" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp2.status(), 201);

    // Reconnect with `?last_event_id=N` to replay missed events.
    let (mut ws2, _) =
        connect_async(format!("{ws_url}?key={key}&last_event_id={event_id}"))
            .await
            .unwrap();
    ws2.send(Message::Text(
        serde_json::json!({ "subscribe": { "event_types": [], "paths": [], "entities": [], "workspaces": [] } })
            .to_string(),
    ))
    .await
    .unwrap();

    // The replayed stream should contain the event created during the disconnect.
    let replayed_event = timeout(Duration::from_secs(5), async {
        loop {
            if let Some(Ok(Message::Text(msg))) = ws2.next().await {
                let v: serde_json::Value = serde_json::from_str(&msg).unwrap_or_default();
                if v["type"].as_str() == Some("WorkspaceCreated") {
                    let replayed_id = v["event_id"].as_u64().unwrap_or(0);
                    if replayed_id > event_id {
                        return v;
                    }
                }
            }
        }
    })
    .await
    .expect("timed out waiting for replayed event");

    assert_eq!(
        replayed_event["type"].as_str(),
        Some("WorkspaceCreated"),
        "replayed event should be WorkspaceCreated"
    );
    let replayed_id = replayed_event["event_id"].as_u64().unwrap();
    assert!(
        replayed_id > event_id,
        "replayed event ID ({replayed_id}) should be greater than the last seen event ID ({event_id})"
    );
    assert!(
        replayed_event["data"]["intent"].as_str().unwrap().contains("second workspace"),
        "replayed event should contain the intent from the second workspace"
    );

    ws2.close(None).await.ok();
    shutdown_tx.send(()).ok();
}

// ── Concurrent non-conflicting workspaces test ───────────────────────────────

/// Two agents work on different files concurrently and both submit successfully
/// without semantic conflicts.
#[tokio::test(flavor = "multi_thread")]
async fn test_concurrent_non_overlapping_workspaces() {
    let (_tmp, _root, vai_dir) = setup_server_repo();
    let (_, key_a) = auth::create(&vai_dir, "nc-agent-a").expect("create key A");
    let (_, key_b) = auth::create(&vai_dir, "nc-agent-b").expect("create key B");

    let (addr, shutdown_tx) = server::start_for_testing(&vai_dir)
        .await
        .expect("start failed");

    let base_url = format!("http://{addr}");
    let client = reqwest::Client::new();

    // Both agents create workspaces.
    let resp_a = client
        .post(format!("{base_url}/api/workspaces"))
        .bearer_auth(&key_a)
        .json(&serde_json::json!({ "intent": "enhance auth module" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp_a.status(), 201);
    let ws_a_id = resp_a.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let resp_b = client
        .post(format!("{base_url}/api/workspaces"))
        .bearer_auth(&key_b)
        .json(&serde_json::json!({ "intent": "enhance config module" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp_b.status(), 201);
    let ws_b_id = resp_b.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Agent A modifies auth.rs, Agent B modifies config.rs — no overlap.
    let auth_enhanced = format!(
        "{AUTH_RS}\npub fn hash_token(t: &str) -> u64 {{\n    t.len() as u64\n}}\n"
    );
    client
        .post(format!("{base_url}/api/workspaces/{ws_a_id}/files"))
        .bearer_auth(&key_a)
        .json(&serde_json::json!({
            "files": [{ "path": "src/auth.rs", "content_base64": b64(auth_enhanced.as_bytes()) }]
        }))
        .send()
        .await
        .unwrap();

    let config_enhanced = format!(
        "{CONFIG_RS}\npub fn is_valid(cfg: &Config) -> bool {{\n    !cfg.name.is_empty()\n}}\n"
    );
    client
        .post(format!("{base_url}/api/workspaces/{ws_b_id}/files"))
        .bearer_auth(&key_b)
        .json(&serde_json::json!({
            "files": [{ "path": "src/config.rs", "content_base64": b64(config_enhanced.as_bytes()) }]
        }))
        .send()
        .await
        .unwrap();

    // Agent A submits → v2.
    let s_a = client
        .post(format!("{base_url}/api/workspaces/{ws_a_id}/submit"))
        .bearer_auth(&key_a)
        .send()
        .await
        .unwrap();
    assert_eq!(s_a.status(), 200);
    assert_eq!(
        s_a.json::<serde_json::Value>().await.unwrap()["version"],
        "v2"
    );

    // Agent B submits → v3 (config.rs not touched by A, so no merge needed).
    let s_b = client
        .post(format!("{base_url}/api/workspaces/{ws_b_id}/submit"))
        .bearer_auth(&key_b)
        .send()
        .await
        .unwrap();
    assert_eq!(s_b.status(), 200);
    assert_eq!(
        s_b.json::<serde_json::Value>().await.unwrap()["version"],
        "v3"
    );

    // Both changes visible in their respective files.
    let auth_resp = client
        .get(format!("{base_url}/api/files/src/auth.rs"))
        .bearer_auth(&key_a)
        .send()
        .await
        .unwrap();
    let auth_content = String::from_utf8(
        from_b64(
            auth_resp
                .json::<serde_json::Value>()
                .await
                .unwrap()["content_base64"]
                .as_str()
                .unwrap(),
        ),
    )
    .unwrap();
    assert!(auth_content.contains("hash_token"), "auth.rs should have hash_token");

    let cfg_resp = client
        .get(format!("{base_url}/api/files/src/config.rs"))
        .bearer_auth(&key_a)
        .send()
        .await
        .unwrap();
    let cfg_content = String::from_utf8(
        from_b64(
            cfg_resp
                .json::<serde_json::Value>()
                .await
                .unwrap()["content_base64"]
                .as_str()
                .unwrap(),
        ),
    )
    .unwrap();
    assert!(cfg_content.contains("is_valid"), "config.rs should have is_valid");

    shutdown_tx.send(()).ok();
}
