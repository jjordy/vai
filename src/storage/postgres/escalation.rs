//! EscalationStore implementation for PostgresStorage.
//!
//! Handles creating, querying, and resolving escalations in the `escalations`
//! table, scoped by `repo_id`.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::Row;
use std::collections::HashMap;
use uuid::Uuid;

use super::super::{EscalationStore, NewEscalation, StorageError};
use super::super::pagination::{ListQuery, ListResult};
use super::PostgresStorage;
use crate::escalation::{
    Escalation, EscalationConflict, EscalationSeverity, EscalationStatus, EscalationType,
    ResolutionOption,
};

#[async_trait]
impl EscalationStore for PostgresStorage {
    async fn create_escalation(
        &self,
        repo_id: &Uuid,
        esc: NewEscalation,
    ) -> Result<Escalation, StorageError> {
        let id = Uuid::new_v4();
        let esc_type = esc.escalation_type.as_str();
        let severity = esc.severity.as_str();
        let workspace_ids: Vec<Uuid> = esc.workspace_ids;
        let resolution_options =
            serde_json::to_value(&esc.resolution_options)
                .map_err(|e| StorageError::Serialization(e.to_string()))?;

        let conflicts =
            serde_json::to_value(&esc.conflicts)
                .map_err(|e| StorageError::Serialization(e.to_string()))?;

        sqlx::query(
            r#"
            INSERT INTO escalations
                (id, repo_id, escalation_type, severity, summary,
                 intents, agents, workspace_ids, affected_entities,
                 conflicts, resolution_options)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
            "#,
        )
        .bind(id)
        .bind(repo_id)
        .bind(esc_type)
        .bind(severity)
        .bind(&esc.summary)
        .bind(&esc.intents)
        .bind(&esc.agents)
        .bind(&workspace_ids)
        .bind(&esc.affected_entities)
        .bind(&conflicts)
        .bind(&resolution_options)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        self.get_escalation(repo_id, &id).await
    }

    async fn get_escalation(
        &self,
        repo_id: &Uuid,
        id: &Uuid,
    ) -> Result<Escalation, StorageError> {
        let row = sqlx::query(
            "SELECT id, escalation_type, severity, summary, intents, agents, workspace_ids, \
                    affected_entities, conflicts, resolution_options, resolved, resolution, \
                    resolved_by, resolved_at, created_at \
             FROM escalations WHERE repo_id = $1 AND id = $2",
        )
        .bind(repo_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?
        .ok_or_else(|| StorageError::NotFound(format!("escalation {id}")))?;

        row_to_escalation(row)
    }

    async fn list_escalations(
        &self,
        repo_id: &Uuid,
        pending_only: bool,
        query: &ListQuery,
    ) -> Result<ListResult<Escalation>, StorageError> {
        let where_clause = if pending_only {
            "repo_id = $1 AND resolved = false"
        } else {
            "repo_id = $1"
        };

        let col_map: HashMap<&str, &str> = [
            ("created_at", "created_at"),
            ("status", "resolved"),
            ("id", "id"),
            ("severity", "severity"),
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

        let count_sql = format!(
            "SELECT COUNT(*) FROM escalations WHERE {where_clause}"
        );
        let select_sql = format!(
            "SELECT id, escalation_type, severity, summary, intents, agents, workspace_ids, \
             affected_entities, conflicts, resolution_options, resolved, resolution, \
             resolved_by, resolved_at, created_at \
             FROM escalations WHERE {where_clause} {order_by}{limit_clause}"
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

        let items: Result<Vec<Escalation>, StorageError> = rows.into_iter().map(row_to_escalation).collect();
        Ok(ListResult { items: items?, total: total as u64 })
    }

    async fn resolve_escalation(
        &self,
        repo_id: &Uuid,
        id: &Uuid,
        resolution: ResolutionOption,
        resolved_by: &str,
    ) -> Result<Escalation, StorageError> {
        sqlx::query(
            "UPDATE escalations \
             SET resolved = true, resolution = $1, resolved_by = $2, resolved_at = now() \
             WHERE repo_id = $3 AND id = $4",
        )
        .bind(resolution.as_str())
        .bind(resolved_by)
        .bind(repo_id)
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        self.get_escalation(repo_id, id).await
    }
}

fn row_to_escalation(row: sqlx::postgres::PgRow) -> Result<Escalation, StorageError> {
    let id: Uuid = row.get("id");
    let esc_type_str: String = row.get("escalation_type");
    let severity_str: String = row.get("severity");
    let summary: String = row.get("summary");
    let intents: Vec<String> = row.get("intents");
    let agents: Vec<String> = row.get("agents");
    let workspace_ids: Vec<Uuid> = row.get("workspace_ids");
    let affected_entities: Vec<String> = row.get("affected_entities");
    let conflicts_val: serde_json::Value = row.get("conflicts");
    let resolution_options_val: serde_json::Value = row.get("resolution_options");
    let resolved: bool = row.get("resolved");
    let resolution_str: Option<String> = row.get("resolution");
    let resolved_by: Option<String> = row.get("resolved_by");
    let resolved_at: Option<DateTime<Utc>> = row.get("resolved_at");
    let created_at: DateTime<Utc> = row.get("created_at");

    let escalation_type = EscalationType::from_db_str(&esc_type_str)
        .unwrap_or(EscalationType::MergeConflict);
    let severity = EscalationSeverity::from_db_str(&severity_str)
        .unwrap_or(EscalationSeverity::High);
    let conflicts: Vec<EscalationConflict> =
        serde_json::from_value(conflicts_val).unwrap_or_default();
    let resolution_options: Vec<ResolutionOption> =
        serde_json::from_value(resolution_options_val)
            .map_err(|e| StorageError::Serialization(e.to_string()))?;
    let resolution: Option<ResolutionOption> = resolution_str
        .and_then(|s| ResolutionOption::from_db_str(&s));
    let status = if resolved {
        EscalationStatus::Resolved
    } else {
        EscalationStatus::Pending
    };

    Ok(Escalation {
        id,
        escalation_type,
        severity,
        status,
        summary,
        intents,
        agents,
        workspace_ids,
        affected_entities,
        conflicts,
        resolution_options,
        resolution,
        resolved_by,
        resolved_at,
        created_at,
    })
}
