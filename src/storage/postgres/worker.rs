//! WorkerStore implementation for PostgresStorage.
//!
//! Covers worker creation, retrieval, heartbeat updates, log ingestion, and
//! terminal-state transitions for the `agent_workers` and `agent_worker_logs`
//! tables (PRD 28).

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::Row;
use uuid::Uuid;

use super::super::{
    AgentWorker, LogStream, NewWorker, StorageError, WorkerDoneReason, WorkerLog, WorkerStore,
};
use super::PostgresStorage;

fn row_to_worker(row: sqlx::postgres::PgRow) -> Result<AgentWorker, StorageError> {
    Ok(AgentWorker {
        id: row.try_get("id").map_err(|e| StorageError::Database(e.to_string()))?,
        repo_id: row.try_get("repo_id").map_err(|e| StorageError::Database(e.to_string()))?,
        provider: row.try_get("provider").map_err(|e| StorageError::Database(e.to_string()))?,
        machine_id: row.try_get("machine_id").map_err(|e| StorageError::Database(e.to_string()))?,
        state: row.try_get("state").map_err(|e| StorageError::Database(e.to_string()))?,
        workspace_id: row.try_get("workspace_id").map_err(|e| StorageError::Database(e.to_string()))?,
        last_heartbeat_at: row.try_get("last_heartbeat_at").map_err(|e| StorageError::Database(e.to_string()))?,
        started_at: row.try_get("started_at").map_err(|e| StorageError::Database(e.to_string()))?,
        ended_at: row.try_get("ended_at").map_err(|e| StorageError::Database(e.to_string()))?,
    })
}

#[async_trait]
impl WorkerStore for PostgresStorage {
    async fn create_worker(&self, worker: NewWorker) -> Result<AgentWorker, StorageError> {
        let id = Uuid::new_v4();
        let row = sqlx::query(
            r#"
            INSERT INTO agent_workers (id, repo_id, provider, machine_id, state)
            VALUES ($1, $2, $3, $4, 'spawning')
            RETURNING id, repo_id, provider, machine_id, state, workspace_id,
                      last_heartbeat_at, started_at, ended_at
            "#,
        )
        .bind(id)
        .bind(worker.repo_id)
        .bind(&worker.provider)
        .bind(&worker.machine_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        row_to_worker(row)
    }

    async fn get_worker(&self, worker_id: &Uuid) -> Result<AgentWorker, StorageError> {
        let row = sqlx::query(
            r#"
            SELECT id, repo_id, provider, machine_id, state, workspace_id,
                   last_heartbeat_at, started_at, ended_at
            FROM agent_workers WHERE id = $1
            "#,
        )
        .bind(worker_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?
        .ok_or_else(|| StorageError::NotFound(format!("agent worker {worker_id}")))?;

        row_to_worker(row)
    }

    async fn count_running_workers(&self, repo_id: &Uuid) -> Result<u64, StorageError> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM agent_workers WHERE repo_id = $1 AND state IN ('spawning', 'running')",
        )
        .bind(repo_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(count.max(0) as u64)
    }

    async fn is_cloud_agent_enabled(&self, repo_id: &Uuid) -> Result<bool, StorageError> {
        let enabled: Option<bool> = sqlx::query_scalar(
            "SELECT cloud_agent_enabled FROM repos WHERE id = $1",
        )
        .bind(repo_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(enabled.unwrap_or(false))
    }

    async fn list_logs(
        &self,
        worker_id: &Uuid,
        since_id: Option<i64>,
    ) -> Result<Vec<WorkerLog>, StorageError> {
        let rows = sqlx::query(
            r#"
            SELECT id, worker_id, ts, stream, chunk
            FROM agent_worker_logs
            WHERE worker_id = $1 AND ($2::BIGINT IS NULL OR id > $2)
            ORDER BY id ASC
            LIMIT 1000
            "#,
        )
        .bind(worker_id)
        .bind(since_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        rows.into_iter()
            .map(|row| {
                let stream_str: String = row.try_get("stream").map_err(|e| StorageError::Database(e.to_string()))?;
                let stream = match stream_str.as_str() {
                    "stdout" => LogStream::Stdout,
                    _ => LogStream::Stderr,
                };
                Ok(WorkerLog {
                    id: row.try_get("id").map_err(|e| StorageError::Database(e.to_string()))?,
                    worker_id: row.try_get("worker_id").map_err(|e| StorageError::Database(e.to_string()))?,
                    ts: row.try_get::<DateTime<Utc>, _>("ts").map_err(|e| StorageError::Database(e.to_string()))?,
                    stream,
                    chunk: row.try_get("chunk").map_err(|e| StorageError::Database(e.to_string()))?,
                })
            })
            .collect()
    }

    async fn set_machine_id(
        &self,
        worker_id: &Uuid,
        machine_id: &str,
    ) -> Result<(), StorageError> {
        sqlx::query(
            "UPDATE agent_workers SET machine_id = $1 WHERE id = $2",
        )
        .bind(machine_id)
        .bind(worker_id)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;
        Ok(())
    }

    async fn update_heartbeat(&self, worker_id: &Uuid) -> Result<(), StorageError> {
        let rows_affected = sqlx::query(
            "UPDATE agent_workers SET last_heartbeat_at = NOW() WHERE id = $1",
        )
        .bind(worker_id)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?
        .rows_affected();

        if rows_affected == 0 {
            return Err(StorageError::NotFound(format!(
                "agent worker {worker_id}"
            )));
        }
        Ok(())
    }

    async fn append_logs(
        &self,
        worker_id: &Uuid,
        stream: LogStream,
        chunks: &[String],
    ) -> Result<(), StorageError> {
        if chunks.is_empty() {
            return Ok(());
        }

        // Verify worker exists.
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM agent_workers WHERE id = $1)")
            .bind(worker_id)
            .fetch_one(&self.pool)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;

        if !exists {
            return Err(StorageError::NotFound(format!(
                "agent worker {worker_id}"
            )));
        }

        let stream_str = match stream {
            LogStream::Stdout => "stdout",
            LogStream::Stderr => "stderr",
        };

        // Bulk-insert all chunks in a single query for efficiency.
        let mut query_builder = sqlx::QueryBuilder::new(
            "INSERT INTO agent_worker_logs (worker_id, stream, chunk) ",
        );
        query_builder.push_values(chunks, |mut b, chunk| {
            b.push_bind(worker_id).push_bind(stream_str).push_bind(chunk);
        });
        query_builder
            .build()
            .execute(&self.pool)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(())
    }

    async fn mark_done(
        &self,
        worker_id: &Uuid,
        reason: WorkerDoneReason,
    ) -> Result<(), StorageError> {
        let state = match reason {
            WorkerDoneReason::Completed => "completed",
            WorkerDoneReason::Failed => "failed",
            WorkerDoneReason::Terminated => "dead",
        };

        let rows_affected = sqlx::query(
            "UPDATE agent_workers SET state = $1, ended_at = NOW() WHERE id = $2",
        )
        .bind(state)
        .bind(worker_id)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?
        .rows_affected();

        if rows_affected == 0 {
            return Err(StorageError::NotFound(format!(
                "agent worker {worker_id}"
            )));
        }
        Ok(())
    }
}
