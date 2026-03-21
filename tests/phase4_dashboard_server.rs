//! Integration test for the TUI dashboard in server mode.
//!
//! Verifies that:
//! 1. `parse_server_url` converts `vai://` URLs correctly.
//! 2. A WebSocket client can connect to a running vai server and receive
//!    `WorkspaceCreated` events in real time.
//! 3. After workspace creation via the REST API the dashboard snapshot
//!    reflects the new workspace.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tempfile::TempDir;
use tokio_tungstenite::tungstenite::Message;

use vai::auth;
use vai::dashboard;
use vai::repo;
use vai::server;
use vai::workspace;

// ── Sample source ─────────────────────────────────────────────────────────────

const AUTH_RS: &str = r#"pub struct AuthService { pub secret: String }
impl AuthService {
    pub fn validate(&self, token: &str) -> bool { token == self.secret }
}
"#;

// ── Helper ────────────────────────────────────────────────────────────────────

fn setup() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("auth.rs"), AUTH_RS).unwrap();
    repo::init(&root).expect("vai init");
    let vai_dir = root.join(".vai");
    (tmp, root, vai_dir)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[test]
fn test_parse_server_url() {
    let ws =
        dashboard::parse_server_url("vai://localhost:7865", "mykey").expect("parse");
    assert_eq!(ws, "ws://localhost:7865/ws/events?key=mykey");
}

#[test]
fn test_parse_server_url_invalid() {
    let err = dashboard::parse_server_url("http://localhost:7865", "k");
    assert!(err.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn test_dashboard_server_receives_workspace_created_event() {
    // ── Start server ──────────────────────────────────────────────────────────
    let (_tmp, _root, vai_dir) = setup();
    let (_, api_key) = auth::create(&vai_dir, "test-agent").expect("create key");
    let (addr, shutdown_tx) = server::start_for_testing(&vai_dir)
        .await
        .expect("start server");

    let base_http = format!("http://{addr}");
    let ws_url = format!("ws://{addr}/ws/events?key={api_key}");

    // ── Connect WebSocket client ───────────────────────────────────────────────
    let (ws_stream, _) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .expect("ws connect");
    let (mut ws_tx, mut ws_rx) = ws_stream.split();

    // The server requires a subscribe message before it forwards events.
    // Send an empty filter (match all) immediately after connecting.
    ws_tx
        .send(Message::Text(
            serde_json::json!({
                "subscribe": {
                    "entities": [],
                    "paths": [],
                    "event_types": [],
                    "workspaces": []
                }
            })
            .to_string(),
        ))
        .await
        .expect("send subscribe");

    // ── Create workspace via REST API ─────────────────────────────────────────
    let http = reqwest::Client::new();
    let resp = http
        .post(format!("{base_http}/api/workspaces"))
        .header("Authorization", format!("Bearer {api_key}"))
        .json(&serde_json::json!({ "intent": "dashboard server test workspace" }))
        .send()
        .await
        .expect("POST /api/workspaces");
    assert!(resp.status().is_success(), "create workspace: {}", resp.status());

    // ── Assert WorkspaceCreated event arrives within 2 seconds ────────────────
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    let mut got_event = false;

    while tokio::time::Instant::now() < deadline {
        let timeout = tokio::time::timeout(
            Duration::from_millis(200),
            ws_rx.next(),
        );
        match timeout.await {
            Ok(Some(Ok(Message::Text(text)))) => {
                let v: serde_json::Value =
                    serde_json::from_str(&text).unwrap_or_default();
                if v["type"] == "WorkspaceCreated" {
                    got_event = true;
                    break;
                }
            }
            _ => continue,
        }
    }

    assert!(got_event, "expected WorkspaceCreated event over WebSocket");

    // ── Dashboard snapshot shows the new workspace ────────────────────────────
    let snap = dashboard::snapshot(&vai_dir).expect("snapshot");
    assert!(
        snap.workspaces
            .iter()
            .any(|w| w.intent.contains("dashboard server test workspace")),
        "snapshot should include the new workspace"
    );

    // ── Shutdown ──────────────────────────────────────────────────────────────
    let _ = shutdown_tx.send(());

    // Verify parse_server_url produces the right URL for this server.
    let expected_ws = format!("ws://{addr}/ws/events?key={api_key}");
    let parsed =
        dashboard::parse_server_url(&format!("vai://{addr}"), &api_key).expect("parse");
    assert_eq!(parsed, expected_ws);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_dashboard_snapshot_after_workspace_create() {
    // Verify that dashboard snapshot (the headless read path used by the TUI
    // refresh loop) reflects workspaces created locally without any server.
    let (_tmp, _root, vai_dir) = setup();

    let head = repo::read_head(&vai_dir).expect("read HEAD");
    let result =
        workspace::create(&vai_dir, "local snapshot test", &head).expect("create workspace");

    let snap = dashboard::snapshot(&vai_dir).expect("snapshot");
    assert!(
        snap.workspaces
            .iter()
            .any(|w| w.id == result.workspace.id),
        "snapshot should list the new workspace"
    );
}
