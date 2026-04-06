//! EventStore implementation for PostgresStorage.
//!
//! Handles appending events to the `events` table, issuing `pg_notify` for
//! WebSocket fan-out, and querying events by type, workspace, time range,
//! sequence ID, and filtered sequence ID.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::Row;
use uuid::Uuid;

use super::super::{EventFilter, EventStore, StorageError};
use super::PostgresStorage;
use crate::event_log::{Event, EventKind};

#[async_trait]
impl EventStore for PostgresStorage {
    async fn append(&self, repo_id: &Uuid, event: EventKind) -> Result<Event, StorageError> {
        let event_type = event.event_type();
        let workspace_id: Option<Uuid> = event.workspace_id();
        let payload =
            serde_json::to_value(&event).map_err(|e| StorageError::Serialization(e.to_string()))?;

        // Use a transaction so the NOTIFY fires only after the INSERT commits.
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;

        let row = sqlx::query(
            r#"
            INSERT INTO events (repo_id, event_type, workspace_id, payload)
            VALUES ($1, $2, $3, $4)
            RETURNING id, created_at
            "#,
        )
        .bind(repo_id)
        .bind(event_type)
        .bind(workspace_id)
        .bind(&payload)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        let id: i64 = row.get("id");
        let created_at: DateTime<Utc> = row.get("created_at");

        // Notify WebSocket listeners that a new event is available.
        // Payload format: "<repo_id>:<event_id>" — lightweight pointer only.
        // The listener queries the full event from the database.
        let notify_payload = format!("{repo_id}:{id}");
        sqlx::query("SELECT pg_notify('vai_events', $1)")
            .bind(&notify_payload)
            .execute(&mut *tx)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;

        tx.commit()
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(Event {
            id: id as u64,
            kind: event,
            timestamp: created_at,
        })
    }

    async fn query_by_type(
        &self,
        repo_id: &Uuid,
        event_type: &str,
    ) -> Result<Vec<Event>, StorageError> {
        let rows = sqlx::query(
            "SELECT id, payload, created_at FROM events \
             WHERE repo_id = $1 AND event_type = $2 ORDER BY id",
        )
        .bind(repo_id)
        .bind(event_type)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        rows_to_events(rows)
    }

    async fn query_by_workspace(
        &self,
        repo_id: &Uuid,
        workspace_id: &Uuid,
    ) -> Result<Vec<Event>, StorageError> {
        let rows = sqlx::query(
            "SELECT id, payload, created_at FROM events \
             WHERE repo_id = $1 AND workspace_id = $2 ORDER BY id",
        )
        .bind(repo_id)
        .bind(workspace_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        rows_to_events(rows)
    }

    async fn query_by_time_range(
        &self,
        repo_id: &Uuid,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<Event>, StorageError> {
        let rows = sqlx::query(
            "SELECT id, payload, created_at FROM events \
             WHERE repo_id = $1 AND created_at >= $2 AND created_at <= $3 ORDER BY id",
        )
        .bind(repo_id)
        .bind(from)
        .bind(to)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        rows_to_events(rows)
    }

    async fn query_since_id(
        &self,
        repo_id: &Uuid,
        last_id: i64,
    ) -> Result<Vec<Event>, StorageError> {
        let rows = sqlx::query(
            "SELECT id, payload, created_at FROM events \
             WHERE repo_id = $1 AND id > $2 ORDER BY id",
        )
        .bind(repo_id)
        .bind(last_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        rows_to_events(rows)
    }

    /// Server-side filtered query — pushes all active filter dimensions to
    /// Postgres so only matching rows are transferred over the wire.
    ///
    /// Dimensions applied in SQL:
    /// - `event_types` → `event_type = ANY($n)`
    /// - `workspace_ids` → `workspace_id = ANY($n)`
    /// - `entity_ids` / `paths` → `payload::text LIKE '%…%'` OR-chain
    async fn query_since_id_filtered(
        &self,
        repo_id: &Uuid,
        last_id: i64,
        filter: &EventFilter,
    ) -> Result<Vec<Event>, StorageError> {
        if filter.is_empty() {
            return self.query_since_id(repo_id, last_id).await;
        }

        let mut qb: sqlx::QueryBuilder<sqlx::Postgres> = sqlx::QueryBuilder::new(
            "SELECT id, payload, created_at FROM events WHERE repo_id = ",
        );
        qb.push_bind(repo_id);
        qb.push(" AND id > ");
        qb.push_bind(last_id);

        if !filter.event_types.is_empty() {
            qb.push(" AND event_type = ANY(");
            qb.push_bind(filter.event_types.clone());
            qb.push(")");
        }

        if !filter.workspace_ids.is_empty() {
            qb.push(" AND workspace_id = ANY(");
            qb.push_bind(filter.workspace_ids.clone());
            qb.push(")");
        }

        // Entity IDs: at least one must appear (substring) in the payload.
        if !filter.entity_ids.is_empty() {
            qb.push(" AND (");
            for (i, eid) in filter.entity_ids.iter().enumerate() {
                if i > 0 {
                    qb.push(" OR ");
                }
                qb.push("payload::text LIKE ");
                qb.push_bind(format!("%{eid}%"));
            }
            qb.push(")");
        }

        // Paths: at least one must appear (substring) in the payload.
        if !filter.paths.is_empty() {
            qb.push(" AND (");
            for (i, path) in filter.paths.iter().enumerate() {
                if i > 0 {
                    qb.push(" OR ");
                }
                qb.push("payload::text LIKE ");
                qb.push_bind(format!("%{path}%"));
            }
            qb.push(")");
        }

        qb.push(" ORDER BY id");

        let rows = qb
            .build()
            .fetch_all(&self.pool)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;

        rows_to_events(rows)
    }

    async fn count(&self, repo_id: &Uuid) -> Result<u64, StorageError> {
        let row = sqlx::query("SELECT COUNT(*) AS n FROM events WHERE repo_id = $1")
            .bind(repo_id)
            .fetch_one(&self.pool)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;
        let n: i64 = row.get("n");
        Ok(n as u64)
    }
}

/// Deserialises a batch of event rows into [`Event`] values.
fn rows_to_events(rows: Vec<sqlx::postgres::PgRow>) -> Result<Vec<Event>, StorageError> {
    let mut events = Vec::with_capacity(rows.len());
    for row in rows {
        let id: i64 = row.get("id");
        let payload: serde_json::Value = row.get("payload");
        let created_at: DateTime<Utc> = row.get("created_at");
        let kind: EventKind = serde_json::from_value(payload)
            .map_err(|e| StorageError::Serialization(e.to_string()))?;
        events.push(Event {
            id: id as u64,
            kind,
            timestamp: created_at,
        });
    }
    Ok(events)
}
