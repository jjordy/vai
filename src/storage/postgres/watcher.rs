//! WatcherRegistryStore implementation for PostgresStorage.
//!
//! Handles registering watchers, querying their state, pausing/resuming,
//! rate-limited discovery preparation, and recording discovery events with
//! duplicate suppression.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::Row;
use uuid::Uuid;

use super::super::{StorageError, WatcherRegistryStore};
use super::PostgresStorage;
use crate::watcher::{
    DiscoveryEventKind, DiscoveryPreparation, DiscoveryRecord, IssueCreationPolicy, Watcher,
    WatchType, WatcherStatus,
};

/// Returns the current UTC hour as a bucket string `YYYY-MM-DDTHH`.
fn pg_hour_bucket() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H").to_string()
}

/// Maps a Postgres row (8 columns) into a [`Watcher`].
fn row_to_watcher(row: &sqlx::postgres::PgRow) -> Result<Watcher, StorageError> {
    let agent_id: String = row.try_get("agent_id").map_err(|e| StorageError::Database(e.to_string()))?;
    let watch_type: String = row.try_get("watch_type").map_err(|e| StorageError::Database(e.to_string()))?;
    let description: String = row.try_get("description").map_err(|e| StorageError::Database(e.to_string()))?;
    let policy_json: serde_json::Value = row.try_get("policy_json").map_err(|e| StorageError::Database(e.to_string()))?;
    let status: String = row.try_get("status").map_err(|e| StorageError::Database(e.to_string()))?;
    let registered_at: DateTime<Utc> = row.try_get("registered_at").map_err(|e| StorageError::Database(e.to_string()))?;
    let last_discovery_at: Option<DateTime<Utc>> = row.try_get("last_discovery_at").map_err(|e| StorageError::Database(e.to_string()))?;
    let discovery_count: i32 = row.try_get("discovery_count").map_err(|e| StorageError::Database(e.to_string()))?;

    let policy: IssueCreationPolicy = serde_json::from_value(policy_json)
        .unwrap_or_default();

    Ok(Watcher {
        agent_id,
        watch_type: WatchType::from_db_str(&watch_type),
        description,
        issue_creation_policy: policy,
        status: WatcherStatus::from_db_str(&status),
        registered_at,
        last_discovery_at,
        discovery_count: discovery_count as u32,
    })
}

#[async_trait]
impl WatcherRegistryStore for PostgresStorage {
    async fn register_watcher(
        &self,
        repo_id: &Uuid,
        watcher: Watcher,
    ) -> Result<Watcher, StorageError> {
        let policy_json = serde_json::to_value(&watcher.issue_creation_policy)
            .map_err(|e| StorageError::Serialization(e.to_string()))?;
        sqlx::query(
            r#"
            INSERT INTO watchers
                (repo_id, agent_id, watch_type, description, policy_json, status,
                 registered_at, last_discovery_at, discovery_count)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            "#,
        )
        .bind(repo_id)
        .bind(&watcher.agent_id)
        .bind(watcher.watch_type.as_str())
        .bind(&watcher.description)
        .bind(&policy_json)
        .bind(watcher.status.as_str())
        .bind(watcher.registered_at)
        .bind(watcher.last_discovery_at)
        .bind(watcher.discovery_count as i32)
        .execute(&self.pool)
        .await
        .map_err(|e| {
            let msg = e.to_string();
            if msg.contains("unique") || msg.contains("duplicate") || msg.contains("23505") {
                StorageError::Conflict(format!(
                    "watcher '{}' is already registered for this repo",
                    watcher.agent_id
                ))
            } else {
                StorageError::Database(msg)
            }
        })?;

        Ok(watcher)
    }

    async fn get_watcher(
        &self,
        repo_id: &Uuid,
        agent_id: &str,
    ) -> Result<Watcher, StorageError> {
        let row = sqlx::query(
            "SELECT agent_id, watch_type, description, policy_json, status, \
             registered_at, last_discovery_at, discovery_count \
             FROM watchers WHERE repo_id = $1 AND agent_id = $2",
        )
        .bind(repo_id)
        .bind(agent_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?
        .ok_or_else(|| StorageError::NotFound(format!("watcher '{agent_id}' not found")))?;

        row_to_watcher(&row)
    }

    async fn list_watchers(&self, repo_id: &Uuid) -> Result<Vec<Watcher>, StorageError> {
        let rows = sqlx::query(
            "SELECT agent_id, watch_type, description, policy_json, status, \
             registered_at, last_discovery_at, discovery_count \
             FROM watchers WHERE repo_id = $1 \
             ORDER BY registered_at DESC",
        )
        .bind(repo_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        rows.iter().map(row_to_watcher).collect()
    }

    async fn pause_watcher(
        &self,
        repo_id: &Uuid,
        agent_id: &str,
    ) -> Result<Watcher, StorageError> {
        let rows_affected = sqlx::query(
            "UPDATE watchers SET status = 'paused' WHERE repo_id = $1 AND agent_id = $2",
        )
        .bind(repo_id)
        .bind(agent_id)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?
        .rows_affected();

        if rows_affected == 0 {
            return Err(StorageError::NotFound(format!(
                "watcher '{agent_id}' not found"
            )));
        }

        self.get_watcher(repo_id, agent_id).await
    }

    async fn resume_watcher(
        &self,
        repo_id: &Uuid,
        agent_id: &str,
    ) -> Result<Watcher, StorageError> {
        let rows_affected = sqlx::query(
            "UPDATE watchers SET status = 'active' WHERE repo_id = $1 AND agent_id = $2",
        )
        .bind(repo_id)
        .bind(agent_id)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?
        .rows_affected();

        if rows_affected == 0 {
            return Err(StorageError::NotFound(format!(
                "watcher '{agent_id}' not found"
            )));
        }

        self.get_watcher(repo_id, agent_id).await
    }

    async fn prepare_discovery(
        &self,
        repo_id: &Uuid,
        agent_id: &str,
        event: &DiscoveryEventKind,
    ) -> Result<DiscoveryPreparation, StorageError> {
        // Step 1: validate watcher is active.
        let watcher = self.get_watcher(repo_id, agent_id).await?;
        if watcher.status == WatcherStatus::Paused {
            return Err(StorageError::NotFound(format!(
                "{agent_id} is paused — resume before submitting discoveries"
            )));
        }

        // Step 2: rate-limit — increment the per-hour counter.
        let bucket = pg_hour_bucket();
        sqlx::query(
            r#"
            INSERT INTO watcher_rate_limits (repo_id, agent_id, hour_bucket, count)
            VALUES ($1, $2, $3, 1)
            ON CONFLICT (repo_id, agent_id, hour_bucket)
            DO UPDATE SET count = watcher_rate_limits.count + 1
            "#,
        )
        .bind(repo_id)
        .bind(agent_id)
        .bind(&bucket)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        let count: i32 = sqlx::query(
            "SELECT count FROM watcher_rate_limits \
             WHERE repo_id = $1 AND agent_id = $2 AND hour_bucket = $3",
        )
        .bind(repo_id)
        .bind(agent_id)
        .bind(&bucket)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?
        .try_get("count")
        .map_err(|e| StorageError::Database(e.to_string()))?;

        let max = watcher.issue_creation_policy.max_per_hour;
        if count as u32 > max {
            // Roll back the increment before returning.
            let _ = sqlx::query(
                "UPDATE watcher_rate_limits SET count = count - 1 \
                 WHERE repo_id = $1 AND agent_id = $2 AND hour_bucket = $3",
            )
            .bind(repo_id)
            .bind(agent_id)
            .bind(&bucket)
            .execute(&self.pool)
            .await;

            return Err(StorageError::RateLimitExceeded(format!(
                "watcher {agent_id} has submitted {count} discoveries this hour (max {max})"
            )));
        }

        // Step 3: duplicate suppression — find existing open issue for this dedup key.
        let dedup_key = event.dedup_key();
        let existing_issue_id: Option<Uuid> = sqlx::query(
            "SELECT created_issue_id FROM watcher_discoveries \
             WHERE repo_id = $1 AND agent_id = $2 AND dedup_key = $3 \
               AND suppressed = FALSE AND created_issue_id IS NOT NULL \
             ORDER BY received_at DESC \
             LIMIT 1",
        )
        .bind(repo_id)
        .bind(agent_id)
        .bind(&dedup_key)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?
        .and_then(|row| row.try_get::<Option<Uuid>, _>("created_issue_id").ok().flatten());

        let priority = event.default_priority();
        let should_create = watcher.issue_creation_policy.should_auto_create(&priority);

        Ok(DiscoveryPreparation {
            record_id: Uuid::new_v4(),
            dedup_key,
            received_at: chrono::Utc::now(),
            suppressed_with_issue_id: existing_issue_id,
            should_create_issue: should_create,
            priority,
        })
    }

    async fn record_discovery(
        &self,
        repo_id: &Uuid,
        agent_id: &str,
        event: &DiscoveryEventKind,
        record_id: Uuid,
        dedup_key: &str,
        received_at: DateTime<Utc>,
        created_issue_id: Option<Uuid>,
        suppressed: bool,
    ) -> Result<DiscoveryRecord, StorageError> {
        let event_json = serde_json::to_value(event)
            .map_err(|e| StorageError::Serialization(e.to_string()))?;

        sqlx::query(
            r#"
            INSERT INTO watcher_discoveries
                (id, repo_id, agent_id, event_type, event_json, dedup_key,
                 received_at, created_issue_id, suppressed)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            "#,
        )
        .bind(record_id)
        .bind(repo_id)
        .bind(agent_id)
        .bind(event.event_type())
        .bind(&event_json)
        .bind(dedup_key)
        .bind(received_at)
        .bind(created_issue_id)
        .bind(suppressed)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        // Update watcher stats.
        sqlx::query(
            r#"
            UPDATE watchers
            SET last_discovery_at = $1,
                discovery_count = discovery_count + 1
            WHERE repo_id = $2 AND agent_id = $3
            "#,
        )
        .bind(received_at)
        .bind(repo_id)
        .bind(agent_id)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(DiscoveryRecord {
            id: record_id,
            agent_id: agent_id.to_string(),
            event: event.clone(),
            received_at,
            created_issue_id,
            suppressed,
        })
    }
}
