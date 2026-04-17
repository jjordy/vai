//! Semantic graph engine — entity extraction and relationship tracking.
//!
//! Parses source files using tree-sitter and represents the codebase as a graph
//! of language-level entities (functions, structs, traits, modules) and their
//! relationships (calls, imports, contains).
//!
//! ## Storage
//!
//! The graph is persisted as a SQLite database at `.vai/graph/snapshot.db`.
//! Entities and relationships are fully rebuildable by re-parsing source files.
//!
//! ## Entity identity
//!
//! Each entity is identified by a stable SHA-256 hash of `{file_path}::{qualified_name}`.
//! This ID is stable across re-parses as long as the qualified name does not change,
//! making it suitable for tracking modifications over time.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tree_sitter::{Language, Node, Parser};
use tree_sitter_typescript::{LANGUAGE_TSX, LANGUAGE_TYPESCRIPT};

// ── Error types ───────────────────────────────────────────────────────────────

/// Errors from graph operations.
#[derive(Debug, Error)]
pub enum GraphError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Failed to load language grammar: {0}")]
    Language(String),
    #[error("Failed to parse source file: {0}")]
    Parse(String),
}

// ── Entity types ──────────────────────────────────────────────────────────────

/// The kind of a language entity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityKind {
    // ── Rust ──────────────────────────────────────────────────────────────────
    /// A standalone function or free function.
    Function,
    /// A method defined inside an `impl` block or class.
    Method,
    /// A struct definition.
    Struct,
    /// An enum definition (Rust or TypeScript).
    Enum,
    /// A trait definition.
    Trait,
    /// An `impl` block (may implement a trait or provide inherent methods).
    Impl,
    /// A module (`mod foo { ... }` or `mod foo;`).
    Module,
    /// A `use` import statement (Rust).
    UseStatement,
    // ── TypeScript / JavaScript ────────────────────────────────────────────
    /// A class declaration.
    Class,
    /// An interface declaration.
    Interface,
    /// A type alias (`type Foo = ...`).
    TypeAlias,
    /// A React component (function or arrow function returning JSX).
    Component,
    /// A custom hook (function whose name starts with `use`).
    Hook,
    /// An export statement (`export { ... }` or `export default ...`).
    ExportStatement,
}

impl EntityKind {
    /// Returns the lowercase string form stored in the database.
    pub fn as_str(&self) -> &'static str {
        match self {
            EntityKind::Function => "function",
            EntityKind::Method => "method",
            EntityKind::Struct => "struct",
            EntityKind::Enum => "enum",
            EntityKind::Trait => "trait",
            EntityKind::Impl => "impl",
            EntityKind::Module => "module",
            EntityKind::UseStatement => "use_statement",
            EntityKind::Class => "class",
            EntityKind::Interface => "interface",
            EntityKind::TypeAlias => "type_alias",
            EntityKind::Component => "component",
            EntityKind::Hook => "hook",
            EntityKind::ExportStatement => "export_statement",
        }
    }

    fn from_str(s: &str) -> Option<EntityKind> {
        match s {
            "function" => Some(EntityKind::Function),
            "method" => Some(EntityKind::Method),
            "struct" => Some(EntityKind::Struct),
            "enum" => Some(EntityKind::Enum),
            "trait" => Some(EntityKind::Trait),
            "impl" => Some(EntityKind::Impl),
            "module" => Some(EntityKind::Module),
            "use_statement" => Some(EntityKind::UseStatement),
            "class" => Some(EntityKind::Class),
            "interface" => Some(EntityKind::Interface),
            "type_alias" => Some(EntityKind::TypeAlias),
            "component" => Some(EntityKind::Component),
            "hook" => Some(EntityKind::Hook),
            "export_statement" => Some(EntityKind::ExportStatement),
            _ => None,
        }
    }
}

impl std::fmt::Display for EntityKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A language-level entity extracted from a source file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    /// Stable SHA-256 identifier based on `{file_path}::{qualified_name}`.
    pub id: String,
    /// The kind of entity.
    pub kind: EntityKind,
    /// The simple name of the entity (e.g., `"validate_token"`).
    pub name: String,
    /// The fully-qualified name within the file (e.g., `"AuthService::validate_token"`).
    pub qualified_name: String,
    /// Path to the source file, relative to the repository root.
    pub file_path: String,
    /// Start and end byte offsets within the file.
    pub byte_range: (usize, usize),
    /// Start and end line numbers (1-indexed).
    pub line_range: (usize, usize),
    /// ID of the parent entity, if any (e.g., the `impl` block containing a method).
    pub parent_entity: Option<String>,
}

impl Entity {
    /// Computes a stable entity ID from file path and qualified name.
    pub fn compute_id(file_path: &str, qualified_name: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(file_path.as_bytes());
        hasher.update(b"::");
        hasher.update(qualified_name.as_bytes());
        format!("{:x}", hasher.finalize())
    }
}

// ── Relationship types ────────────────────────────────────────────────────────

/// The kind of a relationship between two entities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationshipKind {
    /// A parent entity contains a child entity (e.g., `impl` contains `method`).
    Contains,
    /// One entity imports another (via a `use` or `import` statement).
    Imports,
    /// One entity calls another (best-effort, based on identifier matching).
    Calls,
    /// A class implements an interface (`implements` clause in TypeScript).
    Implements,
    /// A class or interface extends another (`extends` clause).
    Extends,
}

impl RelationshipKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            RelationshipKind::Contains => "contains",
            RelationshipKind::Imports => "imports",
            RelationshipKind::Calls => "calls",
            RelationshipKind::Implements => "implements",
            RelationshipKind::Extends => "extends",
        }
    }

    fn from_str(s: &str) -> Option<RelationshipKind> {
        match s {
            "contains" => Some(RelationshipKind::Contains),
            "imports" => Some(RelationshipKind::Imports),
            "calls" => Some(RelationshipKind::Calls),
            "implements" => Some(RelationshipKind::Implements),
            "extends" => Some(RelationshipKind::Extends),
            _ => None,
        }
    }
}

/// A directed relationship between two entities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Relationship {
    /// Unique ID: SHA-256 of `{kind}::{from}::{to}`.
    pub id: String,
    /// The kind of relationship.
    pub kind: RelationshipKind,
    /// The source entity ID.
    pub from_entity: String,
    /// The target entity ID.
    pub to_entity: String,
}

impl Relationship {
    /// Creates a new `Relationship`, computing a stable ID from the kind and entity IDs.
    pub fn new(kind: RelationshipKind, from_entity: &str, to_entity: &str) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(kind.as_str().as_bytes());
        hasher.update(b"::");
        hasher.update(from_entity.as_bytes());
        hasher.update(b"::");
        hasher.update(to_entity.as_bytes());
        let id = format!("{:x}", hasher.finalize());
        Relationship {
            id,
            kind,
            from_entity: from_entity.to_owned(),
            to_entity: to_entity.to_owned(),
        }
    }
}

// ── Graph statistics ──────────────────────────────────────────────────────────

/// Summary statistics about the semantic graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphStats {
    /// Total number of entities in the graph.
    pub entity_count: usize,
    /// Counts by entity kind.
    pub by_kind: HashMap<String, usize>,
    /// Total number of relationships.
    pub relationship_count: usize,
    /// Number of files represented in the graph.
    pub file_count: usize,
}

// ── GraphSnapshot ─────────────────────────────────────────────────────────────

/// The semantic graph for a vai repository, backed by SQLite.
///
/// Provides entity storage, querying, and incremental updates when files change.
pub struct GraphSnapshot {
    db: Connection,
    _db_path: PathBuf,
}

impl GraphSnapshot {
    /// Opens (or creates) the graph snapshot at `path`.
    ///
    /// `path` should be the `.vai/graph/snapshot.db` file path.
    pub fn open(path: &Path) -> Result<Self, GraphError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let db = Connection::open(path)?;
        let snap = GraphSnapshot {
            db,
            _db_path: path.to_owned(),
        };
        snap.init_schema()?;
        Ok(snap)
    }

    fn init_schema(&self) -> Result<(), GraphError> {
        self.db.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS entities (
                 id              TEXT PRIMARY KEY,
                 kind            TEXT NOT NULL,
                 name            TEXT NOT NULL,
                 qualified_name  TEXT NOT NULL,
                 file_path       TEXT NOT NULL,
                 byte_start      INTEGER NOT NULL,
                 byte_end        INTEGER NOT NULL,
                 line_start      INTEGER NOT NULL,
                 line_end        INTEGER NOT NULL,
                 parent_entity   TEXT
             );
             CREATE INDEX IF NOT EXISTS idx_entities_file      ON entities (file_path);
             CREATE INDEX IF NOT EXISTS idx_entities_name      ON entities (name);
             CREATE INDEX IF NOT EXISTS idx_entities_kind      ON entities (kind);
             CREATE TABLE IF NOT EXISTS relationships (
                 id          TEXT PRIMARY KEY,
                 kind        TEXT NOT NULL,
                 from_entity TEXT NOT NULL,
                 to_entity   TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_rels_from ON relationships (from_entity);
             CREATE INDEX IF NOT EXISTS idx_rels_to   ON relationships (to_entity);",
        )?;
        Ok(())
    }

    // ── Write API ─────────────────────────────────────────────────────────────

    /// Inserts or replaces an entity in the graph.
    pub fn upsert_entity(&self, entity: &Entity) -> Result<(), GraphError> {
        self.db.execute(
            "INSERT OR REPLACE INTO entities
             (id, kind, name, qualified_name, file_path, byte_start, byte_end,
              line_start, line_end, parent_entity)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                entity.id,
                entity.kind.as_str(),
                entity.name,
                entity.qualified_name,
                entity.file_path,
                entity.byte_range.0 as i64,
                entity.byte_range.1 as i64,
                entity.line_range.0 as i64,
                entity.line_range.1 as i64,
                entity.parent_entity,
            ],
        )?;
        Ok(())
    }

    /// Inserts or replaces a relationship in the graph.
    pub fn upsert_relationship(&self, rel: &Relationship) -> Result<(), GraphError> {
        self.db.execute(
            "INSERT OR REPLACE INTO relationships (id, kind, from_entity, to_entity)
             VALUES (?1, ?2, ?3, ?4)",
            params![rel.id, rel.kind.as_str(), rel.from_entity, rel.to_entity],
        )?;
        Ok(())
    }

    /// Removes all entities and relationships for the given file path.
    ///
    /// Used before re-parsing a file during incremental update.
    pub fn remove_file(&self, file_path: &str) -> Result<(), GraphError> {
        // Collect IDs of entities in this file so we can remove their relationships.
        let ids = self.get_entity_ids_for_file(file_path)?;
        for id in &ids {
            self.db.execute(
                "DELETE FROM relationships WHERE from_entity = ?1 OR to_entity = ?1",
                params![id],
            )?;
        }
        self.db
            .execute("DELETE FROM entities WHERE file_path = ?1", params![file_path])?;
        Ok(())
    }

    fn get_entity_ids_for_file(&self, file_path: &str) -> Result<Vec<String>, GraphError> {
        let mut stmt = self
            .db
            .prepare("SELECT id FROM entities WHERE file_path = ?1")?;
        let ids = stmt
            .query_map(params![file_path], |row| row.get(0))?
            .collect::<Result<Vec<String>, _>>()?;
        Ok(ids)
    }

    // ── Query API ─────────────────────────────────────────────────────────────

    /// Returns the entity with the given ID, or `None` if not found.
    pub fn get_entity_by_id(&self, id: &str) -> Result<Option<Entity>, GraphError> {
        let mut stmt = self.db.prepare(
            "SELECT id, kind, name, qualified_name, file_path, byte_start, byte_end,
                    line_start, line_end, parent_entity
             FROM entities WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![id], row_to_entity)?;
        Ok(rows.next().transpose()?)
    }

    /// Returns all entities with the given name (case-sensitive).
    pub fn get_entities_by_name(&self, name: &str) -> Result<Vec<Entity>, GraphError> {
        let mut stmt = self.db.prepare(
            "SELECT id, kind, name, qualified_name, file_path, byte_start, byte_end,
                    line_start, line_end, parent_entity
             FROM entities WHERE name = ?1 ORDER BY file_path, line_start",
        )?;
        let rows = stmt.query_map(params![name], row_to_entity)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Returns all entities whose name contains `pattern` (case-insensitive).
    pub fn search_entities_by_name(&self, pattern: &str) -> Result<Vec<Entity>, GraphError> {
        let like_pattern = format!("%{}%", pattern);
        let mut stmt = self.db.prepare(
            "SELECT id, kind, name, qualified_name, file_path, byte_start, byte_end,
                    line_start, line_end, parent_entity
             FROM entities WHERE name LIKE ?1 ORDER BY file_path, line_start",
        )?;
        let rows = stmt.query_map(params![like_pattern], row_to_entity)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Returns entities whose name, qualified name, or file path contains `term` (case-insensitive).
    ///
    /// Wider than [`search_entities_by_name`] — useful for intent-based scope inference
    /// where a term might correspond to a file path component or module path.
    pub fn search_entities_broad(&self, term: &str) -> Result<Vec<Entity>, GraphError> {
        let like = format!("%{}%", term.to_lowercase());
        let mut stmt = self.db.prepare(
            "SELECT id, kind, name, qualified_name, file_path, byte_start, byte_end,
                    line_start, line_end, parent_entity
             FROM entities
             WHERE lower(name) LIKE ?1
                OR lower(qualified_name) LIKE ?2
                OR lower(file_path) LIKE ?3
             ORDER BY file_path, line_start",
        )?;
        let rows = stmt.query_map(params![like, like, like], row_to_entity)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Returns all entities in the given file.
    pub fn get_entities_in_file(&self, file_path: &str) -> Result<Vec<Entity>, GraphError> {
        let mut stmt = self.db.prepare(
            "SELECT id, kind, name, qualified_name, file_path, byte_start, byte_end,
                    line_start, line_end, parent_entity
             FROM entities WHERE file_path = ?1 ORDER BY byte_start",
        )?;
        let rows = stmt.query_map(params![file_path], row_to_entity)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Returns all relationships where the given entity is the source or target.
    pub fn get_relationships_for_entity(
        &self,
        entity_id: &str,
    ) -> Result<Vec<Relationship>, GraphError> {
        let mut stmt = self.db.prepare(
            "SELECT id, kind, from_entity, to_entity FROM relationships
             WHERE from_entity = ?1 OR to_entity = ?1",
        )?;
        let rows = stmt.query_map(params![entity_id], row_to_relationship)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Returns all outgoing relationships from the given entity.
    pub fn get_outgoing_relationships(
        &self,
        entity_id: &str,
    ) -> Result<Vec<Relationship>, GraphError> {
        let mut stmt = self.db.prepare(
            "SELECT id, kind, from_entity, to_entity FROM relationships
             WHERE from_entity = ?1",
        )?;
        let rows = stmt.query_map(params![entity_id], row_to_relationship)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Returns graph-wide statistics.
    pub fn stats(&self) -> Result<GraphStats, GraphError> {
        let entity_count: usize = self
            .db
            .query_row("SELECT COUNT(*) FROM entities", [], |r| r.get::<_, i64>(0))
            .map(|n| n as usize)?;

        let relationship_count: usize = self
            .db
            .query_row("SELECT COUNT(*) FROM relationships", [], |r| {
                r.get::<_, i64>(0)
            })
            .map(|n| n as usize)?;

        let file_count: usize = self
            .db
            .query_row(
                "SELECT COUNT(DISTINCT file_path) FROM entities",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n as usize)?;

        let mut by_kind = HashMap::new();
        let mut stmt = self
            .db
            .prepare("SELECT kind, COUNT(*) FROM entities GROUP BY kind")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as usize))
        })?;
        for row in rows {
            let (kind, count) = row?;
            by_kind.insert(kind, count);
        }

        Ok(GraphStats {
            entity_count,
            by_kind,
            relationship_count,
            file_count,
        })
    }

    /// Returns all entities in the graph.
    pub fn all_entities(&self) -> Result<Vec<Entity>, GraphError> {
        let mut stmt = self.db.prepare(
            "SELECT id, kind, name, qualified_name, file_path, byte_start, byte_end,
                    line_start, line_end, parent_entity
             FROM entities ORDER BY file_path, byte_start",
        )?;
        let rows = stmt.query_map([], row_to_entity)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Returns entities filtered by optional `kind`, `file` path, and `name` substring.
    ///
    /// - `kind` — matches exact entity kind string (e.g. `"function"`)
    /// - `file` — matches exact file path
    /// - `name` — case-insensitive substring match on entity name
    ///
    /// All provided filters are combined with AND. Omit a filter by passing `None`.
    pub fn filter_entities(
        &self,
        kind: Option<&str>,
        file: Option<&str>,
        name: Option<&str>,
    ) -> Result<Vec<Entity>, GraphError> {
        let mut sql = String::from(
            "SELECT id, kind, name, qualified_name, file_path, byte_start, byte_end,
                    line_start, line_end, parent_entity
             FROM entities WHERE 1=1",
        );
        let mut params: Vec<String> = Vec::new();

        if let Some(k) = kind {
            sql.push_str(" AND kind = ?");
            params.push(k.to_string());
        }
        if let Some(f) = file {
            sql.push_str(" AND file_path = ?");
            params.push(f.to_string());
        }
        if let Some(n) = name {
            sql.push_str(" AND name LIKE ?");
            params.push(format!("%{n}%"));
        }
        sql.push_str(" ORDER BY file_path, line_start");

        let mut stmt = self.db.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), row_to_entity)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Fetches entities by a list of IDs in a single query.
    ///
    /// IDs not present in the graph are silently omitted. Order of results is
    /// not guaranteed to match the input order.
    pub fn get_entities_by_ids(&self, ids: &[String]) -> Result<Vec<Entity>, GraphError> {
        if ids.is_empty() {
            return Ok(vec![]);
        }
        let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
        let sql = format!(
            "SELECT id, kind, name, qualified_name, file_path, byte_start, byte_end,
                    line_start, line_end, parent_entity
             FROM entities WHERE id IN ({placeholders}) ORDER BY file_path, line_start"
        );
        let mut stmt = self.db.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(ids.iter()), row_to_entity)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Returns all incoming relationships for an entity (where it is the target).
    pub fn get_incoming_relationships(
        &self,
        entity_id: &str,
    ) -> Result<Vec<Relationship>, GraphError> {
        let mut stmt = self.db.prepare(
            "SELECT id, kind, from_entity, to_entity FROM relationships
             WHERE to_entity = ?1",
        )?;
        let rows = stmt.query_map(params![entity_id], row_to_relationship)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Computes the set of entities reachable from a set of seed entity IDs within
    /// `max_hops` relationship traversals (bidirectional BFS).
    ///
    /// The seed entities themselves are included in the result.
    /// Returns entities and the relationships that connect them within the reached set.
    pub fn reachable_entities(
        &self,
        seed_ids: &[&str],
        max_hops: usize,
    ) -> Result<(Vec<Entity>, Vec<Relationship>), GraphError> {
        use std::collections::{HashMap, HashSet, VecDeque};

        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<(String, usize)> = VecDeque::new();

        for &id in seed_ids {
            if visited.insert(id.to_string()) {
                queue.push_back((id.to_string(), 0));
            }
        }

        // BFS: collect all reachable entity IDs within max_hops.
        while let Some((id, hops)) = queue.pop_front() {
            if hops >= max_hops {
                continue;
            }
            let rels = self.get_relationships_for_entity(&id)?;
            for rel in rels {
                let neighbor = if rel.from_entity == id {
                    rel.to_entity.clone()
                } else {
                    rel.from_entity.clone()
                };
                if visited.insert(neighbor.clone()) {
                    queue.push_back((neighbor, hops + 1));
                }
            }
        }

        let ids: Vec<String> = visited.into_iter().collect();
        let entities = self.get_entities_by_ids(&ids)?;

        // Collect relationships that connect entities within the reached set.
        let entity_ids_set: HashSet<&str> = ids.iter().map(String::as_str).collect();
        let mut seen_rels: HashMap<String, Relationship> = HashMap::new();
        for id in &ids {
            let rels = self.get_relationships_for_entity(id)?;
            for rel in rels {
                if entity_ids_set.contains(rel.from_entity.as_str())
                    && entity_ids_set.contains(rel.to_entity.as_str())
                {
                    seen_rels.entry(rel.id.clone()).or_insert(rel);
                }
            }
        }
        let relationships = seen_rels.into_values().collect();

        Ok((entities, relationships))
    }

    // ── Incremental update ────────────────────────────────────────────────────

    /// Re-parses a single file and updates the graph with the new entities.
    ///
    /// Dispatches to the appropriate language parser based on the file extension.
    /// All existing entities for the file are removed first.
    pub fn update_file(&self, file_path: &str, source: &[u8]) -> Result<ParseStats, GraphError> {
        self.remove_file(file_path)?;
        let (entities, relationships) = parse_source_file(file_path, source)?;
        let count = entities.len();
        for entity in &entities {
            self.upsert_entity(entity)?;
        }
        for rel in &relationships {
            self.upsert_relationship(rel)?;
        }
        Ok(ParseStats {
            entities_found: count,
            relationships_found: relationships.len(),
        })
    }
}

/// Statistics returned from a single file parse.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParseStats {
    /// Number of entities found.
    pub entities_found: usize,
    /// Number of relationships found.
    pub relationships_found: usize,
}

// ── Row helpers ───────────────────────────────────────────────────────────────

fn row_to_entity(row: &rusqlite::Row<'_>) -> rusqlite::Result<Entity> {
    let kind_str: String = row.get(1)?;
    let kind = EntityKind::from_str(&kind_str).unwrap_or(EntityKind::Function);
    Ok(Entity {
        id: row.get(0)?,
        kind,
        name: row.get(2)?,
        qualified_name: row.get(3)?,
        file_path: row.get(4)?,
        byte_range: (
            row.get::<_, i64>(5)? as usize,
            row.get::<_, i64>(6)? as usize,
        ),
        line_range: (
            row.get::<_, i64>(7)? as usize,
            row.get::<_, i64>(8)? as usize,
        ),
        parent_entity: row.get(9)?,
    })
}

fn row_to_relationship(row: &rusqlite::Row<'_>) -> rusqlite::Result<Relationship> {
    let kind_str: String = row.get(1)?;
    let kind = RelationshipKind::from_str(&kind_str).unwrap_or(RelationshipKind::Contains);
    Ok(Relationship {
        id: row.get(0)?,
        kind,
        from_entity: row.get(2)?,
        to_entity: row.get(3)?,
    })
}

// ── Tree-sitter parser ────────────────────────────────────────────────────────

/// Parses a Rust source file and extracts all entities and relationships.
///
/// Returns `(entities, relationships)` extracted from the file.
pub fn parse_rust_source(
    file_path: &str,
    source: &[u8],
) -> Result<(Vec<Entity>, Vec<Relationship>), GraphError> {
    let language: Language = tree_sitter_rust::LANGUAGE.into();
    let mut parser = Parser::new();
    parser
        .set_language(&language)
        .map_err(|e| GraphError::Language(e.to_string()))?;

    let tree = parser
        .parse(source, None)
        .ok_or_else(|| GraphError::Parse(format!("failed to parse {file_path}")))?;

    let mut entities: Vec<Entity> = Vec::new();
    let mut relationships: Vec<Relationship> = Vec::new();

    // A stack of (entity_id, qualified_name_prefix) for nesting.
    let mut parent_stack: Vec<(String, String)> = Vec::new();

    extract_entities(
        tree.root_node(),
        source,
        file_path,
        &mut parent_stack,
        &mut entities,
        &mut relationships,
        false,
    );

    // Second pass: build best-effort Calls relationships.
    build_calls_relationships(tree.root_node(), source, file_path, &entities, &mut relationships);

    Ok((entities, relationships))
}

/// Recursively extracts entities from a tree-sitter node.
fn extract_entities(
    node: Node<'_>,
    source: &[u8],
    file_path: &str,
    parent_stack: &mut Vec<(String, String)>,
    entities: &mut Vec<Entity>,
    relationships: &mut Vec<Relationship>,
    inside_impl: bool,
) {
    let kind = node.kind();
    match kind {
        "function_item" => {
            if let Some(entity) = extract_function(node, source, file_path, parent_stack, inside_impl) {
                // Add Contains relationship if there's a parent.
                if let Some((parent_id, _)) = parent_stack.last() {
                    relationships.push(Relationship::new(
                        RelationshipKind::Contains,
                        parent_id,
                        &entity.id,
                    ));
                }
                parent_stack.push((entity.id.clone(), entity.qualified_name.clone()));
                entities.push(entity);
                // Don't recurse into function body for nested items (Rust doesn't support nested fn items for our purposes)
                parent_stack.pop();
            }
        }
        "struct_item" => {
            if let Some(entity) = extract_named_item(
                node,
                source,
                file_path,
                parent_stack,
                EntityKind::Struct,
                &["type_identifier"],
            ) {
                add_contains_rel(parent_stack, &entity.id, relationships);
                entities.push(entity);
            }
        }
        "enum_item" => {
            if let Some(entity) = extract_named_item(
                node,
                source,
                file_path,
                parent_stack,
                EntityKind::Enum,
                &["type_identifier"],
            ) {
                add_contains_rel(parent_stack, &entity.id, relationships);
                entities.push(entity);
            }
        }
        "trait_item" => {
            if let Some(entity) = extract_named_item(
                node,
                source,
                file_path,
                parent_stack,
                EntityKind::Trait,
                &["type_identifier"],
            ) {
                add_contains_rel(parent_stack, &entity.id, relationships);
                let eid = entity.id.clone();
                let eqn = entity.qualified_name.clone();
                entities.push(entity);
                // Recurse into trait body for methods.
                if let Some(body) = node.child_by_field_name("body") {
                    parent_stack.push((eid, eqn));
                    let mut cursor = body.walk();
                    for child in body.named_children(&mut cursor) {
                        extract_entities(child, source, file_path, parent_stack, entities, relationships, false);
                    }
                    parent_stack.pop();
                }
            }
        }
        "impl_item" => {
            if let Some(entity) = extract_impl_item(node, source, file_path, parent_stack) {
                add_contains_rel(parent_stack, &entity.id, relationships);
                let eid = entity.id.clone();
                let eqn = entity.qualified_name.clone();
                entities.push(entity);
                // Recurse into impl body so methods are extracted.
                if let Some(body) = node.child_by_field_name("body") {
                    parent_stack.push((eid, eqn));
                    let mut cursor = body.walk();
                    for child in body.named_children(&mut cursor) {
                        extract_entities(child, source, file_path, parent_stack, entities, relationships, true);
                    }
                    parent_stack.pop();
                }
            }
        }
        "mod_item" => {
            if let Some(entity) = extract_named_item(
                node,
                source,
                file_path,
                parent_stack,
                EntityKind::Module,
                &["identifier"],
            ) {
                add_contains_rel(parent_stack, &entity.id, relationships);
                let eid = entity.id.clone();
                let eqn = entity.qualified_name.clone();
                entities.push(entity);
                // Recurse into module body.
                if let Some(body) = node.child_by_field_name("body") {
                    parent_stack.push((eid, eqn));
                    let mut cursor = body.walk();
                    for child in body.named_children(&mut cursor) {
                        extract_entities(child, source, file_path, parent_stack, entities, relationships, false);
                    }
                    parent_stack.pop();
                }
            }
        }
        "use_declaration" => {
            if let Some(entity) = extract_use_declaration(node, source, file_path, parent_stack) {
                add_contains_rel(parent_stack, &entity.id, relationships);
                entities.push(entity);
            }
        }
        "source_file" => {
            // Root: recurse into all children.
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                extract_entities(child, source, file_path, parent_stack, entities, relationships, false);
            }
        }
        _ => {
            // Other node kinds: not extracted as entities.
        }
    }
}

/// Adds a Contains relationship from the current parent to a child entity.
fn add_contains_rel(
    parent_stack: &[(String, String)],
    child_id: &str,
    relationships: &mut Vec<Relationship>,
) {
    if let Some((parent_id, _)) = parent_stack.last() {
        relationships.push(Relationship::new(
            RelationshipKind::Contains,
            parent_id,
            child_id,
        ));
    }
}

/// Extracts a `function_item` node as a Function or Method entity.
fn extract_function(
    node: Node<'_>,
    source: &[u8],
    file_path: &str,
    parent_stack: &[(String, String)],
    inside_impl: bool,
) -> Option<Entity> {
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, source)?;
    let qualified_name = qualify(&name, parent_stack);
    let id = Entity::compute_id(file_path, &qualified_name);
    let kind = if inside_impl {
        EntityKind::Method
    } else {
        EntityKind::Function
    };
    let parent_entity = parent_stack.last().map(|(id, _)| id.clone());
    Some(Entity {
        id,
        kind,
        name,
        qualified_name,
        file_path: file_path.to_owned(),
        byte_range: (node.start_byte(), node.end_byte()),
        line_range: (node.start_position().row + 1, node.end_position().row + 1),
        parent_entity,
    })
}

/// Extracts a named item (struct, enum, trait, module) with a given name field kind.
fn extract_named_item(
    node: Node<'_>,
    source: &[u8],
    file_path: &str,
    parent_stack: &[(String, String)],
    kind: EntityKind,
    name_kinds: &[&str],
) -> Option<Entity> {
    let name_node = node.child_by_field_name("name")?;
    // Verify the name node is one of the expected kinds.
    if !name_kinds.contains(&name_node.kind()) {
        return None;
    }
    let name = node_text(name_node, source)?;
    let qualified_name = qualify(&name, parent_stack);
    let id = Entity::compute_id(file_path, &qualified_name);
    let parent_entity = parent_stack.last().map(|(id, _)| id.clone());
    Some(Entity {
        id,
        kind,
        name,
        qualified_name,
        file_path: file_path.to_owned(),
        byte_range: (node.start_byte(), node.end_byte()),
        line_range: (node.start_position().row + 1, node.end_position().row + 1),
        parent_entity,
    })
}

/// Extracts an `impl_item` node as an Impl entity.
///
/// The impl entity name is constructed as:
/// - `impl Foo` → `"Foo"`
/// - `impl Bar for Foo` → `"Bar for Foo"`
fn extract_impl_item(
    node: Node<'_>,
    source: &[u8],
    file_path: &str,
    parent_stack: &[(String, String)],
) -> Option<Entity> {
    let type_node = node.child_by_field_name("type")?;
    let type_name = node_text(type_node, source)?;

    let name = if let Some(trait_node) = node.child_by_field_name("trait") {
        let trait_name = node_text(trait_node, source).unwrap_or_default();
        format!("{trait_name} for {type_name}")
    } else {
        type_name.clone()
    };

    // For impl blocks, the qualified name includes a disambiguation suffix based on byte offset
    // to handle multiple impls for the same type.
    let qualified_name = {
        let base = qualify(&name, parent_stack);
        let offset = node.start_byte();
        format!("{base}@{offset}")
    };
    let id = Entity::compute_id(file_path, &qualified_name);
    let parent_entity = parent_stack.last().map(|(id, _)| id.clone());
    Some(Entity {
        id,
        kind: EntityKind::Impl,
        name,
        qualified_name,
        file_path: file_path.to_owned(),
        byte_range: (node.start_byte(), node.end_byte()),
        line_range: (node.start_position().row + 1, node.end_position().row + 1),
        parent_entity,
    })
}

/// Extracts a `use_declaration` node as a UseStatement entity.
fn extract_use_declaration(
    node: Node<'_>,
    source: &[u8],
    file_path: &str,
    parent_stack: &[(String, String)],
) -> Option<Entity> {
    // Get the full text of the use declaration as its name.
    let text = node_text(node, source)?;
    // Trim "use " prefix and trailing ";"
    let name = text
        .trim_start_matches("use ")
        .trim_end_matches(';')
        .trim()
        .to_string();
    // Use byte offset to disambiguate multiple use statements.
    let offset = node.start_byte();
    let qualified_name = format!("use::{name}@{offset}");
    let id = Entity::compute_id(file_path, &qualified_name);
    let parent_entity = parent_stack.last().map(|(id, _)| id.clone());
    Some(Entity {
        id,
        kind: EntityKind::UseStatement,
        name,
        qualified_name,
        file_path: file_path.to_owned(),
        byte_range: (node.start_byte(), node.end_byte()),
        line_range: (node.start_position().row + 1, node.end_position().row + 1),
        parent_entity,
    })
}

/// Builds best-effort `Calls` relationships by scanning for `call_expression` nodes.
///
/// For each function/method entity, scans its subtree for call expressions and
/// tries to match the callee identifier against known entity names.
fn build_calls_relationships(
    root: Node<'_>,
    source: &[u8],
    _file_path: &str,
    entities: &[Entity],
    relationships: &mut Vec<Relationship>,
) {
    // Build a name → entity ID map for efficient lookup.
    let name_to_ids: HashMap<String, Vec<String>> = {
        let mut map: HashMap<String, Vec<String>> = HashMap::new();
        for entity in entities {
            map.entry(entity.name.clone())
                .or_default()
                .push(entity.id.clone());
        }
        map
    };

    // For each function/method, find call expressions in its body.
    for entity in entities {
        if !matches!(entity.kind, EntityKind::Function | EntityKind::Method) {
            continue;
        }
        // Find the tree node for this entity by byte range.
        let func_node = find_node_at_range(root, entity.byte_range.0, entity.byte_range.1);
        if let Some(func_node) = func_node {
            if let Some(body) = func_node.child_by_field_name("body") {
                collect_calls(
                    body,
                    source,
                    &entity.id,
                    &name_to_ids,
                    relationships,
                );
            }
        }
    }
}

/// Recursively collects `call_expression` nodes, resolving callee names to entity IDs.
fn collect_calls(
    node: Node<'_>,
    source: &[u8],
    caller_id: &str,
    name_to_ids: &HashMap<String, Vec<String>>,
    relationships: &mut Vec<Relationship>,
) {
    if node.kind() == "call_expression" {
        if let Some(func_node) = node.child_by_field_name("function") {
            let callee_name = extract_final_identifier(func_node, source);
            if let Some(name) = callee_name {
                if let Some(ids) = name_to_ids.get(&name) {
                    for target_id in ids {
                        if target_id != caller_id {
                            let rel = Relationship::new(
                                RelationshipKind::Calls,
                                caller_id,
                                target_id,
                            );
                            // Avoid duplicates.
                            if !relationships.iter().any(|r| r.id == rel.id) {
                                relationships.push(rel);
                            }
                        }
                    }
                }
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_calls(child, source, caller_id, name_to_ids, relationships);
    }
}

/// Extracts the final identifier from a call expression's function node.
///
/// Handles `foo(...)`, `foo::bar(...)`, `self.foo(...)` patterns.
fn extract_final_identifier(node: Node<'_>, source: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier" => node_text(node, source),
        "scoped_identifier" => {
            // `foo::bar` → extract `bar`
            node.child_by_field_name("name")
                .and_then(|n| node_text(n, source))
        }
        "field_expression" => {
            // `self.foo` or `obj.method` → extract field name
            node.child_by_field_name("field")
                .and_then(|n| node_text(n, source))
        }
        "generic_function" => {
            // `foo::<T>` → recurse into function field
            node.child_by_field_name("function")
                .and_then(|n| extract_final_identifier(n, source))
        }
        _ => None,
    }
}

/// Finds the smallest node covering the given byte range.
fn find_node_at_range<'a>(node: Node<'a>, start: usize, end: usize) -> Option<Node<'a>> {
    if node.start_byte() == start && node.end_byte() == end {
        return Some(node);
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.start_byte() <= start && child.end_byte() >= end {
            if let Some(found) = find_node_at_range(child, start, end) {
                return Some(found);
            }
        }
    }
    None
}

// ── Language dispatcher ───────────────────────────────────────────────────────

/// Parses a source file using the appropriate grammar for its extension.
///
/// Supported extensions:
/// - `.rs` → Rust
/// - `.ts`, `.js` → TypeScript
/// - `.tsx`, `.jsx` → TSX
pub fn parse_source_file(
    file_path: &str,
    source: &[u8],
) -> Result<(Vec<Entity>, Vec<Relationship>), GraphError> {
    let ext = std::path::Path::new(file_path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    match ext {
        "rs" => parse_rust_source(file_path, source),
        "ts" | "js" => parse_typescript_source(file_path, source, false),
        "tsx" | "jsx" => parse_typescript_source(file_path, source, true),
        _ => Ok((vec![], vec![])),
    }
}

// ── TypeScript / TSX parser ───────────────────────────────────────────────────

/// Parses a TypeScript or TSX source file and extracts all entities and relationships.
///
/// When `tsx` is `true`, uses the TSX grammar (which understands JSX syntax).
pub fn parse_typescript_source(
    file_path: &str,
    source: &[u8],
    tsx: bool,
) -> Result<(Vec<Entity>, Vec<Relationship>), GraphError> {
    let language: Language = if tsx {
        LANGUAGE_TSX.into()
    } else {
        LANGUAGE_TYPESCRIPT.into()
    };
    let mut parser = Parser::new();
    parser
        .set_language(&language)
        .map_err(|e| GraphError::Language(e.to_string()))?;

    let tree = parser
        .parse(source, None)
        .ok_or_else(|| GraphError::Parse(format!("failed to parse {file_path}")))?;

    let mut entities: Vec<Entity> = Vec::new();
    let mut relationships: Vec<Relationship> = Vec::new();
    let mut parent_stack: Vec<(String, String)> = Vec::new();

    extract_ts_entities(
        tree.root_node(),
        source,
        file_path,
        &mut parent_stack,
        &mut entities,
        &mut relationships,
    );

    // Second pass: build Calls relationships.
    build_ts_calls_relationships(
        tree.root_node(),
        source,
        file_path,
        &entities,
        &mut relationships,
    );

    Ok((entities, relationships))
}

/// Recursively extracts entities from a TypeScript/TSX tree-sitter node.
fn extract_ts_entities(
    node: Node<'_>,
    source: &[u8],
    file_path: &str,
    parent_stack: &mut Vec<(String, String)>,
    entities: &mut Vec<Entity>,
    relationships: &mut Vec<Relationship>,
) {
    match node.kind() {
        "program" => {
            // Root node: recurse into all top-level statements.
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                extract_ts_entities(child, source, file_path, parent_stack, entities, relationships);
            }
        }
        "function_declaration" => {
            if let Some(entity) = extract_ts_function(node, source, file_path, parent_stack) {
                add_contains_rel(parent_stack, &entity.id, relationships);
                let eid = entity.id.clone();
                let eqn = entity.qualified_name.clone();
                entities.push(entity);
                // Recurse into the function body for nested items.
                if let Some(body) = node.child_by_field_name("body") {
                    parent_stack.push((eid, eqn));
                    let mut cursor = body.walk();
                    for child in body.named_children(&mut cursor) {
                        extract_ts_entities(child, source, file_path, parent_stack, entities, relationships);
                    }
                    parent_stack.pop();
                }
            }
        }
        "lexical_declaration" | "variable_declaration" => {
            // `const foo = () => {}` or `const Component = () => <div/>`
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if child.kind() == "variable_declarator" {
                    if let Some(entity) =
                        extract_ts_variable_declarator(child, source, file_path, parent_stack)
                    {
                        add_contains_rel(parent_stack, &entity.id, relationships);
                        entities.push(entity);
                    }
                }
            }
        }
        "class_declaration" => {
            if let Some(entity) =
                extract_ts_class(node, source, file_path, parent_stack, relationships)
            {
                add_contains_rel(parent_stack, &entity.id, relationships);
                let eid = entity.id.clone();
                let eqn = entity.qualified_name.clone();
                entities.push(entity);
                // Recurse into class body for methods.
                if let Some(body) = node.child_by_field_name("body") {
                    parent_stack.push((eid, eqn));
                    let mut cursor = body.walk();
                    for child in body.named_children(&mut cursor) {
                        extract_ts_entities(child, source, file_path, parent_stack, entities, relationships);
                    }
                    parent_stack.pop();
                }
            }
        }
        "method_definition" => {
            if let Some(entity) = extract_ts_method(node, source, file_path, parent_stack) {
                add_contains_rel(parent_stack, &entity.id, relationships);
                entities.push(entity);
            }
        }
        "interface_declaration" => {
            if let Some(entity) = extract_ts_named(
                node,
                source,
                file_path,
                parent_stack,
                EntityKind::Interface,
            ) {
                add_contains_rel(parent_stack, &entity.id, relationships);
                entities.push(entity);
            }
        }
        "type_alias_declaration" => {
            if let Some(entity) = extract_ts_named(
                node,
                source,
                file_path,
                parent_stack,
                EntityKind::TypeAlias,
            ) {
                add_contains_rel(parent_stack, &entity.id, relationships);
                entities.push(entity);
            }
        }
        "enum_declaration" => {
            if let Some(entity) =
                extract_ts_named(node, source, file_path, parent_stack, EntityKind::Enum)
            {
                add_contains_rel(parent_stack, &entity.id, relationships);
                entities.push(entity);
            }
        }
        "import_statement" => {
            if let Some(entity) = extract_ts_import(node, source, file_path, parent_stack) {
                add_contains_rel(parent_stack, &entity.id, relationships);
                entities.push(entity);
            }
        }
        "export_statement" => {
            // Export statements may wrap declarations; recurse into them first,
            // then record the export entity itself.
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                match child.kind() {
                    "function_declaration"
                    | "class_declaration"
                    | "interface_declaration"
                    | "type_alias_declaration"
                    | "enum_declaration"
                    | "lexical_declaration"
                    | "variable_declaration" => {
                        extract_ts_entities(
                            child,
                            source,
                            file_path,
                            parent_stack,
                            entities,
                            relationships,
                        );
                    }
                    _ => {}
                }
            }
            if let Some(entity) = extract_ts_export(node, source, file_path, parent_stack) {
                add_contains_rel(parent_stack, &entity.id, relationships);
                entities.push(entity);
            }
        }
        _ => {
            // For other node kinds, recurse into children to catch nested declarations.
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                extract_ts_entities(child, source, file_path, parent_stack, entities, relationships);
            }
        }
    }
}

/// Determines whether a node's subtree contains a JSX element (heuristic for component detection).
fn subtree_has_jsx(node: Node<'_>) -> bool {
    match node.kind() {
        "jsx_element" | "jsx_self_closing_element" | "jsx_fragment" => return true,
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if subtree_has_jsx(child) {
            return true;
        }
    }
    false
}

/// Extracts a `function_declaration` as Function, Hook, or Component.
fn extract_ts_function(
    node: Node<'_>,
    source: &[u8],
    file_path: &str,
    parent_stack: &[(String, String)],
) -> Option<Entity> {
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, source)?;
    let kind = ts_function_kind(&name, node);
    let qualified_name = qualify(&name, parent_stack);
    let id = Entity::compute_id(file_path, &qualified_name);
    let parent_entity = parent_stack.last().map(|(id, _)| id.clone());
    Some(Entity {
        id,
        kind,
        name,
        qualified_name,
        file_path: file_path.to_owned(),
        byte_range: (node.start_byte(), node.end_byte()),
        line_range: (node.start_position().row + 1, node.end_position().row + 1),
        parent_entity,
    })
}

/// Determines the EntityKind for a named function based on name convention and JSX content.
fn ts_function_kind(name: &str, node: Node<'_>) -> EntityKind {
    // Hooks: start with lowercase `use` followed by an uppercase letter or end of name.
    if name.starts_with("use")
        && name.len() > 3
        && name.chars().nth(3).map(|c| c.is_uppercase()).unwrap_or(false)
    {
        return EntityKind::Hook;
    }
    // Components: PascalCase and returns JSX.
    if name.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) && subtree_has_jsx(node) {
        return EntityKind::Component;
    }
    EntityKind::Function
}

/// Extracts a `variable_declarator` node as Function, Hook, or Component (arrow functions).
fn extract_ts_variable_declarator(
    node: Node<'_>,
    source: &[u8],
    file_path: &str,
    parent_stack: &[(String, String)],
) -> Option<Entity> {
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, source)?;
    let value_node = node.child_by_field_name("value")?;
    // Only extract arrow functions and function expressions.
    if !matches!(value_node.kind(), "arrow_function" | "function_expression") {
        return None;
    }
    let kind = ts_function_kind(&name, value_node);
    let qualified_name = qualify(&name, parent_stack);
    let id = Entity::compute_id(file_path, &qualified_name);
    let parent_entity = parent_stack.last().map(|(id, _)| id.clone());
    Some(Entity {
        id,
        kind,
        name,
        qualified_name,
        file_path: file_path.to_owned(),
        byte_range: (node.start_byte(), node.end_byte()),
        line_range: (node.start_position().row + 1, node.end_position().row + 1),
        parent_entity,
    })
}

/// Extracts a `class_declaration` as a Class entity, also recording Extends/Implements relationships.
fn extract_ts_class(
    node: Node<'_>,
    source: &[u8],
    file_path: &str,
    parent_stack: &[(String, String)],
    relationships: &mut Vec<Relationship>,
) -> Option<Entity> {
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, source)?;
    let qualified_name = qualify(&name, parent_stack);
    let id = Entity::compute_id(file_path, &qualified_name);
    let parent_entity = parent_stack.last().map(|(id, _)| id.clone());

    let entity = Entity {
        id: id.clone(),
        kind: EntityKind::Class,
        name,
        qualified_name,
        file_path: file_path.to_owned(),
        byte_range: (node.start_byte(), node.end_byte()),
        line_range: (node.start_position().row + 1, node.end_position().row + 1),
        parent_entity,
    };

    // Detect `extends` and `implements` clauses in the class heritage.
    if let Some(heritage) = node.child_by_field_name("body") {
        // Heritage is a sibling of body; scan siblings directly on the class node.
        let _ = heritage; // used only as a marker that body exists
    }
    // Walk all named children to find class_heritage nodes.
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "class_heritage" {
            extract_ts_heritage(child, source, &id, file_path, relationships);
        }
    }

    Some(entity)
}

/// Extracts `extends` and `implements` relationships from a class heritage node.
fn extract_ts_heritage(
    node: Node<'_>,
    source: &[u8],
    class_id: &str,
    file_path: &str,
    relationships: &mut Vec<Relationship>,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "extends_clause" => {
                // The referenced name becomes the target entity.
                if let Some(value_node) = child.child_by_field_name("value") {
                    if let Some(target_name) = node_text(value_node, source) {
                        let target_id = Entity::compute_id(file_path, &target_name);
                        relationships.push(Relationship::new(
                            RelationshipKind::Extends,
                            class_id,
                            &target_id,
                        ));
                    }
                }
            }
            "implements_clause" => {
                // May have multiple implemented interfaces.
                let mut ic = child.walk();
                for iface in child.named_children(&mut ic) {
                    if let Some(iface_name) = node_text(iface, source) {
                        let target_id = Entity::compute_id(file_path, &iface_name);
                        relationships.push(Relationship::new(
                            RelationshipKind::Implements,
                            class_id,
                            &target_id,
                        ));
                    }
                }
            }
            _ => {}
        }
    }
}

/// Extracts a `method_definition` as a Method entity.
fn extract_ts_method(
    node: Node<'_>,
    source: &[u8],
    file_path: &str,
    parent_stack: &[(String, String)],
) -> Option<Entity> {
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, source)?;
    let qualified_name = qualify(&name, parent_stack);
    let id = Entity::compute_id(file_path, &qualified_name);
    let parent_entity = parent_stack.last().map(|(id, _)| id.clone());
    Some(Entity {
        id,
        kind: EntityKind::Method,
        name,
        qualified_name,
        file_path: file_path.to_owned(),
        byte_range: (node.start_byte(), node.end_byte()),
        line_range: (node.start_position().row + 1, node.end_position().row + 1),
        parent_entity,
    })
}

/// Extracts a named declaration (interface, type alias, enum) by the `name` field.
fn extract_ts_named(
    node: Node<'_>,
    source: &[u8],
    file_path: &str,
    parent_stack: &[(String, String)],
    kind: EntityKind,
) -> Option<Entity> {
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, source)?;
    let qualified_name = qualify(&name, parent_stack);
    let id = Entity::compute_id(file_path, &qualified_name);
    let parent_entity = parent_stack.last().map(|(id, _)| id.clone());
    Some(Entity {
        id,
        kind,
        name,
        qualified_name,
        file_path: file_path.to_owned(),
        byte_range: (node.start_byte(), node.end_byte()),
        line_range: (node.start_position().row + 1, node.end_position().row + 1),
        parent_entity,
    })
}

/// Extracts an `import_statement` as a UseStatement-like import entity.
fn extract_ts_import(
    node: Node<'_>,
    source: &[u8],
    file_path: &str,
    parent_stack: &[(String, String)],
) -> Option<Entity> {
    let text = node_text(node, source)?;
    let name = text.trim().to_string();
    let offset = node.start_byte();
    let qualified_name = format!("import@{offset}");
    let id = Entity::compute_id(file_path, &qualified_name);
    let parent_entity = parent_stack.last().map(|(id, _)| id.clone());
    Some(Entity {
        id,
        kind: EntityKind::UseStatement,
        name,
        qualified_name,
        file_path: file_path.to_owned(),
        byte_range: (node.start_byte(), node.end_byte()),
        line_range: (node.start_position().row + 1, node.end_position().row + 1),
        parent_entity,
    })
}

/// Extracts an `export_statement` as an ExportStatement entity.
fn extract_ts_export(
    node: Node<'_>,
    source: &[u8],
    file_path: &str,
    parent_stack: &[(String, String)],
) -> Option<Entity> {
    let text = node_text(node, source)?;
    // Use a truncated summary as the name to keep it readable.
    let name = text.lines().next().unwrap_or("export").trim().to_string();
    let offset = node.start_byte();
    let qualified_name = format!("export@{offset}");
    let id = Entity::compute_id(file_path, &qualified_name);
    let parent_entity = parent_stack.last().map(|(id, _)| id.clone());
    Some(Entity {
        id,
        kind: EntityKind::ExportStatement,
        name,
        qualified_name,
        file_path: file_path.to_owned(),
        byte_range: (node.start_byte(), node.end_byte()),
        line_range: (node.start_position().row + 1, node.end_position().row + 1),
        parent_entity,
    })
}

/// Builds best-effort `Calls` relationships for TypeScript source.
fn build_ts_calls_relationships(
    root: Node<'_>,
    source: &[u8],
    _file_path: &str,
    entities: &[Entity],
    relationships: &mut Vec<Relationship>,
) {
    let name_to_ids: HashMap<String, Vec<String>> = {
        let mut map: HashMap<String, Vec<String>> = HashMap::new();
        for entity in entities {
            if matches!(
                entity.kind,
                EntityKind::Function
                    | EntityKind::Method
                    | EntityKind::Component
                    | EntityKind::Hook
            ) {
                map.entry(entity.name.clone())
                    .or_default()
                    .push(entity.id.clone());
            }
        }
        map
    };

    for entity in entities {
        if !matches!(
            entity.kind,
            EntityKind::Function
                | EntityKind::Method
                | EntityKind::Component
                | EntityKind::Hook
        ) {
            continue;
        }
        if let Some(func_node) =
            find_node_at_range(root, entity.byte_range.0, entity.byte_range.1)
        {
            collect_calls(func_node, source, &entity.id, &name_to_ids, relationships);
        }
    }
}

// ── Utility ───────────────────────────────────────────────────────────────────

/// Returns the UTF-8 text of a node, or `None` if conversion fails.
fn node_text(node: Node<'_>, source: &[u8]) -> Option<String> {
    node.utf8_text(source).ok().map(|s| s.to_owned())
}

/// Builds a qualified name by prepending the current scope prefix.
fn qualify(name: &str, parent_stack: &[(String, String)]) -> String {
    if let Some((_, prefix)) = parent_stack.last() {
        // Strip any @offset disambiguator before using as prefix.
        let clean_prefix = prefix.split('@').next().unwrap_or(prefix);
        format!("{clean_prefix}::{name}")
    } else {
        name.to_owned()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const SAMPLE_RUST: &str = r#"
use std::collections::HashMap;

pub struct Authenticator {
    secret: String,
}

pub enum AuthError {
    InvalidToken,
    Expired,
}

pub trait Validator {
    fn validate(&self, token: &str) -> bool;
}

impl Authenticator {
    pub fn new(secret: String) -> Self {
        Authenticator { secret }
    }

    pub fn check_token(&self, token: &str) -> bool {
        self.validate(token)
    }
}

impl Validator for Authenticator {
    fn validate(&self, token: &str) -> bool {
        !token.is_empty()
    }
}

pub fn create_auth(secret: &str) -> Authenticator {
    Authenticator::new(secret.to_owned())
}

mod helpers {
    pub fn hash_token(token: &str) -> String {
        format!("{:?}", token)
    }
}
"#;

    fn open_snapshot(tmp: &TempDir) -> GraphSnapshot {
        GraphSnapshot::open(&tmp.path().join("snapshot.db")).expect("open GraphSnapshot")
    }

    #[test]
    fn parse_rust_extracts_entities() {
        let (entities, _rels) =
            parse_rust_source("src/auth.rs", SAMPLE_RUST.as_bytes()).unwrap();

        let kinds: Vec<&EntityKind> = entities.iter().map(|e| &e.kind).collect();
        let names: Vec<&str> = entities.iter().map(|e| e.name.as_str()).collect();

        assert!(names.contains(&"Authenticator"), "missing Authenticator struct");
        assert!(names.contains(&"AuthError"), "missing AuthError enum");
        assert!(names.contains(&"Validator"), "missing Validator trait");
        assert!(names.contains(&"new"), "missing new method");
        assert!(names.contains(&"check_token"), "missing check_token method");
        assert!(names.contains(&"validate"), "missing validate method");
        assert!(names.contains(&"create_auth"), "missing create_auth function");
        assert!(names.contains(&"helpers"), "missing helpers module");
        assert!(names.contains(&"hash_token"), "missing hash_token function");

        assert!(kinds.contains(&&EntityKind::Struct));
        assert!(kinds.contains(&&EntityKind::Enum));
        assert!(kinds.contains(&&EntityKind::Trait));
        assert!(kinds.contains(&&EntityKind::Impl));
        assert!(kinds.contains(&&EntityKind::Method));
        assert!(kinds.contains(&&EntityKind::Function));
        assert!(kinds.contains(&&EntityKind::Module));
        assert!(kinds.contains(&&EntityKind::UseStatement));
    }

    #[test]
    fn parse_rust_extracts_contains_relationships() {
        let (entities, rels) =
            parse_rust_source("src/auth.rs", SAMPLE_RUST.as_bytes()).unwrap();

        let contains: Vec<_> = rels
            .iter()
            .filter(|r| r.kind == RelationshipKind::Contains)
            .collect();
        assert!(!contains.is_empty(), "expected Contains relationships");

        // The impl block should contain the methods.
        let new_entity = entities.iter().find(|e| e.name == "new").expect("new method");
        let check_entity = entities
            .iter()
            .find(|e| e.name == "check_token")
            .expect("check_token method");

        assert!(
            contains.iter().any(|r| r.to_entity == new_entity.id),
            "impl should contain new()"
        );
        assert!(
            contains.iter().any(|r| r.to_entity == check_entity.id),
            "impl should contain check_token()"
        );
    }

    #[test]
    fn parse_rust_builds_calls_relationships() {
        let (_, rels) = parse_rust_source("src/auth.rs", SAMPLE_RUST.as_bytes()).unwrap();
        let calls: Vec<_> = rels
            .iter()
            .filter(|r| r.kind == RelationshipKind::Calls)
            .collect();
        // create_auth calls Authenticator::new.
        assert!(!calls.is_empty(), "expected at least one Calls relationship");
    }

    #[test]
    fn qualified_names_are_nested() {
        let (entities, _) =
            parse_rust_source("src/auth.rs", SAMPLE_RUST.as_bytes()).unwrap();

        let new_method = entities
            .iter()
            .find(|e| e.name == "new" && e.kind == EntityKind::Method)
            .expect("new method");

        assert!(
            new_method.qualified_name.contains("::new"),
            "method qualified_name should include parent: {}",
            new_method.qualified_name
        );

        let nested_fn = entities
            .iter()
            .find(|e| e.name == "hash_token")
            .expect("hash_token");
        assert!(
            nested_fn.qualified_name.contains("helpers::"),
            "nested function should include module: {}",
            nested_fn.qualified_name
        );
    }

    #[test]
    fn snapshot_store_and_query() {
        let tmp = TempDir::new().unwrap();
        let snap = open_snapshot(&tmp);

        let (entities, rels) =
            parse_rust_source("src/auth.rs", SAMPLE_RUST.as_bytes()).unwrap();
        for e in &entities {
            snap.upsert_entity(e).unwrap();
        }
        for r in &rels {
            snap.upsert_relationship(r).unwrap();
        }

        let stats = snap.stats().unwrap();
        assert_eq!(stats.entity_count, entities.len());
        assert_eq!(stats.relationship_count, rels.len());

        let auth_entities = snap.get_entities_by_name("Authenticator").unwrap();
        assert!(!auth_entities.is_empty());

        let in_file = snap.get_entities_in_file("src/auth.rs").unwrap();
        assert_eq!(in_file.len(), entities.len());
    }

    #[test]
    fn snapshot_incremental_update() {
        let tmp = TempDir::new().unwrap();
        let snap = open_snapshot(&tmp);

        snap.update_file("src/auth.rs", SAMPLE_RUST.as_bytes())
            .unwrap();
        let before = snap.stats().unwrap().entity_count;

        // Re-parse with the same content — counts should be stable.
        snap.update_file("src/auth.rs", SAMPLE_RUST.as_bytes())
            .unwrap();
        let after = snap.stats().unwrap().entity_count;
        assert_eq!(before, after, "incremental update should be idempotent");

        // Remove file — graph should be empty.
        snap.remove_file("src/auth.rs").unwrap();
        let empty = snap.stats().unwrap().entity_count;
        assert_eq!(empty, 0);
    }

    #[test]
    fn entity_ids_are_stable() {
        let (entities_a, _) =
            parse_rust_source("src/auth.rs", SAMPLE_RUST.as_bytes()).unwrap();
        let (entities_b, _) =
            parse_rust_source("src/auth.rs", SAMPLE_RUST.as_bytes()).unwrap();

        let ids_a: Vec<&str> = entities_a.iter().map(|e| e.id.as_str()).collect();
        let ids_b: Vec<&str> = entities_b.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids_a, ids_b, "entity IDs should be stable across re-parses");
    }

    // ── TypeScript tests ───────────────────────────────────────────────────────

    const SAMPLE_TS: &str = r#"
import { useState } from 'react';

interface User {
    id: number;
    name: string;
}

type UserId = number;

enum Role {
    Admin = 'admin',
    User = 'user',
}

class UserService {
    private users: User[] = [];

    getUser(id: number): User | undefined {
        return this.users.find(u => u.id === id);
    }

    addUser(user: User): void {
        this.users.push(user);
    }
}

function formatName(user: User): string {
    return user.name.toUpperCase();
}

const useCounter = (initial: number) => {
    const [count, setCount] = useState(initial);
    return { count, setCount };
};

export { UserService, formatName };
"#;

    const SAMPLE_TSX: &str = r#"
import React, { useState } from 'react';

interface Props {
    title: string;
}

export function Header({ title }: Props) {
    return <h1>{title}</h1>;
}

export const useTheme = () => {
    const [dark, setDark] = useState(false);
    return { dark, setDark };
};

export default function App() {
    return (
        <div>
            <Header title="vai" />
        </div>
    );
}
"#;

    #[test]
    fn parse_typescript_extracts_basic_entities() {
        let (entities, _rels) =
            parse_typescript_source("src/service.ts", SAMPLE_TS.as_bytes(), false).unwrap();

        let names: Vec<&str> = entities.iter().map(|e| e.name.as_str()).collect();
        let kinds: Vec<&EntityKind> = entities.iter().map(|e| &e.kind).collect();

        assert!(names.contains(&"User"), "missing User interface");
        assert!(names.contains(&"UserId"), "missing UserId type alias");
        assert!(names.contains(&"Role"), "missing Role enum");
        assert!(names.contains(&"UserService"), "missing UserService class");
        assert!(names.contains(&"getUser"), "missing getUser method");
        assert!(names.contains(&"addUser"), "missing addUser method");
        assert!(names.contains(&"formatName"), "missing formatName function");
        assert!(names.contains(&"useCounter"), "missing useCounter hook");

        assert!(kinds.contains(&&EntityKind::Interface), "missing Interface kind");
        assert!(kinds.contains(&&EntityKind::TypeAlias), "missing TypeAlias kind");
        assert!(kinds.contains(&&EntityKind::Enum), "missing Enum kind");
        assert!(kinds.contains(&&EntityKind::Class), "missing Class kind");
        assert!(kinds.contains(&&EntityKind::Method), "missing Method kind");
        assert!(kinds.contains(&&EntityKind::Function), "missing Function kind");
        assert!(kinds.contains(&&EntityKind::Hook), "missing Hook kind");
        // Note: Component detection requires JSX (TSX grammar) — tested separately.
        assert!(kinds.contains(&&EntityKind::UseStatement), "missing import UseStatement");
        assert!(kinds.contains(&&EntityKind::ExportStatement), "missing ExportStatement");
    }

    #[test]
    fn parse_tsx_extracts_components_and_hooks() {
        let (entities, _rels) =
            parse_typescript_source("src/App.tsx", SAMPLE_TSX.as_bytes(), true).unwrap();

        let names: Vec<&str> = entities.iter().map(|e| e.name.as_str()).collect();
        let kinds: Vec<&EntityKind> = entities.iter().map(|e| &e.kind).collect();

        assert!(names.contains(&"Header"), "missing Header component");
        assert!(names.contains(&"App"), "missing App component");
        assert!(names.contains(&"useTheme"), "missing useTheme hook");
        assert!(names.contains(&"Props"), "missing Props interface");

        assert!(kinds.contains(&&EntityKind::Component), "missing Component kind");
        assert!(kinds.contains(&&EntityKind::Hook), "missing Hook kind");
        assert!(kinds.contains(&&EntityKind::Interface), "missing Interface kind");
    }

    #[test]
    fn parse_typescript_contains_relationships() {
        let (entities, rels) =
            parse_typescript_source("src/service.ts", SAMPLE_TS.as_bytes(), false).unwrap();

        let contains: Vec<_> = rels
            .iter()
            .filter(|r| r.kind == RelationshipKind::Contains)
            .collect();
        assert!(!contains.is_empty(), "expected Contains relationships");

        // UserService class should contain getUser and addUser.
        let class_entity = entities
            .iter()
            .find(|e| e.name == "UserService")
            .expect("UserService");
        let get_user_entity = entities.iter().find(|e| e.name == "getUser").expect("getUser");
        assert!(
            contains.iter().any(|r| r.from_entity == class_entity.id
                && r.to_entity == get_user_entity.id),
            "UserService should contain getUser"
        );
    }

    #[test]
    fn parse_source_file_dispatches_by_extension() {
        let (rs_entities, _) =
            parse_source_file("src/auth.rs", SAMPLE_RUST.as_bytes()).unwrap();
        let (ts_entities, _) =
            parse_source_file("src/service.ts", SAMPLE_TS.as_bytes()).unwrap();
        let (tsx_entities, _) =
            parse_source_file("src/App.tsx", SAMPLE_TSX.as_bytes()).unwrap();
        let (unknown_entities, _) =
            parse_source_file("README.md", b"# hello").unwrap();

        assert!(!rs_entities.is_empty(), "should parse .rs files");
        assert!(!ts_entities.is_empty(), "should parse .ts files");
        assert!(!tsx_entities.is_empty(), "should parse .tsx files");
        assert!(unknown_entities.is_empty(), "should skip unknown extensions");
    }
}
