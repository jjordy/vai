//! OnboardingStore implementation for PostgresStorage.
//!
//! Persists per-user onboarding completion state in the `user_onboarding`
//! table.  Operations are scoped by `user_id` (a TEXT identifier — the vai
//! user UUID as a string) and are designed to be idempotent.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::Row;

use super::super::{OnboardingStore, StorageError};
use super::PostgresStorage;

#[async_trait]
impl OnboardingStore for PostgresStorage {
    async fn get_user_onboarding(
        &self,
        user_id: &str,
    ) -> Result<Option<DateTime<Utc>>, StorageError> {
        let row = sqlx::query(
            "SELECT completed_at FROM user_onboarding WHERE user_id = $1",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(row.map(|r| r.get("completed_at")))
    }

    async fn complete_user_onboarding(
        &self,
        user_id: &str,
    ) -> Result<DateTime<Utc>, StorageError> {
        // Insert with DO NOTHING on conflict so the existing timestamp is
        // preserved if onboarding was already completed.
        let row = sqlx::query(
            "INSERT INTO user_onboarding (user_id, completed_at)
             VALUES ($1, NOW())
             ON CONFLICT (user_id) DO NOTHING
             RETURNING completed_at",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        if let Some(r) = row {
            return Ok(r.get("completed_at"));
        }

        // Row already existed — return the stored timestamp.
        let row = sqlx::query(
            "SELECT completed_at FROM user_onboarding WHERE user_id = $1",
        )
        .bind(user_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(row.get("completed_at"))
    }
}
