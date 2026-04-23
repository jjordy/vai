//! Postgres implementation of all storage traits.
//!
//! [`PostgresStorage`] is the server-mode backend. It stores all data in a
//! shared Postgres database with `repo_id` scoping on every table, allowing
//! multiple repositories to coexist in a single schema.
//!
//! # Connections
//!
//! Uses a [`sqlx::PgPool`] connection pool.  Create one with
//! [`PostgresStorage::connect`] and share it across handlers via `Arc`.
//!
//! # Migrations
//!
//! Call [`PostgresStorage::migrate`] once at server startup to apply any
//! pending SQL migrations from the `migrations/` directory.
//!
//! # Compile-time query checking
//!
//! This module uses `sqlx::query()` (runtime queries) rather than
//! `sqlx::query!` macros so that a live database is not required during CI
//! compilation.  Query correctness is verified by the integration tests in
//! `tests/`.

use sha2::{Digest, Sha256};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use uuid::Uuid;

use super::StorageError;

// Declare submodules — Rust looks for src/storage/postgres/<name>.rs
mod event;
mod issue;
mod escalation;
mod version;
mod workspace;
mod graph;
mod auth;
mod file;
mod org;
mod onboarding;
mod watcher;
mod worker;

// ── PostgresStorage ───────────────────────────────────────────────────────────

/// Connection pool utilization snapshot.
#[derive(Debug, Clone, Copy)]
pub struct PoolStats {
    /// Connections currently checked out (in use by a query).
    pub active: u32,
    /// Connections currently idle in the pool.
    pub idle: u32,
    /// Maximum number of connections allowed by the pool configuration.
    pub max: u32,
}

/// Postgres-backed storage for multi-tenant hosted vai.
///
/// All trait methods accept a `repo_id` parameter and scope every SQL query
/// to that repository.  The underlying connection pool is cheaply cloneable.
#[derive(Clone, Debug)]
pub struct PostgresStorage {
    pub(crate) pool: PgPool,
    /// Configured upper limit for the connection pool (stored so it can be
    /// reported in the server stats endpoint without calling into pool internals).
    max_connections: u32,
    /// In-memory cache of the last time we wrote `last_used_at` for each key
    /// ID. Used to debounce writes to once per minute so high-frequency API
    /// callers don't generate excessive UPDATE traffic.
    pub(crate) last_used_cache: Arc<Mutex<HashMap<String, Instant>>>,
}

impl PostgresStorage {
    /// Connects to Postgres at `database_url` and returns a new storage handle.
    ///
    /// `max_connections` caps the pool size. 25 is a reasonable default for
    /// single-server deployments under moderate load; increase for high-throughput
    /// scenarios.  The pool is configured with a 5-second acquire timeout (so
    /// callers get a clear error instead of hanging indefinitely) and releases
    /// connections idle for more than 10 minutes.
    pub async fn connect(database_url: &str, max_connections: u32) -> Result<Self, StorageError> {
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            // Fail fast with a clear error rather than waiting indefinitely when
            // the pool is exhausted.
            .acquire_timeout(Duration::from_secs(5))
            // Release idle connections after 10 minutes to avoid accumulating
            // stale connections during quiet periods.
            .idle_timeout(Duration::from_secs(600))
            .connect(database_url)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;
        Ok(Self { pool, max_connections, last_used_cache: Arc::new(Mutex::new(HashMap::new())) })
    }

    /// Applies all pending SQL migrations from `migrations/` at `migrations_path`.
    ///
    /// Call once at server startup before serving requests.  The migrations
    /// directory is loaded from disk at runtime so that the binary does not
    /// need to be recompiled when SQL files change.
    pub async fn migrate(&self, migrations_path: &str) -> Result<(), StorageError> {
        let migrator = sqlx::migrate::Migrator::new(std::path::Path::new(migrations_path))
            .await
            .map_err(|e| StorageError::Database(format!("failed to load migrations: {e}")))?;
        migrator
            .run(&self.pool)
            .await
            .map_err(|e| StorageError::Database(format!("migration failed: {e}")))?;
        Ok(())
    }

    /// Returns a reference to the underlying connection pool.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Returns a snapshot of connection pool utilization.
    pub fn pool_stats(&self) -> PoolStats {
        let size = self.pool.size();
        let idle = self.pool.num_idle() as u32;
        PoolStats {
            active: size.saturating_sub(idle),
            idle,
            max: self.max_connections,
        }
    }

    /// Verifies database connectivity by executing a lightweight `SELECT 1`.
    pub async fn ping(&self) -> Result<(), sqlx::Error> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }

    /// Looks up a repository by name, returning `(repo_id, name)` if found.
    ///
    /// Used by [`repo_resolve_middleware`](crate::server) to resolve a repo name
    /// to its UUID in server mode, without touching the filesystem.
    pub async fn get_repo_by_name(
        &self,
        name: &str,
    ) -> Result<Option<(uuid::Uuid, String)>, StorageError> {
        use sqlx::Row as _;
        let row = sqlx::query("SELECT id, name FROM repos WHERE name = $1")
            .bind(name)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;
        Ok(row.map(|r| (r.get("id"), r.get("name"))))
    }

    /// Creates a [`sqlx::postgres::PgListener`] connected via this pool.
    ///
    /// Used by the WebSocket handler to receive `LISTEN/NOTIFY` signals from
    /// Postgres without blocking a pool connection indefinitely.
    pub async fn create_listener(&self) -> Result<sqlx::postgres::PgListener, StorageError> {
        sqlx::postgres::PgListener::connect_with(&self.pool)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))
    }
}

// ── internal helpers ──────────────────────────────────────────────────────────

/// Hashes a plaintext API token and returns the SHA-256 hex digest.
pub(super) fn hash_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Generates a cryptographically suitable random hex token of `hex_chars` length.
pub(super) fn random_token(hex_chars: usize) -> String {
    let mut out = String::with_capacity(hex_chars);
    while out.len() < hex_chars {
        out.push_str(&Uuid::new_v4().simple().to_string());
    }
    out.truncate(hex_chars);
    out
}
