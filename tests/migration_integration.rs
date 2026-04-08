//! Integration test for local-to-remote migration.
//!
//! Creates a local vai repo with real data (events, issues, versions), migrates
//! it to a Postgres-backed test server via `POST /api/migrate`, then verifies
//! with `GET /api/migration-stats` that every record transferred correctly.
//!
//! # Running
//!
//! Start Postgres with:
//! ```bash
//! docker compose up -d postgres
//! ```
//!
//! Then run:
//! ```bash
//! VAI_TEST_DATABASE_URL=postgres://vai:vai@localhost:5432/vai \
//!   cargo test --test migration_integration
//! ```
//!
//! Tests are silently skipped when `VAI_TEST_DATABASE_URL` is not set so that
//! `cargo test` in environments without Postgres continues to pass.

#![cfg(feature = "postgres")]

use std::env;
use std::fs;

use tempfile::TempDir;
use uuid::Uuid;

use vai::event_log::{EventKind, EventLog};
use vai::issue::{IssuePriority, IssueStore};
use vai::migration::{gather_local_data, MigrationSummary};
use vai::repo;
use vai::server::{start_for_testing_pg, MigrationStatsResponse};
use vai::storage::postgres::PostgresStorage;
use vai::version::VersionMeta;

// ── helpers ───────────────────────────────────────────────────────────────────

/// Returns the database URL from the environment, or `None` (test will skip).
fn db_url() -> Option<String> {
    match env::var("VAI_TEST_DATABASE_URL") {
        Ok(url) => Some(url),
        Err(_) => {
            eprintln!("VAI_TEST_DATABASE_URL not set — skipping migration integration tests");
            None
        }
    }
}

/// Deletes the test repo row (all child rows cascade via FK constraints).
async fn teardown(pg: &PostgresStorage, repo_id: Uuid) {
    sqlx::query("DELETE FROM repos WHERE id = $1")
        .bind(repo_id)
        .execute(pg.pool())
        .await
        .expect("teardown: failed to delete test repo");
}

/// Builds a tiny local vai repo with 3 events, 2 issues, and 2 versions.
///
/// Returns the temp directory (kept alive for the duration of the test) and the
/// path to the `.vai/` directory.
fn build_local_repo() -> (TempDir, std::path::PathBuf) {
    let tmp = TempDir::new().expect("failed to create temp dir");
    let root = tmp.path().to_path_buf();

    // Minimal source file so repo::init can parse something.
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src").join("lib.rs"),
        "pub fn hello() -> &'static str { \"hello\" }\n",
    )
    .unwrap();

    repo::init(&root).expect("vai init failed");
    let vai_dir = root.join(".vai");

    // Append two more events to the log.
    let mut log = EventLog::open(&vai_dir.join("event_log")).expect("open event log");
    let ws_id = Uuid::new_v4();
    log.append(EventKind::WorkspaceCreated {
        workspace_id: ws_id,
        intent: "add rate limiting".to_string(),
        base_version: "v1".to_string(),
    })
    .expect("append workspace created");
    log.append(EventKind::WorkspaceSubmitted {
        workspace_id: ws_id,
        changes_summary: "added rate limiting middleware".to_string(),
    })
    .expect("append workspace submitted");

    // Create two issues.
    let mut log2 = EventLog::open(&vai_dir.join("event_log")).expect("open event log for issues");
    let issue_store = IssueStore::open(&vai_dir).expect("open issue store");
    issue_store
        .create(
            "fix auth bug",
            "The token validation is broken.",
            IssuePriority::High,
            vec!["bug".to_string()],
            "agent-1",
            &mut log2,
        )
        .expect("create issue 1");
    issue_store
        .create(
            "add caching",
            "Cache the graph snapshot.",
            IssuePriority::Medium,
            vec!["enhancement".to_string()],
            "agent-2",
            &mut log2,
        )
        .expect("create issue 2");

    // Write a second version.
    let v2 = VersionMeta {
        id: None,
        version_id: "v2".to_string(),
        parent_version_id: Some("v1".to_string()),
        intent: "add rate limiting".to_string(),
        created_by: "agent-1".to_string(),
        created_at: chrono::Utc::now(),
        merge_event_id: None,
    };
    fs::write(
        vai_dir.join("versions").join("v2.toml"),
        toml::to_string_pretty(&v2).expect("serialize v2"),
    )
    .expect("write v2.toml");

    // Update HEAD to v2.
    fs::write(vai_dir.join("head"), "v2\n").expect("write head");

    (tmp, vai_dir)
}

// ── tests ─────────────────────────────────────────────────────────────────────

/// Full migration flow: local repo → Postgres server → verify stats.
#[tokio::test]
async fn test_migrate_local_repo_to_postgres() {
    let Some(url) = db_url() else {
        return;
    };

    // ── Build a local repo with data ──────────────────────────────────────────
    let (_tmp, vai_dir) = build_local_repo();

    let payload = gather_local_data(&vai_dir).expect("gather_local_data failed");

    // Snapshot expected counts before we start the server (values are immutable
    // from this point).
    let expected_events = payload.events.len();
    let expected_issues = payload.issues.len();
    let expected_versions = payload.versions.len();
    let expected_head = payload.head_version.clone();

    assert!(expected_events >= 3, "should have at least 3 events");
    assert_eq!(expected_issues, 2);
    assert!(expected_versions >= 2, "should have at least v1 and v2");

    // ── Start a Postgres-backed test server ───────────────────────────────────
    let (addr, shutdown_tx, repo_id) = start_for_testing_pg(&vai_dir, &url)
        .await
        .expect("start_for_testing_pg failed");

    let repo_config = vai::repo::read_config(&vai_dir).expect("read config");
    let repo = &repo_config.name;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    // ── POST /api/repos/:repo/migrate ─────────────────────────────────────────
    let migrate_resp = client
        .post(format!("{base}/api/repos/{repo}/migrate"))
        .bearer_auth("vai_admin_test")
        .json(&payload)
        .send()
        .await
        .expect("migrate request failed");

    assert_eq!(
        migrate_resp.status(),
        reqwest::StatusCode::OK,
        "migrate returned unexpected status: {}",
        migrate_resp.text().await.unwrap_or_default()
    );

    let summary: MigrationSummary = migrate_resp
        .json()
        .await
        .expect("failed to deserialize MigrationSummary");

    assert_eq!(summary.events_migrated, expected_events);
    assert_eq!(summary.issues_migrated, expected_issues);
    assert_eq!(summary.versions_migrated, expected_versions);
    assert_eq!(summary.head_version, expected_head);

    // ── GET /api/repos/:repo/migration-stats — independent verification ──────
    let stats_resp = client
        .get(format!("{base}/api/repos/{repo}/migration-stats"))
        .bearer_auth("vai_admin_test")
        .send()
        .await
        .expect("migration-stats request failed");

    assert_eq!(
        stats_resp.status(),
        reqwest::StatusCode::OK,
        "migration-stats returned unexpected status: {}",
        stats_resp.text().await.unwrap_or_default()
    );

    let stats: MigrationStatsResponse = stats_resp
        .json()
        .await
        .expect("failed to deserialize MigrationStatsResponse");

    assert_eq!(stats.events as usize, expected_events, "event count mismatch");
    assert_eq!(stats.issues as usize, expected_issues, "issue count mismatch");
    assert_eq!(
        stats.versions as usize,
        expected_versions,
        "version count mismatch"
    );
    assert_eq!(stats.head_version, expected_head, "head version mismatch");

    // ── Idempotency check — second POST must succeed and insert nothing new ────
    let dup_resp = client
        .post(format!("{base}/api/repos/{repo}/migrate"))
        .bearer_auth("vai_admin_test")
        .json(&payload)
        .send()
        .await
        .expect("second migrate request failed");

    assert_eq!(
        dup_resp.status(),
        reqwest::StatusCode::OK,
        "second migration should succeed (idempotent): {}",
        dup_resp.text().await.unwrap_or_default()
    );

    let dup_summary: MigrationSummary = client
        .post(format!("{base}/api/repos/{repo}/migrate"))
        .bearer_auth("vai_admin_test")
        .json(&payload)
        .send()
        .await
        .expect("third migrate request failed")
        .json()
        .await
        .expect("failed to deserialize third MigrationSummary");

    // All rows already exist — nothing new should be inserted.
    assert_eq!(dup_summary.events_migrated, 0, "re-run should skip existing events");
    assert_eq!(dup_summary.issues_migrated, 0, "re-run should skip existing issues");
    assert_eq!(dup_summary.versions_migrated, 0, "re-run should skip existing versions");
    assert_eq!(dup_summary.escalations_migrated, 0, "re-run should skip existing escalations");

    // ── Cleanup ───────────────────────────────────────────────────────────────
    let _ = shutdown_tx.send(());

    let pg = PostgresStorage::connect(&url, 2)
        .await
        .expect("reconnect for teardown");
    teardown(&pg, repo_id).await;
}
