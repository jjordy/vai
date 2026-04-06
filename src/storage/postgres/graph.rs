//! GraphStore implementation for PostgresStorage.
//!
//! Handles upserting entities and relationships into the semantic graph tables
//! (`entities`, `relationships`) and clearing graph data per file path,
//! all scoped by `repo_id`.

use async_trait::async_trait;
use sqlx::Row;
use uuid::Uuid;

use super::super::{GraphStore, StorageError};
use super::PostgresStorage;
use crate::graph::{Entity, EntityKind, Relationship, RelationshipKind};

#[async_trait]
impl GraphStore for PostgresStorage {
    async fn upsert_entities(
        &self,
        repo_id: &Uuid,
        entities: Vec<Entity>,
    ) -> Result<(), StorageError> {
        for entity in entities {
            sqlx::query(
                r#"
                INSERT INTO entities
                    (id, repo_id, kind, name, qualified_name, file_path,
                     line_start, line_end, parent_entity_id)
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                ON CONFLICT (repo_id, id) DO UPDATE SET
                    kind = EXCLUDED.kind,
                    name = EXCLUDED.name,
                    qualified_name = EXCLUDED.qualified_name,
                    file_path = EXCLUDED.file_path,
                    line_start = EXCLUDED.line_start,
                    line_end = EXCLUDED.line_end,
                    parent_entity_id = EXCLUDED.parent_entity_id
                "#,
            )
            .bind(&entity.id)
            .bind(repo_id)
            .bind(entity.kind.as_str())
            .bind(&entity.name)
            .bind(&entity.qualified_name)
            .bind(&entity.file_path)
            .bind(entity.line_range.0 as i32)
            .bind(entity.line_range.1 as i32)
            .bind(&entity.parent_entity)
            .execute(&self.pool)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;
        }
        Ok(())
    }

    async fn upsert_relationships(
        &self,
        repo_id: &Uuid,
        rels: Vec<Relationship>,
    ) -> Result<(), StorageError> {
        for rel in rels {
            sqlx::query(
                r#"
                INSERT INTO relationships (id, repo_id, kind, from_entity_id, to_entity_id)
                VALUES ($1, $2, $3, $4, $5)
                ON CONFLICT (repo_id, id) DO UPDATE SET
                    kind = EXCLUDED.kind,
                    from_entity_id = EXCLUDED.from_entity_id,
                    to_entity_id = EXCLUDED.to_entity_id
                "#,
            )
            .bind(&rel.id)
            .bind(repo_id)
            .bind(rel.kind.as_str())
            .bind(&rel.from_entity)
            .bind(&rel.to_entity)
            .execute(&self.pool)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;
        }
        Ok(())
    }

    async fn get_entity(&self, repo_id: &Uuid, id: &str) -> Result<Entity, StorageError> {
        let row = sqlx::query(
            "SELECT id, kind, name, qualified_name, file_path, \
                    line_start, line_end, parent_entity_id \
             FROM entities WHERE repo_id = $1 AND id = $2",
        )
        .bind(repo_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?
        .ok_or_else(|| StorageError::NotFound(format!("entity {id}")))?;

        row_to_entity(row)
    }

    async fn list_entities(
        &self,
        repo_id: &Uuid,
        file_path: Option<&str>,
    ) -> Result<Vec<Entity>, StorageError> {
        let rows = match file_path {
            Some(fp) => sqlx::query(
                "SELECT id, kind, name, qualified_name, file_path, \
                        line_start, line_end, parent_entity_id \
                 FROM entities WHERE repo_id = $1 AND file_path = $2 ORDER BY line_start",
            )
            .bind(repo_id)
            .bind(fp)
            .fetch_all(&self.pool)
            .await,
            None => sqlx::query(
                "SELECT id, kind, name, qualified_name, file_path, \
                        line_start, line_end, parent_entity_id \
                 FROM entities WHERE repo_id = $1 ORDER BY file_path, line_start",
            )
            .bind(repo_id)
            .fetch_all(&self.pool)
            .await,
        }
        .map_err(|e| StorageError::Database(e.to_string()))?;

        rows.into_iter().map(row_to_entity).collect()
    }

    async fn get_relationships(
        &self,
        repo_id: &Uuid,
        from_entity_id: &str,
    ) -> Result<Vec<Relationship>, StorageError> {
        let rows = sqlx::query(
            "SELECT id, kind, from_entity_id, to_entity_id \
             FROM relationships WHERE repo_id = $1 AND from_entity_id = $2",
        )
        .bind(repo_id)
        .bind(from_entity_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        rows.into_iter().map(row_to_relationship).collect()
    }

    async fn clear_file(&self, repo_id: &Uuid, file_path: &str) -> Result<(), StorageError> {
        // Remove relationships whose source entity lives in this file.
        sqlx::query(
            "DELETE FROM relationships WHERE repo_id = $1 AND from_entity_id IN \
             (SELECT id FROM entities WHERE repo_id = $1 AND file_path = $2)",
        )
        .bind(repo_id)
        .bind(file_path)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        sqlx::query("DELETE FROM entities WHERE repo_id = $1 AND file_path = $2")
            .bind(repo_id)
            .bind(file_path)
            .execute(&self.pool)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(())
    }
}

fn row_to_entity(row: sqlx::postgres::PgRow) -> Result<Entity, StorageError> {
    let id: String = row.get("id");
    let kind_str: String = row.get("kind");
    let name: String = row.get("name");
    let qualified_name: String = row.get("qualified_name");
    let file_path: String = row.get("file_path");
    let line_start: i32 = row.try_get("line_start").unwrap_or(0);
    let line_end: i32 = row.try_get("line_end").unwrap_or(0);
    let parent_entity: Option<String> = row.get("parent_entity_id");

    let kind = match kind_str.as_str() {
        "function" => EntityKind::Function,
        "method" => EntityKind::Method,
        "struct" => EntityKind::Struct,
        "enum" => EntityKind::Enum,
        "trait" => EntityKind::Trait,
        "impl" => EntityKind::Impl,
        "module" => EntityKind::Module,
        "use_statement" => EntityKind::UseStatement,
        "class" => EntityKind::Class,
        "interface" => EntityKind::Interface,
        "type_alias" => EntityKind::TypeAlias,
        "component" => EntityKind::Component,
        "hook" => EntityKind::Hook,
        "export_statement" => EntityKind::ExportStatement,
        _ => EntityKind::Function,
    };

    Ok(Entity {
        id,
        kind,
        name,
        qualified_name,
        file_path,
        // byte_range is not stored in Postgres; use 0..0 as a sentinel.
        byte_range: (0, 0),
        line_range: (line_start as usize, line_end as usize),
        parent_entity,
    })
}

fn row_to_relationship(row: sqlx::postgres::PgRow) -> Result<Relationship, StorageError> {
    let id: String = row.get("id");
    let kind_str: String = row.get("kind");
    let from_entity: String = row.get("from_entity_id");
    let to_entity: String = row.get("to_entity_id");

    let kind = match kind_str.as_str() {
        "contains" => RelationshipKind::Contains,
        "imports" => RelationshipKind::Imports,
        "calls" => RelationshipKind::Calls,
        "implements" => RelationshipKind::Implements,
        "extends" => RelationshipKind::Extends,
        _ => RelationshipKind::Calls,
    };

    Ok(Relationship {
        id,
        kind,
        from_entity,
        to_entity,
    })
}
