//! Integration test for issue-workspace linking (Phase 3 PRD 3.1).
//!
//! Exercises: create issue → create workspace linked to issue → submit →
//! verify issue resolved. Also tests workspace discard → issue reopen.

use tempfile::TempDir;
use vai::event_log::EventLog;
use vai::issue::{IssueFilter, IssuePriority, IssueStatus, IssueStore};
use vai::merge;
use vai::repo;
use vai::workspace;

/// Set up a temp repo and return (TempDir, root, vai_dir).
fn setup() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    repo::init(&root).unwrap();
    let vai_dir = root.join(".vai");
    (tmp, root, vai_dir)
}

#[test]
fn test_workspace_create_links_issue_to_in_progress() {
    let (_tmp, _root, vai_dir) = setup();
    let store = IssueStore::open(&vai_dir).unwrap();
    let mut log = EventLog::open(&vai_dir).unwrap();

    // Create an issue.
    let issue = store
        .create("Fix auth bug", "Details", IssuePriority::High, vec![], "alice", &mut log)
        .unwrap();
    assert_eq!(issue.status, IssueStatus::Open);

    // Create a workspace and link it to the issue.
    let head = repo::read_head(&vai_dir).unwrap();
    let ws_result = workspace::create(&vai_dir, "fix auth bug implementation", &head).unwrap();
    let mut ws_meta = ws_result.workspace;

    // Link workspace to issue (as the CLI handler would do).
    store.set_in_progress(issue.id, ws_meta.id, &mut log).unwrap();
    ws_meta.issue_id = Some(issue.id);
    workspace::update_meta(&vai_dir, &ws_meta).unwrap();

    // Verify issue transitioned to InProgress.
    let updated = store.get(issue.id).unwrap();
    assert_eq!(updated.status, IssueStatus::InProgress);

    // Verify linked workspaces.
    let linked = store.linked_workspaces(issue.id).unwrap();
    assert_eq!(linked.len(), 1);
    assert_eq!(linked[0], ws_meta.id);

    // Verify workspace meta persisted issue_id.
    let persisted = workspace::get(&vai_dir, &ws_meta.id.to_string()).unwrap();
    assert_eq!(persisted.issue_id, Some(issue.id));
}

#[test]
fn test_workspace_discard_reopens_issue() {
    let (_tmp, _root, vai_dir) = setup();
    let store = IssueStore::open(&vai_dir).unwrap();
    let mut log = EventLog::open(&vai_dir).unwrap();

    let issue = store
        .create("Retry task", "", IssuePriority::Medium, vec![], "agent-01", &mut log)
        .unwrap();

    let head = repo::read_head(&vai_dir).unwrap();
    let ws_result = workspace::create(&vai_dir, "retry task impl", &head).unwrap();
    let mut ws_meta = ws_result.workspace;

    // Link issue.
    store.set_in_progress(issue.id, ws_meta.id, &mut log).unwrap();
    ws_meta.issue_id = Some(issue.id);
    workspace::update_meta(&vai_dir, &ws_meta).unwrap();
    assert_eq!(store.get(issue.id).unwrap().status, IssueStatus::InProgress);

    // Discard the workspace.
    let discarded = workspace::discard(&vai_dir, &ws_meta.id.to_string(), None).unwrap();
    assert_eq!(discarded.issue_id, Some(issue.id));

    // Reopen the issue (as the CLI discard handler does).
    store.reopen(issue.id, &mut log).unwrap();

    let reopened = store.get(issue.id).unwrap();
    assert_eq!(reopened.status, IssueStatus::Open);
}

#[test]
fn test_workspace_submit_resolves_issue() {
    let (_tmp, root, vai_dir) = setup();
    let store = IssueStore::open(&vai_dir).unwrap();
    let mut log = EventLog::open(&vai_dir).unwrap();

    let issue = store
        .create("Implement feature X", "", IssuePriority::Medium, vec![], "agent-02", &mut log)
        .unwrap();

    let head = repo::read_head(&vai_dir).unwrap();
    let ws_result = workspace::create(&vai_dir, "implement feature X", &head).unwrap();
    let mut ws_meta = ws_result.workspace;

    // Link issue.
    store.set_in_progress(issue.id, ws_meta.id, &mut log).unwrap();
    ws_meta.issue_id = Some(issue.id);
    workspace::update_meta(&vai_dir, &ws_meta).unwrap();
    assert_eq!(store.get(issue.id).unwrap().status, IssueStatus::InProgress);

    // Submit workspace (local merge).
    let submit_result = merge::submit(&vai_dir, &root).unwrap();
    let new_version_id = submit_result.version.version_id.clone();

    // Resolve the issue (as the CLI submit handler does).
    store.resolve(issue.id, Some(new_version_id.clone()), &mut log).unwrap();

    let resolved = store.get(issue.id).unwrap();
    assert_eq!(resolved.status, IssueStatus::Resolved);
    assert_eq!(resolved.resolution.as_deref(), Some("resolved"));
}

#[test]
fn test_issue_show_displays_linked_workspaces() {
    let (_tmp, _root, vai_dir) = setup();
    let store = IssueStore::open(&vai_dir).unwrap();
    let mut log = EventLog::open(&vai_dir).unwrap();

    let issue = store
        .create("Multi-workspace issue", "", IssuePriority::Low, vec![], "alice", &mut log)
        .unwrap();

    // Link two workspaces (retry scenario).
    let ws1 = uuid::Uuid::new_v4();
    let ws2 = uuid::Uuid::new_v4();
    store.set_in_progress(issue.id, ws1, &mut log).unwrap();
    store.set_in_progress(issue.id, ws2, &mut log).unwrap();

    let linked = store.linked_workspaces(issue.id).unwrap();
    assert_eq!(linked.len(), 2);
    assert!(linked.contains(&ws1));
    assert!(linked.contains(&ws2));
}

#[test]
fn test_issue_filter_by_status_after_transitions() {
    let (_tmp, _root, vai_dir) = setup();
    let store = IssueStore::open(&vai_dir).unwrap();
    let mut log = EventLog::open(&vai_dir).unwrap();

    let i1 = store.create("Open issue", "", IssuePriority::Low, vec![], "alice", &mut log).unwrap();
    let i2 = store.create("In progress issue", "", IssuePriority::Medium, vec![], "alice", &mut log).unwrap();
    let i3 = store.create("Resolved issue", "", IssuePriority::High, vec![], "alice", &mut log).unwrap();

    let ws2 = uuid::Uuid::new_v4();
    let ws3 = uuid::Uuid::new_v4();
    store.set_in_progress(i2.id, ws2, &mut log).unwrap();
    store.set_in_progress(i3.id, ws3, &mut log).unwrap();
    store.resolve(i3.id, Some("v2".into()), &mut log).unwrap();

    let open_issues = store.list(&IssueFilter { status: Some(IssueStatus::Open), ..Default::default() }).unwrap();
    assert_eq!(open_issues.len(), 1);
    assert_eq!(open_issues[0].id, i1.id);

    let in_progress = store.list(&IssueFilter { status: Some(IssueStatus::InProgress), ..Default::default() }).unwrap();
    assert_eq!(in_progress.len(), 1);
    assert_eq!(in_progress[0].id, i2.id);

    let resolved = store.list(&IssueFilter { status: Some(IssueStatus::Resolved), ..Default::default() }).unwrap();
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].id, i3.id);
}
