//! Integration tests for server-side input validation.
//!
//! Verifies that the API rejects malformed, oversized, or dangerous inputs
//! with appropriate 400 Bad Request responses.

#![cfg(feature = "server")]

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use tempfile::TempDir;

use vai::auth;
use vai::repo;
use vai::server;

// ── Setup ─────────────────────────────────────────────────────────────────────

fn setup() -> (TempDir, std::path::PathBuf) {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    repo::init(&root).expect("vai init");
    let vai_dir = root.join(".vai");
    (tmp, vai_dir)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Issue title exceeds 500 characters → 400.
#[tokio::test(flavor = "multi_thread")]
async fn test_create_issue_title_too_long() {
    let (_tmp, vai_dir) = setup();
    let (_, api_key) = auth::create(&vai_dir, "agent").unwrap();
    let (addr, _tx) = server::start_for_testing(&vai_dir).await.unwrap();

    let client = reqwest::Client::new();
    let long_title = "x".repeat(501);
    let resp = client
        .post(format!("http://{addr}/api/issues"))
        .bearer_auth(&api_key)
        .json(&serde_json::json!({ "title": long_title }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 400, "expected 400 for title > 500 chars");
    let body = resp.text().await.unwrap();
    assert!(body.contains("title"), "error should mention 'title'");
}

/// Issue description exceeds 50 KB → 400.
#[tokio::test(flavor = "multi_thread")]
async fn test_create_issue_description_too_long() {
    let (_tmp, vai_dir) = setup();
    let (_, api_key) = auth::create(&vai_dir, "agent").unwrap();
    let (addr, _tx) = server::start_for_testing(&vai_dir).await.unwrap();

    let client = reqwest::Client::new();
    let long_desc = "a".repeat(50 * 1024 + 1);
    let resp = client
        .post(format!("http://{addr}/api/issues"))
        .bearer_auth(&api_key)
        .json(&serde_json::json!({ "title": "ok", "description": long_desc }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 400, "expected 400 for description > 50 KB");
    let body = resp.text().await.unwrap();
    assert!(body.contains("description"), "error should mention 'description'");
}

/// More than 20 labels per issue → 400.
#[tokio::test(flavor = "multi_thread")]
async fn test_create_issue_too_many_labels() {
    let (_tmp, vai_dir) = setup();
    let (_, api_key) = auth::create(&vai_dir, "agent").unwrap();
    let (addr, _tx) = server::start_for_testing(&vai_dir).await.unwrap();

    let client = reqwest::Client::new();
    let labels: Vec<String> = (0..21).map(|i| format!("label-{i}")).collect();
    let resp = client
        .post(format!("http://{addr}/api/issues"))
        .bearer_auth(&api_key)
        .json(&serde_json::json!({ "title": "ok", "labels": labels }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 400, "expected 400 for > 20 labels");
    let body = resp.text().await.unwrap();
    assert!(body.contains("labels") || body.contains("label"), "error should mention labels");
}

/// A single label exceeds 100 characters → 400.
#[tokio::test(flavor = "multi_thread")]
async fn test_create_issue_label_too_long() {
    let (_tmp, vai_dir) = setup();
    let (_, api_key) = auth::create(&vai_dir, "agent").unwrap();
    let (addr, _tx) = server::start_for_testing(&vai_dir).await.unwrap();

    let client = reqwest::Client::new();
    let long_label = "l".repeat(101);
    let resp = client
        .post(format!("http://{addr}/api/issues"))
        .bearer_auth(&api_key)
        .json(&serde_json::json!({ "title": "ok", "labels": [long_label] }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 400, "expected 400 for label > 100 chars");
    let body = resp.text().await.unwrap();
    assert!(body.contains("label"), "error should mention 'label'");
}

/// Workspace intent exceeds 1000 characters → 400.
#[tokio::test(flavor = "multi_thread")]
async fn test_create_workspace_intent_too_long() {
    let (_tmp, vai_dir) = setup();
    let (_, api_key) = auth::create(&vai_dir, "agent").unwrap();
    let (addr, _tx) = server::start_for_testing(&vai_dir).await.unwrap();

    let client = reqwest::Client::new();
    let long_intent = "i".repeat(1001);
    let resp = client
        .post(format!("http://{addr}/api/workspaces"))
        .bearer_auth(&api_key)
        .json(&serde_json::json!({ "intent": long_intent }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 400, "expected 400 for intent > 1000 chars");
    let body = resp.text().await.unwrap();
    assert!(body.contains("intent"), "error should mention 'intent'");
}

/// File upload with path traversal (`../secret`) → 400.
#[tokio::test(flavor = "multi_thread")]
async fn test_upload_path_traversal() {
    let (_tmp, vai_dir) = setup();
    let (_, api_key) = auth::create(&vai_dir, "agent").unwrap();
    let (addr, _tx) = server::start_for_testing(&vai_dir).await.unwrap();
    let client = reqwest::Client::new();

    // First create a workspace.
    let ws_resp = client
        .post(format!("http://{addr}/api/workspaces"))
        .bearer_auth(&api_key)
        .json(&serde_json::json!({ "intent": "traversal test" }))
        .send()
        .await
        .unwrap();
    assert!(ws_resp.status().is_success());
    let ws: serde_json::Value = ws_resp.json().await.unwrap();
    let ws_id = ws["id"].as_str().unwrap();

    let resp = client
        .post(format!("http://{addr}/api/workspaces/{ws_id}/files"))
        .bearer_auth(&api_key)
        .json(&serde_json::json!({
            "files": [{
                "path": "../secret",
                "content_base64": BASE64.encode(b"evil")
            }]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 400, "expected 400 for path traversal");
}

/// File upload with null byte in path → 400.
#[tokio::test(flavor = "multi_thread")]
async fn test_upload_path_null_byte() {
    let (_tmp, vai_dir) = setup();
    let (_, api_key) = auth::create(&vai_dir, "agent").unwrap();
    let (addr, _tx) = server::start_for_testing(&vai_dir).await.unwrap();
    let client = reqwest::Client::new();

    let ws_resp = client
        .post(format!("http://{addr}/api/workspaces"))
        .bearer_auth(&api_key)
        .json(&serde_json::json!({ "intent": "null byte test" }))
        .send()
        .await
        .unwrap();
    assert!(ws_resp.status().is_success());
    let ws: serde_json::Value = ws_resp.json().await.unwrap();
    let ws_id = ws["id"].as_str().unwrap();

    // Encode a path with a null byte.
    let evil_path = "foo\0bar.rs";
    let resp = client
        .post(format!("http://{addr}/api/workspaces/{ws_id}/files"))
        .bearer_auth(&api_key)
        .json(&serde_json::json!({
            "files": [{
                "path": evil_path,
                "content_base64": BASE64.encode(b"data")
            }]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 400, "expected 400 for path with null byte");
}

/// File upload with > 100 files → 400.
#[tokio::test(flavor = "multi_thread")]
async fn test_upload_too_many_files() {
    let (_tmp, vai_dir) = setup();
    let (_, api_key) = auth::create(&vai_dir, "agent").unwrap();
    let (addr, _tx) = server::start_for_testing(&vai_dir).await.unwrap();
    let client = reqwest::Client::new();

    let ws_resp = client
        .post(format!("http://{addr}/api/workspaces"))
        .bearer_auth(&api_key)
        .json(&serde_json::json!({ "intent": "many files test" }))
        .send()
        .await
        .unwrap();
    assert!(ws_resp.status().is_success());
    let ws: serde_json::Value = ws_resp.json().await.unwrap();
    let ws_id = ws["id"].as_str().unwrap();

    let files: Vec<serde_json::Value> = (0..101)
        .map(|i| {
            serde_json::json!({
                "path": format!("file_{i}.rs"),
                "content_base64": BASE64.encode(b"hello")
            })
        })
        .collect();

    let resp = client
        .post(format!("http://{addr}/api/workspaces/{ws_id}/files"))
        .bearer_auth(&api_key)
        .json(&serde_json::json!({ "files": files }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 400, "expected 400 for > 100 files");
    let body = resp.text().await.unwrap();
    assert!(body.contains("files") || body.contains("100"), "error should mention file count");
}

/// File path exceeds 1000 characters → 400.
#[tokio::test(flavor = "multi_thread")]
async fn test_upload_path_too_long() {
    let (_tmp, vai_dir) = setup();
    let (_, api_key) = auth::create(&vai_dir, "agent").unwrap();
    let (addr, _tx) = server::start_for_testing(&vai_dir).await.unwrap();
    let client = reqwest::Client::new();

    let ws_resp = client
        .post(format!("http://{addr}/api/workspaces"))
        .bearer_auth(&api_key)
        .json(&serde_json::json!({ "intent": "path length test" }))
        .send()
        .await
        .unwrap();
    assert!(ws_resp.status().is_success());
    let ws: serde_json::Value = ws_resp.json().await.unwrap();
    let ws_id = ws["id"].as_str().unwrap();

    let long_path = "a".repeat(1001);
    let resp = client
        .post(format!("http://{addr}/api/workspaces/{ws_id}/files"))
        .bearer_auth(&api_key)
        .json(&serde_json::json!({
            "files": [{
                "path": long_path,
                "content_base64": BASE64.encode(b"data")
            }]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 400, "expected 400 for path > 1000 chars");
    let body = resp.text().await.unwrap();
    assert!(body.contains("path"), "error should mention 'path'");
}

/// Pagination per_page above 100 → 400.
#[tokio::test(flavor = "multi_thread")]
async fn test_list_versions_limit_too_large() {
    let (_tmp, vai_dir) = setup();
    let (_, api_key) = auth::create(&vai_dir, "agent").unwrap();
    let (addr, _tx) = server::start_for_testing(&vai_dir).await.unwrap();

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("http://{addr}/api/versions?per_page=101"))
        .bearer_auth(&api_key)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 400, "expected 400 for per_page > 100");
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("101") || body.contains("per_page") || body.contains("100"),
        "error should mention the limit: {body}"
    );
}

/// Update issue with title too long → 400.
#[tokio::test(flavor = "multi_thread")]
async fn test_update_issue_title_too_long() {
    let (_tmp, vai_dir) = setup();
    let (_, api_key) = auth::create(&vai_dir, "agent").unwrap();
    let (addr, _tx) = server::start_for_testing(&vai_dir).await.unwrap();
    let client = reqwest::Client::new();

    // Create an issue first.
    let create_resp = client
        .post(format!("http://{addr}/api/issues"))
        .bearer_auth(&api_key)
        .json(&serde_json::json!({ "title": "Original" }))
        .send()
        .await
        .unwrap();
    assert!(create_resp.status().is_success());
    let issue: serde_json::Value = create_resp.json().await.unwrap();
    let issue_id = issue["id"].as_str().unwrap();

    // Attempt to update with a too-long title.
    let long_title = "t".repeat(501);
    let resp = client
        .patch(format!("http://{addr}/api/issues/{issue_id}"))
        .bearer_auth(&api_key)
        .json(&serde_json::json!({ "title": long_title }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 400, "expected 400 for title > 500 chars in update");
}

/// Valid issue creation succeeds (smoke test ensuring validation doesn't block normal requests).
#[tokio::test(flavor = "multi_thread")]
async fn test_valid_issue_creation_succeeds() {
    let (_tmp, vai_dir) = setup();
    let (_, api_key) = auth::create(&vai_dir, "agent").unwrap();
    let (addr, _tx) = server::start_for_testing(&vai_dir).await.unwrap();

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{addr}/api/issues"))
        .bearer_auth(&api_key)
        .json(&serde_json::json!({
            "title": "Fix the thing",
            "description": "It is broken.",
            "labels": ["bug", "urgent"],
            "priority": "high"
        }))
        .send()
        .await
        .unwrap();

    assert!(resp.status().is_success(), "valid issue creation should succeed: {}", resp.status());
}
