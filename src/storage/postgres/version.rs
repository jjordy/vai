//! VersionStore implementation for PostgresStorage.
//!
//! Handles creating version records, querying version history, reading and
//! advancing the HEAD pointer for a repository.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::Row;
use std::collections::HashMap;
use uuid::Uuid;

use super::super::{NewVersion, StorageError, VersionStore};
use super::super::pagination::{ListQuery, ListResult};
use super::PostgresStorage;
use crate::version::VersionMeta;

#[async_trait]
impl VersionStore for PostgresStorage {
    async fn create_version(
        &self,
        repo_id: &Uuid,
        version: NewVersion,
    ) -> Result<VersionMeta, StorageError> {
        let id = Uuid::new_v4();
        let merge_event_id = version.merge_event_id.map(|x| x as i64);

        sqlx::query(
            r#"
            INSERT INTO versions
                (id, repo_id, version_id, parent_version_id, intent, created_by, merge_event_id)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            "#,
        )
        .bind(id)
        .bind(repo_id)
        .bind(&version.version_id)
        .bind(&version.parent_version_id)
        .bind(&version.intent)
        .bind(&version.created_by)
        .bind(merge_event_id)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        self.get_version(repo_id, &version.version_id).await
    }

    async fn get_version(
        &self,
        repo_id: &Uuid,
        version_id: &str,
    ) -> Result<VersionMeta, StorageError> {
        // Try UUID lookup first (the API exposes the database UUID as `id`).
        if let Ok(uuid) = Uuid::parse_str(version_id) {
            let row = sqlx::query(
                "SELECT id, version_id, parent_version_id, intent, created_by, merge_event_id, created_at \
                 FROM versions WHERE repo_id = $1 AND id = $2",
            )
            .bind(repo_id)
            .bind(uuid)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;

            if let Some(row) = row {
                return row_to_version(row);
            }
        }

        // Fall back to human-readable version_id lookup (e.g. "v1").
        let row = sqlx::query(
            "SELECT id, version_id, parent_version_id, intent, created_by, merge_event_id, created_at \
             FROM versions WHERE repo_id = $1 AND version_id = $2",
        )
        .bind(repo_id)
        .bind(version_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?
        .ok_or_else(|| StorageError::NotFound(format!("version {version_id}")))?;

        row_to_version(row)
    }

    async fn list_versions(
        &self,
        repo_id: &Uuid,
        query: &ListQuery,
    ) -> Result<ListResult<VersionMeta>, StorageError> {
        let col_map: HashMap<&str, &str> = [
            ("created_at", "created_at"),
            ("version_id", "version_id"),
            ("created_by", "created_by"),
        ]
        .into_iter()
        .collect();
        let order_by = query.sql_order_by(&col_map);
        let order_by = if order_by.is_empty() {
            "ORDER BY created_at DESC".to_string()
        } else {
            order_by
        };

        let (limit, offset) = query.sql_limit_offset();
        let limit_clause = if limit == i64::MAX {
            String::new()
        } else {
            format!(" LIMIT {limit} OFFSET {offset}")
        };

        let count_row = sqlx::query("SELECT COUNT(*) FROM versions WHERE repo_id = $1")
            .bind(repo_id)
            .fetch_one(&self.pool)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;
        let total: i64 = count_row.get(0);

        let select_sql = format!(
            "SELECT id, version_id, parent_version_id, intent, created_by, merge_event_id, created_at \
             FROM versions WHERE repo_id = $1 {order_by}{limit_clause}"
        );
        let rows = sqlx::query(&select_sql)
            .bind(repo_id)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;

        let items: Result<Vec<VersionMeta>, StorageError> = rows.into_iter().map(row_to_version).collect();
        Ok(ListResult { items: items?, total: total as u64 })
    }

    async fn list_versions_since(
        &self,
        repo_id: &Uuid,
        since_num: u64,
        head_num: u64,
    ) -> Result<Vec<VersionMeta>, StorageError> {
        // Cast the numeric suffix of version_id (e.g. "v7" → 7) for range filtering.
        // This avoids loading all versions into memory for large repos.
        let rows = sqlx::query(
            "SELECT id, version_id, parent_version_id, intent, created_by, merge_event_id, created_at \
             FROM versions \
             WHERE repo_id = $1 \
               AND CAST(SUBSTRING(version_id FROM 2) AS BIGINT) > $2 \
               AND CAST(SUBSTRING(version_id FROM 2) AS BIGINT) <= $3 \
             ORDER BY CAST(SUBSTRING(version_id FROM 2) AS BIGINT)",
        )
        .bind(repo_id)
        .bind(since_num as i64)
        .bind(head_num as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        rows.into_iter().map(row_to_version).collect()
    }

    async fn read_head(&self, repo_id: &Uuid) -> Result<Option<String>, StorageError> {
        let row = sqlx::query(
            "SELECT version_id FROM version_head WHERE repo_id = $1",
        )
        .bind(repo_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(row.map(|r| r.get("version_id")))
    }

    async fn advance_head(&self, repo_id: &Uuid, version_id: &str) -> Result<(), StorageError> {
        sqlx::query(
            r#"
            INSERT INTO version_head (repo_id, version_id) VALUES ($1, $2)
            ON CONFLICT (repo_id) DO UPDATE SET version_id = EXCLUDED.version_id
            "#,
        )
        .bind(repo_id)
        .bind(version_id)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(())
    }
}

fn row_to_version(row: sqlx::postgres::PgRow) -> Result<VersionMeta, StorageError> {
    let id: Uuid = row.get("id");
    let version_id: String = row.get("version_id");
    let parent_version_id: Option<String> = row.get("parent_version_id");
    let intent: String = row.get("intent");
    let created_by: String = row.get("created_by");
    let merge_event_id: Option<i64> = row.get("merge_event_id");
    let created_at: DateTime<Utc> = row.get("created_at");

    Ok(VersionMeta {
        id: Some(id),
        version_id,
        parent_version_id,
        intent,
        created_by,
        merge_event_id: merge_event_id.map(|x| x as u64),
        created_at,
    })
}
