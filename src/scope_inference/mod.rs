//! Scope inference engine — predict which semantic entities will be affected
//! by a natural language intent.
//!
//! ## Algorithm
//!
//! 1. **Term extraction** — tokenize the intent text, lower-case, remove stop
//!    words, and deduplicate.  Terms shorter than 3 characters are dropped.
//!
//! 2. **Direct matching** — for each term, query the semantic graph for entities
//!    whose name, qualified name, or file path contains the term
//!    (case-insensitive).  An exact name match yields `HIGH` confidence; a
//!    substring or path match yields `MEDIUM`.
//!
//! 3. **N-hop expansion** — starting from the directly-matched seed set, do a
//!    BFS over the relationship graph up to `max_hops` (default 2).  Entities
//!    reached at hop 1 get `MEDIUM` confidence; those at hop 2 get `LOW`.
//!    Entities already assigned a higher confidence are not downgraded.
//!
//! ## Usage
//!
//! ```rust,ignore
//! let snapshot = GraphSnapshot::open(&snapshot_path)?;
//! let result = infer(&snapshot, "add rate limiting to auth service", 2)?;
//! for item in &result.predicted_scope {
//!     println!("{:?}  {}", item.confidence, item.entity.qualified_name);
//! }
//! ```

use std::collections::{HashMap, VecDeque};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::graph::{Entity, GraphError, GraphSnapshot};
use crate::scope_history::{ScopeHistoryError, ScopeHistoryStore};

// ── Error type ────────────────────────────────────────────────────────────────

/// Errors from scope inference operations.
#[derive(Debug, Error)]
pub enum ScopeInferenceError {
    #[error("Graph error: {0}")]
    Graph(#[from] GraphError),

    #[error("Scope history error: {0}")]
    History(#[from] ScopeHistoryError),
}

// ── Confidence levels ─────────────────────────────────────────────────────────

/// The confidence that a predicted entity will be affected by an intent.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum ConfidenceLevel {
    /// 2-hop transitive dependency — weakest signal.
    Low,
    /// Partial name / path match, or 1-hop relationship from a direct match.
    Medium,
    /// Direct name match against an extracted term.
    High,
}

impl ConfidenceLevel {
    /// Returns the uppercase label used in human-readable output.
    pub fn label(&self) -> &'static str {
        match self {
            ConfidenceLevel::High => "HIGH",
            ConfidenceLevel::Medium => "MEDIUM",
            ConfidenceLevel::Low => "LOW",
        }
    }
}

// ── Result types ──────────────────────────────────────────────────────────────

/// A single entity in the predicted scope, with a confidence level and reason.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopedEntity {
    /// The predicted entity.
    pub entity: Entity,
    /// How confident the engine is that this entity will be affected.
    pub confidence: ConfidenceLevel,
    /// Human-readable explanation of why this entity was included.
    pub reason: String,
}

/// A single historical record that influenced the scope prediction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryInfluence {
    /// The past intent text.
    pub past_intent: String,
    /// Entity IDs from the past intent that were added or boosted.
    pub entity_ids: Vec<String>,
    /// Number of query terms that overlapped with this past intent.
    pub term_overlap: usize,
}

/// The result of a scope inference operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopeInference {
    /// The original intent text.
    pub intent: String,
    /// Terms extracted from the intent after stop-word removal.
    pub terms: Vec<String>,
    /// The predicted set of affected entities, sorted by confidence (descending).
    pub predicted_scope: Vec<ScopedEntity>,
    /// Historical records that influenced this prediction (only present when
    /// [`infer_with_history`] is used).
    pub history_influences: Vec<HistoryInfluence>,
    /// Wall-clock timestamp of the inference.
    pub inferred_at: DateTime<Utc>,
}

// ── Stop words ────────────────────────────────────────────────────────────────

/// Common English stop words plus programming action verbs.
static STOP_WORDS: &[&str] = &[
    // Articles / determiners
    "a", "an", "the", "this", "that", "these", "those", "each", "every",
    "some", "any", "all", "both", "few", "more", "most", "other", "such",
    // Prepositions / conjunctions
    "in", "on", "at", "by", "for", "of", "to", "up", "as", "or", "and",
    "but", "not", "nor", "so", "yet", "with", "into", "from", "about",
    "above", "after", "before", "below", "between", "during", "through",
    // Pronouns
    "it", "its", "they", "them", "their", "we", "our", "you", "your",
    "he", "she", "his", "her", "who", "which", "what", "where", "when",
    "how", "why",
    // Common verbs (too generic to indicate scope)
    "is", "are", "was", "were", "be", "been", "being", "have", "has",
    "had", "do", "does", "did", "will", "would", "could", "should",
    "may", "might", "shall", "can",
    // Programming action verbs
    "add", "fix", "update", "implement", "change", "refactor", "remove",
    "delete", "create", "make", "build", "use", "using", "get", "set",
    "new", "old", "now", "also", "just", "very", "too", "than", "then",
    "there", "if", "else", "while", "return", "need", "needs", "want",
];

// ── Public API ────────────────────────────────────────────────────────────────

/// Extracts key terms from an intent string.
///
/// Tokenizes on whitespace and non-alphanumeric characters, lower-cases each
/// token, removes stop words, and deduplicates.  Tokens shorter than 3
/// characters are dropped.
pub fn extract_terms(intent: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    // Split on characters that are neither alphanumeric nor underscore, so that
    // snake_case identifiers like `validate_token` are kept as a single token.
    intent
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .map(|t| t.trim_matches('_').to_lowercase())
        .filter(|t| t.len() >= 3 && !STOP_WORDS.contains(&t.as_str()))
        .filter(|t| seen.insert(t.clone()))
        .collect()
}

/// Infers which semantic entities are likely to be affected by `intent`.
///
/// # Parameters
///
/// * `snapshot` — the graph to query.
/// * `intent` — free-text description of the planned change.
/// * `max_hops` — how many relationship hops to traverse from direct matches.
///   Use `2` for the standard prediction depth.
pub fn infer(
    snapshot: &GraphSnapshot,
    intent: &str,
    max_hops: usize,
) -> Result<ScopeInference, ScopeInferenceError> {
    let terms = extract_terms(intent);

    // entity_id → (entity, confidence, reason)
    let mut scope: HashMap<String, ScopedEntity> = HashMap::new();

    // ── Step 1: direct term matching ─────────────────────────────────────────
    for term in &terms {
        let candidates = snapshot.search_entities_broad(term)?;
        for entity in candidates {
            let name_lower = entity.name.to_lowercase();
            let conf = if name_lower == *term {
                ConfidenceLevel::High
            } else {
                ConfidenceLevel::Medium
            };
            let reason = format!("name/path matches term '{term}'");
            scope
                .entry(entity.id.clone())
                .and_modify(|e| {
                    if conf > e.confidence {
                        e.confidence = conf.clone();
                        e.reason = reason.clone();
                    }
                })
                .or_insert(ScopedEntity {
                    entity,
                    confidence: conf,
                    reason,
                });
        }
    }

    // ── Step 2: N-hop BFS expansion ──────────────────────────────────────────
    if max_hops > 0 && !scope.is_empty() {
        let seed_ids: Vec<String> = scope.keys().cloned().collect();

        // BFS: track each entity ID and its hop distance from any seed.
        let mut hop_dist: HashMap<String, usize> = seed_ids.iter().map(|id| (id.clone(), 0)).collect();
        let mut queue: VecDeque<(String, usize)> =
            seed_ids.into_iter().map(|id| (id, 0usize)).collect();

        while let Some((id, hops)) = queue.pop_front() {
            if hops >= max_hops {
                continue;
            }
            let rels = snapshot.get_relationships_for_entity(&id)?;
            for rel in rels {
                let neighbor = if rel.from_entity == id {
                    rel.to_entity.clone()
                } else {
                    rel.from_entity.clone()
                };
                // Only visit each node at its shortest path distance.
                if !hop_dist.contains_key(&neighbor) {
                    hop_dist.insert(neighbor.clone(), hops + 1);
                    queue.push_back((neighbor, hops + 1));
                }
            }
        }

        // Resolve entities for newly discovered hop IDs.
        let new_ids: Vec<String> = hop_dist
            .iter()
            .filter(|(id, &h)| h > 0 && !scope.contains_key(*id))
            .map(|(id, _)| id.clone())
            .collect();

        let new_entities = snapshot.get_entities_by_ids(&new_ids)?;
        for entity in new_entities {
            let hops = *hop_dist.get(&entity.id).unwrap_or(&max_hops);
            let conf = if hops == 1 {
                ConfidenceLevel::Medium
            } else {
                ConfidenceLevel::Low
            };
            scope.entry(entity.id.clone()).or_insert(ScopedEntity {
                reason: format!("{hops}-hop relationship from direct match"),
                entity,
                confidence: conf,
            });
        }
    }

    // Sort: HIGH first, then MEDIUM, then LOW; stable within each band.
    let mut predicted_scope: Vec<ScopedEntity> = scope.into_values().collect();
    predicted_scope.sort_by(|a, b| b.confidence.cmp(&a.confidence));

    Ok(ScopeInference {
        intent: intent.to_string(),
        terms,
        predicted_scope,
        history_influences: Vec::new(),
        inferred_at: Utc::now(),
    })
}

/// Infers scope like [`infer`], then boosts entity confidence using historical
/// data from `history`.
///
/// Entities that appear frequently in past intents with similar terms are
/// upgraded one confidence level (LOW → MEDIUM, MEDIUM → HIGH) and appear with
/// an additional `"historical match"` reason suffix.  Entities not yet in the
/// graph-based prediction but present in history are added at `LOW` confidence.
///
/// The `history_influences` field of the returned [`ScopeInference`] is
/// populated with the specific past records that contributed to the prediction.
pub fn infer_with_history(
    snapshot: &GraphSnapshot,
    history: &ScopeHistoryStore,
    intent: &str,
    max_hops: usize,
) -> Result<ScopeInference, ScopeInferenceError> {
    let mut result = infer(snapshot, intent, max_hops)?;

    // Compute per-entity historical weights keyed by entity ID.
    let weights = history.compute_entity_weights(&result.terms)?;
    if weights.is_empty() {
        return Ok(result);
    }

    let weight_map: HashMap<String, f64> = weights
        .iter()
        .map(|w| (w.entity_id.clone(), w.weight))
        .collect();

    // Boost entities already in predicted scope.
    for scoped in &mut result.predicted_scope {
        if let Some(&w) = weight_map.get(&scoped.entity.id) {
            if w > 0.0 {
                let boosted = match scoped.confidence {
                    ConfidenceLevel::Low => ConfidenceLevel::Medium,
                    ConfidenceLevel::Medium => ConfidenceLevel::High,
                    ConfidenceLevel::High => ConfidenceLevel::High,
                };
                if boosted > scoped.confidence {
                    scoped.confidence = boosted;
                    scoped.reason = format!("{} + historical match (weight {w:.2})", scoped.reason);
                }
            }
        }
    }

    // Add historically relevant entities not yet in scope (at LOW confidence).
    let existing_ids: std::collections::HashSet<String> =
        result.predicted_scope.iter().map(|s| s.entity.id.clone()).collect();

    let new_ids: Vec<String> = weight_map
        .keys()
        .filter(|id| !existing_ids.contains(*id))
        .cloned()
        .collect();

    if !new_ids.is_empty() {
        let new_entities = snapshot.get_entities_by_ids(&new_ids)?;
        for entity in new_entities {
            let w = weight_map[&entity.id];
            result.predicted_scope.push(ScopedEntity {
                reason: format!("historical match (weight {w:.2})"),
                entity,
                confidence: ConfidenceLevel::Low,
            });
        }
    }

    // Re-sort: HIGH first, MEDIUM, LOW.
    result.predicted_scope.sort_by(|a, b| b.confidence.cmp(&a.confidence));

    // Populate history_influences from the raw records.
    let all_records = history.list_recent(10_000)?;
    for record in &all_records {
        let overlap = record
            .terms
            .iter()
            .filter(|t| result.terms.contains(t))
            .count();
        if overlap == 0 || record.actual_entity_ids.is_empty() {
            continue;
        }
        result.history_influences.push(HistoryInfluence {
            past_intent: record.intent_text.clone(),
            entity_ids: record.actual_entity_ids.clone(),
            term_overlap: overlap,
        });
    }
    // Most influential records first.
    result
        .history_influences
        .sort_by_key(|b| std::cmp::Reverse(b.term_overlap));

    Ok(result)
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Entity, EntityKind, GraphSnapshot, Relationship, RelationshipKind};
    use tempfile::tempdir;

    fn make_entity(name: &str, qualified_name: &str, file_path: &str) -> Entity {
        Entity {
            id: Entity::compute_id(file_path, qualified_name),
            kind: EntityKind::Function,
            name: name.to_string(),
            qualified_name: qualified_name.to_string(),
            file_path: file_path.to_string(),
            byte_range: (0, 10),
            line_range: (1, 5),
            parent_entity: None,
        }
    }

    /// Build a small graph: AuthService struct + validate_token / rate_limit methods,
    /// with Contains relationships from AuthService to its methods.
    fn build_auth_graph(snapshot: &GraphSnapshot) {
        let auth_svc = Entity {
            id: Entity::compute_id("src/auth.rs", "AuthService"),
            kind: EntityKind::Struct,
            name: "AuthService".to_string(),
            qualified_name: "AuthService".to_string(),
            file_path: "src/auth.rs".to_string(),
            byte_range: (0, 200),
            line_range: (1, 20),
            parent_entity: None,
        };
        let validate = make_entity("validate_token", "AuthService::validate_token", "src/auth.rs");
        let rate_limit = make_entity("rate_limit", "AuthService::rate_limit", "src/auth.rs");
        let unrelated = make_entity("unrelated_fn", "unrelated_fn", "src/other.rs");

        snapshot.upsert_entity(&auth_svc).unwrap();
        snapshot.upsert_entity(&validate).unwrap();
        snapshot.upsert_entity(&rate_limit).unwrap();
        snapshot.upsert_entity(&unrelated).unwrap();

        // AuthService contains validate_token
        let rel1 = Relationship::new(
            RelationshipKind::Contains,
            &auth_svc.id,
            &validate.id,
        );
        // AuthService contains rate_limit
        let rel2 = Relationship::new(
            RelationshipKind::Contains,
            &auth_svc.id,
            &rate_limit.id,
        );
        snapshot.upsert_relationship(&rel1).unwrap();
        snapshot.upsert_relationship(&rel2).unwrap();
    }

    #[test]
    fn test_extract_terms_removes_stop_words() {
        let terms = extract_terms("add rate limiting to the auth service");
        assert!(terms.contains(&"rate".to_string()));
        assert!(terms.contains(&"limiting".to_string()));
        assert!(terms.contains(&"auth".to_string()));
        assert!(terms.contains(&"service".to_string()));
        // stop words removed
        assert!(!terms.contains(&"add".to_string()));
        assert!(!terms.contains(&"to".to_string()));
        assert!(!terms.contains(&"the".to_string()));
    }

    #[test]
    fn test_extract_terms_deduplicates() {
        let terms = extract_terms("auth auth auth");
        assert_eq!(terms.iter().filter(|t| *t == "auth").count(), 1);
    }

    #[test]
    fn test_extract_terms_min_length() {
        // "to" and "is" and "of" are short AND stop words; "db" is short but not stop word
        let terms = extract_terms("fix db error");
        // "fix" is a stop word, "db" < 3 chars, "error" survives
        assert!(terms.contains(&"error".to_string()));
        assert!(!terms.contains(&"fix".to_string()));
        assert!(!terms.contains(&"db".to_string()));
    }

    #[test]
    fn test_infer_matches_auth_entities() {
        let dir = tempdir().unwrap();
        let snapshot = GraphSnapshot::open(&dir.path().join("graph.db")).unwrap();
        build_auth_graph(&snapshot);

        let result = infer(&snapshot, "add rate limiting to auth", 0).unwrap();

        let names: Vec<&str> = result
            .predicted_scope
            .iter()
            .map(|s| s.entity.name.as_str())
            .collect();

        // "auth" matches AuthService and validate_token/rate_limit (via file path src/auth.rs)
        assert!(names.contains(&"AuthService"), "expected AuthService in scope");
        // "rate" matches rate_limit
        assert!(names.contains(&"rate_limit"), "expected rate_limit in scope");
        // unrelated_fn should NOT appear (no term matches it)
        assert!(!names.contains(&"unrelated_fn"), "unrelated_fn should not be in scope");
    }

    #[test]
    fn test_infer_hop_expansion() {
        let dir = tempdir().unwrap();
        let snapshot = GraphSnapshot::open(&dir.path().join("graph.db")).unwrap();
        build_auth_graph(&snapshot);

        // With 1 hop: matching "authservice" directly and expanding to methods
        let result = infer(&snapshot, "refactor authservice", 1).unwrap();
        let names: Vec<&str> = result
            .predicted_scope
            .iter()
            .map(|s| s.entity.name.as_str())
            .collect();

        // validate_token and rate_limit should appear via hop expansion from AuthService
        assert!(names.contains(&"validate_token"), "expected validate_token via 1-hop");
        assert!(names.contains(&"rate_limit"), "expected rate_limit via 1-hop");
    }

    #[test]
    fn test_infer_high_confidence_exact_match() {
        let dir = tempdir().unwrap();
        let snapshot = GraphSnapshot::open(&dir.path().join("graph.db")).unwrap();
        build_auth_graph(&snapshot);

        let result = infer(&snapshot, "fix validate_token", 0).unwrap();
        let scoped = result
            .predicted_scope
            .iter()
            .find(|s| s.entity.name == "validate_token");

        assert!(scoped.is_some());
        assert_eq!(scoped.unwrap().confidence, ConfidenceLevel::High);
    }

    #[test]
    fn test_infer_empty_graph() {
        let dir = tempdir().unwrap();
        let snapshot = GraphSnapshot::open(&dir.path().join("graph.db")).unwrap();
        let result = infer(&snapshot, "add rate limiting to auth", 2).unwrap();
        assert!(result.predicted_scope.is_empty());
        assert_eq!(result.intent, "add rate limiting to auth");
    }
}
