//! Merge pattern library — learn from resolved conflicts to improve future
//! auto-resolution.
//!
//! Every conflict resolution is recorded with the conflict pattern, strategy
//! chosen, and whether the resolution was successful.  Patterns that achieve
//! a >90% success rate across more than 10 instances are promoted to
//! auto-resolution.  A feedback loop demotes patterns back to manual
//! resolution when auto-resolved merges are subsequently rolled back.
//!
//! ## Storage
//!
//! Pattern data lives in `.vai/merge_patterns.db` (SQLite).  Two tables:
//!
//! - `merge_patterns` — one row per distinct conflict pattern.
//! - `resolution_instances` — one row per individual conflict resolution.
//!
//! ## Pattern Classification
//!
//! Patterns are classified from a [`ConflictRecord`] into one of several
//! named [`PatternType`] values.  The classification considers:
//! - The merge level (1 = textual, 2 = structural, 3 = referential).
//! - Keywords in the conflict description (rename, import, body, struct).
//! - The number of involved entities.
//!
//! A stable [`pattern_hash`] is derived from the pattern type and merge
//! level so that recurring identical patterns map to the same library entry.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::merge::ConflictRecord;

// ── Errors ────────────────────────────────────────────────────────────────────

/// Errors from merge pattern operations.
#[derive(Debug, Error)]
pub enum MergePatternError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("Pattern not found: {0}")]
    NotFound(i64),

    #[error("Pattern store not initialized at {0}")]
    NotInitialized(PathBuf),
}

// ── Pattern type ──────────────────────────────────────────────────────────────

/// The canonical type of a merge conflict pattern.
///
/// Classification is based on merge level, description keywords, and entity
/// types involved in the conflict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PatternType {
    /// Two agents concurrently add import/use statements to the same file.
    TwoAgentsAddImports,
    /// One agent renames an identifier; another adds a usage of the old name.
    RenameWithStaleReference,
    /// Two agents modify the body of the same function or method differently.
    TwoAgentsModifySameBody,
    /// Two agents modify the same struct definition (fields, derives, etc.).
    TwoAgentsModifySameStruct,
    /// Two agents add methods or associated functions to the same impl block.
    TwoAgentsAddMethods,
    /// Concurrent textual (line-level) modifications to the same file region.
    ConcurrentTextualModification,
    /// Any conflict not matched by a more specific pattern.
    Generic,
}

impl PatternType {
    /// Storage string used in SQLite.
    pub fn as_str(&self) -> &'static str {
        match self {
            PatternType::TwoAgentsAddImports => "two_agents_add_imports",
            PatternType::RenameWithStaleReference => "rename_with_stale_reference",
            PatternType::TwoAgentsModifySameBody => "two_agents_modify_same_body",
            PatternType::TwoAgentsModifySameStruct => "two_agents_modify_same_struct",
            PatternType::TwoAgentsAddMethods => "two_agents_add_methods",
            PatternType::ConcurrentTextualModification => "concurrent_textual_modification",
            PatternType::Generic => "generic",
        }
    }

    fn from_str(s: &str) -> Self {
        match s {
            "two_agents_add_imports" => PatternType::TwoAgentsAddImports,
            "rename_with_stale_reference" => PatternType::RenameWithStaleReference,
            "two_agents_modify_same_body" => PatternType::TwoAgentsModifySameBody,
            "two_agents_modify_same_struct" => PatternType::TwoAgentsModifySameStruct,
            "two_agents_add_methods" => PatternType::TwoAgentsAddMethods,
            "concurrent_textual_modification" => PatternType::ConcurrentTextualModification,
            _ => PatternType::Generic,
        }
    }

    /// Human-readable description of the pattern.
    pub fn description(&self) -> &'static str {
        match self {
            PatternType::TwoAgentsAddImports => {
                "Two agents add imports to the same file"
            }
            PatternType::RenameWithStaleReference => {
                "One agent renames an identifier, another adds usage of old name"
            }
            PatternType::TwoAgentsModifySameBody => {
                "Two agents modify the same function body differently"
            }
            PatternType::TwoAgentsModifySameStruct => {
                "Two agents modify the same struct definition"
            }
            PatternType::TwoAgentsAddMethods => {
                "Two agents add methods to the same impl block"
            }
            PatternType::ConcurrentTextualModification => {
                "Concurrent line-level modifications to the same file region"
            }
            PatternType::Generic => "Generic unclassified merge conflict",
        }
    }
}

// ── Core data types ───────────────────────────────────────────────────────────

/// A merge conflict pattern tracked in the library.
///
/// Each pattern represents a recurring class of conflict.  Success rate and
/// instance count determine whether the pattern is eligible for auto-resolution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergePattern {
    /// Auto-incrementing database ID.
    pub id: i64,
    /// Pattern classification.
    pub pattern_type: PatternType,
    /// Stable hash for deduplicating recurrences of the same pattern.
    pub pattern_hash: String,
    /// Human-readable description of the pattern.
    pub description: String,
    /// Total number of recorded resolutions for this pattern.
    pub instance_count: i64,
    /// Number of resolutions that were NOT subsequently rolled back.
    pub success_count: i64,
    /// Whether this pattern is currently eligible for auto-resolution.
    /// `true` only when success_rate ≥ 90 % and instance_count > 10 and not
    /// disabled by a human.
    pub auto_resolution_enabled: bool,
    /// Set to `true` when a human has explicitly disabled auto-resolution.
    pub disabled_by_human: bool,
    /// When the pattern was first observed.
    pub created_at: DateTime<Utc>,
    /// When the pattern was last updated.
    pub updated_at: DateTime<Utc>,
}

impl MergePattern {
    /// Success rate as a value in `[0.0, 1.0]`.  Returns 1.0 if no instances.
    pub fn success_rate(&self) -> f64 {
        if self.instance_count == 0 {
            1.0
        } else {
            self.success_count as f64 / self.instance_count as f64
        }
    }

    /// Whether this pattern currently meets the promotion criteria.
    ///
    /// Criteria: success_rate ≥ 0.90 AND instance_count > 10 AND not disabled.
    pub fn meets_promotion_criteria(&self) -> bool {
        !self.disabled_by_human
            && self.instance_count > 10
            && self.success_rate() >= 0.90
    }
}

/// A single recorded conflict resolution instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolutionInstance {
    /// Auto-incrementing database ID.
    pub id: i64,
    /// Pattern this instance belongs to.
    pub pattern_id: i64,
    /// The conflict ID from the merge engine.
    pub conflict_id: Uuid,
    /// The workspace that triggered the conflict.
    pub workspace_id: Uuid,
    /// The strategy used to resolve the conflict (e.g., "manual", "auto").
    pub strategy: String,
    /// Whether this resolution was auto-applied via pattern matching.
    pub was_auto_resolved: bool,
    /// Whether this resolution was subsequently rolled back.
    pub rolled_back: bool,
    /// When the resolution was recorded.
    pub resolved_at: DateTime<Utc>,
}

// ── Store ─────────────────────────────────────────────────────────────────────

/// SQLite-backed merge pattern library.
pub struct MergePatternStore {
    conn: Connection,
}

impl MergePatternStore {
    /// Opens (or creates) the pattern store at `.vai/merge_patterns.db`.
    pub fn open(vai_dir: &Path) -> Result<Self, MergePatternError> {
        let db_path = vai_dir.join("merge_patterns.db");
        let conn = Connection::open(&db_path)?;
        let store = Self { conn };
        store.init_schema()?;
        Ok(store)
    }

    fn init_schema(&self) -> Result<(), MergePatternError> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS merge_patterns (
                id                    INTEGER PRIMARY KEY AUTOINCREMENT,
                pattern_type          TEXT NOT NULL,
                pattern_hash          TEXT NOT NULL UNIQUE,
                description           TEXT NOT NULL,
                instance_count        INTEGER NOT NULL DEFAULT 0,
                success_count         INTEGER NOT NULL DEFAULT 0,
                auto_resolution_enabled INTEGER NOT NULL DEFAULT 0,
                disabled_by_human     INTEGER NOT NULL DEFAULT 0,
                created_at            TEXT NOT NULL,
                updated_at            TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS resolution_instances (
                id                    INTEGER PRIMARY KEY AUTOINCREMENT,
                pattern_id            INTEGER NOT NULL REFERENCES merge_patterns(id),
                conflict_id           TEXT NOT NULL,
                workspace_id          TEXT NOT NULL,
                strategy              TEXT NOT NULL,
                was_auto_resolved     INTEGER NOT NULL DEFAULT 0,
                rolled_back           INTEGER NOT NULL DEFAULT 0,
                resolved_at           TEXT NOT NULL
            );",
        )?;
        Ok(())
    }

    // ── Pattern classification ─────────────────────────────────────────────

    /// Classifies a [`ConflictRecord`] into a [`PatternType`].
    pub fn classify(record: &ConflictRecord) -> PatternType {
        let desc = record.description.to_lowercase();

        if record.merge_level == 1 {
            return PatternType::ConcurrentTextualModification;
        }

        // Check for rename / stale reference signals.
        if desc.contains("rename") || desc.contains("stale") || desc.contains("old name") {
            return PatternType::RenameWithStaleReference;
        }

        // Import-level conflicts (level 2, use statements).
        if desc.contains("import") || desc.contains("use statement") || desc.contains("use decl") {
            return PatternType::TwoAgentsAddImports;
        }

        // Struct-level conflicts.
        if desc.contains("struct") || desc.contains("field") {
            return PatternType::TwoAgentsModifySameStruct;
        }

        // Method / impl-block additions.
        if desc.contains("method") || desc.contains("impl") || desc.contains("associated") {
            return PatternType::TwoAgentsAddMethods;
        }

        // Function body conflicts (level 3, most common).
        if record.merge_level == 3
            || desc.contains("function")
            || desc.contains("body")
            || desc.contains("fn ")
        {
            return PatternType::TwoAgentsModifySameBody;
        }

        PatternType::Generic
    }

    /// Computes a stable hash for a conflict pattern.
    ///
    /// The hash is derived from the pattern type and merge level, so identical
    /// patterns recurring across different workspaces map to the same entry.
    pub fn compute_hash(pattern_type: &PatternType, merge_level: u8) -> String {
        let mut hasher = Sha256::new();
        hasher.update(pattern_type.as_str().as_bytes());
        hasher.update([merge_level]);
        format!("{:x}", hasher.finalize())
    }

    // ── Write operations ───────────────────────────────────────────────────

    /// Records a conflict resolution and updates pattern statistics.
    ///
    /// If a pattern with the same hash already exists it is updated in-place;
    /// otherwise a new pattern row is inserted.  Returns the pattern that was
    /// updated or created.
    pub fn record_resolution(
        &mut self,
        record: &ConflictRecord,
        strategy: &str,
        was_auto_resolved: bool,
    ) -> Result<MergePattern, MergePatternError> {
        let pattern_type = Self::classify(record);
        let hash = Self::compute_hash(&pattern_type, record.merge_level);
        let now = Utc::now().to_rfc3339();

        // Upsert the pattern row.
        let existing = self.find_by_hash(&hash)?;
        let pattern_id = match existing {
            Some(ref p) => {
                self.conn.execute(
                    "UPDATE merge_patterns
                     SET instance_count = instance_count + 1,
                         success_count  = success_count + 1,
                         updated_at     = ?1
                     WHERE id = ?2",
                    params![now, p.id],
                )?;
                p.id
            }
            None => {
                self.conn.execute(
                    "INSERT INTO merge_patterns
                     (pattern_type, pattern_hash, description, instance_count,
                      success_count, auto_resolution_enabled, disabled_by_human,
                      created_at, updated_at)
                     VALUES (?1, ?2, ?3, 1, 1, 0, 0, ?4, ?4)",
                    params![
                        pattern_type.as_str(),
                        hash,
                        pattern_type.description(),
                        now
                    ],
                )?;
                self.conn.last_insert_rowid()
            }
        };

        // Record the individual resolution instance.
        self.conn.execute(
            "INSERT INTO resolution_instances
             (pattern_id, conflict_id, workspace_id, strategy,
              was_auto_resolved, rolled_back, resolved_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6)",
            params![
                pattern_id,
                record.conflict_id.to_string(),
                record.workspace_id.to_string(),
                strategy,
                was_auto_resolved as i32,
                now
            ],
        )?;

        // Re-evaluate auto-resolution promotion.
        self.update_promotion(pattern_id)?;

        self.get_pattern(pattern_id)
    }

    /// Records that a previously auto-resolved merge was rolled back.
    ///
    /// Decrements the pattern's success count.  If the success rate drops
    /// below 90 %, the pattern is demoted back to manual resolution.
    pub fn record_rollback(
        &mut self,
        conflict_id: Uuid,
    ) -> Result<Option<MergePattern>, MergePatternError> {
        // Find the resolution instance for this conflict.
        let row: Option<(i64, i64)> = self
            .conn
            .query_row(
                "SELECT id, pattern_id FROM resolution_instances
                 WHERE conflict_id = ?1 AND rolled_back = 0
                 LIMIT 1",
                params![conflict_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;

        let (instance_id, pattern_id) = match row {
            Some(r) => r,
            None => return Ok(None),
        };

        let now = Utc::now().to_rfc3339();

        // Mark the instance as rolled back.
        self.conn.execute(
            "UPDATE resolution_instances SET rolled_back = 1 WHERE id = ?1",
            params![instance_id],
        )?;

        // Decrement success count (floor at 0).
        self.conn.execute(
            "UPDATE merge_patterns
             SET success_count = MAX(0, success_count - 1),
                 updated_at    = ?1
             WHERE id = ?2",
            params![now, pattern_id],
        )?;

        // Re-evaluate promotion / demotion.
        self.update_promotion(pattern_id)?;

        Ok(Some(self.get_pattern(pattern_id)?))
    }

    /// Disables auto-resolution for a pattern by ID (human override).
    ///
    /// The pattern remains in the library for analytics but will not be used
    /// to auto-resolve future conflicts.
    pub fn disable_pattern(&mut self, pattern_id: i64) -> Result<MergePattern, MergePatternError> {
        let now = Utc::now().to_rfc3339();
        let rows = self.conn.execute(
            "UPDATE merge_patterns
             SET disabled_by_human = 1, auto_resolution_enabled = 0, updated_at = ?1
             WHERE id = ?2",
            params![now, pattern_id],
        )?;
        if rows == 0 {
            return Err(MergePatternError::NotFound(pattern_id));
        }
        self.get_pattern(pattern_id)
    }

    /// Re-enables auto-resolution for a previously disabled pattern.
    ///
    /// The pattern must still meet promotion criteria after re-enabling.
    pub fn enable_pattern(&mut self, pattern_id: i64) -> Result<MergePattern, MergePatternError> {
        let now = Utc::now().to_rfc3339();
        let rows = self.conn.execute(
            "UPDATE merge_patterns
             SET disabled_by_human = 0, updated_at = ?1
             WHERE id = ?2",
            params![now, pattern_id],
        )?;
        if rows == 0 {
            return Err(MergePatternError::NotFound(pattern_id));
        }
        self.update_promotion(pattern_id)?;
        self.get_pattern(pattern_id)
    }

    // ── Read operations ────────────────────────────────────────────────────

    /// Returns all patterns sorted by instance count descending.
    pub fn list_patterns(&self) -> Result<Vec<MergePattern>, MergePatternError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, pattern_type, pattern_hash, description, instance_count,
                    success_count, auto_resolution_enabled, disabled_by_human,
                    created_at, updated_at
             FROM merge_patterns
             ORDER BY instance_count DESC",
        )?;
        let rows = stmt.query_map([], |row| self.row_to_pattern(row))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Returns a single pattern by ID.
    pub fn get_pattern(&self, id: i64) -> Result<MergePattern, MergePatternError> {
        self.conn
            .query_row(
                "SELECT id, pattern_type, pattern_hash, description, instance_count,
                        success_count, auto_resolution_enabled, disabled_by_human,
                        created_at, updated_at
                 FROM merge_patterns WHERE id = ?1",
                params![id],
                |row| self.row_to_pattern(row),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => MergePatternError::NotFound(id),
                other => MergePatternError::Sqlite(other),
            })
    }

    /// Checks whether a conflict has a matching auto-resolution pattern.
    ///
    /// Returns `Some(pattern)` if the conflict should be auto-resolved, or
    /// `None` if it must be escalated for manual resolution.
    pub fn check_auto_resolution(
        &self,
        record: &ConflictRecord,
    ) -> Result<Option<MergePattern>, MergePatternError> {
        let pattern_type = Self::classify(record);
        let hash = Self::compute_hash(&pattern_type, record.merge_level);
        let pattern = self.find_by_hash(&hash)?;
        Ok(pattern.filter(|p| p.auto_resolution_enabled))
    }

    // ── Private helpers ────────────────────────────────────────────────────

    fn find_by_hash(&self, hash: &str) -> Result<Option<MergePattern>, MergePatternError> {
        self.conn
            .query_row(
                "SELECT id, pattern_type, pattern_hash, description, instance_count,
                        success_count, auto_resolution_enabled, disabled_by_human,
                        created_at, updated_at
                 FROM merge_patterns WHERE pattern_hash = ?1",
                params![hash],
                |row| self.row_to_pattern(row),
            )
            .optional()
            .map_err(Into::into)
    }

    /// Updates `auto_resolution_enabled` based on current statistics.
    fn update_promotion(&self, pattern_id: i64) -> Result<(), MergePatternError> {
        let pattern = self.get_pattern(pattern_id)?;
        if pattern.disabled_by_human {
            // Human override takes precedence — never auto-promote.
            return Ok(());
        }
        let should_enable = pattern.meets_promotion_criteria();
        self.conn.execute(
            "UPDATE merge_patterns SET auto_resolution_enabled = ?1 WHERE id = ?2",
            params![should_enable as i32, pattern_id],
        )?;
        Ok(())
    }

    fn row_to_pattern(
        &self,
        row: &rusqlite::Row<'_>,
    ) -> rusqlite::Result<MergePattern> {
        let created_str: String = row.get(8)?;
        let updated_str: String = row.get(9)?;
        Ok(MergePattern {
            id: row.get(0)?,
            pattern_type: PatternType::from_str(&row.get::<_, String>(1)?),
            pattern_hash: row.get(2)?,
            description: row.get(3)?,
            instance_count: row.get(4)?,
            success_count: row.get(5)?,
            auto_resolution_enabled: row.get::<_, i32>(6)? != 0,
            disabled_by_human: row.get::<_, i32>(7)? != 0,
            created_at: created_str.parse().unwrap_or_else(|_| Utc::now()),
            updated_at: updated_str.parse().unwrap_or_else(|_| Utc::now()),
        })
    }
}

// ── Trait for optional query ──────────────────────────────────────────────────

trait OptionalExt<T> {
    fn optional(self) -> Result<Option<T>, rusqlite::Error>;
}

impl<T> OptionalExt<T> for Result<T, rusqlite::Error> {
    fn optional(self) -> Result<Option<T>, rusqlite::Error> {
        match self {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_log::ConflictSeverity;
    use tempfile::tempdir;

    fn make_conflict(merge_level: u8, description: &str) -> ConflictRecord {
        ConflictRecord {
            conflict_id: Uuid::new_v4(),
            workspace_id: Uuid::new_v4(),
            file_path: "src/auth.rs".to_string(),
            entity_ids: vec!["auth::AuthService".to_string()],
            description: description.to_string(),
            severity: ConflictSeverity::Medium,
            merge_level,
            resolved: false,
        }
    }

    #[test]
    fn test_classify_import_conflict() {
        let record = make_conflict(2, "use statement conflict in imports");
        assert_eq!(MergePatternStore::classify(&record), PatternType::TwoAgentsAddImports);
    }

    #[test]
    fn test_classify_body_conflict() {
        let record = make_conflict(3, "function body modified by both agents");
        assert_eq!(
            MergePatternStore::classify(&record),
            PatternType::TwoAgentsModifySameBody
        );
    }

    #[test]
    fn test_classify_rename_conflict() {
        let record = make_conflict(3, "rename conflict: stale reference to old name");
        assert_eq!(
            MergePatternStore::classify(&record),
            PatternType::RenameWithStaleReference
        );
    }

    #[test]
    fn test_classify_textual_conflict() {
        let record = make_conflict(1, "overlapping line changes");
        assert_eq!(
            MergePatternStore::classify(&record),
            PatternType::ConcurrentTextualModification
        );
    }

    #[test]
    fn test_record_resolution_creates_pattern() {
        let dir = tempdir().unwrap();
        let mut store = MergePatternStore::open(dir.path()).unwrap();
        let record = make_conflict(3, "function body conflict");

        let pattern = store.record_resolution(&record, "manual", false).unwrap();
        assert_eq!(pattern.instance_count, 1);
        assert_eq!(pattern.success_count, 1);
        assert!(!pattern.auto_resolution_enabled);
    }

    #[test]
    fn test_record_resolution_upserts_existing_pattern() {
        let dir = tempdir().unwrap();
        let mut store = MergePatternStore::open(dir.path()).unwrap();

        for _ in 0..5 {
            let r = make_conflict(3, "function body conflict");
            store.record_resolution(&r, "manual", false).unwrap();
        }

        let patterns = store.list_patterns().unwrap();
        assert_eq!(patterns.len(), 1);
        assert_eq!(patterns[0].instance_count, 5);
    }

    #[test]
    fn test_promotion_after_threshold() {
        let dir = tempdir().unwrap();
        let mut store = MergePatternStore::open(dir.path()).unwrap();

        // Record 11 successful resolutions — should trigger auto-promotion.
        for _ in 0..11 {
            let r = make_conflict(3, "function body conflict in auth module");
            store.record_resolution(&r, "manual", false).unwrap();
        }

        let patterns = store.list_patterns().unwrap();
        assert_eq!(patterns.len(), 1);
        assert!(patterns[0].auto_resolution_enabled, "should be promoted");
    }

    #[test]
    fn test_rollback_decrements_success_and_demotes() {
        let dir = tempdir().unwrap();
        let mut store = MergePatternStore::open(dir.path()).unwrap();

        // Build up a promoted pattern.
        for _ in 0..11 {
            let mut r = make_conflict(3, "function body conflict in test module");
            let _ = r.conflict_id;
            r.resolved = true;
            store.record_resolution(&r, "manual", false).unwrap();
        }

        // Verify promoted.
        let pattern = store.list_patterns().unwrap().remove(0);
        assert!(pattern.auto_resolution_enabled);
        let pattern_id = pattern.id;

        // Simulate rolling back many resolutions to drop below 90 %.
        // We need to roll back enough to go below threshold.
        // instance_count=11, success_count=11 → need success_count < 9.9
        // Roll back 3 → success_count=8, rate=8/11=72.7% < 90%
        let conflict_ids: Vec<Uuid> = {
            let mut stmt = store.conn.prepare(
                "SELECT conflict_id FROM resolution_instances WHERE pattern_id = ?1 LIMIT 3"
            ).unwrap();
            stmt.query_map(params![pattern_id], |row| {
                let s: String = row.get(0)?;
                Ok(s.parse::<Uuid>().unwrap())
            })
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
        };

        for cid in conflict_ids {
            store.record_rollback(cid).unwrap();
        }

        let pattern = store.get_pattern(pattern_id).unwrap();
        assert!(!pattern.auto_resolution_enabled, "should be demoted");
    }

    #[test]
    fn test_disable_prevents_auto_resolution() {
        let dir = tempdir().unwrap();
        let mut store = MergePatternStore::open(dir.path()).unwrap();

        // Build a promoted pattern.
        for _ in 0..11 {
            let r = make_conflict(2, "use statement conflict in imports section");
            store.record_resolution(&r, "manual", false).unwrap();
        }

        let pattern = store.list_patterns().unwrap().remove(0);
        assert!(pattern.auto_resolution_enabled);

        store.disable_pattern(pattern.id).unwrap();

        let r = make_conflict(2, "use statement conflict in imports section");
        let auto = store.check_auto_resolution(&r).unwrap();
        assert!(auto.is_none(), "disabled pattern should not auto-resolve");
    }

    #[test]
    fn test_check_auto_resolution_returns_none_below_threshold() {
        let dir = tempdir().unwrap();
        let mut store = MergePatternStore::open(dir.path()).unwrap();

        // Only 5 instances — not enough for promotion.
        for _ in 0..5 {
            let r = make_conflict(3, "function body changed by two agents");
            store.record_resolution(&r, "manual", false).unwrap();
        }

        let r = make_conflict(3, "function body changed by two agents");
        let result = store.check_auto_resolution(&r).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_pattern_hash_stable() {
        let h1 = MergePatternStore::compute_hash(&PatternType::TwoAgentsModifySameBody, 3);
        let h2 = MergePatternStore::compute_hash(&PatternType::TwoAgentsModifySameBody, 3);
        assert_eq!(h1, h2);
        let h3 = MergePatternStore::compute_hash(&PatternType::TwoAgentsAddImports, 3);
        assert_ne!(h1, h3);
    }
}
