//! Integration test for the issue CLI lifecycle.
//!
//! Exercises: create → list → show → update → close

use tempfile::TempDir;
use vai::event_log::EventLog;
use vai::issue::{IssueFilter, IssuePriority, IssueStatus, IssueStore};
use vai::repo;

/// Set up a temp repo and return (TempDir, vai_dir).
fn setup() -> (TempDir, std::path::PathBuf) {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    repo::init(&root).unwrap();
    let vai_dir = root.join(".vai");
    (tmp, vai_dir)
}

#[test]
fn test_issue_full_lifecycle() {
    let (_tmp, vai_dir) = setup();
    let store = IssueStore::open(&vai_dir).unwrap();
    let mut log = EventLog::open(&vai_dir).unwrap();

    // Create two issues.
    let i1 = store
        .create("Fix login bug", "Auth is broken", IssuePriority::High, vec!["bug".into()], "alice", &mut log)
        .unwrap();
    let i2 = store
        .create("Add rate limiting", "", IssuePriority::Medium, vec!["feature".into()], "bob", &mut log)
        .unwrap();

    assert_eq!(i1.status, IssueStatus::Open);
    assert_eq!(i2.priority, IssuePriority::Medium);

    // List all issues — should return both.
    let all = store.list(&IssueFilter::default()).unwrap();
    assert_eq!(all.len(), 2);

    // Filter by priority.
    let high_priority = store
        .list(&IssueFilter { priority: Some(IssuePriority::High), ..Default::default() })
        .unwrap();
    assert_eq!(high_priority.len(), 1);
    assert_eq!(high_priority[0].title, "Fix login bug");

    // Filter by label.
    let bugs = store
        .list(&IssueFilter { label: Some("bug".into()), ..Default::default() })
        .unwrap();
    assert_eq!(bugs.len(), 1);

    // Filter by creator.
    let bobs = store
        .list(&IssueFilter { creator: Some("bob".into()), ..Default::default() })
        .unwrap();
    assert_eq!(bobs.len(), 1);
    assert_eq!(bobs[0].title, "Add rate limiting");

    // Update issue 1 — change priority and add a label.
    let updated = store
        .update(
            i1.id,
            None,
            None,
            Some(IssuePriority::Critical),
            Some(vec!["bug".into(), "auth".into()]),
            &mut log,
        )
        .unwrap();
    assert_eq!(updated.priority, IssuePriority::Critical);
    assert!(updated.labels.contains(&"auth".to_string()));

    // Show issue 1.
    let fetched = store.get(i1.id).unwrap();
    assert_eq!(fetched.title, "Fix login bug");
    assert_eq!(fetched.priority, IssuePriority::Critical);

    // Close issue 2 as wontfix.
    let closed = store.close(i2.id, "wontfix", &mut log).unwrap();
    assert_eq!(closed.status, IssueStatus::Closed);
    assert_eq!(closed.resolution.as_deref(), Some("wontfix"));

    // Filter by status open — should show only i1.
    let open_issues = store
        .list(&IssueFilter { status: Some(IssueStatus::Open), ..Default::default() })
        .unwrap();
    assert_eq!(open_issues.len(), 1);
    assert_eq!(open_issues[0].id, i1.id);

    // Filter by status closed.
    let closed_issues = store
        .list(&IssueFilter { status: Some(IssueStatus::Closed), ..Default::default() })
        .unwrap();
    assert_eq!(closed_issues.len(), 1);
    assert_eq!(closed_issues[0].id, i2.id);
}

#[test]
fn test_issue_workspace_link_and_resolve() {
    let (_tmp, vai_dir) = setup();
    let store = IssueStore::open(&vai_dir).unwrap();
    let mut log = EventLog::open(&vai_dir).unwrap();

    let issue = store
        .create("Implement feature X", "", IssuePriority::Medium, vec![], "agent-01", &mut log)
        .unwrap();

    // Link to a workspace → transitions to InProgress.
    let ws_id = uuid::Uuid::new_v4();
    let in_progress = store.set_in_progress(issue.id, ws_id, &mut log).unwrap();
    assert_eq!(in_progress.status, IssueStatus::InProgress);

    // Verify linked workspaces.
    let linked = store.linked_workspaces(issue.id).unwrap();
    assert_eq!(linked.len(), 1);
    assert_eq!(linked[0], ws_id);

    // Merge workspace → resolve issue.
    let resolved = store.resolve(issue.id, Some("v2".into()), &mut log).unwrap();
    assert_eq!(resolved.status, IssueStatus::Resolved);
}

#[test]
fn test_issue_reopen_on_workspace_discard() {
    let (_tmp, vai_dir) = setup();
    let store = IssueStore::open(&vai_dir).unwrap();
    let mut log = EventLog::open(&vai_dir).unwrap();

    let issue = store
        .create("Retry task", "", IssuePriority::Low, vec![], "agent-02", &mut log)
        .unwrap();

    let ws_id = uuid::Uuid::new_v4();
    store.set_in_progress(issue.id, ws_id, &mut log).unwrap();

    // Discard workspace → reopen issue.
    let reopened = store.reopen(issue.id, &mut log).unwrap();
    assert_eq!(reopened.status, IssueStatus::Open);
}
