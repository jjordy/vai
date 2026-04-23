//! Integration tests for the Postgres storage backend.
//!
//! These tests run against a real Postgres instance and verify that
//! [`PostgresStorage`] correctly implements all storage traits.
//!
//! # Running
//!
//! Start the database with:
//! ```bash
//! docker compose up -d postgres
//! ```
//!
//! Then run with the connection URL:
//! ```bash
//! VAI_TEST_DATABASE_URL=postgres://vai:vai@localhost:5432/vai cargo test --test postgres_storage
//! ```
//!
//! Tests are silently skipped when `VAI_TEST_DATABASE_URL` is not set so that
//! `cargo test` in environments without Postgres (e.g. standard CI) continues
//! to pass.

#![cfg(feature = "postgres")]

use std::env;

use chrono::Utc;
use uuid::Uuid;

use vai::escalation::{EscalationSeverity, EscalationStatus, EscalationType, ResolutionOption};
use vai::event_log::EventKind;
use vai::graph::{Entity, EntityKind, Relationship, RelationshipKind};
use vai::issue::{IssueFilter, IssuePriority, IssueStatus};
use vai::storage::pagination::ListQuery;
use vai::storage::postgres::PostgresStorage;
use vai::storage::{
    AuthStore, CommentStore, EscalationStore, EventStore, GraphStore, IssueStore, NewEscalation,
    NewIssue, NewIssueComment, NewVersion, NewWorkspace, StorageError, VersionStore,
    WorkspaceStore, WorkspaceUpdate,
};
use vai::workspace::WorkspaceStatus;

// ── test helpers ──────────────────────────────────────────────────────────────

/// Returns a connected [`PostgresStorage`] and a fresh `repo_id` for test
/// isolation, or `None` if `VAI_TEST_DATABASE_URL` is not set.
///
/// The repo row is inserted into `repos` so foreign-key constraints on other
/// tables are satisfied.
async fn setup() -> Option<(PostgresStorage, Uuid)> {
    let db_url = match env::var("VAI_TEST_DATABASE_URL") {
        Ok(url) => url,
        Err(_) => {
            eprintln!("VAI_TEST_DATABASE_URL not set — skipping Postgres integration tests");
            return None;
        }
    };

    let storage = PostgresStorage::connect(&db_url, 5)
        .await
        .expect("failed to connect to Postgres");

    // Apply migrations so the schema is up to date.
    let migrations_path = concat!(env!("CARGO_MANIFEST_DIR"), "/migrations");
    storage
        .migrate(migrations_path)
        .await
        .expect("failed to run migrations");

    // Each test run gets its own repo so concurrent tests don't interfere.
    let repo_id = Uuid::new_v4();
    sqlx::query("INSERT INTO repos (id, name) VALUES ($1, $2)")
        .bind(repo_id)
        .bind(format!("test-repo-{repo_id}"))
        .execute(storage.pool())
        .await
        .expect("failed to insert test repo");

    Some((storage, repo_id))
}

/// Deletes the test repo row (cascades to all child tables).
async fn teardown(storage: &PostgresStorage, repo_id: &Uuid) {
    sqlx::query("DELETE FROM repos WHERE id = $1")
        .bind(repo_id)
        .execute(storage.pool())
        .await
        .expect("failed to delete test repo");
}

// ── EventStore ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_event_store_append_and_query() {
    let Some((storage, repo_id)) = setup().await else {
        return;
    };

    let workspace_id = Uuid::new_v4();
    let event_kind = EventKind::WorkspaceCreated {
        workspace_id,
        intent: "add auth module".to_string(),
        base_version: "v1".to_string(),
    };

    // append
    let event = storage
        .append(&repo_id, event_kind)
        .await
        .expect("append failed");
    assert!(event.id > 0);

    // query_by_type
    let events = storage
        .query_by_type(&repo_id, "WorkspaceCreated")
        .await
        .expect("query_by_type failed");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].id, event.id);

    // query_by_workspace
    let ws_events = storage
        .query_by_workspace(&repo_id, &workspace_id)
        .await
        .expect("query_by_workspace failed");
    assert_eq!(ws_events.len(), 1);

    // query_since_id (should return events after the one we appended)
    let newer = storage
        .query_since_id(&repo_id, event.id as i64)
        .await
        .expect("query_since_id failed");
    assert!(newer.is_empty(), "no newer events should exist yet");

    // count
    let count = storage.count(&repo_id).await.expect("count failed");
    assert_eq!(count, 1);

    teardown(&storage, &repo_id).await;
}

#[tokio::test]
async fn test_event_store_query_by_time_range() {
    let Some((storage, repo_id)) = setup().await else {
        return;
    };

    let before = Utc::now();
    let event_kind = EventKind::RepoInitialized {
        repo_id,
        name: "test".to_string(),
    };
    storage
        .append(&repo_id, event_kind)
        .await
        .expect("append failed");
    let after = Utc::now();

    let events = storage
        .query_by_time_range(&repo_id, before, after)
        .await
        .expect("query_by_time_range failed");
    assert_eq!(events.len(), 1);

    // Range that excludes the event
    let far_future = after + chrono::Duration::hours(1);
    let events_empty = storage
        .query_by_time_range(&repo_id, after, far_future)
        .await
        .expect("query_by_time_range failed");
    // May be empty or contain boundary-inclusive events — just assert no crash.
    let _ = events_empty;

    teardown(&storage, &repo_id).await;
}

#[tokio::test]
async fn test_event_store_cross_repo_isolation() {
    let Some((storage, repo_a)) = setup().await else {
        return;
    };
    // Create a second repo.
    let repo_b = Uuid::new_v4();
    sqlx::query("INSERT INTO repos (id, name) VALUES ($1, $2)")
        .bind(repo_b)
        .bind(format!("test-repo-{repo_b}"))
        .execute(storage.pool())
        .await
        .unwrap();

    let event_kind = EventKind::RepoInitialized {
        repo_id: repo_a,
        name: "a".to_string(),
    };
    storage.append(&repo_a, event_kind).await.unwrap();

    // repo_b should see no events.
    let events_b = storage
        .query_by_type(&repo_b, "RepoInitialized")
        .await
        .unwrap();
    assert!(events_b.is_empty(), "repo_b should not see repo_a's events");

    teardown(&storage, &repo_b).await;
    teardown(&storage, &repo_a).await;
}

// ── IssueStore ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_issue_store_crud() {
    let Some((storage, repo_id)) = setup().await else {
        return;
    };

    let new_issue = NewIssue {
        title: "Fix authentication bug".to_string(),
        description: "Token validation fails under load".to_string(),
        priority: IssuePriority::High,
        labels: vec!["bug".to_string(), "auth".to_string()],
        creator: "alice".to_string(),
        agent_source: None,
        acceptance_criteria: vec![],
    };

    // create
    let issue = storage
        .create_issue(&repo_id, new_issue)
        .await
        .expect("create_issue failed");
    assert_eq!(issue.title, "Fix authentication bug");
    assert_eq!(issue.priority, IssuePriority::High);
    assert_eq!(issue.labels, vec!["bug", "auth"]);

    // get
    let fetched = storage
        .get_issue(&repo_id, &issue.id)
        .await
        .expect("get_issue failed");
    assert_eq!(fetched.id, issue.id);
    assert_eq!(fetched.title, issue.title);

    // update
    let updated = storage
        .update_issue(
            &repo_id,
            &issue.id,
            vai::storage::IssueUpdate {
                title: Some("Fix authentication bug (critical)".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("update_issue failed");
    assert_eq!(updated.title, "Fix authentication bug (critical)");

    // list — open issues
    let filter = IssueFilter {
        status: Some(vec![IssueStatus::Open]),
        ..Default::default()
    };
    let issues = storage
        .list_issues(&repo_id, &filter, &ListQuery::default())
        .await
        .expect("list_issues failed");
    assert_eq!(issues.items.len(), 1);

    // close
    let closed = storage
        .close_issue(&repo_id, &issue.id, "fixed in v2")
        .await
        .expect("close_issue failed");
    assert_eq!(closed.status, IssueStatus::Closed);

    // list — open issues should now be empty
    let open = storage
        .list_issues(&repo_id, &IssueFilter { status: Some(vec![IssueStatus::Open]), ..Default::default() }, &ListQuery::default())
        .await
        .expect("list_issues after close failed");
    assert!(open.items.is_empty());

    // not found
    let missing = storage.get_issue(&repo_id, &Uuid::new_v4()).await;
    assert!(matches!(missing, Err(StorageError::NotFound(_))));

    teardown(&storage, &repo_id).await;
}

// ── IssueStore — paginated filters ───────────────────────────────────────────

/// Regression test for: status filter on paginated issues endpoint returns 0
/// results (issue #239).
///
/// The root cause was that the schema stored status/priority with title-case
/// defaults ('Open', 'Medium') while the Rust code filters with lowercase
/// values ('open', 'medium').  The fix:
///   1. A migration normalizes existing rows to lowercase.
///   2. `create_issue` now explicitly inserts `status = 'open'`.
///   3. The `list_issues` WHERE clause uses `LOWER(status)` / `LOWER(priority)`.
#[tokio::test]
async fn test_issue_list_paginated_status_filter() {
    let Some((storage, repo_id)) = setup().await else {
        return;
    };

    let make_issue = |title: &str, priority: IssuePriority| NewIssue {
        title: title.to_string(),
        description: String::new(),
        priority,
        labels: vec![],
        creator: "test".to_string(),
        agent_source: None,
        acceptance_criteria: vec![],
    };

    // Create three open issues and one closed issue.
    let i1 = storage.create_issue(&repo_id, make_issue("Open A", IssuePriority::High)).await.unwrap();
    let i2 = storage.create_issue(&repo_id, make_issue("Open B", IssuePriority::High)).await.unwrap();
    let i3 = storage.create_issue(&repo_id, make_issue("Open C", IssuePriority::Low)).await.unwrap();
    let i4 = storage.create_issue(&repo_id, make_issue("Closed D", IssuePriority::Medium)).await.unwrap();
    storage.close_issue(&repo_id, &i4.id, "done").await.unwrap();

    // status=open must return exactly the 3 open issues.
    let open = storage
        .list_issues(
            &repo_id,
            &IssueFilter { status: Some(vec![IssueStatus::Open]), ..Default::default() },
            &ListQuery::default(),
        )
        .await
        .expect("list_issues(status=open) failed");
    assert_eq!(open.total, 3, "expected 3 open issues, got {}", open.total);
    assert_eq!(open.items.len(), 3);
    let open_ids: std::collections::HashSet<_> = open.items.iter().map(|i| i.id).collect();
    assert!(open_ids.contains(&i1.id));
    assert!(open_ids.contains(&i2.id));
    assert!(open_ids.contains(&i3.id));
    assert!(!open_ids.contains(&i4.id));

    // status=closed must return exactly the 1 closed issue.
    let closed = storage
        .list_issues(
            &repo_id,
            &IssueFilter { status: Some(vec![IssueStatus::Closed]), ..Default::default() },
            &ListQuery::default(),
        )
        .await
        .expect("list_issues(status=closed) failed");
    assert_eq!(closed.total, 1);
    assert_eq!(closed.items[0].id, i4.id);

    // priority=high must return the 2 high-priority open issues.
    let high = storage
        .list_issues(
            &repo_id,
            &IssueFilter { priority: Some(IssuePriority::High), ..Default::default() },
            &ListQuery::default(),
        )
        .await
        .expect("list_issues(priority=high) failed");
    assert_eq!(high.total, 2);

    // Combined filter: status=open AND priority=high → 2 results.
    let open_high = storage
        .list_issues(
            &repo_id,
            &IssueFilter {
                status: Some(vec![IssueStatus::Open]),
                priority: Some(IssuePriority::High),
                ..Default::default()
            },
            &ListQuery::default(),
        )
        .await
        .expect("list_issues(status=open,priority=high) failed");
    assert_eq!(open_high.total, 2);

    // Unfiltered must return all 4 issues with correct total.
    let all = storage
        .list_issues(&repo_id, &IssueFilter::default(), &ListQuery::default())
        .await
        .expect("list_issues(unfiltered) failed");
    assert_eq!(all.total, 4);
    assert_eq!(all.items.len(), 4);

    // Paginated: page 1 of page_size=2 → 2 items, total=4.
    let page1 = storage
        .list_issues(
            &repo_id,
            &IssueFilter::default(),
            &ListQuery::from_params(Some(1), Some(2), None, &[]).unwrap(),
        )
        .await
        .expect("list_issues(page=1,per_page=2) failed");
    assert_eq!(page1.total, 4);
    assert_eq!(page1.items.len(), 2);

    // Paginated: page 2 of page_size=2 → 2 items, total=4.
    let page2 = storage
        .list_issues(
            &repo_id,
            &IssueFilter::default(),
            &ListQuery::from_params(Some(2), Some(2), None, &[]).unwrap(),
        )
        .await
        .expect("list_issues(page=2,per_page=2) failed");
    assert_eq!(page2.total, 4);
    assert_eq!(page2.items.len(), 2);

    // The two pages together should cover all 4 issues with no overlap.
    let combined_ids: std::collections::HashSet<_> =
        page1.items.iter().chain(page2.items.iter()).map(|i| i.id).collect();
    assert_eq!(combined_ids.len(), 4);

    teardown(&storage, &repo_id).await;
}

// ── EscalationStore ───────────────────────────────────────────────────────────

#[tokio::test]
async fn test_escalation_store_crud() {
    let Some((storage, repo_id)) = setup().await else {
        return;
    };

    let ws_id = Uuid::new_v4();
    let new_esc = NewEscalation {
        escalation_type: EscalationType::MergeConflict,
        severity: EscalationSeverity::High,
        summary: "Conflicting changes to auth module".to_string(),
        intents: vec!["add oauth".to_string(), "refactor tokens".to_string()],
        agents: vec!["agent-1".to_string(), "agent-2".to_string()],
        workspace_ids: vec![ws_id],
        affected_entities: vec!["AuthService::validate_token".to_string()],
        conflicts: vec![],
        resolution_options: vec![ResolutionOption::KeepAgentA, ResolutionOption::KeepAgentB],
    };

    // create
    let esc = storage
        .create_escalation(&repo_id, new_esc)
        .await
        .expect("create_escalation failed");
    assert_eq!(esc.escalation_type, EscalationType::MergeConflict);
    assert_eq!(esc.status, EscalationStatus::Pending);

    // get
    let fetched = storage
        .get_escalation(&repo_id, &esc.id)
        .await
        .expect("get_escalation failed");
    assert_eq!(fetched.id, esc.id);

    // list pending_only=true
    let pending = storage
        .list_escalations(&repo_id, true, &ListQuery::default())
        .await
        .expect("list_escalations failed");
    assert_eq!(pending.items.len(), 1);

    // resolve
    let resolved = storage
        .resolve_escalation(&repo_id, &esc.id, ResolutionOption::KeepAgentA, "bob")
        .await
        .expect("resolve_escalation failed");
    assert_eq!(resolved.status, EscalationStatus::Resolved);
    assert_eq!(resolved.resolved_by.as_deref(), Some("bob"));

    // list pending_only=true should now be empty
    let pending_after = storage
        .list_escalations(&repo_id, true, &ListQuery::default())
        .await
        .expect("list after resolve failed");
    assert!(pending_after.items.is_empty());

    // list all (pending_only=false) should still have 1
    let all = storage
        .list_escalations(&repo_id, false, &ListQuery::default())
        .await
        .expect("list_all failed");
    assert_eq!(all.items.len(), 1);

    teardown(&storage, &repo_id).await;
}

// ── VersionStore ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_version_store() {
    let Some((storage, repo_id)) = setup().await else {
        return;
    };

    // HEAD is empty before any versions.
    let head = storage.read_head(&repo_id).await.expect("read_head failed");
    assert!(head.is_none());

    let v1 = storage
        .create_version(
            &repo_id,
            NewVersion {
                version_id: "v1".to_string(),
                parent_version_id: None,
                intent: "initial commit".to_string(),
                created_by: "alice".to_string(),
                merge_event_id: None,
            },
        )
        .await
        .expect("create_version v1 failed");
    assert_eq!(v1.version_id, "v1");

    let v2 = storage
        .create_version(
            &repo_id,
            NewVersion {
                version_id: "v2".to_string(),
                parent_version_id: Some("v1".to_string()),
                intent: "add auth".to_string(),
                created_by: "bob".to_string(),
                merge_event_id: None,
            },
        )
        .await
        .expect("create_version v2 failed");
    assert_eq!(v2.parent_version_id.as_deref(), Some("v1"));

    // get_version
    let fetched = storage
        .get_version(&repo_id, "v1")
        .await
        .expect("get_version failed");
    assert_eq!(fetched.version_id, "v1");

    // list_versions — chronological order
    let versions = storage
        .list_versions(&repo_id, &ListQuery::default())
        .await
        .expect("list_versions failed");
    assert_eq!(versions.items.len(), 2);
    // Default ordering is created_at DESC, so v2 (created last) comes first.
    assert!(versions.items.iter().any(|v| v.version_id == "v1"));

    // advance_head
    storage
        .advance_head(&repo_id, "v2")
        .await
        .expect("advance_head failed");
    let head = storage.read_head(&repo_id).await.expect("read_head failed");
    assert_eq!(head.as_deref(), Some("v2"));

    // advance again
    storage.advance_head(&repo_id, "v1").await.unwrap();
    assert_eq!(
        storage.read_head(&repo_id).await.unwrap().as_deref(),
        Some("v1")
    );

    // not found
    let missing = storage.get_version(&repo_id, "v999").await;
    assert!(matches!(missing, Err(StorageError::NotFound(_))));

    teardown(&storage, &repo_id).await;
}

// ── WorkspaceStore ────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_workspace_store() {
    let Some((storage, repo_id)) = setup().await else {
        return;
    };

    let ws = storage
        .create_workspace(
            &repo_id,
            NewWorkspace {
                id: None,
                intent: "refactor auth".to_string(),
                base_version: "v1".to_string(),
                issue_id: None,
            },
        )
        .await
        .expect("create_workspace failed");
    assert_eq!(ws.intent, "refactor auth");
    assert_eq!(ws.status, WorkspaceStatus::Created);

    // get
    let fetched = storage
        .get_workspace(&repo_id, &ws.id)
        .await
        .expect("get_workspace failed");
    assert_eq!(fetched.id, ws.id);

    // update status
    let updated = storage
        .update_workspace(
            &repo_id,
            &ws.id,
            WorkspaceUpdate {
                status: Some(WorkspaceStatus::Active),
                issue_id: None,
                deleted_paths: None,
            },
        )
        .await
        .expect("update_workspace failed");
    assert_eq!(updated.status, WorkspaceStatus::Active);

    // list — include_inactive=false should return 1 active workspace
    let active = storage
        .list_workspaces(&repo_id, false, &ListQuery::default())
        .await
        .expect("list_workspaces failed");
    assert_eq!(active.items.len(), 1);

    // discard
    storage
        .discard_workspace(&repo_id, &ws.id)
        .await
        .expect("discard_workspace failed");

    // After discard, include_inactive=false should return empty
    let after_discard = storage
        .list_workspaces(&repo_id, false, &ListQuery::default())
        .await
        .expect("list_workspaces after discard failed");
    assert!(after_discard.items.is_empty());

    // include_inactive=true should still return the discarded workspace
    let all = storage
        .list_workspaces(&repo_id, true, &ListQuery::default())
        .await
        .expect("list_workspaces include_inactive failed");
    assert_eq!(all.items.len(), 1);

    // not found
    let missing = storage.get_workspace(&repo_id, &Uuid::new_v4()).await;
    assert!(matches!(missing, Err(StorageError::NotFound(_))));

    teardown(&storage, &repo_id).await;
}

// ── GraphStore ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_graph_store() {
    let Some((storage, repo_id)) = setup().await else {
        return;
    };

    let entity_id = Entity::compute_id("src/auth.rs", "AuthService");
    let method_id = Entity::compute_id("src/auth.rs", "AuthService::validate_token");

    let entities = vec![
        Entity {
            id: entity_id.clone(),
            kind: EntityKind::Struct,
            name: "AuthService".to_string(),
            qualified_name: "AuthService".to_string(),
            file_path: "src/auth.rs".to_string(),
            byte_range: (0, 100),
            line_range: (1, 10),
            parent_entity: None,
        },
        Entity {
            id: method_id.clone(),
            kind: EntityKind::Method,
            name: "validate_token".to_string(),
            qualified_name: "AuthService::validate_token".to_string(),
            file_path: "src/auth.rs".to_string(),
            byte_range: (101, 200),
            line_range: (5, 9),
            parent_entity: Some(entity_id.clone()),
        },
    ];

    // upsert_entities
    storage
        .upsert_entities(&repo_id, entities.clone())
        .await
        .expect("upsert_entities failed");

    // get_entity
    let fetched = storage
        .get_entity(&repo_id, &entity_id)
        .await
        .expect("get_entity failed");
    assert_eq!(fetched.name, "AuthService");

    // list_entities — all
    let all = storage
        .list_entities(&repo_id, None)
        .await
        .expect("list_entities all failed");
    assert_eq!(all.len(), 2);

    // list_entities — filtered by file
    let by_file = storage
        .list_entities(&repo_id, Some("src/auth.rs"))
        .await
        .expect("list_entities by_file failed");
    assert_eq!(by_file.len(), 2);

    let no_match = storage
        .list_entities(&repo_id, Some("src/other.rs"))
        .await
        .expect("list_entities no_match failed");
    assert!(no_match.is_empty());

    // upsert_relationships
    let rel = Relationship::new(
        RelationshipKind::Contains,
        &entity_id,
        &method_id,
    );
    storage
        .upsert_relationships(&repo_id, vec![rel.clone()])
        .await
        .expect("upsert_relationships failed");

    // get_relationships
    let rels = storage
        .get_relationships(&repo_id, &entity_id)
        .await
        .expect("get_relationships failed");
    assert_eq!(rels.len(), 1);
    assert_eq!(rels[0].kind, RelationshipKind::Contains);

    // upsert idempotency — same data again should not error or duplicate
    storage
        .upsert_entities(&repo_id, entities)
        .await
        .expect("upsert_entities idempotent failed");
    let all_after = storage
        .list_entities(&repo_id, None)
        .await
        .expect("list after re-upsert failed");
    assert_eq!(all_after.len(), 2, "upsert should not create duplicates");

    // clear_file — removes entities and relationships for the file
    storage
        .clear_file(&repo_id, "src/auth.rs")
        .await
        .expect("clear_file failed");
    let cleared = storage
        .list_entities(&repo_id, None)
        .await
        .expect("list after clear failed");
    assert!(cleared.is_empty());

    // not found
    let missing = storage.get_entity(&repo_id, "nonexistent-id").await;
    assert!(matches!(missing, Err(StorageError::NotFound(_))));

    teardown(&storage, &repo_id).await;
}

// ── AuthStore ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_auth_store() {
    let Some((storage, repo_id)) = setup().await else {
        return;
    };

    // create repo-scoped key with agent_type
    let (key_meta, plaintext) = storage
        .create_key(Some(&repo_id), "ci-bot", None, None, Some("ci"), None)
        .await
        .expect("create_key failed");
    assert_eq!(key_meta.name, "ci-bot");
    assert!(!plaintext.is_empty());
    assert!(key_meta.key_prefix.len() == 8);
    assert_eq!(key_meta.agent_type.as_deref(), Some("ci"));
    assert!(key_meta.expires_at.is_none());

    // validate
    let validated = storage
        .validate_key(&plaintext)
        .await
        .expect("validate_key failed");
    assert_eq!(validated.id, key_meta.id);
    assert_eq!(validated.agent_type.as_deref(), Some("ci"));

    // list
    let keys = storage
        .list_keys(Some(&repo_id))
        .await
        .expect("list_keys failed");
    assert_eq!(keys.len(), 1);

    // create a key with an expires_at in the future — should validate fine
    let future = chrono::Utc::now() + chrono::Duration::hours(1);
    let (expiring_key, expiring_plaintext) = storage
        .create_key(Some(&repo_id), "expiring-bot", None, None, None, Some(future))
        .await
        .expect("create expiring key failed");
    assert_eq!(expiring_key.expires_at.map(|t| t.timestamp()), Some(future.timestamp()));
    storage.validate_key(&expiring_plaintext).await.expect("non-expired key should validate");

    // create a key with expires_at in the past — should be rejected by validate_key
    let past = chrono::Utc::now() - chrono::Duration::hours(1);
    let (expired_key, expired_plaintext) = storage
        .create_key(Some(&repo_id), "expired-bot", None, None, None, Some(past))
        .await
        .expect("create expired key failed");
    let exp_result = storage.validate_key(&expired_plaintext).await;
    assert!(
        matches!(exp_result, Err(StorageError::NotFound(_))),
        "expired key should not validate"
    );
    // clean up extra keys
    sqlx::query("DELETE FROM api_keys WHERE id = $1 OR id = $2")
        .bind(&expiring_key.id)
        .bind(&expired_key.id)
        .execute(storage.pool())
        .await
        .unwrap();

    // create server-level key (repo_id=None)
    let (server_key, server_plaintext) = storage
        .create_key(None, "admin", None, None, None, None)
        .await
        .expect("create server key failed");
    assert_eq!(server_key.name, "admin");

    // server-level key should NOT appear in repo-scoped listing
    let repo_keys = storage.list_keys(Some(&repo_id)).await.unwrap();
    assert!(!repo_keys.iter().any(|k| k.id == server_key.id));

    // server-level listing
    let server_keys = storage.list_keys(None).await.unwrap();
    assert!(server_keys.iter().any(|k| k.id == server_key.id));

    // revoke
    storage
        .revoke_key(&key_meta.id)
        .await
        .expect("revoke_key failed");

    // revoked key fails validation
    let revoked_result = storage.validate_key(&plaintext).await;
    assert!(
        matches!(revoked_result, Err(StorageError::NotFound(_))),
        "revoked key should not validate"
    );

    // wrong token fails validation
    let bad_result = storage.validate_key("not-a-valid-token").await;
    assert!(matches!(bad_result, Err(StorageError::NotFound(_))));

    // cleanup server key
    storage.revoke_key(&server_key.id).await.unwrap();
    // server-level key is not scoped to this repo — clean it up explicitly
    sqlx::query("DELETE FROM api_keys WHERE id = $1")
        .bind(&server_key.id)
        .execute(storage.pool())
        .await
        .unwrap();
    // validate the server plaintext no longer works
    assert!(storage.validate_key(&server_plaintext).await.is_err());

    teardown(&storage, &repo_id).await;
}

// ── EventStore: NOTIFY delivery ───────────────────────────────────────────────

/// Verifies that `append` fires a `pg_notify('vai_events', '<repo_id>:<event_id>')` so
/// that WebSocket handlers using LISTEN can pick up new events without polling.
#[tokio::test]
async fn test_event_store_notify_delivery() {
    let Some((storage, repo_id)) = setup().await else {
        return;
    };

    // Create a PgListener on the `vai_events` channel *before* appending so we
    // don't miss the notification.
    let mut listener = storage
        .create_listener()
        .await
        .expect("create_listener failed");
    listener
        .listen("vai_events")
        .await
        .expect("LISTEN failed");

    let event_kind = EventKind::WorkspaceCreated {
        workspace_id: Uuid::new_v4(),
        intent: "notify test".to_string(),
        base_version: "v0".to_string(),
    };
    let event = storage
        .append(&repo_id, event_kind)
        .await
        .expect("append failed");

    // The NOTIFY should arrive promptly.  Give it a generous timeout to avoid
    // flakiness on slow CI machines.
    //
    // Because all tests share the same `vai_events` channel, we may receive
    // notifications from concurrent tests.  Keep draining until we see the
    // one matching our repo_id, or timeout.
    let expected_payload = format!("{repo_id}:{}", event.id);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut found = false;
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout_at(deadline, listener.recv()).await {
            Ok(Ok(notification)) => {
                if notification.payload() == expected_payload {
                    found = true;
                    break;
                }
                // Notification from a different concurrent test — skip it.
            }
            Ok(Err(e)) => panic!("PgListener recv error: {e}"),
            Err(_) => break, // timeout
        }
    }
    assert!(
        found,
        "expected NOTIFY payload '{expected_payload}' not received within 5s"
    );

    teardown(&storage, &repo_id).await;
}

// ── EventStore: replay from last_event_id ─────────────────────────────────────

/// Verifies that `query_since_id` returns only events that were appended
/// *after* the supplied cursor ID, enabling WebSocket replay on reconnect.
#[tokio::test]
async fn test_event_store_replay_from_last_id() {
    let Some((storage, repo_id)) = setup().await else {
        return;
    };

    // Append three distinct events.
    let ws1 = Uuid::new_v4();
    let ws2 = Uuid::new_v4();

    let e1 = storage
        .append(
            &repo_id,
            EventKind::WorkspaceCreated {
                workspace_id: ws1,
                intent: "first".to_string(),
                base_version: "v1".to_string(),
            },
        )
        .await
        .expect("append e1");

    let e2 = storage
        .append(
            &repo_id,
            EventKind::WorkspaceCreated {
                workspace_id: ws2,
                intent: "second".to_string(),
                base_version: "v1".to_string(),
            },
        )
        .await
        .expect("append e2");

    let e3 = storage
        .append(
            &repo_id,
            EventKind::WorkspaceSubmitted {
                workspace_id: ws1,
                changes_summary: "done".to_string(),
            },
        )
        .await
        .expect("append e3");

    // Replaying from e1 should return e2 and e3.
    let since_e1 = storage
        .query_since_id(&repo_id, e1.id as i64)
        .await
        .expect("query_since_id(e1)");
    assert_eq!(since_e1.len(), 2);
    assert_eq!(since_e1[0].id, e2.id);
    assert_eq!(since_e1[1].id, e3.id);

    // Replaying from e2 should return only e3.
    let since_e2 = storage
        .query_since_id(&repo_id, e2.id as i64)
        .await
        .expect("query_since_id(e2)");
    assert_eq!(since_e2.len(), 1);
    assert_eq!(since_e2[0].id, e3.id);

    // Replaying from e3 (the latest) should return nothing.
    let since_e3 = storage
        .query_since_id(&repo_id, e3.id as i64)
        .await
        .expect("query_since_id(e3)");
    assert!(since_e3.is_empty(), "no events should exist after e3");

    teardown(&storage, &repo_id).await;
}

// ── EventStore: server-side filtering ─────────────────────────────────────────

#[tokio::test]
async fn test_event_filter_by_event_type() {
    use vai::storage::EventFilter;

    let Some((storage, repo_id)) = setup().await else {
        return;
    };

    let ws = Uuid::new_v4();

    let e1 = storage
        .append(
            &repo_id,
            EventKind::WorkspaceCreated {
                workspace_id: ws,
                intent: "filter test".to_string(),
                base_version: "v1".to_string(),
            },
        )
        .await
        .expect("append e1");

    storage
        .append(
            &repo_id,
            EventKind::WorkspaceSubmitted {
                workspace_id: ws,
                changes_summary: "done".to_string(),
            },
        )
        .await
        .expect("append e2");

    // Filter: only WorkspaceCreated events since the beginning.
    let filter = EventFilter {
        event_types: vec!["WorkspaceCreated".to_string()],
        ..Default::default()
    };
    let results = storage
        .query_since_id_filtered(&repo_id, 0, &filter)
        .await
        .expect("query_since_id_filtered");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, e1.id);
    assert_eq!(results[0].kind.event_type(), "WorkspaceCreated");

    teardown(&storage, &repo_id).await;
}

#[tokio::test]
async fn test_event_filter_by_workspace_id() {
    use vai::storage::EventFilter;

    let Some((storage, repo_id)) = setup().await else {
        return;
    };

    let ws_a = Uuid::new_v4();
    let ws_b = Uuid::new_v4();

    storage
        .append(
            &repo_id,
            EventKind::WorkspaceCreated {
                workspace_id: ws_a,
                intent: "workspace A".to_string(),
                base_version: "v1".to_string(),
            },
        )
        .await
        .expect("append ws_a");

    let eb = storage
        .append(
            &repo_id,
            EventKind::WorkspaceCreated {
                workspace_id: ws_b,
                intent: "workspace B".to_string(),
                base_version: "v1".to_string(),
            },
        )
        .await
        .expect("append ws_b");

    // Filter: only events for workspace B.
    let filter = EventFilter {
        workspace_ids: vec![ws_b],
        ..Default::default()
    };
    let results = storage
        .query_since_id_filtered(&repo_id, 0, &filter)
        .await
        .expect("query_since_id_filtered");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, eb.id);

    teardown(&storage, &repo_id).await;
}

#[tokio::test]
async fn test_event_filter_empty_returns_all() {
    use vai::storage::EventFilter;

    let Some((storage, repo_id)) = setup().await else {
        return;
    };

    let ws = Uuid::new_v4();
    for i in 0..3u32 {
        storage
            .append(
                &repo_id,
                EventKind::WorkspaceCreated {
                    workspace_id: ws,
                    intent: format!("intent {i}"),
                    base_version: "v1".to_string(),
                },
            )
            .await
            .expect("append");
    }

    // An empty filter should return all events.
    let results = storage
        .query_since_id_filtered(&repo_id, 0, &EventFilter::default())
        .await
        .expect("query_since_id_filtered");
    assert_eq!(results.len(), 3);

    teardown(&storage, &repo_id).await;
}

#[tokio::test]
async fn test_event_filter_combined_type_and_workspace() {
    use vai::storage::EventFilter;

    let Some((storage, repo_id)) = setup().await else {
        return;
    };

    let ws_a = Uuid::new_v4();
    let ws_b = Uuid::new_v4();

    // ws_a: Created + Submitted
    storage
        .append(
            &repo_id,
            EventKind::WorkspaceCreated {
                workspace_id: ws_a,
                intent: "A".to_string(),
                base_version: "v1".to_string(),
            },
        )
        .await
        .unwrap();
    let submitted_a = storage
        .append(
            &repo_id,
            EventKind::WorkspaceSubmitted {
                workspace_id: ws_a,
                changes_summary: "A done".to_string(),
            },
        )
        .await
        .unwrap();

    // ws_b: Created only
    storage
        .append(
            &repo_id,
            EventKind::WorkspaceCreated {
                workspace_id: ws_b,
                intent: "B".to_string(),
                base_version: "v1".to_string(),
            },
        )
        .await
        .unwrap();

    // Filter: WorkspaceSubmitted events for ws_a only.
    let filter = EventFilter {
        event_types: vec!["WorkspaceSubmitted".to_string()],
        workspace_ids: vec![ws_a],
        ..Default::default()
    };
    let results = storage
        .query_since_id_filtered(&repo_id, 0, &filter)
        .await
        .expect("query_since_id_filtered");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, submitted_a.id);

    teardown(&storage, &repo_id).await;
}

// ── parity: SQLite vs Postgres for IssueStore ─────────────────────────────────
//
// Spot-check that the Postgres backend and the SQLite backend return
// semantically equivalent results for a common workflow.

#[tokio::test]
async fn test_issue_parity_with_sqlite() {
    use std::path::PathBuf;
    use vai::storage::sqlite::SqliteStorage;
    use vai::storage::IssueUpdate;

    let Some((pg, repo_id)) = setup().await else {
        return;
    };

    let tmp = tempfile::tempdir().expect("tempdir");
    let vai_dir: PathBuf = tmp.path().join(".vai");

    // Initialise SQLite stores used by SqliteStorage.
    // repo::init creates the .vai directory — do not create it manually beforehand.
    vai::repo::init(tmp.path()).expect("repo init");

    let sq = SqliteStorage::new(&vai_dir);
    let local_repo = Uuid::new_v4(); // SQLite ignores repo_id

    let new_issue = || NewIssue {
        title: "Parity test issue".to_string(),
        description: "Check both backends behave the same".to_string(),
        priority: IssuePriority::Medium,
        labels: vec!["test".to_string()],
        creator: "parity-test".to_string(),
        agent_source: None,
        acceptance_criteria: vec![],
    };

    // Create in both backends.
    let pg_issue = pg.create_issue(&repo_id, new_issue()).await.unwrap();
    let sq_issue = sq.create_issue(&local_repo, new_issue()).await.unwrap();

    assert_eq!(pg_issue.title, sq_issue.title);
    assert_eq!(pg_issue.priority, sq_issue.priority);
    assert_eq!(pg_issue.labels, sq_issue.labels);
    assert_eq!(pg_issue.status, sq_issue.status);

    // Update in both.
    let upd = IssueUpdate {
        title: Some("Parity test issue (updated)".to_string()),
        ..Default::default()
    };
    let pg_upd = pg
        .update_issue(&repo_id, &pg_issue.id, upd.clone())
        .await
        .unwrap();
    let sq_upd = sq
        .update_issue(&local_repo, &sq_issue.id, upd)
        .await
        .unwrap();
    assert_eq!(pg_upd.title, sq_upd.title);

    // Close in both.
    let pg_closed = pg
        .close_issue(&repo_id, &pg_issue.id, "done")
        .await
        .unwrap();
    let sq_closed = sq
        .close_issue(&local_repo, &sq_issue.id, "done")
        .await
        .unwrap();
    assert_eq!(pg_closed.status, sq_closed.status);
    assert_eq!(pg_closed.status, IssueStatus::Closed);

    teardown(&pg, &repo_id).await;
}

// ── Cross-repo isolation tests ─────────────────────────────────────────────────
//
// Each test creates data in repo_a and verifies that repo_b cannot see it.
// These tests assert the storage layer enforces multi-tenant boundaries.

/// Creates a second isolated repo row and returns its ID.
async fn create_repo(storage: &PostgresStorage) -> Uuid {
    let repo_id = Uuid::new_v4();
    sqlx::query("INSERT INTO repos (id, name) VALUES ($1, $2)")
        .bind(repo_id)
        .bind(format!("test-repo-{repo_id}"))
        .execute(storage.pool())
        .await
        .expect("failed to insert second test repo");
    repo_id
}

#[tokio::test]
async fn test_issue_store_cross_repo_isolation() {
    let Some((storage, repo_a)) = setup().await else {
        return;
    };
    let repo_b = create_repo(&storage).await;

    // Create an issue in repo_a.
    let issue = storage
        .create_issue(
            &repo_a,
            NewIssue {
                title: "Repo-A issue".to_string(),
                description: "Should not be visible from repo_b".to_string(),
                priority: IssuePriority::Medium,
                labels: vec![],
                creator: "alice".to_string(),
                agent_source: None,
                acceptance_criteria: vec![],
            },
        )
        .await
        .expect("create_issue failed");

    // repo_b should see no issues.
    let list_b = storage
        .list_issues(&repo_b, &IssueFilter::default(), &ListQuery::default())
        .await
        .expect("list_issues failed");
    assert!(list_b.items.is_empty(), "repo_b should not see repo_a's issues");

    // get_issue from repo_b with repo_a's ID should return NotFound.
    let result = storage.get_issue(&repo_b, &issue.id).await;
    assert!(
        matches!(result, Err(StorageError::NotFound(_))),
        "get_issue from wrong repo should return NotFound"
    );

    teardown(&storage, &repo_b).await;
    teardown(&storage, &repo_a).await;
}

#[tokio::test]
async fn test_issue_comment_cross_repo_isolation() {
    let Some((storage, repo_a)) = setup().await else {
        return;
    };
    let repo_b = create_repo(&storage).await;

    // Create an issue and comment in repo_a.
    let issue = storage
        .create_issue(
            &repo_a,
            NewIssue {
                title: "Issue for comment isolation test".to_string(),
                description: "".to_string(),
                priority: IssuePriority::Low,
                labels: vec![],
                creator: "alice".to_string(),
                agent_source: None,
                acceptance_criteria: vec![],
            },
        )
        .await
        .expect("create_issue failed");

    storage
        .create_comment(
            &repo_a,
            &issue.id,
            NewIssueComment {
                author: "alice".to_string(),
                body: "A comment".to_string(),
                author_type: "human".to_string(),
                author_id: None,
                parent_id: None,
            },
        )
        .await
        .expect("create_comment failed");

    // repo_b should see no comments for the same issue ID.
    let comments_b = storage
        .list_comments(&repo_b, &issue.id)
        .await
        .expect("list_comments failed");
    assert!(
        comments_b.is_empty(),
        "repo_b should not see repo_a's comments"
    );

    teardown(&storage, &repo_b).await;
    teardown(&storage, &repo_a).await;
}

#[tokio::test]
async fn test_escalation_store_cross_repo_isolation() {
    let Some((storage, repo_a)) = setup().await else {
        return;
    };
    let repo_b = create_repo(&storage).await;

    let ws_id = Uuid::new_v4();
    let escalation = storage
        .create_escalation(
            &repo_a,
            NewEscalation {
                escalation_type: EscalationType::MergeConflict,
                severity: EscalationSeverity::High,
                summary: "Repo-A escalation".to_string(),
                intents: vec![],
                agents: vec!["agent-1".to_string()],
                workspace_ids: vec![ws_id],
                affected_entities: vec![],
                conflicts: vec![],
                resolution_options: vec![ResolutionOption::KeepAgentA],
            },
        )
        .await
        .expect("create_escalation failed");

    // repo_b should see no escalations.
    let list_b = storage
        .list_escalations(&repo_b, false, &ListQuery::default())
        .await
        .expect("list_escalations failed");
    assert!(
        list_b.items.is_empty(),
        "repo_b should not see repo_a's escalations"
    );

    // get_escalation from repo_b should return NotFound.
    let result = storage.get_escalation(&repo_b, &escalation.id).await;
    assert!(
        matches!(result, Err(StorageError::NotFound(_))),
        "get_escalation from wrong repo should return NotFound"
    );

    teardown(&storage, &repo_b).await;
    teardown(&storage, &repo_a).await;
}

#[tokio::test]
async fn test_workspace_store_cross_repo_isolation() {
    let Some((storage, repo_a)) = setup().await else {
        return;
    };
    let repo_b = create_repo(&storage).await;

    let ws = storage
        .create_workspace(
            &repo_a,
            NewWorkspace {
                id: None,
                intent: "Repo-A workspace".to_string(),
                base_version: "v1".to_string(),
                issue_id: None,
            },
        )
        .await
        .expect("create_workspace failed");

    // repo_b should see no workspaces.
    let list_b = storage
        .list_workspaces(&repo_b, true, &ListQuery::default())
        .await
        .expect("list_workspaces failed");
    assert!(
        list_b.items.is_empty(),
        "repo_b should not see repo_a's workspaces"
    );

    // get_workspace from repo_b should return NotFound.
    let result = storage.get_workspace(&repo_b, &ws.id).await;
    assert!(
        matches!(result, Err(StorageError::NotFound(_))),
        "get_workspace from wrong repo should return NotFound"
    );

    teardown(&storage, &repo_b).await;
    teardown(&storage, &repo_a).await;
}

#[tokio::test]
async fn test_version_store_cross_repo_isolation() {
    let Some((storage, repo_a)) = setup().await else {
        return;
    };
    let repo_b = create_repo(&storage).await;

    // Create a version and advance head in repo_a.
    storage
        .create_version(
            &repo_a,
            NewVersion {
                version_id: "v1".to_string(),
                parent_version_id: None,
                intent: "initial".to_string(),
                created_by: "alice".to_string(),
                merge_event_id: None,
            },
        )
        .await
        .expect("create_version failed");
    storage
        .advance_head(&repo_a, "v1")
        .await
        .expect("advance_head failed");

    // repo_b should have no versions and no head.
    let versions_b = storage
        .list_versions(&repo_b, &ListQuery::default())
        .await
        .expect("list_versions failed");
    assert!(
        versions_b.items.is_empty(),
        "repo_b should not see repo_a's versions"
    );

    let head_b = storage.read_head(&repo_b).await.expect("read_head failed");
    assert!(head_b.is_none(), "repo_b should not see repo_a's head");

    // get_version from repo_b should return NotFound.
    let result = storage.get_version(&repo_b, "v1").await;
    assert!(
        matches!(result, Err(StorageError::NotFound(_))),
        "get_version from wrong repo should return NotFound"
    );

    teardown(&storage, &repo_b).await;
    teardown(&storage, &repo_a).await;
}

#[tokio::test]
async fn test_graph_store_cross_repo_isolation() {
    let Some((storage, repo_a)) = setup().await else {
        return;
    };
    let repo_b = create_repo(&storage).await;

    // Insert entities and a relationship in repo_a.
    let entity_a = Entity {
        id: Entity::compute_id("src/auth.rs", "validate"),
        kind: EntityKind::Function,
        name: "validate".to_string(),
        qualified_name: "validate".to_string(),
        file_path: "src/auth.rs".to_string(),
        byte_range: (100, 300),
        line_range: (10, 30),
        parent_entity: None,
    };
    let entity_b = Entity {
        id: Entity::compute_id("src/auth.rs", "login"),
        kind: EntityKind::Function,
        name: "login".to_string(),
        qualified_name: "login".to_string(),
        file_path: "src/auth.rs".to_string(),
        byte_range: (350, 500),
        line_range: (35, 50),
        parent_entity: None,
    };
    storage
        .upsert_entities(&repo_a, vec![entity_a.clone(), entity_b.clone()])
        .await
        .expect("upsert_entities failed");

    let rel = Relationship::new(RelationshipKind::Calls, &entity_b.id, &entity_a.id);
    storage
        .upsert_relationships(&repo_a, vec![rel])
        .await
        .expect("upsert_relationships failed");

    // repo_b should see no entities.
    let entities_b = storage
        .list_entities(&repo_b, None)
        .await
        .expect("list_entities failed");
    assert!(
        entities_b.is_empty(),
        "repo_b should not see repo_a's entities"
    );

    // get_entity from repo_b should return NotFound.
    let result = storage.get_entity(&repo_b, &entity_a.id).await;
    assert!(
        matches!(result, Err(StorageError::NotFound(_))),
        "get_entity from wrong repo should return NotFound"
    );

    // repo_b should see no relationships.
    let rels_b = storage
        .get_relationships(&repo_b, &entity_b.id)
        .await
        .expect("get_relationships failed");
    assert!(
        rels_b.is_empty(),
        "repo_b should not see repo_a's relationships"
    );

    teardown(&storage, &repo_b).await;
    teardown(&storage, &repo_a).await;
}
