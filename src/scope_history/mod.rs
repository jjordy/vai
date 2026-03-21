//! Intent history store — learns from past workspace completions to improve
//! scope predictions.
//!
//! When a workspace is successfully merged, the intent text and the set of
//! entities actually touched are recorded here. Future scope inference calls
//! can query this store to bias predictions toward entities that were
//! historically relevant for similar intents.
//!
//! ## Storage
//!
//! Records are persisted in a SQLite database at `.vai/graph/history.db`.
//!
//! ## Accuracy tracking
//!
//! For intents where a predicted scope was recorded alongside the actual
//! entities, precision and recall metrics can be computed with [`accuracy`].

use std::path::Path;

use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

// ── Error type ────────────────────────────────────────────────────────────────

/// Errors from scope history operations.
#[derive(Debug, Error)]
pub enum ScopeHistoryError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

// ── Data types ────────────────────────────────────────────────────────────────

/// A recorded entry linking an intent to the entities it actually touched.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntentRecord {
    /// Unique record ID.
    pub id: Uuid,
    /// The original intent text.
    pub intent_text: String,
    /// Terms extracted from the intent at record time.
    pub terms: Vec<String>,
    /// Entity IDs predicted by the scope inference engine (may be empty if
    /// inference was not run before submission).
    pub predicted_entity_ids: Vec<String>,
    /// Entity IDs actually modified in the workspace.
    pub actual_entity_ids: Vec<String>,
    /// Workspace ID this record originated from (for traceability).
    pub workspace_id: Option<String>,
    /// When the workspace was merged.
    pub recorded_at: DateTime<Utc>,
}

/// Aggregated accuracy metrics over a set of intent records.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccuracyMetrics {
    /// Number of intents evaluated (those with both predicted and actual IDs).
    pub sample_count: usize,
    /// Average recall — fraction of actual entities that were predicted.
    /// Target: ≥ 0.70.
    pub avg_recall: f64,
    /// Average precision — fraction of predictions that were actual entities.
    /// Target: ≤ 0.30 false-positive rate (i.e. precision ≥ 0.70).
    pub avg_precision: f64,
    /// Combined F1 score.
    pub f1_score: f64,
}

/// An entity weight entry — entity ID plus its historical relevance score.
#[derive(Debug, Clone)]
pub struct EntityWeight {
    /// Stable entity ID.
    pub entity_id: String,
    /// Accumulated weight from matching historical records.
    pub weight: f64,
}

// ── Store ─────────────────────────────────────────────────────────────────────

/// Persistent store for intent history records.
pub struct ScopeHistoryStore {
    conn: Connection,
}

impl ScopeHistoryStore {
    /// Opens (or creates) the history database at `db_path`.
    pub fn open(db_path: &Path) -> Result<Self, ScopeHistoryError> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let conn = Connection::open(db_path)?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<(), ScopeHistoryError> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS intent_history (
                id                       TEXT PRIMARY KEY,
                intent_text              TEXT NOT NULL,
                terms_json               TEXT NOT NULL,
                predicted_entity_ids_json TEXT NOT NULL,
                actual_entity_ids_json   TEXT NOT NULL,
                workspace_id             TEXT,
                recorded_at              TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_ih_recorded_at
                ON intent_history(recorded_at);",
        )?;
        Ok(())
    }

    /// Records an intent along with the entities it actually touched.
    ///
    /// `predicted_entity_ids` may be empty if inference was not run before
    /// the workspace was submitted.
    pub fn record(
        &self,
        intent_text: &str,
        terms: &[String],
        predicted_entity_ids: &[String],
        actual_entity_ids: &[String],
        workspace_id: Option<&str>,
    ) -> Result<IntentRecord, ScopeHistoryError> {
        let id = Uuid::new_v4();
        let recorded_at = Utc::now();

        let terms_json = serde_json::to_string(terms)?;
        let predicted_json = serde_json::to_string(predicted_entity_ids)?;
        let actual_json = serde_json::to_string(actual_entity_ids)?;

        self.conn.execute(
            "INSERT INTO intent_history
                (id, intent_text, terms_json, predicted_entity_ids_json,
                 actual_entity_ids_json, workspace_id, recorded_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                id.to_string(),
                intent_text,
                terms_json,
                predicted_json,
                actual_json,
                workspace_id,
                recorded_at.to_rfc3339(),
            ],
        )?;

        Ok(IntentRecord {
            id,
            intent_text: intent_text.to_string(),
            terms: terms.to_vec(),
            predicted_entity_ids: predicted_entity_ids.to_vec(),
            actual_entity_ids: actual_entity_ids.to_vec(),
            workspace_id: workspace_id.map(str::to_string),
            recorded_at,
        })
    }

    /// Returns up to `limit` of the most recent intent records.
    pub fn list_recent(&self, limit: usize) -> Result<Vec<IntentRecord>, ScopeHistoryError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, intent_text, terms_json, predicted_entity_ids_json,
                    actual_entity_ids_json, workspace_id, recorded_at
             FROM intent_history
             ORDER BY recorded_at DESC
             LIMIT ?1",
        )?;

        let rows = stmt.query_map(params![limit as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, String>(6)?,
            ))
        })?;

        let mut records = Vec::new();
        for row in rows {
            let (id_str, intent_text, terms_json, predicted_json, actual_json, workspace_id, recorded_at_str) = row?;
            records.push(IntentRecord {
                id: id_str.parse().unwrap_or_default(),
                intent_text,
                terms: serde_json::from_str(&terms_json)?,
                predicted_entity_ids: serde_json::from_str(&predicted_json)?,
                actual_entity_ids: serde_json::from_str(&actual_json)?,
                workspace_id,
                recorded_at: recorded_at_str
                    .parse::<DateTime<Utc>>()
                    .unwrap_or_else(|_| Utc::now()),
            });
        }

        Ok(records)
    }

    /// Computes per-entity weights by finding historical intents that share
    /// terms with the query terms, then weighting each entity in those intents
    /// by the fraction of query terms that overlap.
    ///
    /// Returns weights sorted by descending score.
    pub fn compute_entity_weights(
        &self,
        query_terms: &[String],
    ) -> Result<Vec<EntityWeight>, ScopeHistoryError> {
        if query_terms.is_empty() {
            return Ok(Vec::new());
        }

        // Load all records (intent history is expected to be small, O(thousands)).
        let all_records = self.list_recent(10_000)?;

        let mut weights: std::collections::HashMap<String, f64> = std::collections::HashMap::new();

        for record in &all_records {
            if record.actual_entity_ids.is_empty() {
                continue;
            }
            // Jaccard-like overlap: shared_terms / query_terms
            let overlap = record
                .terms
                .iter()
                .filter(|t| query_terms.contains(t))
                .count();
            if overlap == 0 {
                continue;
            }
            let score = overlap as f64 / query_terms.len() as f64;
            for entity_id in &record.actual_entity_ids {
                *weights.entry(entity_id.clone()).or_insert(0.0) += score;
            }
        }

        let mut result: Vec<EntityWeight> = weights
            .into_iter()
            .map(|(entity_id, weight)| EntityWeight { entity_id, weight })
            .collect();
        result.sort_by(|a, b| b.weight.partial_cmp(&a.weight).unwrap_or(std::cmp::Ordering::Equal));
        Ok(result)
    }

    /// Computes precision and recall metrics over the last `limit` intent
    /// records that have both predicted and actual entity IDs.
    pub fn accuracy(&self, limit: usize) -> Result<AccuracyMetrics, ScopeHistoryError> {
        let records = self.list_recent(limit * 2)?; // fetch more to find valid ones

        let mut recall_sum = 0.0f64;
        let mut precision_sum = 0.0f64;
        let mut count = 0usize;

        for record in &records {
            if record.predicted_entity_ids.is_empty() || record.actual_entity_ids.is_empty() {
                continue;
            }
            if count >= limit {
                break;
            }

            let actual_set: std::collections::HashSet<&str> =
                record.actual_entity_ids.iter().map(String::as_str).collect();
            let predicted_set: std::collections::HashSet<&str> =
                record.predicted_entity_ids.iter().map(String::as_str).collect();

            let true_positives = predicted_set.intersection(&actual_set).count() as f64;
            let recall = true_positives / actual_set.len() as f64;
            let precision = true_positives / predicted_set.len() as f64;

            recall_sum += recall;
            precision_sum += precision;
            count += 1;
        }

        if count == 0 {
            return Ok(AccuracyMetrics {
                sample_count: 0,
                avg_recall: 0.0,
                avg_precision: 0.0,
                f1_score: 0.0,
            });
        }

        let avg_recall = recall_sum / count as f64;
        let avg_precision = precision_sum / count as f64;
        let f1_score = if avg_recall + avg_precision > 0.0 {
            2.0 * avg_precision * avg_recall / (avg_precision + avg_recall)
        } else {
            0.0
        };

        Ok(AccuracyMetrics {
            sample_count: count,
            avg_recall,
            avg_precision,
            f1_score,
        })
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn open_store() -> (ScopeHistoryStore, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let store = ScopeHistoryStore::open(&dir.path().join("history.db")).unwrap();
        (store, dir)
    }

    #[test]
    fn test_record_and_list() {
        let (store, _dir) = open_store();
        store
            .record(
                "add rate limiting to auth",
                &["rate".to_string(), "limiting".to_string(), "auth".to_string()],
                &["entity_a".to_string()],
                &["entity_a".to_string(), "entity_b".to_string()],
                Some("ws-001"),
            )
            .unwrap();

        let records = store.list_recent(10).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].intent_text, "add rate limiting to auth");
        assert_eq!(records[0].actual_entity_ids.len(), 2);
    }

    #[test]
    fn test_compute_entity_weights_matching_terms() {
        let (store, _dir) = open_store();
        // Record: auth intent touched entity_auth
        store
            .record(
                "fix auth token validation",
                &["auth".to_string(), "token".to_string(), "validation".to_string()],
                &[],
                &["entity_auth".to_string()],
                None,
            )
            .unwrap();
        // Record: rate intent touched entity_rate
        store
            .record(
                "improve rate limiting",
                &["rate".to_string(), "limiting".to_string()],
                &[],
                &["entity_rate".to_string()],
                None,
            )
            .unwrap();

        // Query with auth terms — entity_auth should have higher weight.
        let weights = store
            .compute_entity_weights(&["auth".to_string(), "token".to_string()])
            .unwrap();

        let auth_weight = weights.iter().find(|w| w.entity_id == "entity_auth");
        let rate_weight = weights.iter().find(|w| w.entity_id == "entity_rate");
        assert!(auth_weight.is_some(), "entity_auth should appear");
        assert!(auth_weight.unwrap().weight > 0.0);
        // entity_rate should not appear (no overlapping terms)
        assert!(rate_weight.is_none());
    }

    #[test]
    fn test_compute_entity_weights_no_overlap() {
        let (store, _dir) = open_store();
        store
            .record(
                "fix something unrelated",
                &["something".to_string(), "unrelated".to_string()],
                &[],
                &["entity_x".to_string()],
                None,
            )
            .unwrap();

        let weights = store
            .compute_entity_weights(&["auth".to_string(), "token".to_string()])
            .unwrap();
        assert!(weights.is_empty());
    }

    #[test]
    fn test_accuracy_with_predictions() {
        let (store, _dir) = open_store();
        // Perfect prediction
        store
            .record(
                "fix auth",
                &["auth".to_string()],
                &["e1".to_string(), "e2".to_string()],
                &["e1".to_string(), "e2".to_string()],
                None,
            )
            .unwrap();
        // Partial prediction: predicted 3, actual 2 (1 in common)
        store
            .record(
                "fix token",
                &["token".to_string()],
                &["e1".to_string(), "e3".to_string(), "e4".to_string()],
                &["e1".to_string(), "e5".to_string()],
                None,
            )
            .unwrap();

        let metrics = store.accuracy(10).unwrap();
        assert_eq!(metrics.sample_count, 2);
        // First record: recall=1.0, precision=1.0
        // Second record: recall=0.5 (1/2), precision=0.333 (1/3)
        assert!((metrics.avg_recall - 0.75).abs() < 0.01);
    }

    #[test]
    fn test_accuracy_empty() {
        let (store, _dir) = open_store();
        let metrics = store.accuracy(10).unwrap();
        assert_eq!(metrics.sample_count, 0);
        assert_eq!(metrics.avg_recall, 0.0);
        assert_eq!(metrics.avg_precision, 0.0);
    }
}
