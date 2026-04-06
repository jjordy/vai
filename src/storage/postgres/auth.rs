//! AuthStore implementation for PostgresStorage.
//!
//! Handles API key creation, validation (with debounced `last_used_at` updates),
//! revocation, session validation via the Better Auth session table, and
//! refresh token management.

use async_trait::async_trait;
use chrono::Utc;
use sqlx::Row;
use std::time::{Duration, Instant};
use uuid::Uuid;

use super::super::{AuthStore, StorageError};
use super::{hash_token, random_token, PostgresStorage};
use crate::auth::ApiKey;

#[async_trait]
impl AuthStore for PostgresStorage {
    async fn create_key(
        &self,
        repo_id: Option<&Uuid>,
        name: &str,
        user_id: Option<&Uuid>,
        role_override: Option<&str>,
        agent_type: Option<&str>,
        expires_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<(ApiKey, String), StorageError> {
        let id = Uuid::new_v4().to_string();
        let token = random_token(64);
        let key_hash = hash_token(&token);
        let key_prefix = token[..8].to_string();

        sqlx::query(
            r#"
            INSERT INTO api_keys
                (id, repo_id, name, key_hash, key_prefix, user_id, role_override,
                 agent_type, expires_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            "#,
        )
        .bind(&id)
        .bind(repo_id)
        .bind(name)
        .bind(&key_hash)
        .bind(&key_prefix)
        .bind(user_id)
        .bind(role_override)
        .bind(agent_type)
        .bind(expires_at)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        let key = ApiKey {
            id,
            name: name.to_string(),
            key_prefix,
            last_used_at: None,
            created_at: Utc::now(),
            revoked: false,
            user_id: user_id.copied(),
            role_override: role_override.map(|s| s.to_string()),
            agent_type: agent_type.map(|s| s.to_string()),
            expires_at,
        };

        Ok((key, token))
    }

    async fn validate_key(&self, token: &str) -> Result<ApiKey, StorageError> {
        let key_hash = hash_token(token);

        let row = sqlx::query(
            "SELECT id, name, key_prefix, last_used_at, created_at, revoked, \
                    user_id, role_override, agent_type, expires_at \
             FROM api_keys \
             WHERE key_hash = $1 \
               AND revoked = false \
               AND (expires_at IS NULL OR expires_at > now())",
        )
        .bind(&key_hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?
        .ok_or_else(|| StorageError::NotFound("invalid or revoked API key".to_string()))?;

        // Update last_used_at asynchronously (best-effort), debounced to at
        // most once per minute per key to avoid write amplification under
        // high-frequency API traffic.
        let id: String = row.get("id");
        let should_update = {
            let mut cache = self.last_used_cache.lock().unwrap_or_else(|p| p.into_inner());
            let now = Instant::now();
            match cache.get(&id) {
                Some(&last) if now.duration_since(last) < Duration::from_secs(60) => false,
                _ => {
                    cache.insert(id.clone(), now);
                    true
                }
            }
        };
        if should_update {
            let pool = self.pool.clone();
            let id_clone = id.clone();
            tokio::spawn(async move {
                let _ = sqlx::query("UPDATE api_keys SET last_used_at = now() WHERE id = $1")
                    .bind(&id_clone)
                    .execute(&pool)
                    .await;
            });
        }

        row_to_api_key(row)
    }

    async fn list_keys(&self, repo_id: Option<&Uuid>) -> Result<Vec<ApiKey>, StorageError> {
        let rows = match repo_id {
            Some(rid) => sqlx::query(
                "SELECT id, name, key_prefix, last_used_at, created_at, revoked, \
                        user_id, role_override, agent_type, expires_at \
                 FROM api_keys WHERE repo_id = $1 AND revoked = false ORDER BY created_at",
            )
            .bind(rid)
            .fetch_all(&self.pool)
            .await,
            None => sqlx::query(
                "SELECT id, name, key_prefix, last_used_at, created_at, revoked, \
                        user_id, role_override, agent_type, expires_at \
                 FROM api_keys WHERE repo_id IS NULL AND revoked = false ORDER BY created_at",
            )
            .fetch_all(&self.pool)
            .await,
        }
        .map_err(|e| StorageError::Database(e.to_string()))?;

        rows.into_iter().map(row_to_api_key).collect()
    }

    async fn list_keys_by_user(&self, user_id: &Uuid) -> Result<Vec<ApiKey>, StorageError> {
        let rows = sqlx::query(
            "SELECT id, name, key_prefix, last_used_at, created_at, revoked, \
                    user_id, role_override, agent_type, expires_at \
             FROM api_keys WHERE user_id = $1 AND revoked = false ORDER BY created_at",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        rows.into_iter().map(row_to_api_key).collect()
    }

    async fn revoke_key(&self, id: &str) -> Result<(), StorageError> {
        sqlx::query("UPDATE api_keys SET revoked = true WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(())
    }

    async fn revoke_keys_by_repo(&self, repo_id: &Uuid) -> Result<u64, StorageError> {
        let result =
            sqlx::query("UPDATE api_keys SET revoked = true WHERE repo_id = $1 AND revoked = false")
                .bind(repo_id)
                .execute(&self.pool)
                .await
                .map_err(|e| StorageError::Database(e.to_string()))?;
        Ok(result.rows_affected())
    }

    async fn revoke_keys_by_user(&self, user_id: &Uuid) -> Result<u64, StorageError> {
        let result = sqlx::query(
            "UPDATE api_keys SET revoked = true WHERE user_id = $1 AND revoked = false",
        )
        .bind(user_id)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;
        Ok(result.rows_affected())
    }

    async fn validate_session(&self, session_token: &str) -> Result<String, StorageError> {
        // Query the Better Auth `session` table. Better Auth uses camelCase
        // column names: "userId", "expiresAt", "token".
        let row = sqlx::query(
            r#"SELECT "userId" FROM session WHERE token = $1 AND "expiresAt" > now()"#,
        )
        .bind(session_token)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?
        .ok_or_else(|| StorageError::NotFound("invalid or expired session".to_string()))?;

        Ok(row.get("userId"))
    }

    async fn get_better_auth_user(
        &self,
        ba_user_id: &str,
    ) -> Result<(String, String), StorageError> {
        // Query the Better Auth `user` table (camelCase columns).
        let row = sqlx::query(r#"SELECT email, name FROM "user" WHERE id = $1"#)
            .bind(ba_user_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?
            .ok_or_else(|| {
                StorageError::NotFound(format!("Better Auth user '{ba_user_id}'"))
            })?;

        let email: String = row.get("email");
        let name: String = row.get("name");
        Ok((email, name))
    }

    async fn create_refresh_token(
        &self,
        user_id: &Uuid,
        expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<String, StorageError> {
        let token = random_token(64);
        let token_hash = hash_token(&token);
        let plaintext = format!("rt_{token}");

        sqlx::query(
            "INSERT INTO refresh_tokens (user_id, token_hash, expires_at) \
             VALUES ($1, $2, $3)",
        )
        .bind(user_id)
        .bind(&token_hash)
        .bind(expires_at)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(plaintext)
    }

    async fn validate_refresh_token(&self, token: &str) -> Result<Uuid, StorageError> {
        let token_hash = hash_token(token);

        let row = sqlx::query(
            "SELECT user_id FROM refresh_tokens \
             WHERE token_hash = $1 \
               AND expires_at > now() \
               AND revoked_at IS NULL",
        )
        .bind(&token_hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?
        .ok_or_else(|| {
            StorageError::NotFound("invalid, expired, or revoked refresh token".to_string())
        })?;

        let user_id: Uuid = row.get("user_id");
        Ok(user_id)
    }

    async fn revoke_refresh_token(&self, token: &str) -> Result<(), StorageError> {
        let token_hash = hash_token(token);

        let result = sqlx::query(
            "UPDATE refresh_tokens \
             SET revoked_at = now() \
             WHERE token_hash = $1 AND revoked_at IS NULL",
        )
        .bind(&token_hash)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        if result.rows_affected() == 0 {
            return Err(StorageError::NotFound(
                "refresh token not found or already revoked".to_string(),
            ));
        }
        Ok(())
    }
}

fn row_to_api_key(row: sqlx::postgres::PgRow) -> Result<ApiKey, StorageError> {
    Ok(ApiKey {
        id: row.get("id"),
        name: row.get("name"),
        key_prefix: row.get("key_prefix"),
        last_used_at: row.get("last_used_at"),
        created_at: row.get("created_at"),
        revoked: row.get("revoked"),
        user_id: row.get("user_id"),
        role_override: row.get("role_override"),
        agent_type: row.get("agent_type"),
        expires_at: row.get("expires_at"),
    })
}
