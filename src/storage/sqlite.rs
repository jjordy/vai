//! SQLite implementation of all storage traits.
//!
//! [`SqliteStorage`] delegates to the existing per-module store types and free
//! functions that already use SQLite and the local filesystem. This wrapper makes
//! the concrete implementations available behind the uniform trait API defined in
//! [`super`].
//!
//! In local CLI mode every method ignores `repo_id` — there is always exactly one
//! repository in a `.vai/` directory. The parameter is accepted for interface
//! compatibility so the same trait can be used with the Postgres backend.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::auth;
use crate::escalation::{self, EscalationStatus};
use crate::event_log::EventLog;
use crate::graph::GraphSnapshot;
use crate::issue::{self, IssueStatus};
use crate::version;
use crate::workspace;

use super::{
    AttachmentStore, AuthStore, CommentStore, EscalationStore, EventStore, FileMetadata, FileStore,
    GraphStore, IssueAttachment, IssueComment, IssueLink, IssueLinkStore, IssueStore, IssueUpdate,
    NewEscalation, NewIssue, NewIssueAttachment, NewIssueComment, NewIssueLink, NewOrg, NewUser,
    NewVersion, NewWorkspace, OrgMember, OrgRole, OrgStore, Organization, RepoCollaborator,
    RepoRole, StorageError, User, VersionStore, WatcherRegistryStore, WorkspaceStore, WorkspaceUpdate,
};
use crate::watcher::{DiscoveryEventKind, DiscoveryPreparation, DiscoveryRecord, Watcher, WatcherStore};
use crate::auth::ApiKey;
use crate::escalation::{Escalation, ResolutionOption};
use crate::event_log::{Event, EventKind};
use crate::graph::{Entity, Relationship};
use crate::issue::{Issue, IssueFilter};
use crate::version::VersionMeta;
use crate::workspace::WorkspaceMeta;

// ── SqliteStorage ─────────────────────────────────────────────────────────────

/// SQLite-backed storage for a single local vai repository.
///
/// All trait methods ignore the `repo_id` parameter and operate on the single
/// repository located at `vai_dir` (`.vai/` in the project root).
#[derive(Clone, Debug)]
pub struct SqliteStorage {
    /// Path to the `.vai/` directory for this repository.
    vai_dir: PathBuf,
}

impl SqliteStorage {
    /// Creates a new `SqliteStorage` backed by the given `.vai/` directory.
    pub fn new(vai_dir: impl Into<PathBuf>) -> Self {
        Self {
            vai_dir: vai_dir.into(),
        }
    }

    /// Returns the `.vai/` directory path.
    pub fn vai_dir(&self) -> &Path {
        &self.vai_dir
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    fn open_event_log(&self) -> Result<EventLog, StorageError> {
        let log_dir = self.vai_dir.join("event_log");
        EventLog::open(&log_dir).map_err(|e| StorageError::Io(e.to_string()))
    }

    fn open_issue_store(&self) -> Result<issue::IssueStore, StorageError> {
        issue::IssueStore::open(&self.vai_dir)
            .map_err(|e| StorageError::Database(e.to_string()))
    }

    fn open_escalation_store(&self) -> Result<escalation::EscalationStore, StorageError> {
        escalation::EscalationStore::open(&self.vai_dir)
            .map_err(|e| StorageError::Database(e.to_string()))
    }

    fn open_graph(&self) -> Result<GraphSnapshot, StorageError> {
        let path = self.vai_dir.join("graph").join("snapshot.db");
        GraphSnapshot::open(&path).map_err(|e| StorageError::Database(e.to_string()))
    }
}

// ── EventStore ────────────────────────────────────────────────────────────────

#[async_trait]
impl EventStore for SqliteStorage {
    async fn append(&self, _repo_id: &Uuid, event: EventKind) -> Result<Event, StorageError> {
        let mut log = self.open_event_log()?;
        log.append(event)
            .map_err(|e| StorageError::Database(e.to_string()))
    }

    async fn query_by_type(
        &self,
        _repo_id: &Uuid,
        event_type: &str,
    ) -> Result<Vec<Event>, StorageError> {
        let log = self.open_event_log()?;
        log.query_by_type(event_type)
            .map_err(|e| StorageError::Database(e.to_string()))
    }

    async fn query_by_workspace(
        &self,
        _repo_id: &Uuid,
        workspace_id: &Uuid,
    ) -> Result<Vec<Event>, StorageError> {
        let log = self.open_event_log()?;
        log.query_by_workspace(*workspace_id)
            .map_err(|e| StorageError::Database(e.to_string()))
    }

    async fn query_by_time_range(
        &self,
        _repo_id: &Uuid,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<Event>, StorageError> {
        let log = self.open_event_log()?;
        log.query_by_time_range(from, to)
            .map_err(|e| StorageError::Database(e.to_string()))
    }

    async fn query_since_id(
        &self,
        _repo_id: &Uuid,
        last_id: i64,
    ) -> Result<Vec<Event>, StorageError> {
        let log = self.open_event_log()?;
        let all = log
            .all()
            .map_err(|e| StorageError::Database(e.to_string()))?;
        Ok(all
            .into_iter()
            .filter(|e| e.id as i64 > last_id)
            .collect())
    }

    async fn count(&self, _repo_id: &Uuid) -> Result<u64, StorageError> {
        let log = self.open_event_log()?;
        log.count()
            .map_err(|e| StorageError::Database(e.to_string()))
    }
}

// ── IssueStore ────────────────────────────────────────────────────────────────

#[async_trait]
impl IssueStore for SqliteStorage {
    async fn create_issue(
        &self,
        _repo_id: &Uuid,
        issue: NewIssue,
    ) -> Result<Issue, StorageError> {
        let store = self.open_issue_store()?;
        let mut log = self.open_event_log()?;
        let mut created = store
            .create(
                issue.title,
                issue.description,
                issue.priority,
                issue.labels,
                issue.creator,
                &mut log,
            )
            .map_err(|e| StorageError::Database(e.to_string()))?;

        // If agent_source metadata was provided, persist it now.
        // The legacy `create()` method doesn't accept this parameter, so we
        // apply it as a follow-up UPDATE and attach it to the returned value.
        if let Some(ref src) = issue.agent_source {
            let src_str = src.to_string();
            store
                .set_agent_source(created.id, &src_str)
                .map_err(|e| StorageError::Database(e.to_string()))?;

            // Attach the parsed agent_source to the returned issue.
            created.agent_source = serde_json::from_value(src.clone()).ok();
        }

        // Persist acceptance criteria if provided.
        if !issue.acceptance_criteria.is_empty() {
            store
                .set_acceptance_criteria(created.id, &issue.acceptance_criteria)
                .map_err(|e| StorageError::Database(e.to_string()))?;
            created.acceptance_criteria = issue.acceptance_criteria.clone();
        }

        Ok(created)
    }

    async fn get_issue(
        &self,
        _repo_id: &Uuid,
        id: &Uuid,
    ) -> Result<Issue, StorageError> {
        let store = self.open_issue_store()?;
        store.get(*id).map_err(|e| match e {
            issue::IssueError::NotFound(_) => {
                StorageError::NotFound(format!("issue {id}"))
            }
            other => StorageError::Database(other.to_string()),
        })
    }

    async fn list_issues(
        &self,
        _repo_id: &Uuid,
        filter: &IssueFilter,
    ) -> Result<Vec<Issue>, StorageError> {
        let store = self.open_issue_store()?;
        store
            .list(filter)
            .map_err(|e| StorageError::Database(e.to_string()))
    }

    async fn update_issue(
        &self,
        _repo_id: &Uuid,
        id: &Uuid,
        update: IssueUpdate,
    ) -> Result<Issue, StorageError> {
        let store = self.open_issue_store()?;
        let mut log = self.open_event_log()?;

        // Apply basic field updates (title, description, priority, labels).
        let has_field_updates = update.title.is_some()
            || update.description.is_some()
            || update.priority.is_some()
            || update.labels.is_some();

        if has_field_updates {
            store
                .update(
                    *id,
                    update.title,
                    update.description,
                    update.priority,
                    update.labels,
                    &mut log,
                )
                .map_err(|e| StorageError::Database(e.to_string()))?;
        }

        // Apply status transitions.
        if let Some(status) = update.status {
            match status {
                IssueStatus::InProgress => {
                    if let Some(ws_id) = update.workspace_id {
                        store
                            .set_in_progress(*id, ws_id, &mut log)
                            .map_err(|e| match e {
                                issue::IssueError::InvalidTransition { from, to } => {
                                    StorageError::InvalidTransition(format!(
                                        "{from} → {to}"
                                    ))
                                }
                                other => StorageError::Database(other.to_string()),
                            })?;
                    }
                }
                IssueStatus::Open => {
                    store
                        .reopen(*id, &mut log)
                        .map_err(|e| match e {
                            issue::IssueError::InvalidTransition { from, to } => {
                                StorageError::InvalidTransition(format!("{from} → {to}"))
                            }
                            other => StorageError::Database(other.to_string()),
                        })?;
                }
                IssueStatus::Resolved => {
                    store
                        .resolve(*id, update.resolution.clone(), &mut log)
                        .map_err(|e| match e {
                            issue::IssueError::InvalidTransition { from, to } => {
                                StorageError::InvalidTransition(format!("{from} → {to}"))
                            }
                            other => StorageError::Database(other.to_string()),
                        })?;
                }
                // Closed status is handled by close_issue.
                IssueStatus::Closed => {
                    let resolution = update.resolution.unwrap_or_else(|| "closed".to_string());
                    store
                        .close(*id, &resolution, &mut log)
                        .map_err(|e| StorageError::Database(e.to_string()))?;
                }
            }
        }

        // Update acceptance criteria if provided.
        if let Some(criteria) = update.acceptance_criteria {
            store
                .set_acceptance_criteria(*id, &criteria)
                .map_err(|e| StorageError::Database(e.to_string()))?;
        }

        // Return the updated issue.
        store.get(*id).map_err(|e| match e {
            issue::IssueError::NotFound(_) => StorageError::NotFound(format!("issue {id}")),
            other => StorageError::Database(other.to_string()),
        })
    }

    async fn close_issue(
        &self,
        _repo_id: &Uuid,
        id: &Uuid,
        resolution: &str,
    ) -> Result<Issue, StorageError> {
        let store = self.open_issue_store()?;
        let mut log = self.open_event_log()?;
        store
            .close(*id, resolution, &mut log)
            .map_err(|e| match e {
                issue::IssueError::NotFound(_) => {
                    StorageError::NotFound(format!("issue {id}"))
                }
                other => StorageError::Database(other.to_string()),
            })
    }
}

// ── CommentStore ──────────────────────────────────────────────────────────────

#[async_trait]
impl CommentStore for SqliteStorage {
    async fn create_comment(
        &self,
        _repo_id: &Uuid,
        issue_id: &Uuid,
        comment: NewIssueComment,
    ) -> Result<IssueComment, StorageError> {
        let store = self.open_issue_store()?;
        store
            .create_comment(
                *issue_id,
                &comment.author,
                &comment.body,
                &comment.author_type,
                comment.author_id.as_deref(),
            )
            .map_err(|e| StorageError::Database(e.to_string()))
    }

    async fn list_comments(
        &self,
        _repo_id: &Uuid,
        issue_id: &Uuid,
    ) -> Result<Vec<IssueComment>, StorageError> {
        let store = self.open_issue_store()?;
        store
            .list_comments(*issue_id)
            .map_err(|e| StorageError::Database(e.to_string()))
    }
}

// ── IssueLinkStore ────────────────────────────────────────────────────────────

#[async_trait]
impl IssueLinkStore for SqliteStorage {
    async fn create_link(
        &self,
        _repo_id: &Uuid,
        _source_id: &Uuid,
        _link: NewIssueLink,
    ) -> Result<IssueLink, StorageError> {
        Err(StorageError::Database(
            "Issue links are not supported in local SQLite mode".into(),
        ))
    }

    async fn list_links(
        &self,
        _repo_id: &Uuid,
        _issue_id: &Uuid,
    ) -> Result<Vec<IssueLink>, StorageError> {
        Ok(vec![])
    }

    async fn delete_link(
        &self,
        _repo_id: &Uuid,
        _source_id: &Uuid,
        _target_id: &Uuid,
    ) -> Result<(), StorageError> {
        Ok(())
    }
}

// ── AttachmentStore ───────────────────────────────────────────────────────────

#[async_trait]
impl AttachmentStore for SqliteStorage {
    async fn create_attachment(
        &self,
        _repo_id: &Uuid,
        _issue_id: &Uuid,
        _attachment: NewIssueAttachment,
    ) -> Result<IssueAttachment, StorageError> {
        Err(StorageError::Database(
            "Issue attachments are not supported in local SQLite mode".into(),
        ))
    }

    async fn list_attachments(
        &self,
        _repo_id: &Uuid,
        _issue_id: &Uuid,
    ) -> Result<Vec<IssueAttachment>, StorageError> {
        Ok(vec![])
    }

    async fn get_attachment(
        &self,
        _repo_id: &Uuid,
        _issue_id: &Uuid,
        filename: &str,
    ) -> Result<IssueAttachment, StorageError> {
        Err(StorageError::NotFound(format!(
            "attachment '{filename}' not found (SQLite mode)"
        )))
    }

    async fn delete_attachment(
        &self,
        _repo_id: &Uuid,
        _issue_id: &Uuid,
        _filename: &str,
    ) -> Result<(), StorageError> {
        Ok(())
    }
}

// ── EscalationStore ───────────────────────────────────────────────────────────

#[async_trait]
impl EscalationStore for SqliteStorage {
    async fn create_escalation(
        &self,
        _repo_id: &Uuid,
        esc: NewEscalation,
    ) -> Result<Escalation, StorageError> {
        let store = self.open_escalation_store()?;
        let mut log = self.open_event_log()?;
        store
            .create(
                esc.escalation_type,
                esc.severity,
                esc.summary,
                esc.intents,
                esc.agents,
                esc.workspace_ids,
                esc.affected_entities,
                esc.conflicts,
                &mut log,
            )
            .map_err(|e| StorageError::Database(e.to_string()))
    }

    async fn get_escalation(
        &self,
        _repo_id: &Uuid,
        id: &Uuid,
    ) -> Result<Escalation, StorageError> {
        let store = self.open_escalation_store()?;
        store.get(*id).map_err(|e| match e {
            escalation::EscalationError::NotFound(_) => {
                StorageError::NotFound(format!("escalation {id}"))
            }
            other => StorageError::Database(other.to_string()),
        })
    }

    async fn list_escalations(
        &self,
        _repo_id: &Uuid,
        pending_only: bool,
    ) -> Result<Vec<Escalation>, StorageError> {
        let store = self.open_escalation_store()?;
        let status_filter = if pending_only {
            Some(EscalationStatus::Pending)
        } else {
            None
        };
        store
            .list(status_filter.as_ref())
            .map_err(|e| StorageError::Database(e.to_string()))
    }

    async fn resolve_escalation(
        &self,
        _repo_id: &Uuid,
        id: &Uuid,
        resolution: ResolutionOption,
        resolved_by: &str,
    ) -> Result<Escalation, StorageError> {
        let store = self.open_escalation_store()?;
        let mut log = self.open_event_log()?;
        store
            .resolve(*id, resolution, resolved_by.to_string(), &mut log)
            .map_err(|e| match e {
                escalation::EscalationError::NotFound(_) => {
                    StorageError::NotFound(format!("escalation {id}"))
                }
                escalation::EscalationError::AlreadyResolved(_) => {
                    StorageError::InvalidTransition("escalation already resolved".to_string())
                }
                other => StorageError::Database(other.to_string()),
            })
    }
}

// ── GraphStore ────────────────────────────────────────────────────────────────

#[async_trait]
impl GraphStore for SqliteStorage {
    async fn upsert_entities(
        &self,
        _repo_id: &Uuid,
        entities: Vec<Entity>,
    ) -> Result<(), StorageError> {
        let snap = self.open_graph()?;
        for entity in &entities {
            snap.upsert_entity(entity)
                .map_err(|e| StorageError::Database(e.to_string()))?;
        }
        Ok(())
    }

    async fn upsert_relationships(
        &self,
        _repo_id: &Uuid,
        rels: Vec<Relationship>,
    ) -> Result<(), StorageError> {
        let snap = self.open_graph()?;
        for rel in &rels {
            snap.upsert_relationship(rel)
                .map_err(|e| StorageError::Database(e.to_string()))?;
        }
        Ok(())
    }

    async fn get_entity(
        &self,
        _repo_id: &Uuid,
        id: &str,
    ) -> Result<Entity, StorageError> {
        let snap = self.open_graph()?;
        snap.get_entity_by_id(id)
            .map_err(|e| StorageError::Database(e.to_string()))?
            .ok_or_else(|| StorageError::NotFound(format!("entity {id}")))
    }

    async fn list_entities(
        &self,
        _repo_id: &Uuid,
        file_path: Option<&str>,
    ) -> Result<Vec<Entity>, StorageError> {
        let snap = self.open_graph()?;
        if let Some(fp) = file_path {
            snap.get_entities_in_file(fp)
                .map_err(|e| StorageError::Database(e.to_string()))
        } else {
            snap.all_entities()
                .map_err(|e| StorageError::Database(e.to_string()))
        }
    }

    async fn get_relationships(
        &self,
        _repo_id: &Uuid,
        from_entity_id: &str,
    ) -> Result<Vec<Relationship>, StorageError> {
        let snap = self.open_graph()?;
        snap.get_outgoing_relationships(from_entity_id)
            .map_err(|e| StorageError::Database(e.to_string()))
    }

    async fn clear_file(
        &self,
        _repo_id: &Uuid,
        file_path: &str,
    ) -> Result<(), StorageError> {
        let snap = self.open_graph()?;
        snap.remove_file(file_path)
            .map_err(|e| StorageError::Database(e.to_string()))
    }
}

// ── VersionStore ──────────────────────────────────────────────────────────────

#[async_trait]
impl VersionStore for SqliteStorage {
    async fn create_version(
        &self,
        _repo_id: &Uuid,
        v: NewVersion,
    ) -> Result<VersionMeta, StorageError> {
        version::create_version(
            &self.vai_dir,
            &v.version_id,
            v.parent_version_id.as_deref(),
            &v.intent,
            &v.created_by,
            v.merge_event_id,
        )
        .map_err(|e| StorageError::Io(e.to_string()))
    }

    async fn get_version(
        &self,
        _repo_id: &Uuid,
        version_id: &str,
    ) -> Result<VersionMeta, StorageError> {
        version::get_version(&self.vai_dir, version_id).map_err(|e| match e {
            version::VersionError::NotFound(id) => StorageError::NotFound(format!("version {id}")),
            other => StorageError::Io(other.to_string()),
        })
    }

    async fn list_versions(&self, _repo_id: &Uuid) -> Result<Vec<VersionMeta>, StorageError> {
        version::list_versions(&self.vai_dir)
            .map_err(|e| StorageError::Io(e.to_string()))
    }

    async fn read_head(&self, _repo_id: &Uuid) -> Result<Option<String>, StorageError> {
        let head_path = self.vai_dir.join("head");
        match fs::read_to_string(&head_path) {
            Ok(s) => Ok(Some(s.trim().to_string())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(StorageError::Io(e.to_string())),
        }
    }

    async fn advance_head(
        &self,
        _repo_id: &Uuid,
        version_id: &str,
    ) -> Result<(), StorageError> {
        let head_path = self.vai_dir.join("head");
        fs::write(&head_path, format!("{version_id}\n"))
            .map_err(|e| StorageError::Io(e.to_string()))
    }
}

// ── WorkspaceStore ────────────────────────────────────────────────────────────

#[async_trait]
impl WorkspaceStore for SqliteStorage {
    async fn create_workspace(
        &self,
        _repo_id: &Uuid,
        ws: NewWorkspace,
    ) -> Result<WorkspaceMeta, StorageError> {
        let result = if let Some(id) = ws.id {
            workspace::create_with_id(&self.vai_dir, &ws.intent, &ws.base_version, id)
        } else {
            workspace::create(&self.vai_dir, &ws.intent, &ws.base_version)
        };

        result
            .map(|r| {
                // Link issue if provided (update meta on disk).
                if let Some(issue_id) = ws.issue_id {
                    let mut meta = r.workspace.clone();
                    meta.issue_id = Some(issue_id);
                    meta.updated_at = Utc::now();
                    let _ = workspace::update_meta(&self.vai_dir, &meta);
                    meta
                } else {
                    r.workspace
                }
            })
            .map_err(|e| StorageError::Io(e.to_string()))
    }

    async fn get_workspace(
        &self,
        _repo_id: &Uuid,
        id: &Uuid,
    ) -> Result<WorkspaceMeta, StorageError> {
        workspace::get(&self.vai_dir, &id.to_string()).map_err(|e| match e {
            workspace::WorkspaceError::NotFound(msg) => StorageError::NotFound(msg),
            other => StorageError::Io(other.to_string()),
        })
    }

    async fn list_workspaces(
        &self,
        _repo_id: &Uuid,
        include_inactive: bool,
    ) -> Result<Vec<WorkspaceMeta>, StorageError> {
        if include_inactive {
            workspace::list_all(&self.vai_dir)
        } else {
            workspace::list(&self.vai_dir)
        }
        .map_err(|e| StorageError::Io(e.to_string()))
    }

    async fn update_workspace(
        &self,
        _repo_id: &Uuid,
        id: &Uuid,
        update: WorkspaceUpdate,
    ) -> Result<WorkspaceMeta, StorageError> {
        let mut meta =
            workspace::get(&self.vai_dir, &id.to_string()).map_err(|e| match e {
                workspace::WorkspaceError::NotFound(msg) => StorageError::NotFound(msg),
                other => StorageError::Io(other.to_string()),
            })?;

        if let Some(status) = update.status {
            meta.status = status;
        }
        if let Some(issue_id) = update.issue_id {
            meta.issue_id = Some(issue_id);
        }
        if let Some(deleted_paths) = update.deleted_paths {
            meta.deleted_paths = deleted_paths;
        }
        meta.updated_at = Utc::now();

        workspace::update_meta(&self.vai_dir, &meta)
            .map_err(|e| StorageError::Io(e.to_string()))?;

        Ok(meta)
    }

    async fn discard_workspace(
        &self,
        _repo_id: &Uuid,
        id: &Uuid,
    ) -> Result<(), StorageError> {
        workspace::discard(&self.vai_dir, &id.to_string(), None)
            .map(|_| ())
            .map_err(|e| match e {
                workspace::WorkspaceError::NotFound(msg) => StorageError::NotFound(msg),
                other => StorageError::Io(other.to_string()),
            })
    }
}

// ── AuthStore ─────────────────────────────────────────────────────────────────

#[async_trait]
impl AuthStore for SqliteStorage {
    async fn create_key(
        &self,
        _repo_id: Option<&Uuid>,
        name: &str,
        _user_id: Option<&Uuid>,
        _role_override: Option<&str>,
        _agent_type: Option<&str>,
        _expires_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<(ApiKey, String), StorageError> {
        // SQLite (local) mode has no user/RBAC concept; user_id, role_override,
        // agent_type, and expires_at are ignored. Keys are stored in the local keys.db.
        auth::create(&self.vai_dir, name).map_err(|e| match e {
            auth::AuthError::Duplicate(n) => StorageError::Conflict(format!("key name '{n}' already exists")),
            other => StorageError::Database(other.to_string()),
        })
    }

    async fn validate_key(&self, token: &str) -> Result<ApiKey, StorageError> {
        auth::validate(&self.vai_dir, token)
            .map_err(|e| StorageError::Database(e.to_string()))?
            .ok_or_else(|| StorageError::NotFound("API key not found or revoked".to_string()))
    }

    async fn list_keys(
        &self,
        _repo_id: Option<&Uuid>,
    ) -> Result<Vec<ApiKey>, StorageError> {
        auth::list(&self.vai_dir)
            .map_err(|e| StorageError::Database(e.to_string()))
    }

    async fn list_keys_by_user(&self, _user_id: &Uuid) -> Result<Vec<ApiKey>, StorageError> {
        // SQLite (local) mode has no user concept; return all keys.
        auth::list(&self.vai_dir).map_err(|e| StorageError::Database(e.to_string()))
    }

    async fn revoke_key(&self, id: &str) -> Result<(), StorageError> {
        // The existing auth module revokes by name, but the trait uses the record ID.
        // Look up the key by ID and revoke it directly via SQL.
        let keys = auth::list(&self.vai_dir)
            .map_err(|e| StorageError::Database(e.to_string()))?;
        let key = keys
            .into_iter()
            .find(|k| k.id == id)
            .ok_or_else(|| StorageError::NotFound(format!("API key {id}")))?;
        auth::revoke(&self.vai_dir, &key.name).map_err(|e| match e {
            auth::AuthError::NotFound(n) => StorageError::NotFound(n),
            other => StorageError::Database(other.to_string()),
        })
    }

    async fn validate_session(&self, _session_token: &str) -> Result<Uuid, StorageError> {
        // Session exchange is a server-only (Postgres) feature. SQLite mode has
        // no Better Auth integration.
        Err(StorageError::Database(
            "session_exchange grant is not supported in local (SQLite) mode".to_string(),
        ))
    }

    async fn create_refresh_token(
        &self,
        _user_id: &Uuid,
        _expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<String, StorageError> {
        // Refresh tokens are a server-only (Postgres) feature.
        Err(StorageError::Database(
            "refresh tokens are not supported in local (SQLite) mode".to_string(),
        ))
    }

    async fn validate_refresh_token(&self, _token: &str) -> Result<Uuid, StorageError> {
        Err(StorageError::Database(
            "refresh tokens are not supported in local (SQLite) mode".to_string(),
        ))
    }

    async fn revoke_refresh_token(&self, _token: &str) -> Result<(), StorageError> {
        Err(StorageError::Database(
            "refresh tokens are not supported in local (SQLite) mode".to_string(),
        ))
    }

    async fn revoke_keys_by_repo(&self, _repo_id: &Uuid) -> Result<u64, StorageError> {
        Err(StorageError::Database(
            "bulk revocation by repo is not supported in local (SQLite) mode".to_string(),
        ))
    }

    async fn revoke_keys_by_user(&self, _user_id: &Uuid) -> Result<u64, StorageError> {
        Err(StorageError::Database(
            "bulk revocation by user is not supported in local (SQLite) mode".to_string(),
        ))
    }
}

// ── FileStore ─────────────────────────────────────────────────────────────────

/// Computes the SHA-256 hex digest of `data`.
fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[async_trait]
impl FileStore for SqliteStorage {
    async fn put(
        &self,
        _repo_id: &Uuid,
        path: &str,
        content: &[u8],
    ) -> Result<String, StorageError> {
        let full_path = self.vai_dir.join("files").join(path);
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent).map_err(|e| StorageError::Io(e.to_string()))?;
        }
        let hash = sha256_hex(content);
        let mut file =
            fs::File::create(&full_path).map_err(|e| StorageError::Io(e.to_string()))?;
        file.write_all(content)
            .map_err(|e| StorageError::Io(e.to_string()))?;
        Ok(hash)
    }

    async fn get(
        &self,
        _repo_id: &Uuid,
        path: &str,
    ) -> Result<Vec<u8>, StorageError> {
        let full_path = self.vai_dir.join("files").join(path);
        fs::read(&full_path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                StorageError::NotFound(format!("file {path}"))
            } else {
                StorageError::Io(e.to_string())
            }
        })
    }

    async fn list(
        &self,
        _repo_id: &Uuid,
        prefix: &str,
    ) -> Result<Vec<FileMetadata>, StorageError> {
        let base = self.vai_dir.join("files");
        let mut results = Vec::new();
        collect_files(&base, &base, prefix, &mut results)?;
        Ok(results)
    }

    async fn delete(
        &self,
        _repo_id: &Uuid,
        path: &str,
    ) -> Result<(), StorageError> {
        let full_path = self.vai_dir.join("files").join(path);
        fs::remove_file(&full_path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                StorageError::NotFound(format!("file {path}"))
            } else {
                StorageError::Io(e.to_string())
            }
        })
    }

    async fn exists(
        &self,
        _repo_id: &Uuid,
        path: &str,
    ) -> Result<bool, StorageError> {
        let full_path = self.vai_dir.join("files").join(path);
        Ok(full_path.exists())
    }
}

/// Recursively walks `dir` and collects `FileMetadata` for all files whose
/// path relative to `base` starts with `prefix`.
fn collect_files(
    base: &Path,
    dir: &Path,
    prefix: &str,
    results: &mut Vec<FileMetadata>,
) -> Result<(), StorageError> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(StorageError::Io(e.to_string())),
    };

    for entry in entries {
        let entry = entry.map_err(|e| StorageError::Io(e.to_string()))?;
        let path = entry.path();
        if path.is_dir() {
            collect_files(base, &path, prefix, results)?;
        } else {
            let rel = path
                .strip_prefix(base)
                .map_err(|e| StorageError::Io(e.to_string()))?;
            let rel_str = rel.to_string_lossy();
            if rel_str.starts_with(prefix) {
                let meta = entry
                    .metadata()
                    .map_err(|e| StorageError::Io(e.to_string()))?;
                let content = fs::read(&path).map_err(|e| StorageError::Io(e.to_string()))?;
                let hash = sha256_hex(&content);
                let updated_at = meta
                    .modified()
                    .ok()
                    .and_then(|t| {
                        t.duration_since(std::time::UNIX_EPOCH)
                            .ok()
                            .map(|d| DateTime::from_timestamp(d.as_secs() as i64, 0).unwrap_or_else(Utc::now))
                    })
                    .unwrap_or_else(Utc::now);
                results.push(FileMetadata {
                    path: rel_str.into_owned(),
                    size: meta.len(),
                    content_hash: hash,
                    updated_at,
                });
            }
        }
    }
    Ok(())
}

// ── OrgStore ──────────────────────────────────────────────────────────────────
//
// RBAC is not supported in local CLI mode — organizations and multi-user
// permission management require a hosted Postgres backend.  These stubs return
// a clear error so that accidental usage is surfaced immediately.

fn org_store_unsupported() -> StorageError {
    StorageError::InvalidTransition(
        "OrgStore is not supported in local CLI mode; use the hosted server backend".to_string(),
    )
}

#[async_trait]
impl OrgStore for SqliteStorage {
    async fn create_org(&self, _org: NewOrg) -> Result<Organization, StorageError> {
        Err(org_store_unsupported())
    }

    async fn get_org(&self, _org_id: &Uuid) -> Result<Organization, StorageError> {
        Err(org_store_unsupported())
    }

    async fn get_org_by_slug(&self, _slug: &str) -> Result<Organization, StorageError> {
        Err(org_store_unsupported())
    }

    async fn list_orgs(&self) -> Result<Vec<Organization>, StorageError> {
        Err(org_store_unsupported())
    }

    async fn delete_org(&self, _org_id: &Uuid) -> Result<(), StorageError> {
        Err(org_store_unsupported())
    }

    async fn create_user(&self, _user: NewUser) -> Result<User, StorageError> {
        Err(org_store_unsupported())
    }

    async fn get_user(&self, _user_id: &Uuid) -> Result<User, StorageError> {
        Err(org_store_unsupported())
    }

    async fn get_user_by_email(&self, _email: &str) -> Result<User, StorageError> {
        Err(org_store_unsupported())
    }

    async fn add_org_member(
        &self,
        _org_id: &Uuid,
        _user_id: &Uuid,
        _role: OrgRole,
    ) -> Result<OrgMember, StorageError> {
        Err(org_store_unsupported())
    }

    async fn update_org_member(
        &self,
        _org_id: &Uuid,
        _user_id: &Uuid,
        _role: OrgRole,
    ) -> Result<OrgMember, StorageError> {
        Err(org_store_unsupported())
    }

    async fn remove_org_member(
        &self,
        _org_id: &Uuid,
        _user_id: &Uuid,
    ) -> Result<(), StorageError> {
        Err(org_store_unsupported())
    }

    async fn list_org_members(
        &self,
        _org_id: &Uuid,
    ) -> Result<Vec<OrgMember>, StorageError> {
        Err(org_store_unsupported())
    }

    async fn get_repo_id_in_org(
        &self,
        _org_id: &Uuid,
        _repo_name: &str,
    ) -> Result<Uuid, StorageError> {
        Err(org_store_unsupported())
    }

    async fn add_collaborator(
        &self,
        _repo_id: &Uuid,
        _user_id: &Uuid,
        _role: RepoRole,
    ) -> Result<RepoCollaborator, StorageError> {
        Err(org_store_unsupported())
    }

    async fn update_collaborator(
        &self,
        _repo_id: &Uuid,
        _user_id: &Uuid,
        _role: RepoRole,
    ) -> Result<RepoCollaborator, StorageError> {
        Err(org_store_unsupported())
    }

    async fn remove_collaborator(
        &self,
        _repo_id: &Uuid,
        _user_id: &Uuid,
    ) -> Result<(), StorageError> {
        Err(org_store_unsupported())
    }

    async fn list_collaborators(
        &self,
        _repo_id: &Uuid,
    ) -> Result<Vec<RepoCollaborator>, StorageError> {
        Err(org_store_unsupported())
    }

    async fn resolve_repo_role(
        &self,
        _user_id: &Uuid,
        _repo_id: &Uuid,
    ) -> Result<Option<RepoRole>, StorageError> {
        Err(org_store_unsupported())
    }
}

// ── WatcherRegistryStore ──────────────────────────────────────────────────────

#[async_trait]
impl WatcherRegistryStore for SqliteStorage {
    async fn register_watcher(
        &self,
        _repo_id: &Uuid,
        watcher: Watcher,
    ) -> Result<Watcher, StorageError> {
        let store = WatcherStore::open(&self.vai_dir)
            .map_err(|e| StorageError::Database(e.to_string()))?;
        store.register(&watcher).map_err(|e| {
            use crate::watcher::WatcherError;
            match &e {
                WatcherError::AlreadyExists(id) => {
                    StorageError::Conflict(format!("watcher '{id}' is already registered"))
                }
                _ => StorageError::Database(e.to_string()),
            }
        })?;
        Ok(watcher)
    }

    async fn get_watcher(
        &self,
        _repo_id: &Uuid,
        agent_id: &str,
    ) -> Result<Watcher, StorageError> {
        let store = WatcherStore::open(&self.vai_dir)
            .map_err(|e| StorageError::Database(e.to_string()))?;
        store.get(agent_id).map_err(|e| {
            use crate::watcher::WatcherError;
            match &e {
                WatcherError::NotFound(id) => {
                    StorageError::NotFound(format!("watcher '{id}' not found"))
                }
                _ => StorageError::Database(e.to_string()),
            }
        })
    }

    async fn list_watchers(&self, _repo_id: &Uuid) -> Result<Vec<Watcher>, StorageError> {
        let store = WatcherStore::open(&self.vai_dir)
            .map_err(|e| StorageError::Database(e.to_string()))?;
        store.list().map_err(|e| StorageError::Database(e.to_string()))
    }

    async fn pause_watcher(
        &self,
        _repo_id: &Uuid,
        agent_id: &str,
    ) -> Result<Watcher, StorageError> {
        let store = WatcherStore::open(&self.vai_dir)
            .map_err(|e| StorageError::Database(e.to_string()))?;
        store.pause(agent_id).map_err(|e| {
            use crate::watcher::WatcherError;
            match &e {
                WatcherError::NotFound(id) => {
                    StorageError::NotFound(format!("watcher '{id}' not found"))
                }
                _ => StorageError::Database(e.to_string()),
            }
        })?;
        store.get(agent_id).map_err(|e| StorageError::Database(e.to_string()))
    }

    async fn resume_watcher(
        &self,
        _repo_id: &Uuid,
        agent_id: &str,
    ) -> Result<Watcher, StorageError> {
        let store = WatcherStore::open(&self.vai_dir)
            .map_err(|e| StorageError::Database(e.to_string()))?;
        store.resume(agent_id).map_err(|e| {
            use crate::watcher::WatcherError;
            match &e {
                WatcherError::NotFound(id) => {
                    StorageError::NotFound(format!("watcher '{id}' not found"))
                }
                _ => StorageError::Database(e.to_string()),
            }
        })?;
        store.get(agent_id).map_err(|e| StorageError::Database(e.to_string()))
    }

    async fn prepare_discovery(
        &self,
        _repo_id: &Uuid,
        agent_id: &str,
        event: &DiscoveryEventKind,
    ) -> Result<DiscoveryPreparation, StorageError> {
        let store = WatcherStore::open(&self.vai_dir)
            .map_err(|e| StorageError::Database(e.to_string()))?;
        store.prepare_discovery(agent_id, event).map_err(|e| {
            use crate::watcher::WatcherError;
            match &e {
                WatcherError::NotFound(id) => {
                    StorageError::NotFound(format!("watcher '{id}' not found"))
                }
                WatcherError::RateLimitExceeded { .. } => {
                    StorageError::RateLimitExceeded(e.to_string())
                }
                _ => StorageError::Database(e.to_string()),
            }
        })
    }

    async fn record_discovery(
        &self,
        _repo_id: &Uuid,
        agent_id: &str,
        event: &DiscoveryEventKind,
        record_id: Uuid,
        dedup_key: &str,
        received_at: chrono::DateTime<chrono::Utc>,
        created_issue_id: Option<Uuid>,
        suppressed: bool,
    ) -> Result<DiscoveryRecord, StorageError> {
        let store = WatcherStore::open(&self.vai_dir)
            .map_err(|e| StorageError::Database(e.to_string()))?;
        store
            .record_discovery(
                agent_id,
                event,
                record_id,
                dedup_key,
                received_at,
                created_issue_id,
                suppressed,
            )
            .map_err(|e| StorageError::Database(e.to_string()))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::issue::IssuePriority;
    use crate::repo;

    fn setup() -> (TempDir, SqliteStorage, Uuid) {
        let tmp = TempDir::new().unwrap();
        // Initialize a minimal repo so event_log, versions, etc. work.
        repo::init(tmp.path()).unwrap();
        let vai_dir = tmp.path().join(".vai");
        let storage = SqliteStorage::new(vai_dir);
        let repo_id = Uuid::new_v4();
        (tmp, storage, repo_id)
    }

    #[tokio::test]
    async fn event_store_append_and_query() {
        let (_tmp, storage, repo_id) = setup();

        let event = storage
            .append(
                &repo_id,
                EventKind::RepoInitialized {
                    repo_id,
                    name: "test".to_string(),
                },
            )
            .await
            .unwrap();

        // repo::init already appends a RepoInitialized event, so id ≥ 1.
        assert!(event.id >= 1);

        let count = storage.count(&repo_id).await.unwrap();
        // The repo was initialized with `repo::init` which already appends events,
        // so count ≥ 1.
        assert!(count >= 1);

        let by_type = storage
            .query_by_type(&repo_id, "RepoInitialized")
            .await
            .unwrap();
        assert!(!by_type.is_empty());
    }

    #[tokio::test]
    async fn event_store_query_since_id() {
        let (_tmp, storage, repo_id) = setup();

        storage
            .append(
                &repo_id,
                EventKind::RepoInitialized {
                    repo_id,
                    name: "test".to_string(),
                },
            )
            .await
            .unwrap();

        let initial_count = storage.count(&repo_id).await.unwrap();
        let all_before = storage.query_since_id(&repo_id, 0).await.unwrap();
        assert_eq!(all_before.len() as u64, initial_count);

        let since = storage
            .query_since_id(&repo_id, (initial_count - 1) as i64)
            .await
            .unwrap();
        assert_eq!(since.len(), 1);
    }

    #[tokio::test]
    async fn issue_store_create_and_get() {
        let (_tmp, storage, repo_id) = setup();

        let issue = storage
            .create_issue(
                &repo_id,
                NewIssue {
                    title: "Test issue".to_string(),
                    description: "A test".to_string(),
                    priority: IssuePriority::Medium,
                    labels: vec!["bug".to_string()],
                    creator: "agent-1".to_string(),
                    agent_source: None,
                    acceptance_criteria: vec![],
                },
            )
            .await
            .unwrap();

        assert_eq!(issue.title, "Test issue");

        let fetched = storage.get_issue(&repo_id, &issue.id).await.unwrap();
        assert_eq!(fetched.id, issue.id);

        let issues = storage
            .list_issues(&repo_id, &IssueFilter::default())
            .await
            .unwrap();
        assert_eq!(issues.len(), 1);
    }

    #[tokio::test]
    async fn issue_store_close() {
        let (_tmp, storage, repo_id) = setup();

        let issue = storage
            .create_issue(
                &repo_id,
                NewIssue {
                    title: "Close me".to_string(),
                    description: String::new(),
                    priority: IssuePriority::Low,
                    labels: vec![],
                    creator: "agent".to_string(),
                    agent_source: None,
                    acceptance_criteria: vec![],
                },
            )
            .await
            .unwrap();

        let closed = storage
            .close_issue(&repo_id, &issue.id, "wontfix")
            .await
            .unwrap();
        assert_eq!(closed.resolution.as_deref(), Some("wontfix"));
    }

    #[tokio::test]
    async fn version_store_create_and_head() {
        let (_tmp, storage, repo_id) = setup();

        storage
            .create_version(
                &repo_id,
                NewVersion {
                    version_id: "v1".to_string(),
                    parent_version_id: None,
                    intent: "initial".to_string(),
                    created_by: "agent".to_string(),
                    merge_event_id: None,
                },
            )
            .await
            .unwrap();

        storage.advance_head(&repo_id, "v1").await.unwrap();
        let head = storage.read_head(&repo_id).await.unwrap();
        assert_eq!(head.as_deref(), Some("v1"));

        let versions = storage.list_versions(&repo_id).await.unwrap();
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].version_id, "v1");
    }

    #[tokio::test]
    async fn workspace_store_create_and_list() {
        let (_tmp, storage, repo_id) = setup();

        // Need a version first (workspace::create reads HEAD).
        storage
            .create_version(
                &repo_id,
                NewVersion {
                    version_id: "v1".to_string(),
                    parent_version_id: None,
                    intent: "initial".to_string(),
                    created_by: "agent".to_string(),
                    merge_event_id: None,
                },
            )
            .await
            .unwrap();
        storage.advance_head(&repo_id, "v1").await.unwrap();

        let ws = storage
            .create_workspace(
                &repo_id,
                NewWorkspace {
                    id: None,
                    intent: "implement feature X".to_string(),
                    base_version: "v1".to_string(),
                    issue_id: None,
                },
            )
            .await
            .unwrap();

        let fetched = storage.get_workspace(&repo_id, &ws.id).await.unwrap();
        assert_eq!(fetched.intent, "implement feature X");

        let list = storage.list_workspaces(&repo_id, false).await.unwrap();
        assert_eq!(list.len(), 1);
    }

    #[tokio::test]
    async fn auth_store_create_and_validate() {
        let (_tmp, storage, repo_id) = setup();

        let (key_meta, plaintext) = storage.create_key(Some(&repo_id), "test-key", None, None, None, None).await.unwrap();
        assert_eq!(key_meta.name, "test-key");

        let validated = storage.validate_key(&plaintext).await.unwrap();
        assert_eq!(validated.id, key_meta.id);

        storage.revoke_key(&key_meta.id).await.unwrap();
        let invalid = storage.validate_key(&plaintext).await;
        assert!(invalid.is_err());
    }

    #[tokio::test]
    async fn file_store_put_get_delete() {
        let (_tmp, storage, repo_id) = setup();

        let hash = storage
            .put(&repo_id, "src/main.rs", b"fn main() {}")
            .await
            .unwrap();
        assert_eq!(hash.len(), 64); // SHA-256 hex

        let content = storage.get(&repo_id, "src/main.rs").await.unwrap();
        assert_eq!(&content, b"fn main() {}");

        assert!(storage.exists(&repo_id, "src/main.rs").await.unwrap());

        let listing = storage.list(&repo_id, "src/").await.unwrap();
        assert_eq!(listing.len(), 1);
        assert_eq!(listing[0].path, "src/main.rs");

        storage.delete(&repo_id, "src/main.rs").await.unwrap();
        assert!(!storage.exists(&repo_id, "src/main.rs").await.unwrap());
    }
}
