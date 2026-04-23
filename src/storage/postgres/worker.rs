//! WorkerStore implementation for PostgresStorage.
//!
//! Covers heartbeat updates, log ingestion, and terminal-state transitions
//! for the `agent_workers` and `agent_worker_logs` tables (PRD 28).

use async_trait::async_trait;
use uuid::Uuid;

use super::super::{LogStream, StorageError, WorkerDoneReason, WorkerStore};
use super::PostgresStorage;

#[async_trait]
impl WorkerStore for PostgresStorage {
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
