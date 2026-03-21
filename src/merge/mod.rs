//! Semantic merge engine — workspace submission, fast-forward merge, and
//! three-level semantic merge.
//!
//! When an agent submits a workspace the merge engine determines how to
//! integrate the changes into the main version history.
//!
//! ## Fast-Forward Merge
//!
//! A fast-forward merge applies when `HEAD == workspace.base_version`. In that
//! case there is nothing to reconcile — we simply copy the overlay files into
//! the project root, update the semantic graph, and create a new version.
//!
//! ## Three-Level Semantic Merge
//!
//! When HEAD has advanced past the workspace base version, the engine performs
//! three levels of analysis for each file changed by both the workspace and
//! HEAD:
//!
//! 1. **Textual (Level 1)** — compare changed line ranges. If the workspace
//!    and HEAD changes touch different lines, auto-merge by applying both sets
//!    of changes to the base.
//! 2. **Structural / AST (Level 2)** — if lines overlap, check whether the
//!    changes are in different semantic entities. If so, auto-merge by
//!    replacing each entity in HEAD content with the workspace version.
//! 3. **Referential (Level 3)** — if the same entity was changed by both
//!    sides, or a workspace removal conflicts with a HEAD modification, record
//!    a `MergeConflictDetected` event and persist the conflict for manual
//!    resolution.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::diff::{self, DiffError};
use crate::event_log::{ConflictInfo, EventKind, EventLog, EventLogError};
use crate::merge_patterns::{MergePatternError, MergePatternStore};

/// Re-exported for callers (e.g. CLI) that need to match on severity.
pub use crate::event_log::ConflictSeverity;
use crate::graph::{Entity, GraphError, GraphSnapshot, parse_rust_source};
use crate::repo::{self, RepoError};
use crate::version::{self, VersionError, VersionMeta};
use crate::workspace::{self, WorkspaceError, WorkspaceStatus};

// ── Errors ────────────────────────────────────────────────────────────────────

/// Errors from merge operations.
#[derive(Debug, Error)]
pub enum MergeError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Workspace error: {0}")]
    Workspace(#[from] WorkspaceError),

    #[error("Diff error: {0}")]
    Diff(#[from] DiffError),

    #[error("Event log error: {0}")]
    EventLog(#[from] EventLogError),

    #[error("Graph error: {0}")]
    Graph(#[from] GraphError),

    #[error("Version error: {0}")]
    Version(#[from] VersionError),

    #[error("Repo error: {0}")]
    Repo(#[from] RepoError),

    #[error("TOML serialization error: {0}")]
    TomlSer(#[from] toml::ser::Error),

    #[error("TOML deserialization error: {0}")]
    TomlDe(#[from] toml::de::Error),

    #[error("Merge pattern error: {0}")]
    MergePattern(#[from] MergePatternError),

    #[error("Conflict not found: {0}")]
    ConflictNotFound(String),

    #[error(
        "HEAD has advanced since workspace creation — fast-forward not possible \
         (workspace base: {base}, current HEAD: {current})"
    )]
    HeadAdvanced { base: String, current: String },

    #[error("Semantic merge detected {count} conflict(s) that require manual resolution")]
    SemanticConflicts {
        count: usize,
        conflicts: Vec<ConflictRecord>,
    },
}

// ── Conflict persistence ───────────────────────────────────────────────────────

/// Detailed record of a merge conflict, persisted to
/// `.vai/merge/conflicts/<id>.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictRecord {
    /// Unique identifier for this conflict.
    pub conflict_id: Uuid,
    /// Workspace that triggered the conflict.
    pub workspace_id: Uuid,
    /// Repository-relative file path where the conflict was detected.
    pub file_path: String,
    /// Stable entity IDs involved in the conflict.
    pub entity_ids: Vec<String>,
    /// Human-readable description.
    pub description: String,
    /// Conflict severity assessment.
    pub severity: ConflictSeverity,
    /// Merge level at which this conflict was detected (1, 2, or 3).
    pub merge_level: u8,
    /// Whether this conflict has been resolved.
    pub resolved: bool,
}

// ── Public result types ────────────────────────────────────────────────────────

/// Result of a successful workspace submission and merge.
#[derive(Debug, Serialize)]
pub struct SubmitResult {
    /// The new version created by this merge.
    pub version: VersionMeta,
    /// Number of files applied to the project root.
    pub files_applied: usize,
    /// Number of entity-level changes (added + modified + removed).
    pub entities_changed: usize,
    /// Number of conflicts that were auto-resolved during a semantic merge.
    pub auto_resolved: u32,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Submits the active workspace for merging.
///
/// If HEAD has not advanced since the workspace was created, performs a
/// fast-forward merge (apply changes directly). If HEAD has advanced,
/// attempts a three-level semantic merge. Returns
/// `MergeError::SemanticConflicts` if any conflicts cannot be auto-resolved.
pub fn submit(vai_dir: &Path, repo_root: &Path) -> Result<SubmitResult, MergeError> {
    let ws_meta = workspace::active(vai_dir)?;

    // 1. Compute diff and record file/entity events.
    let workspace_diff = diff::compute(vai_dir, repo_root)?;
    diff::record_events(vai_dir, &workspace_diff)?;

    // 2. Record WorkspaceSubmitted.
    let log_dir = vai_dir.join("event_log");
    let mut log = EventLog::open(&log_dir)?;
    let changes_summary = format!(
        "{} file(s), {} entity change(s)",
        workspace_diff.file_diffs.len(),
        workspace_diff.entity_changes.len()
    );
    log.append(EventKind::WorkspaceSubmitted {
        workspace_id: ws_meta.id,
        changes_summary,
    })?;

    // 3. Check HEAD position.
    let current_head = repo::read_head(vai_dir)?;

    if current_head == ws_meta.base_version {
        // Fast-forward path.
        return fast_forward_merge(vai_dir, repo_root, &ws_meta, workspace_diff, log);
    }

    // Semantic merge path.
    semantic_merge(vai_dir, repo_root, &ws_meta, workspace_diff, log)
}

/// Lists all conflict records from `.vai/merge/conflicts/`.
pub fn list_conflicts(vai_dir: &Path) -> Result<Vec<ConflictRecord>, MergeError> {
    let dir = vai_dir.join("merge").join("conflicts");
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut records = Vec::new();
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("toml") {
            let content = fs::read_to_string(&path)?;
            if let Ok(record) = toml::from_str::<ConflictRecord>(&content) {
                records.push(record);
            }
        }
    }
    records.sort_by_key(|r| r.conflict_id);
    Ok(records)
}

/// Marks a conflict as resolved.
///
/// The conflict file at `.vai/merge/conflicts/<id>.toml` is updated in-place
/// and a `MergeConflictResolved` event is appended to the event log.
pub fn resolve_conflict(
    vai_dir: &Path,
    conflict_id_str: &str,
) -> Result<ConflictRecord, MergeError> {
    let id: Uuid = conflict_id_str
        .parse()
        .map_err(|_| MergeError::ConflictNotFound(conflict_id_str.to_string()))?;

    let dir = vai_dir.join("merge").join("conflicts");
    let path = dir.join(format!("{id}.toml"));
    if !path.exists() {
        return Err(MergeError::ConflictNotFound(conflict_id_str.to_string()));
    }

    let content = fs::read_to_string(&path)?;
    let mut record: ConflictRecord = toml::from_str(&content)?;
    record.resolved = true;
    fs::write(&path, toml::to_string_pretty(&record)?)?;

    // Record event.
    let log_dir = vai_dir.join("event_log");
    let mut log = EventLog::open(&log_dir)?;
    log.append(EventKind::MergeConflictResolved {
        conflict_id: id,
        resolution: "manual".to_string(),
        resolved_by: "agent".to_string(),
    })?;

    // Record the resolution in the pattern library (best-effort: ignore errors).
    if let Ok(mut pattern_store) = MergePatternStore::open(vai_dir) {
        let _ = pattern_store.record_resolution(&record, "manual", false);
    }

    Ok(record)
}

/// Notifies the merge pattern library that a previously resolved conflict was
/// rolled back.
///
/// Call this after `version::rollback` completes to let the pattern library
/// adjust success rates and demote patterns that are no longer reliable.
pub fn record_pattern_rollback(
    vai_dir: &Path,
    conflict_id: Uuid,
) -> Result<(), MergeError> {
    let mut store = MergePatternStore::open(vai_dir)?;
    store.record_rollback(conflict_id)?;
    Ok(())
}

// ── Private — fast-forward merge ──────────────────────────────────────────────

fn fast_forward_merge(
    vai_dir: &Path,
    repo_root: &Path,
    ws_meta: &workspace::WorkspaceMeta,
    workspace_diff: diff::WorkspaceDiff,
    mut log: EventLog,
) -> Result<SubmitResult, MergeError> {
    // Save pre-change snapshot for rollback support.
    let new_version_id = version::next_version_id(vai_dir)?;
    save_pre_change_snapshot(vai_dir, &new_version_id, &workspace_diff, repo_root)?;

    // Apply overlay files to the project root.
    let overlay = workspace::overlay_dir(vai_dir, &ws_meta.id.to_string());
    let files_applied = apply_overlay(&overlay, repo_root)?;

    // Update semantic graph for changed Rust files.
    update_graph_for_diff(vai_dir, repo_root, &workspace_diff)?;

    // Record MergeCompleted and VersionCreated.
    let merge_event = log.append(EventKind::MergeCompleted {
        workspace_id: ws_meta.id,
        new_version_id: new_version_id.clone(),
        auto_resolved_conflicts: 0,
    })?;
    log.append(EventKind::VersionCreated {
        version_id: new_version_id.clone(),
        parent_version_id: Some(ws_meta.base_version.clone()),
        intent: ws_meta.intent.clone(),
    })?;

    let version_meta = version::create_version(
        vai_dir,
        &new_version_id,
        Some(&ws_meta.base_version),
        &ws_meta.intent,
        "agent",
        Some(merge_event.id),
    )?;

    advance_head_and_close_workspace(vai_dir, &new_version_id, ws_meta)?;

    Ok(SubmitResult {
        version: version_meta,
        files_applied,
        entities_changed: workspace_diff.entity_changes.len(),
        auto_resolved: 0,
    })
}

// ── Private — three-level semantic merge ──────────────────────────────────────

fn semantic_merge(
    vai_dir: &Path,
    repo_root: &Path,
    ws_meta: &workspace::WorkspaceMeta,
    workspace_diff: diff::WorkspaceDiff,
    mut log: EventLog,
) -> Result<SubmitResult, MergeError> {
    let overlay_dir = workspace::overlay_dir(vai_dir, &ws_meta.id.to_string());

    // Build map: file_path → base-version content for each file changed by HEAD.
    let head_changed = collect_head_changed_files(vai_dir, &ws_meta.base_version)?;

    let mut merged_files: Vec<(String, Vec<u8>)> = Vec::new();
    let mut auto_resolved: u32 = 0;
    let mut conflicts: Vec<ConflictRecord> = Vec::new();

    for fd in &workspace_diff.file_diffs {
        let overlay_path = overlay_dir.join(&fd.path);
        let ws_content = fs::read(&overlay_path)?;

        if let Some(base_content) = head_changed.get(&fd.path) {
            // File changed by both workspace and HEAD — run 3-level analysis.
            let head_content = {
                let p = repo_root.join(&fd.path);
                if p.exists() {
                    fs::read(&p)?
                } else {
                    Vec::new()
                }
            };

            match try_merge_file(
                &fd.path,
                base_content,
                &head_content,
                &ws_content,
                ws_meta.id,
            ) {
                FileMergeResult::AutoMerged(merged, level) => {
                    merged_files.push((fd.path.clone(), merged));
                    auto_resolved += 1;
                    let _ = level; // level info used for logging purposes
                }
                FileMergeResult::Conflicts(file_conflicts) => {
                    conflicts.extend(file_conflicts);
                }
            }
        } else {
            // File only changed by workspace — safe to apply directly.
            merged_files.push((fd.path.clone(), ws_content));
        }
    }

    if !conflicts.is_empty() {
        // Persist conflicts and record events.
        store_all_conflicts(vai_dir, &conflicts)?;
        for c in &conflicts {
            log.append(EventKind::MergeConflictDetected {
                workspace_id: ws_meta.id,
                conflict: ConflictInfo {
                    conflict_id: c.conflict_id,
                    entity_a: c.entity_ids.first().cloned().unwrap_or_default(),
                    entity_b: c.entity_ids.get(1).cloned().unwrap_or_default(),
                    description: c.description.clone(),
                    severity: c.severity.clone(),
                },
            })?;
        }
        return Err(MergeError::SemanticConflicts {
            count: conflicts.len(),
            conflicts,
        });
    }

    // All files merged successfully — apply to disk.
    let new_version_id = version::next_version_id(vai_dir)?;
    save_pre_change_snapshot(vai_dir, &new_version_id, &workspace_diff, repo_root)?;

    let mut files_applied = 0;
    for (rel_path, content) in &merged_files {
        let dest = repo_root.join(rel_path);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&dest, content)?;
        files_applied += 1;
    }

    // Update graph for changed Rust files.
    update_graph_for_diff(vai_dir, repo_root, &workspace_diff)?;

    // Record events and create version.
    let merge_event = log.append(EventKind::MergeCompleted {
        workspace_id: ws_meta.id,
        new_version_id: new_version_id.clone(),
        auto_resolved_conflicts: auto_resolved,
    })?;
    log.append(EventKind::VersionCreated {
        version_id: new_version_id.clone(),
        parent_version_id: Some(ws_meta.base_version.clone()),
        intent: ws_meta.intent.clone(),
    })?;

    let version_meta = version::create_version(
        vai_dir,
        &new_version_id,
        Some(&ws_meta.base_version),
        &ws_meta.intent,
        "agent",
        Some(merge_event.id),
    )?;

    advance_head_and_close_workspace(vai_dir, &new_version_id, ws_meta)?;

    Ok(SubmitResult {
        version: version_meta,
        files_applied,
        entities_changed: workspace_diff.entity_changes.len(),
        auto_resolved,
    })
}

// ── Private — three-level analysis for a single file ──────────────────────────

/// Result of attempting to merge a single file.
enum FileMergeResult {
    /// Successfully auto-merged at the given level (1=textual, 2=structural).
    AutoMerged(Vec<u8>, u8),
    /// One or more conflicts that require manual resolution.
    Conflicts(Vec<ConflictRecord>),
}

/// Attempts three-level semantic merge of a single file.
fn try_merge_file(
    file_path: &str,
    base: &[u8],
    head: &[u8],
    workspace: &[u8],
    workspace_id: Uuid,
) -> FileMergeResult {
    // ── Level 1: textual line diff ────────────────────────────────────────────
    let base_lines = split_lines(base);
    let head_lines = split_lines(head);
    let ws_lines = split_lines(workspace);

    let head_hunks = compute_hunks(&base_lines, &head_lines);
    let ws_hunks = compute_hunks(&base_lines, &ws_lines);

    if !hunks_overlap(&head_hunks, &ws_hunks) {
        // Level 1 auto-merge: apply both sets of line changes to base.
        let merged = apply_two_hunk_sets(&base_lines, head, workspace, &head_hunks, &ws_hunks);
        return FileMergeResult::AutoMerged(merged, 1);
    }

    // ── Level 2: structural (AST) — Rust files only ───────────────────────────
    if file_path.ends_with(".rs") {
        if let (Ok((base_ents, _)), Ok((head_ents, _)), Ok((ws_ents, _))) = (
            parse_rust_source(file_path, base),
            parse_rust_source(file_path, head),
            parse_rust_source(file_path, workspace),
        ) {
            let head_changed_ids = changed_entity_ids(&base_ents, base, &head_ents, head);
            let ws_changed_ids = changed_entity_ids(&base_ents, base, &ws_ents, workspace);

            let overlap: HashSet<&String> =
                head_changed_ids.intersection(&ws_changed_ids).collect();

            if overlap.is_empty() {
                // Level 2: different entities. Merge by entity substitution.
                if let Some(merged) = merge_by_entity_substitution(
                    head,
                    workspace,
                    &head_ents,
                    &ws_ents,
                    &ws_changed_ids,
                ) {
                    return FileMergeResult::AutoMerged(merged, 2);
                }
            }

            // Level 3: detect referential conflicts.
            let file_conflicts = detect_referential_conflicts(
                file_path,
                workspace_id,
                base,
                head,
                workspace,
                &base_ents,
                &head_ents,
                &ws_ents,
                &overlap,
            );
            return FileMergeResult::Conflicts(file_conflicts);
        }
    }

    // Non-Rust file with line overlap — generic textual conflict.
    FileMergeResult::Conflicts(vec![make_conflict(
        file_path,
        workspace_id,
        vec![],
        &format!("Overlapping text changes in `{file_path}`"),
        ConflictSeverity::Medium,
        1,
    )])
}

// ── Private — diff / hunk helpers ─────────────────────────────────────────────

/// A contiguous range changed in a diff from `base` to `modified`.
#[derive(Debug, Clone)]
struct Hunk {
    /// First line in base that is part of this change (0-indexed, inclusive).
    base_start: usize,
    /// First line in base *after* this change (exclusive).
    base_end: usize,
    /// Replacement lines (what the `modified` version has instead).
    new_lines: Vec<String>,
}

/// Splits `content` into a `Vec<&str>` of lines (no trailing newline on each).
fn split_lines(content: &[u8]) -> Vec<String> {
    let s = String::from_utf8_lossy(content);
    if s.is_empty() {
        return Vec::new();
    }
    // `lines()` strips the trailing newline from each element.
    s.lines().map(|l| l.to_string()).collect()
}

/// Computes the Longest Common Subsequence of two line sequences as index pairs
/// `(base_index, modified_index)` using an O(n·m) DP table.
fn lcs_indices(a: &[String], b: &[String]) -> Vec<(usize, usize)> {
    let m = a.len();
    let n = b.len();
    if m == 0 || n == 0 {
        return Vec::new();
    }

    // dp[i][j] = length of LCS for a[i..] and b[j..]
    let mut dp = vec![vec![0usize; n + 1]; m + 1];
    for i in (0..m).rev() {
        for j in (0..n).rev() {
            dp[i][j] = if a[i] == b[j] {
                1 + dp[i + 1][j + 1]
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }

    let mut pairs = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < m && j < n {
        if a[i] == b[j] {
            pairs.push((i, j));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    pairs
}

/// Converts an LCS pair list into a sequence of [`Hunk`]s.
fn compute_hunks(base: &[String], modified: &[String]) -> Vec<Hunk> {
    let pairs = lcs_indices(base, modified);
    let mut hunks = Vec::new();
    let mut base_pos = 0usize;
    let mut mod_pos = 0usize;

    for &(bi, mi) in &pairs {
        if bi > base_pos || mi > mod_pos {
            hunks.push(Hunk {
                base_start: base_pos,
                base_end: bi,
                new_lines: modified[mod_pos..mi].to_vec(),
            });
        }
        base_pos = bi + 1;
        mod_pos = mi + 1;
    }

    // Trailing change.
    if base_pos < base.len() || mod_pos < modified.len() {
        hunks.push(Hunk {
            base_start: base_pos,
            base_end: base.len(),
            new_lines: modified[mod_pos..].to_vec(),
        });
    }

    // Filter no-ops (base range empty AND no new lines).
    hunks.retain(|h| h.base_start < h.base_end || !h.new_lines.is_empty());
    hunks
}

/// Returns `true` if any hunk in `a` overlaps (in base coordinates) with any
/// hunk in `b`.
///
/// Two ranges overlap when `a_start < b_end && b_start < a_end`.
/// Two pure insertions (base_start == base_end) at the *same* position are
/// also considered overlapping.
fn hunks_overlap(a: &[Hunk], b: &[Hunk]) -> bool {
    for ha in a {
        for hb in b {
            let a_start = ha.base_start;
            let a_end = ha.base_end;
            let b_start = hb.base_start;
            let b_end = hb.base_end;

            if a_start < a_end && b_start < b_end {
                // Both are deletions/replacements — standard range overlap.
                if a_start < b_end && b_start < a_end {
                    return true;
                }
            } else if a_start == a_end && b_start == b_end && a_start == b_start {
                // Both are pure insertions at the same position.
                if !ha.new_lines.is_empty() && !hb.new_lines.is_empty() {
                    return true;
                }
            }
        }
    }
    false
}

/// Applies both sets of hunks (HEAD and workspace) to `base` lines and
/// returns the merged bytes.
///
/// Assumes the two hunk sets do not overlap in base coordinates.
fn apply_two_hunk_sets(
    base: &[String],
    head_raw: &[u8],
    ws_raw: &[u8],
    head_hunks: &[Hunk],
    ws_hunks: &[Hunk],
) -> Vec<u8> {
    // If one side has no hunks, just return the other side as-is (preserves
    // encoding and trailing newline from the original).
    if head_hunks.is_empty() {
        return ws_raw.to_vec();
    }
    if ws_hunks.is_empty() {
        return head_raw.to_vec();
    }

    // Merge all hunks sorted by base_start.
    let mut all: Vec<&Hunk> = head_hunks.iter().chain(ws_hunks.iter()).collect();
    all.sort_by_key(|h| h.base_start);

    let mut result_lines: Vec<&str> = Vec::new();
    let mut pos = 0usize;

    for hunk in &all {
        // Unchanged lines before this hunk.
        for line in &base[pos..hunk.base_start] {
            result_lines.push(line.as_str());
        }
        // Replacement lines from the hunk.
        for line in &hunk.new_lines {
            result_lines.push(line.as_str());
        }
        if hunk.base_end > pos {
            pos = hunk.base_end;
        }
    }
    // Remaining unchanged lines after the last hunk.
    for line in &base[pos..] {
        result_lines.push(line.as_str());
    }

    let mut out = result_lines.join("\n").into_bytes();
    // Preserve trailing newline if the base or either modified version had one.
    let had_newline = head_raw.ends_with(b"\n") || ws_raw.ends_with(b"\n");
    if had_newline && !out.ends_with(b"\n") {
        out.push(b'\n');
    }
    out
}

// ── Private — entity-level helpers ────────────────────────────────────────────

/// Returns the set of entity IDs that differ between `base` and `modified`.
///
/// An entity is "changed" if it was added, removed, or its byte-span content
/// differs between the two source buffers.
fn changed_entity_ids(
    base_ents: &[Entity],
    base_content: &[u8],
    modified_ents: &[Entity],
    modified_content: &[u8],
) -> HashSet<String> {
    let base_map: HashMap<&str, &Entity> =
        base_ents.iter().map(|e| (e.id.as_str(), e)).collect();
    let mod_map: HashMap<&str, &Entity> =
        modified_ents.iter().map(|e| (e.id.as_str(), e)).collect();

    let mut changed = HashSet::new();

    for (id, mod_ent) in &mod_map {
        if let Some(base_ent) = base_map.get(id) {
            // Same ID — check if byte content changed.
            let b_bytes = base_content
                .get(base_ent.byte_range.0..base_ent.byte_range.1.min(base_content.len()))
                .unwrap_or(&[]);
            let m_bytes = modified_content
                .get(mod_ent.byte_range.0..mod_ent.byte_range.1.min(modified_content.len()))
                .unwrap_or(&[]);
            if b_bytes != m_bytes {
                changed.insert((*id).to_string());
            }
        } else {
            // Added entity.
            changed.insert((*id).to_string());
        }
    }

    // Removed entities.
    for id in base_map.keys() {
        if !mod_map.contains_key(id) {
            changed.insert((*id).to_string());
        }
    }

    changed
}

/// Attempts Level-2 auto-merge: apply workspace entity changes onto HEAD
/// content.
///
/// For each entity that workspace changed (compared to base) but HEAD did not,
/// finds that entity in HEAD and replaces its byte range.  Returns `None` if
/// any replacement cannot be located cleanly.
fn merge_by_entity_substitution(
    head: &[u8],
    workspace: &[u8],
    head_ents: &[Entity],
    ws_ents: &[Entity],
    ws_changed_ids: &HashSet<String>,
) -> Option<Vec<u8>> {
    let head_id_map: HashMap<&str, &Entity> =
        head_ents.iter().map(|e| (e.id.as_str(), e)).collect();
    let ws_id_map: HashMap<&str, &Entity> =
        ws_ents.iter().map(|e| (e.id.as_str(), e)).collect();

    // Collect replacements: (head_start, head_end, replacement_bytes).
    let mut replacements: Vec<(usize, usize, Vec<u8>)> = Vec::new();

    for id in ws_changed_ids {
        let ws_ent = match ws_id_map.get(id.as_str()) {
            Some(e) => e,
            None => continue, // entity removed in workspace — skip for now
        };
        let ws_bytes = workspace
            .get(ws_ent.byte_range.0..ws_ent.byte_range.1.min(workspace.len()))?;

        if let Some(head_ent) = head_id_map.get(id.as_str()) {
            // Entity exists in HEAD — replace its span.
            replacements.push((
                head_ent.byte_range.0,
                head_ent.byte_range.1.min(head.len()),
                ws_bytes.to_vec(),
            ));
        }
        // If the entity is new (not in HEAD), we append it later.
    }

    // Sort replacements by start position descending so offsets stay valid.
    replacements.sort_by(|a, b| b.0.cmp(&a.0));

    let mut result = head.to_vec();
    for (start, end, replacement) in &replacements {
        result.splice(start..end, replacement.iter().cloned());
    }

    // Append workspace-added entities that don't exist in HEAD.
    let mut appended = false;
    for id in ws_changed_ids {
        if head_id_map.contains_key(id.as_str()) {
            continue; // already handled above
        }
        if let Some(ws_ent) = ws_id_map.get(id.as_str()) {
            let ws_bytes =
                workspace.get(ws_ent.byte_range.0..ws_ent.byte_range.1.min(workspace.len()))?;
            if !appended && !result.ends_with(b"\n") {
                result.push(b'\n');
            }
            result.extend_from_slice(ws_bytes);
            if !result.ends_with(b"\n") {
                result.push(b'\n');
            }
            appended = true;
        }
    }

    Some(result)
}

// ── Private — Level-3 referential conflict detection ──────────────────────────

/// Detects Level-3 referential conflicts for a Rust file.
///
/// Checks for:
/// - Same entity modified by both workspace and HEAD (overlapping entity IDs).
/// - Entity removed in workspace but still modified by HEAD.
/// - Workspace-removed entity whose name is still referenced in HEAD content
///   (proxy for "old name used after rename").
#[allow(clippy::too_many_arguments)]
fn detect_referential_conflicts(
    file_path: &str,
    workspace_id: Uuid,
    base: &[u8],
    head: &[u8],
    _workspace: &[u8],
    base_ents: &[Entity],
    head_ents: &[Entity],
    ws_ents: &[Entity],
    overlapping_ids: &HashSet<&String>,
) -> Vec<ConflictRecord> {
    let mut conflicts = Vec::new();

    let base_id_set: HashSet<&str> = base_ents.iter().map(|e| e.id.as_str()).collect();
    let head_id_set: HashSet<&str> = head_ents.iter().map(|e| e.id.as_str()).collect();
    let ws_id_set: HashSet<&str> = ws_ents.iter().map(|e| e.id.as_str()).collect();

    // 1. Same entity modified by both sides — HIGH conflict.
    for id in overlapping_ids {
        let name = base_ents
            .iter()
            .find(|e| &e.id == *id)
            .or_else(|| head_ents.iter().find(|e| &e.id == *id))
            .map(|e| e.qualified_name.as_str())
            .unwrap_or("unknown");

        conflicts.push(make_conflict(
            file_path,
            workspace_id,
            vec![(*id).clone()],
            &format!(
                "Entity `{name}` was modified by both workspace and HEAD — manual resolution required"
            ),
            ConflictSeverity::High,
            3,
        ));
    }

    // 2. Entity removed in workspace but still modified by HEAD — HIGH.
    let ws_removed: Vec<&Entity> = base_ents
        .iter()
        .filter(|e| base_id_set.contains(e.id.as_str()) && !ws_id_set.contains(e.id.as_str()))
        .collect();

    for removed in &ws_removed {
        // Conflict only if HEAD also changed this entity (not just kept it).
        let head_changed_it = !head_id_set.contains(removed.id.as_str())
            || head_ents.iter().any(|he| {
                he.id == removed.id && {
                    // Compare byte content.
                    let b_bytes = base
                        .get(removed.byte_range.0..removed.byte_range.1.min(base.len()))
                        .unwrap_or(&[]);
                    let h_bytes = head
                        .get(he.byte_range.0..he.byte_range.1.min(head.len()))
                        .unwrap_or(&[]);
                    b_bytes != h_bytes
                }
            });

        if head_changed_it {
            conflicts.push(make_conflict(
                file_path,
                workspace_id,
                vec![removed.id.clone()],
                &format!(
                    "Entity `{}` removed in workspace but modified in HEAD — cannot auto-merge",
                    removed.qualified_name
                ),
                ConflictSeverity::High,
                3,
            ));
        }
    }

    // 3. Workspace-removed entity whose name still appears in HEAD — MEDIUM.
    //    Acts as a proxy for "rename without updating callers" conflicts.
    if let Ok(head_str) = std::str::from_utf8(head) {
        for removed in &ws_removed {
            // Skip if already reported in check 2.
            if conflicts
                .iter()
                .any(|c| c.entity_ids.contains(&removed.id))
            {
                continue;
            }
            // Simple name presence check — avoids false negatives for renamed
            // entities whose old name is still used as a call site.
            let name = &removed.name;
            if !name.is_empty() && head_str.contains(name.as_str()) {
                conflicts.push(make_conflict(
                    file_path,
                    workspace_id,
                    vec![removed.id.clone()],
                    &format!(
                        "Entity `{name}` removed/renamed in workspace but old name \
                         still referenced in HEAD — possible stale call site"
                    ),
                    ConflictSeverity::Medium,
                    3,
                ));
            }
        }
    }

    // If no conflicts detected even with entity overlap, fall back to a generic
    // high-severity conflict so we never silently discard changes.
    if conflicts.is_empty() {
        conflicts.push(make_conflict(
            file_path,
            workspace_id,
            vec![],
            &format!("Overlapping changes in `{file_path}` could not be auto-merged"),
            ConflictSeverity::High,
            3,
        ));
    }

    conflicts
}

// ── Private — conflict helpers ─────────────────────────────────────────────────

fn make_conflict(
    file_path: &str,
    workspace_id: Uuid,
    entity_ids: Vec<String>,
    description: &str,
    severity: ConflictSeverity,
    merge_level: u8,
) -> ConflictRecord {
    ConflictRecord {
        conflict_id: Uuid::new_v4(),
        workspace_id,
        file_path: file_path.to_string(),
        entity_ids,
        description: description.to_string(),
        severity,
        merge_level,
        resolved: false,
    }
}

fn store_all_conflicts(vai_dir: &Path, records: &[ConflictRecord]) -> Result<(), MergeError> {
    let dir = vai_dir.join("merge").join("conflicts");
    fs::create_dir_all(&dir)?;
    for record in records {
        let path = dir.join(format!("{}.toml", record.conflict_id));
        fs::write(path, toml::to_string_pretty(record)?)?;
    }
    Ok(())
}

// ── Private — HEAD-change collection ──────────────────────────────────────────

/// Builds a map of `file_path → content-at-base-version` for all files that
/// were modified by versions between `base_version` (exclusive) and the
/// current HEAD (inclusive).
///
/// The content is read from the pre-change snapshots stored under
/// `.vai/versions/<id>/snapshot/` by the merge engine when each version was
/// created.
fn collect_head_changed_files(
    vai_dir: &Path,
    base_version: &str,
) -> Result<HashMap<String, Vec<u8>>, MergeError> {
    let base_n = parse_version_num(base_version);
    let head = repo::read_head(vai_dir)?;
    let head_n = parse_version_num(&head);

    let mut result: HashMap<String, Vec<u8>> = HashMap::new();

    for n in (base_n + 1)..=(head_n) {
        let version_id = format!("v{n}");
        let snapshot_dir = vai_dir
            .join("versions")
            .join(&version_id)
            .join("snapshot");
        if !snapshot_dir.exists() {
            continue;
        }
        let files = collect_snapshot_files(&snapshot_dir)?;
        for (rel_path, content) in files {
            // First snapshot after base_version has the base-version content.
            result.entry(rel_path).or_insert(content);
        }
    }

    Ok(result)
}

/// Recursively collects all files under a snapshot directory, returning
/// `(relative_path, content)` pairs.
fn collect_snapshot_files(snapshot_dir: &Path) -> Result<Vec<(String, Vec<u8>)>, MergeError> {
    let mut files = Vec::new();
    collect_snapshot_recursive(snapshot_dir, snapshot_dir, &mut files)?;
    Ok(files)
}

fn collect_snapshot_recursive(
    root: &Path,
    dir: &Path,
    out: &mut Vec<(String, Vec<u8>)>,
) -> Result<(), MergeError> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_snapshot_recursive(root, &path, out)?;
        } else {
            let rel = path
                .strip_prefix(root)
                .expect("path inside snapshot dir")
                .to_string_lossy()
                .to_string();
            let content = fs::read(&path)?;
            out.push((rel, content));
        }
    }
    Ok(())
}

/// Parses a version string like `"v3"` into the integer `3`.
/// Returns `0` for unrecognised formats.
fn parse_version_num(version: &str) -> usize {
    version
        .trim_start_matches('v')
        .parse::<usize>()
        .unwrap_or(0)
}

// ── Private — shared helpers ───────────────────────────────────────────────────

/// Updates the semantic graph for all `.rs` files in `workspace_diff`.
fn update_graph_for_diff(
    vai_dir: &Path,
    repo_root: &Path,
    workspace_diff: &diff::WorkspaceDiff,
) -> Result<(), MergeError> {
    let snapshot_path = vai_dir.join("graph").join("snapshot.db");
    let snapshot = GraphSnapshot::open(&snapshot_path)?;
    for fd in &workspace_diff.file_diffs {
        if fd.path.ends_with(".rs") {
            let abs_path = repo_root.join(&fd.path);
            if let Ok(content) = fs::read(&abs_path) {
                let _ = snapshot.update_file(&fd.path, &content);
            }
        }
    }
    Ok(())
}

/// Saves the pre-change content of files about to be overwritten.
fn save_pre_change_snapshot(
    vai_dir: &Path,
    new_version_id: &str,
    workspace_diff: &diff::WorkspaceDiff,
    repo_root: &Path,
) -> Result<(), MergeError> {
    let snapshot_dir = vai_dir
        .join("versions")
        .join(new_version_id)
        .join("snapshot");

    for fd in &workspace_diff.file_diffs {
        let src = repo_root.join(&fd.path);
        if src.exists() {
            let dest = snapshot_dir.join(&fd.path);
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&src, &dest)?;
        }
    }

    Ok(())
}

/// Copies all files from an overlay directory into the project root.
fn apply_overlay(overlay: &Path, repo_root: &Path) -> Result<usize, MergeError> {
    if !overlay.exists() {
        return Ok(0);
    }
    let files = collect_overlay_files(overlay)?;
    let count = files.len();
    for abs_path in files {
        let rel = abs_path
            .strip_prefix(overlay)
            .expect("path inside overlay")
            .to_path_buf();
        let dest = repo_root.join(&rel);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(&abs_path, &dest)?;
    }
    Ok(count)
}

fn collect_overlay_files(dir: &Path) -> Result<Vec<PathBuf>, MergeError> {
    let mut files = Vec::new();
    collect_overlay_recursive(dir, &mut files)?;
    Ok(files)
}

fn collect_overlay_recursive(dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), MergeError> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_overlay_recursive(&path, files)?;
        } else {
            files.push(path);
        }
    }
    Ok(())
}

/// Advances HEAD to `new_version_id` and marks the workspace as `Merged`.
fn advance_head_and_close_workspace(
    vai_dir: &Path,
    new_version_id: &str,
    ws_meta: &workspace::WorkspaceMeta,
) -> Result<(), MergeError> {
    fs::write(vai_dir.join("head"), format!("{new_version_id}\n"))?;

    let mut updated = ws_meta.clone();
    updated.status = WorkspaceStatus::Merged;
    updated.updated_at = Utc::now();
    workspace::update_meta(vai_dir, &updated)?;

    let active_file = vai_dir.join("workspaces").join("active");
    let _ = fs::remove_file(active_file);

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup_repo(source_files: &[(&str, &str)]) -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_path_buf();

        for (rel, content) in source_files {
            let abs = root.join(rel);
            if let Some(p) = abs.parent() {
                fs::create_dir_all(p).unwrap();
            }
            fs::write(&abs, content).unwrap();
        }

        crate::repo::init(&root).unwrap();
        (dir, root)
    }

    fn write_overlay(vai_dir: &Path, ws_id: &Uuid, files: &[(&str, &str)]) {
        let overlay = vai_dir
            .join("workspaces")
            .join(ws_id.to_string())
            .join("overlay");
        for (rel, content) in files {
            let abs = overlay.join(rel);
            if let Some(p) = abs.parent() {
                fs::create_dir_all(p).unwrap();
            }
            fs::write(&abs, content).unwrap();
        }
    }

    const BASE_RS: &str = "fn hello() -> &'static str { \"hello\" }\n";
    const MODIFIED_RS: &str =
        "fn hello() -> &'static str { \"hello, world!\" }\nfn greet() {}\n";

    // ── Fast-forward tests ──────────────────────────────────────────────────

    #[test]
    fn test_fast_forward_merge_creates_new_version() {
        let (_dir, root) = setup_repo(&[("src/lib.rs", BASE_RS)]);
        let vai_dir = root.join(".vai");

        let result = workspace::create(&vai_dir, "add greeting", "v1").unwrap();
        write_overlay(&vai_dir, &result.workspace.id, &[("src/lib.rs", MODIFIED_RS)]);

        let submit_result = submit(&vai_dir, &root).unwrap();

        assert_eq!(submit_result.version.version_id, "v2");
        assert_eq!(submit_result.version.intent, "add greeting");
        assert_eq!(submit_result.files_applied, 1);
    }

    #[test]
    fn test_fast_forward_advances_head() {
        let (_dir, root) = setup_repo(&[("src/lib.rs", BASE_RS)]);
        let vai_dir = root.join(".vai");

        let result = workspace::create(&vai_dir, "test", "v1").unwrap();
        write_overlay(&vai_dir, &result.workspace.id, &[("src/lib.rs", MODIFIED_RS)]);

        submit(&vai_dir, &root).unwrap();

        let head = repo::read_head(&vai_dir).unwrap();
        assert_eq!(head, "v2");
    }

    #[test]
    fn test_fast_forward_applies_overlay_to_root() {
        let (_dir, root) = setup_repo(&[("src/lib.rs", BASE_RS)]);
        let vai_dir = root.join(".vai");

        let result = workspace::create(&vai_dir, "test", "v1").unwrap();
        write_overlay(
            &vai_dir,
            &result.workspace.id,
            &[("src/lib.rs", MODIFIED_RS)],
        );

        submit(&vai_dir, &root).unwrap();

        let content = fs::read_to_string(root.join("src/lib.rs")).unwrap();
        assert_eq!(content, MODIFIED_RS);
    }

    #[test]
    fn test_fast_forward_marks_workspace_merged() {
        let (_dir, root) = setup_repo(&[("src/lib.rs", BASE_RS)]);
        let vai_dir = root.join(".vai");

        let result = workspace::create(&vai_dir, "test", "v1").unwrap();
        write_overlay(&vai_dir, &result.workspace.id, &[("src/lib.rs", MODIFIED_RS)]);

        let ws_id = result.workspace.id.to_string();
        submit(&vai_dir, &root).unwrap();

        let ws = workspace::get(&vai_dir, &ws_id).unwrap();
        assert_eq!(ws.status, WorkspaceStatus::Merged);
    }

    #[test]
    fn test_empty_overlay_fast_forward() {
        let (_dir, root) = setup_repo(&[("src/lib.rs", BASE_RS)]);
        let vai_dir = root.join(".vai");

        workspace::create(&vai_dir, "no-op", "v1").unwrap();

        let result = submit(&vai_dir, &root).unwrap();
        assert_eq!(result.files_applied, 0);
        assert_eq!(result.version.version_id, "v2");
    }

    // ── Three-level semantic merge tests ────────────────────────────────────

    /// Helper: submit workspace A (advances HEAD), then set up workspace B for testing.
    fn submit_first_workspace(
        vai_dir: &Path,
        root: &Path,
        intent_a: &str,
        files_a: &[(&str, &str)],
    ) {
        let result = workspace::create(vai_dir, intent_a, "v1").unwrap();
        write_overlay(vai_dir, &result.workspace.id, files_a);
        submit(vai_dir, root).unwrap();
    }

    #[test]
    fn test_two_changes_same_function_different_lines_auto_merge() {
        // Workspace: modifies line 2 (body of `foo`).
        // HEAD:      modifies line 5 (body of `bar`) — different function.
        // Expected:  Level-1 auto-merge, both changes present.
        let base = "fn foo() {\n    let x = 1;\n}\n\nfn bar() {\n    let y = 2;\n}\n";
        let (_dir, root) = setup_repo(&[("src/lib.rs", base)]);
        let vai_dir = root.join(".vai");

        // HEAD workspace: change bar.
        let head_ws = "fn foo() {\n    let x = 1;\n}\n\nfn bar() {\n    let y = 20;\n}\n";
        submit_first_workspace(&vai_dir, &root, "change bar", &[("src/lib.rs", head_ws)]);

        // Workspace B: based on v1, change foo (different lines from bar).
        let result = workspace::create(&vai_dir, "change foo", "v1").unwrap();
        let ws_content = "fn foo() {\n    let x = 10;\n}\n\nfn bar() {\n    let y = 2;\n}\n";
        write_overlay(&vai_dir, &result.workspace.id, &[("src/lib.rs", ws_content)]);

        let submit_result = submit(&vai_dir, &root).unwrap();

        assert_eq!(submit_result.auto_resolved, 1, "expected auto-merge");

        // Verify both changes are present in the merged file.
        let merged = fs::read_to_string(root.join("src/lib.rs")).unwrap();
        assert!(
            merged.contains("let x = 10;"),
            "workspace change (foo) should be present"
        );
        assert!(
            merged.contains("let y = 20;"),
            "head change (bar) should be present"
        );
    }

    #[test]
    fn test_both_add_new_functions_level2_merge() {
        // Workspace adds fn new_ws_fn; HEAD adds fn new_head_fn.
        // Both insert at the end of the same file (same insertion point in
        // base). Level-2 entity substitution should merge both.
        let base = "fn existing() {}\n";
        let (_dir, root) = setup_repo(&[("src/lib.rs", base)]);
        let vai_dir = root.join(".vai");

        // HEAD workspace: adds new_head_fn.
        let head_content = "fn existing() {}\nfn new_head_fn() {}\n";
        submit_first_workspace(
            &vai_dir,
            &root,
            "add head fn",
            &[("src/lib.rs", head_content)],
        );

        // Workspace B: based on v1, adds new_ws_fn.
        let result = workspace::create(&vai_dir, "add ws fn", "v1").unwrap();
        let ws_content = "fn existing() {}\nfn new_ws_fn() {}\n";
        write_overlay(&vai_dir, &result.workspace.id, &[("src/lib.rs", ws_content)]);

        // Should succeed (auto-merge at Level 1 or 2).
        let submit_result = submit(&vai_dir, &root);
        assert!(
            submit_result.is_ok(),
            "expected successful auto-merge, got: {submit_result:?}"
        );

        let merged = fs::read_to_string(root.join("src/lib.rs")).unwrap();
        assert!(
            merged.contains("new_ws_fn"),
            "workspace-added function should be present"
        );
    }

    #[test]
    fn test_rename_without_update_conflict() {
        // Workspace removes fn foo (renames to bar).
        // HEAD keeps fn foo (still has the name).
        // Level-3 should detect conflict.
        let base = "fn foo() -> u32 { 1 }\nfn caller() -> u32 { foo() }\n";
        let (_dir, root) = setup_repo(&[("src/lib.rs", base)]);
        let vai_dir = root.join(".vai");

        // HEAD workspace: modifies foo's body (keeps the name).
        let head_content = "fn foo() -> u32 { 42 }\nfn caller() -> u32 { foo() }\n";
        submit_first_workspace(&vai_dir, &root, "modify foo", &[("src/lib.rs", head_content)]);

        // Workspace B: removes foo, adds bar (rename without updating caller).
        let result = workspace::create(&vai_dir, "rename foo", "v1").unwrap();
        let ws_content = "fn bar() -> u32 { 1 }\nfn caller() -> u32 { foo() }\n";
        write_overlay(&vai_dir, &result.workspace.id, &[("src/lib.rs", ws_content)]);

        let submit_result = submit(&vai_dir, &root);
        assert!(
            matches!(submit_result, Err(MergeError::SemanticConflicts { .. })),
            "expected SemanticConflicts error, got: {submit_result:?}"
        );
    }

    #[test]
    fn test_same_entity_modified_by_both_conflict() {
        // Both workspace and HEAD modify the same function body.
        let base = "fn foo() -> u32 { 1 }\n";
        let (_dir, root) = setup_repo(&[("src/lib.rs", base)]);
        let vai_dir = root.join(".vai");

        // HEAD workspace: modifies foo.
        let head_content = "fn foo() -> u32 { 100 }\n";
        submit_first_workspace(&vai_dir, &root, "head change", &[("src/lib.rs", head_content)]);

        // Workspace B: also modifies foo differently.
        let result = workspace::create(&vai_dir, "ws change", "v1").unwrap();
        let ws_content = "fn foo() -> u32 { 200 }\n";
        write_overlay(&vai_dir, &result.workspace.id, &[("src/lib.rs", ws_content)]);

        let submit_result = submit(&vai_dir, &root);
        assert!(
            matches!(submit_result, Err(MergeError::SemanticConflicts { .. })),
            "expected SemanticConflicts, got: {submit_result:?}"
        );
    }

    #[test]
    fn test_conflict_persistence_and_resolve() {
        let base = "fn foo() -> u32 { 1 }\n";
        let (_dir, root) = setup_repo(&[("src/lib.rs", base)]);
        let vai_dir = root.join(".vai");

        // Advance HEAD.
        let head_content = "fn foo() -> u32 { 100 }\n";
        submit_first_workspace(&vai_dir, &root, "head", &[("src/lib.rs", head_content)]);

        // Workspace that conflicts.
        let result = workspace::create(&vai_dir, "ws", "v1").unwrap();
        let ws_content = "fn foo() -> u32 { 200 }\n";
        write_overlay(&vai_dir, &result.workspace.id, &[("src/lib.rs", ws_content)]);

        let _ = submit(&vai_dir, &root);

        // Conflicts should be persisted.
        let conflicts = list_conflicts(&vai_dir).unwrap();
        assert!(!conflicts.is_empty(), "expected persisted conflicts");

        let cid = conflicts[0].conflict_id.to_string();
        let resolved = resolve_conflict(&vai_dir, &cid).unwrap();
        assert!(resolved.resolved);

        // Re-reading should show resolved=true.
        let updated = list_conflicts(&vai_dir).unwrap();
        assert!(updated.iter().all(|c| c.resolved || c.conflict_id.to_string() != cid));
    }
}
