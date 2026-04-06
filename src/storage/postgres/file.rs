//! FileStore stub implementation for PostgresStorage.
//!
//! The Postgres backend does not implement FileStore — file content is stored
//! in S3.  The `S3FileStore` (issue #76) will provide the real implementation.
//! This stub returns an error at runtime so that accidental usage is surfaced
//! immediately rather than silently failing.

use async_trait::async_trait;
use uuid::Uuid;

use super::super::{FileMetadata, FileStore, StorageError};
use super::PostgresStorage;

/// Placeholder FileStore that always returns an error.
///
/// In server mode, wire an `S3FileStore` for file content storage.
#[async_trait]
impl FileStore for PostgresStorage {
    async fn put(
        &self,
        _repo_id: &Uuid,
        _path: &str,
        _content: &[u8],
    ) -> Result<String, StorageError> {
        Err(StorageError::Io(
            "PostgresStorage does not implement FileStore; use S3FileStore".to_string(),
        ))
    }

    async fn get(&self, _repo_id: &Uuid, _path: &str) -> Result<Vec<u8>, StorageError> {
        Err(StorageError::Io(
            "PostgresStorage does not implement FileStore; use S3FileStore".to_string(),
        ))
    }

    async fn list(
        &self,
        _repo_id: &Uuid,
        _prefix: &str,
    ) -> Result<Vec<FileMetadata>, StorageError> {
        Err(StorageError::Io(
            "PostgresStorage does not implement FileStore; use S3FileStore".to_string(),
        ))
    }

    async fn delete(&self, _repo_id: &Uuid, _path: &str) -> Result<(), StorageError> {
        Err(StorageError::Io(
            "PostgresStorage does not implement FileStore; use S3FileStore".to_string(),
        ))
    }

    async fn exists(&self, _repo_id: &Uuid, _path: &str) -> Result<bool, StorageError> {
        Err(StorageError::Io(
            "PostgresStorage does not implement FileStore; use S3FileStore".to_string(),
        ))
    }
}
