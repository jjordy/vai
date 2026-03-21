//! Conflict engine — real-time workspace scope tracking and overlap detection.
//!
//! The conflict engine monitors active workspaces and detects overlapping
//! work based on semantic graph analysis. Each workspace maintains a *scope
//! footprint* — the set of files and entities it has modified — plus a
//! *blast radius* of entities transitively reachable from that write scope.
//!
//! When a workspace's scope is updated the engine immediately checks all
//! other tracked workspaces for overlap and returns a ranked list of
//! [`OverlapResult`]s that the caller can broadcast via WebSocket.
//!
//! ## Overlap Classification
//!
//! | Level    | Criteria                                              |
//! |----------|-------------------------------------------------------|
//! | None     | No shared files between workspaces                    |
//! | Low      | Same file modified, but different entities            |
//! | Medium   | Same entity modified in both workspaces               |
//! | High     | One workspace's writes fall in the other's blast radius |
//! | Critical | Multiple shared entities (high coordination risk)    |

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::graph::GraphSnapshot;

// ── Error type ────────────────────────────────────────────────────────────────

/// Errors from conflict engine operations.
#[derive(Debug, Error)]
pub enum ConflictError {
    #[error("Graph error: {0}")]
    Graph(#[from] crate::graph::GraphError),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

// ── Scope footprint ───────────────────────────────────────────────────────────

/// The scope footprint of a single workspace.
///
/// Tracks which files and entities a workspace has written, plus the
/// transitive blast radius derived from those entities via the semantic graph.
#[derive(Debug, Clone)]
pub struct WorkspaceScope {
    /// Workspace identifier.
    pub workspace_id: Uuid,
    /// Agent's stated intent for this workspace.
    pub intent: String,
    /// Relative file paths that have been modified in this workspace.
    pub write_files: HashSet<String>,
    /// Entity IDs for entities contained in the modified files.
    pub write_entities: HashSet<String>,
    /// Entities transitively reachable from `write_entities` (up to 2 hops).
    ///
    /// Always a superset of `write_entities`.
    pub blast_radius: HashSet<String>,
}

// ── Overlap classification ────────────────────────────────────────────────────

/// Severity of overlap between two workspaces.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OverlapLevel {
    /// No shared files or referential overlap — safe to proceed.
    None,
    /// Same file modified in both, but no shared entities.
    Low,
    /// Same entity modified in both workspaces.
    Medium,
    /// One workspace's writes affect entities the other workspace depends on.
    High,
    /// Multiple shared entities — high coordination risk.
    Critical,
}

impl OverlapLevel {
    /// Returns the lowercase severity string.
    pub fn as_str(&self) -> &'static str {
        match self {
            OverlapLevel::None => "none",
            OverlapLevel::Low => "low",
            OverlapLevel::Medium => "medium",
            OverlapLevel::High => "high",
            OverlapLevel::Critical => "critical",
        }
    }
}

impl std::fmt::Display for OverlapLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The result of an overlap check between two workspaces.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverlapResult {
    /// Severity classification.
    pub level: OverlapLevel,
    /// The workspace whose scope was updated, triggering this check.
    pub your_workspace: Uuid,
    /// The workspace this overlaps with.
    pub other_workspace: Uuid,
    /// The other workspace's stated intent.
    pub other_intent: String,
    /// File paths present in both write scopes.
    pub overlapping_files: Vec<String>,
    /// Entity IDs that overlap (shared write entities or referential).
    pub overlapping_entities: Vec<String>,
    /// Human-readable recommendation for the agent.
    pub recommendation: String,
}

// ── Conflict engine ───────────────────────────────────────────────────────────

/// Tracks workspace scope footprints and detects overlaps in real-time.
///
/// Call [`ConflictEngine::update_scope`] whenever a workspace uploads files.
/// Call [`ConflictEngine::remove_workspace`] when a workspace is submitted
/// or discarded so it no longer participates in overlap checks.
#[derive(Debug, Default)]
pub struct ConflictEngine {
    scopes: HashMap<Uuid, WorkspaceScope>,
}

impl ConflictEngine {
    /// Creates a new, empty conflict engine.
    pub fn new() -> Self {
        Self {
            scopes: HashMap::new(),
        }
    }

    /// Updates the write scope for `workspace_id` and returns all overlaps.
    ///
    /// Loads entities for every file currently in the workspace's write scope
    /// from the semantic graph, computes the blast radius (2 hops), then
    /// checks all other tracked workspaces for overlap.
    ///
    /// Returns one [`OverlapResult`] per overlapping workspace, sorted by
    /// severity descending (Critical first). Results with level `None` are
    /// omitted.
    ///
    /// If the graph snapshot does not exist yet (e.g., the repo was just
    /// cloned and has not been initialised), scope tracking proceeds without
    /// entity-level resolution and only file-level overlap is detected.
    pub fn update_scope(
        &mut self,
        workspace_id: Uuid,
        intent: &str,
        new_write_files: &[String],
        vai_dir: &Path,
    ) -> Result<Vec<OverlapResult>, ConflictError> {
        // Update (or insert) the scope entry for this workspace.
        let scope = self
            .scopes
            .entry(workspace_id)
            .or_insert_with(|| WorkspaceScope {
                workspace_id,
                intent: intent.to_string(),
                write_files: HashSet::new(),
                write_entities: HashSet::new(),
                blast_radius: HashSet::new(),
            });

        scope.intent = intent.to_string();
        for f in new_write_files {
            scope.write_files.insert(f.clone());
        }

        // Refresh entity-level scope from the graph snapshot.
        let graph_path = vai_dir.join("graph").join("snapshot.db");
        if graph_path.exists() {
            let graph = GraphSnapshot::open(&graph_path)?;

            let mut write_entities: HashSet<String> = HashSet::new();
            for file in &scope.write_files {
                for entity in graph.get_entities_in_file(file)? {
                    write_entities.insert(entity.id);
                }
            }
            scope.write_entities = write_entities;

            // Compute blast radius (2 hops from write entities).
            let seeds: Vec<&str> = scope.write_entities.iter().map(String::as_str).collect();
            if !seeds.is_empty() {
                let (reachable, _) = graph.reachable_entities(&seeds, 2)?;
                scope.blast_radius = reachable.into_iter().map(|e| e.id).collect();
            } else {
                scope.blast_radius = scope.write_entities.clone();
            }
        }

        // Snapshot the updated scope before borrowing `self.scopes` for iteration.
        let updated = scope.clone();

        // Check against all other tracked workspaces.
        let mut results: Vec<OverlapResult> = self
            .scopes
            .values()
            .filter(|s| s.workspace_id != workspace_id)
            .map(|other| classify_overlap(&updated, other))
            .filter(|r| r.level != OverlapLevel::None)
            .collect();

        // Highest severity first.
        results.sort_by(|a, b| b.level.cmp(&a.level));
        Ok(results)
    }

    /// Removes a workspace from scope tracking.
    ///
    /// Should be called when a workspace is submitted or discarded so it no
    /// longer generates overlap notifications for other workspaces.
    pub fn remove_workspace(&mut self, workspace_id: &Uuid) {
        self.scopes.remove(workspace_id);
    }

    /// Returns the current scope footprint for a workspace, if tracked.
    pub fn get_scope(&self, workspace_id: &Uuid) -> Option<&WorkspaceScope> {
        self.scopes.get(workspace_id)
    }

    /// Returns the number of workspaces currently being tracked.
    pub fn tracked_count(&self) -> usize {
        self.scopes.len()
    }
}

// ── Overlap classification logic ──────────────────────────────────────────────

/// Classifies the overlap between two workspace scopes.
///
/// Examines file-level, entity-level, and referential overlap in order of
/// increasing severity, returning the worst applicable level.
fn classify_overlap(a: &WorkspaceScope, b: &WorkspaceScope) -> OverlapResult {
    let mut overlapping_files: Vec<String> = a
        .write_files
        .intersection(&b.write_files)
        .cloned()
        .collect();
    overlapping_files.sort();

    let mut shared_entities: Vec<String> = a
        .write_entities
        .intersection(&b.write_entities)
        .cloned()
        .collect();
    shared_entities.sort();

    // Entities that A writes which fall in B's blast radius (but are not
    // already counted as shared write entities).
    let mut a_in_b_blast: Vec<String> = a
        .write_entities
        .iter()
        .filter(|id| b.blast_radius.contains(*id) && !b.write_entities.contains(*id))
        .cloned()
        .collect();
    a_in_b_blast.sort();

    // Entities that B writes which fall in A's blast radius.
    let mut b_in_a_blast: Vec<String> = b
        .write_entities
        .iter()
        .filter(|id| a.blast_radius.contains(*id) && !a.write_entities.contains(*id))
        .cloned()
        .collect();
    b_in_a_blast.sort();

    let mut referential_entities: Vec<String> = {
        let mut v = a_in_b_blast;
        v.extend(b_in_a_blast);
        v.sort();
        v.dedup();
        v
    };
    referential_entities.dedup();

    // Critical: multiple shared entities — high coordination risk.
    if shared_entities.len() > 1 {
        return OverlapResult {
            level: OverlapLevel::Critical,
            your_workspace: a.workspace_id,
            other_workspace: b.workspace_id,
            other_intent: b.intent.clone(),
            overlapping_files,
            overlapping_entities: shared_entities,
            recommendation: format!(
                "Multiple entities are modified in both your workspace and workspace '{}' \
                 (intent: {}). Coordinate before submitting to avoid severe conflicts.",
                b.workspace_id, b.intent
            ),
        };
    }

    // Medium: same entity modified in both.
    if !shared_entities.is_empty() {
        return OverlapResult {
            level: OverlapLevel::Medium,
            your_workspace: a.workspace_id,
            other_workspace: b.workspace_id,
            other_intent: b.intent.clone(),
            overlapping_files,
            overlapping_entities: shared_entities,
            recommendation: format!(
                "You and workspace '{}' (intent: {}) are modifying the same entity. \
                 Consider submitting in sequence to avoid merge conflicts.",
                b.workspace_id, b.intent
            ),
        };
    }

    // High: referential dependency overlap.
    if !referential_entities.is_empty() {
        return OverlapResult {
            level: OverlapLevel::High,
            your_workspace: a.workspace_id,
            other_workspace: b.workspace_id,
            other_intent: b.intent.clone(),
            overlapping_files,
            overlapping_entities: referential_entities,
            recommendation: format!(
                "Your changes affect entities that workspace '{}' (intent: {}) depends on. \
                 Review the dependency chain before submitting.",
                b.workspace_id, b.intent
            ),
        };
    }

    // Low: same file, different entities.
    if !overlapping_files.is_empty() {
        return OverlapResult {
            level: OverlapLevel::Low,
            your_workspace: a.workspace_id,
            other_workspace: b.workspace_id,
            other_intent: b.intent.clone(),
            overlapping_files,
            overlapping_entities: vec![],
            recommendation: format!(
                "Both you and workspace '{}' (intent: {}) have modified the same file(s) \
                 but different entities. Verify there are no unintended overwrites.",
                b.workspace_id, b.intent
            ),
        };
    }

    // None.
    OverlapResult {
        level: OverlapLevel::None,
        your_workspace: a.workspace_id,
        other_workspace: b.workspace_id,
        other_intent: b.intent.clone(),
        overlapping_files: vec![],
        overlapping_entities: vec![],
        recommendation: String::new(),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    use crate::repo;

    /// Builds a [`WorkspaceScope`] directly from pre-computed sets (no graph).
    fn make_scope(
        ws_id: Uuid,
        intent: &str,
        files: &[&str],
        entities: &[&str],
        blast: &[&str],
    ) -> WorkspaceScope {
        WorkspaceScope {
            workspace_id: ws_id,
            intent: intent.to_string(),
            write_files: files.iter().map(|s| s.to_string()).collect(),
            write_entities: entities.iter().map(|s| s.to_string()).collect(),
            blast_radius: blast.iter().map(|s| s.to_string()).collect(),
        }
    }

    // ── classify_overlap unit tests ───────────────────────────────────────────

    #[test]
    fn overlap_none_disjoint_files() {
        let a = make_scope(Uuid::new_v4(), "refactor auth", &["src/auth.rs"], &["e1"], &["e1", "e2"]);
        let b = make_scope(Uuid::new_v4(), "add logging", &["src/log.rs"], &["e3"], &["e3"]);
        assert_eq!(classify_overlap(&a, &b).level, OverlapLevel::None);
    }

    #[test]
    fn overlap_low_shared_file_different_entities() {
        let a = make_scope(Uuid::new_v4(), "refactor main", &["src/main.rs"], &["e1"], &["e1"]);
        let b = make_scope(Uuid::new_v4(), "fix main", &["src/main.rs"], &["e2"], &["e2"]);
        let r = classify_overlap(&a, &b);
        assert_eq!(r.level, OverlapLevel::Low);
        assert_eq!(r.overlapping_files, vec!["src/main.rs"]);
        assert!(r.overlapping_entities.is_empty());
    }

    #[test]
    fn overlap_medium_single_shared_entity() {
        let a = make_scope(
            Uuid::new_v4(), "refactor auth",
            &["src/auth.rs"], &["shared"], &["shared"],
        );
        let b = make_scope(
            Uuid::new_v4(), "fix auth bug",
            &["src/auth.rs"], &["shared"], &["shared"],
        );
        let r = classify_overlap(&a, &b);
        assert_eq!(r.level, OverlapLevel::Medium);
        assert!(r.overlapping_entities.contains(&"shared".to_string()));
    }

    #[test]
    fn overlap_high_referential_dependency() {
        // A modifies entity X; B writes Y which has X in its blast radius.
        let a = make_scope(Uuid::new_v4(), "rename X", &["src/a.rs"], &["X"], &["X"]);
        let b = WorkspaceScope {
            workspace_id: Uuid::new_v4(),
            intent: "use X indirectly".to_string(),
            write_files: ["src/b.rs"].iter().map(|s| s.to_string()).collect(),
            write_entities: ["Y"].iter().map(|s| s.to_string()).collect(),
            // B's blast radius includes X because Y depends on X.
            blast_radius: ["Y", "X"].iter().map(|s| s.to_string()).collect(),
        };
        let r = classify_overlap(&a, &b);
        assert_eq!(r.level, OverlapLevel::High);
        assert!(r.overlapping_entities.contains(&"X".to_string()));
    }

    #[test]
    fn overlap_critical_multiple_shared_entities() {
        let a = make_scope(
            Uuid::new_v4(), "rewrite core",
            &["src/core.rs"], &["e1", "e2"], &["e1", "e2", "e3"],
        );
        let b = make_scope(
            Uuid::new_v4(), "also rewrite core",
            &["src/core.rs"], &["e1", "e2"], &["e1", "e2"],
        );
        let r = classify_overlap(&a, &b);
        assert_eq!(r.level, OverlapLevel::Critical);
        assert_eq!(r.overlapping_entities.len(), 2);
    }

    // ── ConflictEngine integration tests ──────────────────────────────────────

    fn init_repo_with_file(content: &[u8]) -> (TempDir, std::path::PathBuf) {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/lib.rs"), content).unwrap();
        repo::init(&root).unwrap();
        let vai_dir = root.join(".vai");
        (tmp, vai_dir)
    }

    #[test]
    fn engine_no_overlap_on_first_workspace() {
        let (_tmp, vai_dir) = init_repo_with_file(b"pub fn foo() {}\n");
        let mut engine = ConflictEngine::new();

        let overlaps = engine
            .update_scope(Uuid::new_v4(), "add feature", &["src/lib.rs".to_string()], &vai_dir)
            .unwrap();
        assert!(overlaps.is_empty(), "single workspace — no other to overlap with");
    }

    #[test]
    fn engine_detects_file_overlap_between_two_workspaces() {
        let (_tmp, vai_dir) = init_repo_with_file(b"pub fn foo() {}\n");
        let mut engine = ConflictEngine::new();

        let ws_a = Uuid::new_v4();
        let ws_b = Uuid::new_v4();

        engine
            .update_scope(ws_a, "refactor foo", &["src/lib.rs".to_string()], &vai_dir)
            .unwrap();

        let overlaps = engine
            .update_scope(ws_b, "fix foo", &["src/lib.rs".to_string()], &vai_dir)
            .unwrap();

        assert!(!overlaps.is_empty(), "ws_b should detect overlap with ws_a");
        let r = overlaps.iter().find(|r| r.other_workspace == ws_a).unwrap();
        assert!(r.level >= OverlapLevel::Low);
    }

    #[test]
    fn engine_no_overlap_on_disjoint_files() {
        let (_tmp, vai_dir) = init_repo_with_file(b"pub fn foo() {}\n");
        // Add a second file.
        std::fs::write(vai_dir.parent().unwrap().join("src/other.rs"), b"pub fn bar() {}\n")
            .unwrap();

        let mut engine = ConflictEngine::new();
        let ws_a = Uuid::new_v4();
        let ws_b = Uuid::new_v4();

        engine
            .update_scope(ws_a, "edit lib", &["src/lib.rs".to_string()], &vai_dir)
            .unwrap();

        let overlaps = engine
            .update_scope(ws_b, "edit other", &["src/other.rs".to_string()], &vai_dir)
            .unwrap();

        assert!(
            overlaps.iter().all(|r| r.level == OverlapLevel::None),
            "disjoint files — should not overlap"
        );
    }

    #[test]
    fn engine_remove_workspace_ends_tracking() {
        let (_tmp, vai_dir) = init_repo_with_file(b"pub fn foo() {}\n");
        let mut engine = ConflictEngine::new();
        let ws_id = Uuid::new_v4();

        engine
            .update_scope(ws_id, "test", &["src/lib.rs".to_string()], &vai_dir)
            .unwrap();
        assert_eq!(engine.tracked_count(), 1);

        engine.remove_workspace(&ws_id);
        assert_eq!(engine.tracked_count(), 0);

        // After removal, a new workspace touching the same file sees no overlap.
        let overlaps = engine
            .update_scope(Uuid::new_v4(), "other", &["src/lib.rs".to_string()], &vai_dir)
            .unwrap();
        assert!(overlaps.is_empty());
    }

    #[test]
    fn engine_scope_accumulates_across_calls() {
        let (_tmp, vai_dir) = init_repo_with_file(b"pub fn foo() {}\n");
        let mut engine = ConflictEngine::new();
        let ws_id = Uuid::new_v4();

        engine
            .update_scope(ws_id, "work", &["src/a.rs".to_string()], &vai_dir)
            .unwrap();
        engine
            .update_scope(ws_id, "work", &["src/b.rs".to_string()], &vai_dir)
            .unwrap();

        let scope = engine.get_scope(&ws_id).unwrap();
        assert!(scope.write_files.contains("src/a.rs"));
        assert!(scope.write_files.contains("src/b.rs"));
    }
}
