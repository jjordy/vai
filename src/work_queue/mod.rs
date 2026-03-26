//! Smart work queue — ranks open issues by safe-to-parallelize workability.
//!
//! Uses keyword extraction and semantic graph matching to predict which entities
//! an issue will affect, then compares those predictions against active workspace
//! scopes to determine which issues can be started safely in parallel.
//!
//! ## Scope Prediction
//!
//! Given an issue title and description, the engine:
//! 1. Extracts keywords (removes stop words, normalises case)
//! 2. Matches keywords against entity names in the semantic graph
//! 3. Expands matches via 1-hop and 2-hop BFS traversal
//! 4. Returns predictions with confidence levels (High / Medium / Low)
//!
//! ## Conflict Analysis
//!
//! Each open issue's predicted scope is compared against every active workspace's
//! blast radius from the conflict engine.  Issues with no overlap are `available`;
//! those that intersect any active workspace are `blocked`.

use std::collections::HashSet;
use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::conflict::ConflictEngine;
use crate::graph::GraphSnapshot;
use crate::issue::{IssueFilter, IssueStatus, IssueStore};

// ── Error type ────────────────────────────────────────────────────────────────

/// Errors from work queue operations.
#[derive(Debug, Error)]
pub enum WorkQueueError {
    #[error("Graph error: {0}")]
    Graph(#[from] crate::graph::GraphError),

    #[error("Issue error: {0}")]
    Issue(#[from] crate::issue::IssueError),

    #[error("Workspace error: {0}")]
    Workspace(#[from] crate::workspace::WorkspaceError),

    #[error("Event log error: {0}")]
    EventLog(#[from] crate::event_log::EventLogError),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Issue {0} is no longer open — refresh the work queue and try again")]
    IssueNotOpen(Uuid),

    #[error("Issue {0} conflicts with active workspaces — refresh the work queue and try again")]
    IssueConflicting(Uuid),
}

// ── Stop words ────────────────────────────────────────────────────────────────

/// Words excluded from keyword extraction.
const STOP_WORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "do", "for", "from",
    "has", "he", "in", "is", "it", "its", "not", "of", "on", "or", "so",
    "that", "the", "to", "up", "use", "was", "were", "will", "with",
];

// ── Confidence levels ─────────────────────────────────────────────────────────

/// Confidence level of a scope prediction for a single entity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum PredictionConfidence {
    /// Direct name match against the semantic graph.
    High,
    /// 1-hop related entity (caller / callee / sibling).
    Medium,
    /// 2-hop transitive dependency.
    Low,
}

// ── Predicted entity ──────────────────────────────────────────────────────────

/// A single entity included in a scope prediction.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PredictedEntity {
    /// Stable entity identifier (SHA-256 of `file::qualified_name`).
    pub id: String,
    /// Short entity name (function / struct / trait name, etc.).
    pub name: String,
    /// Repository-relative file path containing this entity.
    pub file_path: String,
    /// How confidently this entity is predicted to be affected.
    pub confidence: PredictionConfidence,
}

// ── Scope prediction ──────────────────────────────────────────────────────────

/// Predicted scope of work for an issue.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ScopePrediction {
    /// Predicted entities ordered by confidence (High first).
    pub entities: Vec<PredictedEntity>,
    /// Unique repository-relative file paths across all predicted entities.
    pub files: Vec<String>,
    /// Total number of entities in the predicted blast radius.
    pub blast_radius: usize,
}

impl ScopePrediction {
    /// Returns the entity IDs as a set for overlap checking.
    pub fn entity_ids(&self) -> HashSet<String> {
        self.entities.iter().map(|e| e.id.clone()).collect()
    }

    /// Returns the file paths as a set for overlap checking.
    pub fn file_set(&self) -> HashSet<String> {
        self.files.iter().cloned().collect()
    }
}

/// Extract keywords from an intent or issue text.
///
/// Splits on non-alphanumeric characters (preserving underscores), lowercases
/// all tokens, and removes stop words and tokens shorter than 3 characters.
pub fn extract_keywords(text: &str) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    text.split(|c: char| !c.is_alphanumeric() && c != '_')
        .map(|t| t.to_lowercase())
        .filter(|t| t.len() >= 3 && !STOP_WORDS.contains(&t.as_str()))
        .filter(|t| seen.insert(t.clone()))
        .collect()
}

/// Predict the scope of work for the given text (issue title + description).
///
/// Queries the semantic graph for entities whose names contain any of the
/// extracted keywords (case-insensitive substring match).  Direct matches are
/// tagged `High` confidence; 1-hop neighbours `Medium`; 2-hop `Low`.
///
/// Returns an empty prediction when the graph snapshot does not yet exist
/// (e.g., the repository has no parsed source files).
pub fn predict_scope(text: &str, vai_dir: &Path) -> Result<ScopePrediction, WorkQueueError> {
    let graph_path = vai_dir.join("graph").join("snapshot.db");
    if !graph_path.exists() {
        return Ok(ScopePrediction { entities: vec![], files: vec![], blast_radius: 0 });
    }

    let graph = GraphSnapshot::open(&graph_path)?;
    let keywords = extract_keywords(text);

    if keywords.is_empty() {
        return Ok(ScopePrediction { entities: vec![], files: vec![], blast_radius: 0 });
    }

    // ── Direct matches (HIGH confidence) ─────────────────────────────────────

    let mut direct_ids: HashSet<String> = HashSet::new();
    let mut predicted: Vec<PredictedEntity> = Vec::new();

    for kw in &keywords {
        for entity in graph.search_entities_by_name(kw)? {
            if direct_ids.insert(entity.id.clone()) {
                predicted.push(PredictedEntity {
                    id: entity.id,
                    name: entity.name,
                    file_path: entity.file_path,
                    confidence: PredictionConfidence::High,
                });
            }
        }
    }

    // ── 1-hop expansion (MEDIUM confidence) ──────────────────────────────────

    let mut medium_ids: HashSet<String> = HashSet::new();
    if !direct_ids.is_empty() {
        let seeds: Vec<&str> = direct_ids.iter().map(String::as_str).collect();
        let (reachable, _) = graph.reachable_entities(&seeds, 1)?;
        for entity in reachable {
            if !direct_ids.contains(&entity.id) && medium_ids.insert(entity.id.clone()) {
                predicted.push(PredictedEntity {
                    id: entity.id,
                    name: entity.name,
                    file_path: entity.file_path,
                    confidence: PredictionConfidence::Medium,
                });
            }
        }
    }

    // ── 2-hop expansion (LOW confidence) ─────────────────────────────────────

    if !direct_ids.is_empty() || !medium_ids.is_empty() {
        let all: Vec<&str> = direct_ids
            .iter()
            .chain(medium_ids.iter())
            .map(String::as_str)
            .collect();
        let (reachable, _) = graph.reachable_entities(&all, 2)?;
        for entity in reachable {
            if !direct_ids.contains(&entity.id) && !medium_ids.contains(&entity.id) {
                predicted.push(PredictedEntity {
                    id: entity.id,
                    name: entity.name,
                    file_path: entity.file_path,
                    confidence: PredictionConfidence::Low,
                });
            }
        }
    }

    let blast_radius = predicted.len();

    // Collect unique files while preserving entity order.
    let mut seen_files: HashSet<String> = HashSet::new();
    let files: Vec<String> = predicted
        .iter()
        .filter_map(|e| {
            if seen_files.insert(e.file_path.clone()) {
                Some(e.file_path.clone())
            } else {
                None
            }
        })
        .collect();

    Ok(ScopePrediction { entities: predicted, files, blast_radius })
}

// ── Work queue entries ────────────────────────────────────────────────────────

/// An issue safe to start — no conflicts with in-flight workspace scopes.
#[derive(Debug, Serialize, ToSchema)]
pub struct AvailableWork {
    /// Issue identifier.
    pub issue_id: String,
    /// Issue title.
    pub title: String,
    /// Issue priority.
    pub priority: String,
    /// Predicted scope (entities and files likely to be touched).
    pub predicted_scope: ScopePrediction,
}

/// An issue that cannot be safely started due to active workspace conflicts.
#[derive(Debug, Serialize, ToSchema)]
pub struct BlockedWork {
    /// Issue identifier.
    pub issue_id: String,
    /// Issue title.
    pub title: String,
    /// Issue priority.
    pub priority: String,
    /// Workspace IDs whose scopes conflict with this issue's predicted scope.
    pub blocked_by: Vec<String>,
    /// Human-readable explanation of why the issue is blocked.
    pub reason: String,
}

/// Full work queue response.
#[derive(Debug, Serialize, ToSchema)]
pub struct WorkQueue {
    /// Issues safe to start immediately.
    pub available_work: Vec<AvailableWork>,
    /// Issues blocked by active workspaces.
    pub blocked_work: Vec<BlockedWork>,
}

// ── Work queue computation ────────────────────────────────────────────────────

/// Compute the work queue for the current repository state.
///
/// Lists all `Open` issues, predicts their scope via keyword matching, and
/// compares each against the active workspace scopes tracked by the conflict
/// engine.  Results are ranked by priority (critical → high → medium → low).
pub fn compute(
    vai_dir: &Path,
    conflict_engine: &ConflictEngine,
) -> Result<WorkQueue, WorkQueueError> {
    let issue_store = IssueStore::open(vai_dir)?;
    let open_issues = issue_store.list(&IssueFilter {
        status: Some(IssueStatus::Open),
        ..Default::default()
    })?;

    // Snapshot active workspace scopes so we can check overlaps without
    // holding a lock across the (potentially slow) scope prediction calls.
    let active_scopes: Vec<_> = conflict_engine.all_scopes().cloned().collect();

    let mut available: Vec<AvailableWork> = Vec::new();
    let mut blocked: Vec<BlockedWork> = Vec::new();

    for issue in open_issues {
        let text = format!("{} {}", issue.title, issue.description);
        let prediction = predict_scope(&text, vai_dir)?;

        let pred_ids = prediction.entity_ids();
        let pred_files = prediction.file_set();

        let mut conflicting_ws: Vec<String> = Vec::new();
        let mut reasons: Vec<String> = Vec::new();

        for scope in &active_scopes {
            let file_conflict = pred_files.iter().any(|f| scope.write_files.contains(f));
            let entity_conflict = pred_ids.iter().any(|id| scope.blast_radius.contains(id));

            if file_conflict || entity_conflict {
                conflicting_ws.push(scope.workspace_id.to_string());
                reasons.push(format!(
                    "workspace {} is modifying related code (intent: \"{}\")",
                    scope.workspace_id, scope.intent
                ));
            }
        }

        if conflicting_ws.is_empty() {
            available.push(AvailableWork {
                issue_id: issue.id.to_string(),
                title: issue.title,
                priority: issue.priority.as_str().to_string(),
                predicted_scope: prediction,
            });
        } else {
            blocked.push(BlockedWork {
                issue_id: issue.id.to_string(),
                title: issue.title,
                priority: issue.priority.as_str().to_string(),
                blocked_by: conflicting_ws,
                reason: reasons.join("; "),
            });
        }
    }

    // Sort both lists by priority (critical first).
    available.sort_by_key(|w| priority_rank(&w.priority));
    blocked.sort_by_key(|w| priority_rank(&w.priority));

    Ok(WorkQueue { available_work: available, blocked_work: blocked })
}

// ── Atomic claim ──────────────────────────────────────────────────────────────

/// Result of a successful issue claim.
#[derive(Debug, Serialize, ToSchema)]
pub struct ClaimResult {
    /// The issue that was claimed.
    pub issue_id: String,
    /// Workspace created for this claim.
    pub workspace_id: String,
    /// Intent used for the workspace.
    pub intent: String,
    /// Predicted scope of the claimed issue.
    pub predicted_scope: ScopePrediction,
}

/// Atomically claim an issue: verify it is still open and uncontested, create
/// a workspace, and transition the issue to `InProgress`.
///
/// Returns [`WorkQueueError::IssueNotOpen`] if the issue is no longer `Open`.
/// Returns [`WorkQueueError::IssueConflicting`] if a conflict has appeared
/// since the queue was last fetched.  Both errors indicate the caller should
/// refresh the queue and retry with a different issue.
pub fn claim(
    vai_dir: &Path,
    issue_id: Uuid,
    conflict_engine: &ConflictEngine,
) -> Result<ClaimResult, WorkQueueError> {
    let issue_store = IssueStore::open(vai_dir)?;
    let issue = issue_store.get(issue_id).map_err(WorkQueueError::Issue)?;

    // Guard: issue must still be Open.
    if issue.status != IssueStatus::Open {
        return Err(WorkQueueError::IssueNotOpen(issue_id));
    }

    // Guard: re-check for conflicts against current active scopes.
    let text = format!("{} {}", issue.title, issue.description);
    let prediction = predict_scope(&text, vai_dir)?;
    let pred_ids = prediction.entity_ids();
    let pred_files = prediction.file_set();

    for scope in conflict_engine.all_scopes() {
        let file_conflict = pred_files.iter().any(|f| scope.write_files.contains(f));
        let entity_conflict = pred_ids.iter().any(|id| scope.blast_radius.contains(id));
        if file_conflict || entity_conflict {
            return Err(WorkQueueError::IssueConflicting(issue_id));
        }
    }

    // Create workspace using the issue title as intent.
    let head = crate::repo::read_head(vai_dir)
        .map_err(|e| WorkQueueError::Io(std::io::Error::other(e.to_string())))?;
    let ws_result = crate::workspace::create(vai_dir, &issue.title, &head)?;

    // Transition the issue to InProgress.
    let mut event_log = crate::event_log::EventLog::open(vai_dir)?;
    issue_store.set_in_progress(issue_id, ws_result.workspace.id, &mut event_log)?;

    Ok(ClaimResult {
        issue_id: issue_id.to_string(),
        workspace_id: ws_result.workspace.id.to_string(),
        intent: issue.title,
        predicted_scope: prediction,
    })
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Numeric rank for priority sorting (lower = higher priority).
pub fn priority_rank(priority: &str) -> u8 {
    match priority {
        "critical" => 0,
        "high" => 1,
        "medium" => 2,
        "low" => 3,
        _ => 4,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_keywords_removes_stop_words() {
        let kw = extract_keywords("add rate limiting to the auth service");
        assert!(kw.contains(&"rate".to_string()));
        assert!(kw.contains(&"limiting".to_string()));
        assert!(kw.contains(&"auth".to_string()));
        assert!(kw.contains(&"service".to_string()));
        // Stop words removed
        assert!(!kw.contains(&"to".to_string()));
        assert!(!kw.contains(&"the".to_string()));
    }

    #[test]
    fn extract_keywords_deduplicates() {
        let kw = extract_keywords("auth auth auth");
        assert_eq!(kw.iter().filter(|k| *k == "auth").count(), 1);
    }

    #[test]
    fn extract_keywords_min_length() {
        let kw = extract_keywords("do it now");
        // "do", "it" are too short or stop words; "now" passes length but check stop words
        assert!(!kw.contains(&"it".to_string()));
    }

    #[test]
    fn priority_rank_ordering() {
        assert!(priority_rank("critical") < priority_rank("high"));
        assert!(priority_rank("high") < priority_rank("medium"));
        assert!(priority_rank("medium") < priority_rank("low"));
    }

    #[test]
    fn predict_scope_no_graph_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let prediction = predict_scope("add auth service", dir.path()).unwrap();
        assert!(prediction.entities.is_empty());
        assert!(prediction.files.is_empty());
        assert_eq!(prediction.blast_radius, 0);
    }
}
