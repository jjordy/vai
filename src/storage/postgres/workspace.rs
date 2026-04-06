//! WorkspaceStore implementation for PostgresStorage.
//!
//! Handles creating, querying, updating, and discarding workspaces in the
//! `workspaces` table, scoped by `repo_id`.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::Row;
use std::collections::HashMap;
use uuid::Uuid;

use super::super::{NewWorkspace, StorageError, WorkspaceStore, WorkspaceUpdate};
use super::super::pagination::{ListQuery, ListResult};
use super::PostgresStorage;
use crate::workspace::{WorkspaceMeta, WorkspaceStatus};

#[async_trait]
impl WorkspaceStore for PostgresStorage {
    async fn create_workspace(
        &self,
        repo_id: &Uuid,
        ws: NewWorkspace,
    ) -> Result<WorkspaceMeta, StorageError> {
        let id = ws.id.unwrap_or_else(Uuid::new_v4);

        sqlx::query(
            r#"
            INSERT INTO workspaces (id, repo_id, intent, base_version, issue_id)
            VALUES ($1, $2, $3, $4, $5)
            "#,
        )
        .bind(id)
        .bind(repo_id)
        .bind(&ws.intent)
        .bind(&ws.base_version)
        .bind(ws.issue_id)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        self.get_workspace(repo_id, &id).await
    }

    async fn get_workspace(
        &self,
        repo_id: &Uuid,
        id: &Uuid,
    ) -> Result<WorkspaceMeta, StorageError> {
        let row = sqlx::query(
            "SELECT id, intent, base_version, status, issue_id, deleted_paths, \
             created_at, updated_at \
             FROM workspaces WHERE repo_id = $1 AND id = $2",
        )
        .bind(repo_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?
        .ok_or_else(|| StorageError::NotFound(format!("workspace {id}")))?;

        row_to_workspace(row)
    }

    async fn list_workspaces(
        &self,
        repo_id: &Uuid,
        include_inactive: bool,
        query: &ListQuery,
    ) -> Result<ListResult<WorkspaceMeta>, StorageError> {
        let where_clause = if include_inactive {
            "repo_id = $1"
        } else {
            "repo_id = $1 AND status NOT IN ('Discarded', 'Merged')"
        };

        let col_map: HashMap<&str, &str> = [
            ("created_at", "created_at"),
            ("updated_at", "updated_at"),
            ("status", "status"),
            ("intent", "intent"),
            ("id", "id"),
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

        let count_sql = format!("SELECT COUNT(*) FROM workspaces WHERE {where_clause}");
        let select_sql = format!(
            "SELECT id, intent, base_version, status, issue_id, deleted_paths, \
             created_at, updated_at \
             FROM workspaces WHERE {where_clause} {order_by}{limit_clause}"
        );

        let count_row = sqlx::query(&count_sql)
            .bind(repo_id)
            .fetch_one(&self.pool)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;
        let total: i64 = count_row.get(0);

        let rows = sqlx::query(&select_sql)
            .bind(repo_id)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;

        let items: Result<Vec<WorkspaceMeta>, StorageError> = rows.into_iter().map(row_to_workspace).collect();
        Ok(ListResult { items: items?, total: total as u64 })
    }

    async fn update_workspace(
        &self,
        repo_id: &Uuid,
        id: &Uuid,
        update: WorkspaceUpdate,
    ) -> Result<WorkspaceMeta, StorageError> {
        let current = self.get_workspace(repo_id, id).await?;
        let status = update
            .status
            .map(|s| s.as_str().to_string())
            .unwrap_or_else(|| current.status.as_str().to_string());
        let issue_id = update.issue_id.or(current.issue_id);
        let deleted_paths = update.deleted_paths.unwrap_or(current.deleted_paths);

        sqlx::query(
            "UPDATE workspaces SET status = $1, issue_id = $2, deleted_paths = $3, \
             updated_at = now() WHERE repo_id = $4 AND id = $5",
        )
        .bind(&status)
        .bind(issue_id)
        .bind(&deleted_paths)
        .bind(repo_id)
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        self.get_workspace(repo_id, id).await
    }

    async fn discard_workspace(&self, repo_id: &Uuid, id: &Uuid) -> Result<(), StorageError> {
        sqlx::query(
            "UPDATE workspaces SET status = 'Discarded', updated_at = now() \
             WHERE repo_id = $1 AND id = $2",
        )
        .bind(repo_id)
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(())
    }
}

fn row_to_workspace(row: sqlx::postgres::PgRow) -> Result<WorkspaceMeta, StorageError> {
    let id: Uuid = row.get("id");
    let intent: String = row.get("intent");
    let base_version: String = row.get("base_version");
    let status_str: String = row.get("status");
    let issue_id: Option<Uuid> = row.get("issue_id");
    let deleted_paths: Vec<String> = row.try_get("deleted_paths").unwrap_or_default();
    let created_at: DateTime<Utc> = row.get("created_at");
    let updated_at: DateTime<Utc> = row.get("updated_at");

    let status = match status_str.as_str() {
        "Active" => WorkspaceStatus::Active,
        "Submitted" => WorkspaceStatus::Submitted,
        "Merged" => WorkspaceStatus::Merged,
        "Discarded" => WorkspaceStatus::Discarded,
        _ => WorkspaceStatus::Created,
    };

    Ok(WorkspaceMeta {
        id,
        intent,
        base_version,
        status,
        issue_id,
        deleted_paths,
        created_at,
        updated_at,
    })
}
