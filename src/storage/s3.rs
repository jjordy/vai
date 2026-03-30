//! S3-compatible implementation of [`FileStore`].
//!
//! [`S3FileStore`] stores file content in an S3 bucket using content-addressable
//! storage: each file is keyed by its SHA-256 hash (`{repo_id}/{sha256}`), so
//! multiple paths pointing to identical bytes share a single S3 object.
//!
//! A `file_index` Postgres table maintains the `(repo_id, path) → s3_key`
//! mapping required to resolve paths back to S3 keys on reads.
//!
//! # MinIO compatibility
//!
//! Set `S3Config::endpoint_url` to the MinIO server URL (e.g. `http://localhost:9000`)
//! and `S3Config::force_path_style` to `true`.  The AWS SDK then uses
//! `http://localhost:9000/{bucket}/{key}` instead of virtual-host addressing.
//!
//! # Credentials
//!
//! `S3FileStore::connect` uses the AWS default credential chain (environment
//! variables `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY`, instance profile,
//! etc.).  For MinIO, set the MinIO access/secret as `AWS_ACCESS_KEY_ID` and
//! `AWS_SECRET_ACCESS_KEY` respectively.

use std::fmt;

use async_trait::async_trait;
use aws_sdk_s3::primitives::ByteStream;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use super::{FileMetadata, FileStore, StorageError};

// ── S3Config ──────────────────────────────────────────────────────────────────

/// Configuration for connecting to an S3-compatible object store.
///
/// The AWS default credential chain (env vars, EC2 instance profile, etc.) is
/// used for authentication.  For MinIO set `endpoint_url` and export
/// `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct S3Config {
    /// Bucket name.  For vai the convention is `vai-{environment}`.
    pub bucket: String,
    /// AWS region (e.g. `"us-east-1"`).  Required even for MinIO (use any value).
    pub region: String,
    /// Optional endpoint URL override for MinIO or other S3-compatible stores.
    pub endpoint_url: Option<String>,
    /// Use path-style S3 addressing (`{endpoint}/{bucket}/{key}`) instead of
    /// virtual-host style.  Required for MinIO.
    pub force_path_style: bool,
}

// ── S3FileStore ───────────────────────────────────────────────────────────────

/// [`FileStore`] backed by an S3-compatible object store with a Postgres path index.
///
/// Files are stored content-addressably: the S3 key is `{repo_id}/{sha256hex}`.
/// The path-to-key mapping lives in the `file_index` Postgres table (created by
/// migration `20260323000003_file_index.sql`).
///
/// The underlying [`aws_sdk_s3::Client`] and [`PgPool`] are both cheaply
/// cloneable (`Arc`-backed), so `S3FileStore` itself is cheap to clone.
#[derive(Clone)]
pub struct S3FileStore {
    client: aws_sdk_s3::Client,
    bucket: String,
    pool: PgPool,
}

impl fmt::Debug for S3FileStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("S3FileStore")
            .field("bucket", &self.bucket)
            .finish_non_exhaustive()
    }
}

impl S3FileStore {
    /// Creates a new `S3FileStore` from an already-configured S3 client.
    ///
    /// Use [`S3FileStore::connect`] to build a client from [`S3Config`] with
    /// the AWS default credential chain.
    pub fn new(client: aws_sdk_s3::Client, bucket: impl Into<String>, pool: PgPool) -> Self {
        Self {
            client,
            bucket: bucket.into(),
            pool,
        }
    }

    /// Connects to an S3-compatible store using [`S3Config`] and the AWS
    /// default credential chain, then returns a ready-to-use `S3FileStore`.
    ///
    /// For MinIO: set `config.endpoint_url` and `config.force_path_style = true`.
    pub async fn connect(config: S3Config, pool: PgPool) -> Result<Self, StorageError> {
        let sdk_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(aws_sdk_s3::config::Region::new(config.region))
            .load()
            .await;

        let mut s3_builder = aws_sdk_s3::config::Builder::from(&sdk_config);
        if let Some(endpoint) = config.endpoint_url {
            s3_builder = s3_builder.endpoint_url(endpoint);
        }
        if config.force_path_style {
            s3_builder = s3_builder.force_path_style(true);
        }

        let client = aws_sdk_s3::Client::from_conf(s3_builder.build());
        Ok(Self::new(client, config.bucket, pool))
    }

    /// Verifies S3 connectivity by listing at most one object with a health-check prefix.
    pub async fn ping(&self) -> Result<(), StorageError> {
        self.client
            .list_objects_v2()
            .bucket(&self.bucket)
            .prefix("_health_check/")
            .max_keys(1)
            .send()
            .await
            .map(|_| ())
            .map_err(|e| StorageError::Io(format!("S3 unreachable: {e}")))
    }

    /// Computes the S3 object key for a `(repo_id, content_hash)` pair.
    ///
    /// Convention: `{repo_id}/{sha256hex}`
    fn s3_key(repo_id: &Uuid, hash: &str) -> String {
        format!("{repo_id}/{hash}")
    }
}

// ── FileStore impl ─────────────────────────────────────────────────────────────

#[async_trait]
impl FileStore for S3FileStore {
    /// Stores `content` in S3 (content-addressably) and records the path→key
    /// mapping in `file_index`.  Returns the SHA-256 hex digest of the content.
    ///
    /// If a file already exists at `path` it is updated (both the S3 object and
    /// the index entry).  Old S3 objects that are no longer referenced are **not**
    /// garbage-collected by `put` — use `delete` to trigger cleanup.
    async fn put(
        &self,
        repo_id: &Uuid,
        path: &str,
        content: &[u8],
    ) -> Result<String, StorageError> {
        let hash = sha256_hex(content);
        let key = Self::s3_key(repo_id, &hash);

        // Upload to S3.  Content-addressable: if the same bytes are uploaded again
        // the object is just overwritten with identical data (idempotent).
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(&key)
            .body(ByteStream::from(content.to_vec()))
            .send()
            .await
            .map_err(|e| StorageError::Io(format!("S3 put failed for key {key}: {e}")))?;

        // Upsert the path → s3_key mapping.
        sqlx::query(
            "INSERT INTO file_index (repo_id, path, s3_key, content_hash, size)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT ON CONSTRAINT file_index_repo_path DO UPDATE
             SET s3_key       = EXCLUDED.s3_key,
                 content_hash = EXCLUDED.content_hash,
                 size         = EXCLUDED.size,
                 updated_at   = now()",
        )
        .bind(repo_id)
        .bind(path)
        .bind(&key)
        .bind(&hash)
        .bind(content.len() as i64)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Database(format!("file_index upsert failed: {e}")))?;

        Ok(hash)
    }

    /// Retrieves the content stored at `path` by resolving the path→S3 key
    /// mapping from `file_index` and fetching from S3.
    async fn get(&self, repo_id: &Uuid, path: &str) -> Result<Vec<u8>, StorageError> {
        let row = sqlx::query(
            "SELECT s3_key FROM file_index WHERE repo_id = $1 AND path = $2",
        )
        .bind(repo_id)
        .bind(path)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?
        .ok_or_else(|| StorageError::NotFound(format!("file {path}")))?;

        let s3_key: String = row.get("s3_key");

        let resp = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(&s3_key)
            .send()
            .await
            .map_err(|e| StorageError::Io(format!("S3 get failed for key {s3_key}: {e}")))?;

        let bytes = resp
            .body
            .collect()
            .await
            .map_err(|e| StorageError::Io(format!("S3 body read failed: {e}")))?
            .into_bytes()
            .to_vec();

        Ok(bytes)
    }

    /// Lists all files whose path starts with `prefix`, returning metadata from
    /// `file_index` (no S3 round-trips required).
    ///
    /// An empty `prefix` lists all files in the repository.
    async fn list(
        &self,
        repo_id: &Uuid,
        prefix: &str,
    ) -> Result<Vec<FileMetadata>, StorageError> {
        let rows = sqlx::query(
            "SELECT path, content_hash, size, updated_at
             FROM file_index
             WHERE repo_id = $1
               AND path LIKE $2
             ORDER BY path",
        )
        .bind(repo_id)
        // Append '%' to turn `prefix` into a LIKE pattern.  File paths do not
        // contain '%' or '_' so this is safe without additional escaping.
        .bind(format!("{prefix}%"))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(rows
            .iter()
            .map(|r| FileMetadata {
                path: r.get("path"),
                content_hash: r.get("content_hash"),
                size: r.get::<i64, _>("size") as u64,
                updated_at: r.get("updated_at"),
            })
            .collect())
    }

    /// Removes the `path` entry from `file_index` and, if no other paths in the
    /// same repository reference the same S3 object, deletes the object from S3.
    async fn delete(&self, repo_id: &Uuid, path: &str) -> Result<(), StorageError> {
        // Fetch the key before deleting so we can check the ref-count afterwards.
        let row = sqlx::query(
            "SELECT s3_key FROM file_index WHERE repo_id = $1 AND path = $2",
        )
        .bind(repo_id)
        .bind(path)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?
        .ok_or_else(|| StorageError::NotFound(format!("file {path}")))?;

        let s3_key: String = row.get("s3_key");

        // Remove index entry.
        sqlx::query("DELETE FROM file_index WHERE repo_id = $1 AND path = $2")
            .bind(repo_id)
            .bind(path)
            .execute(&self.pool)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;

        // Count remaining references to this S3 object within the same repo.
        let remaining: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM file_index WHERE repo_id = $1 AND s3_key = $2",
        )
        .bind(repo_id)
        .bind(&s3_key)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        if remaining == 0 {
            // No other paths reference this object — safe to delete from S3.
            self.client
                .delete_object()
                .bucket(&self.bucket)
                .key(&s3_key)
                .send()
                .await
                .map_err(|e| {
                    StorageError::Io(format!("S3 delete failed for key {s3_key}: {e}"))
                })?;
        }

        Ok(())
    }

    /// Returns `true` if `path` has an entry in `file_index` (no S3 round-trip).
    async fn exists(&self, repo_id: &Uuid, path: &str) -> Result<bool, StorageError> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM file_index WHERE repo_id = $1 AND path = $2",
        )
        .bind(repo_id)
        .bind(path)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(count > 0)
    }
}

// ── helpers ────────────────────────────────────────────────────────────────────

/// Returns the SHA-256 hex digest of `data` (64 lowercase hex chars).
fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_hex_is_64_chars() {
        let h = sha256_hex(b"hello");
        assert_eq!(h.len(), 64);
    }

    #[test]
    fn s3_key_format() {
        let repo_id = Uuid::nil();
        let key = S3FileStore::s3_key(&repo_id, "abc123");
        assert_eq!(
            key,
            "00000000-0000-0000-0000-000000000000/abc123"
        );
    }

    #[test]
    fn s3_config_is_clonable() {
        let cfg = S3Config {
            bucket: "vai-dev".to_string(),
            region: "us-east-1".to_string(),
            endpoint_url: Some("http://localhost:9000".to_string()),
            force_path_style: true,
        };
        let _ = cfg.clone();
    }
}
