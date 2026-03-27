# Phase 9: Storage Abstraction Layer

## Summary

Introduce Rust traits to abstract storage operations so that the same codebase supports both SQLite (local CLI mode) and Postgres (server mode). This is the foundation that enables the hosted platform without breaking the zero-dependency local experience.

## Motivation

vai currently uses SQLite and filesystem storage directly throughout the codebase. To support a hosted multi-tenant server with Postgres and S3, we need a clean abstraction layer. The merge engine, conflict engine, scope inference, and all other business logic should be unaware of which storage backend is in use.

## Requirements

### 9.1: Database Trait

Define a `Storage` trait (or a set of focused traits) that covers all database operations:

```rust
#[async_trait]
pub trait EventStore: Send + Sync {
    async fn append(&self, repo_id: &Uuid, event: EventKind) -> Result<Event, StorageError>;
    async fn query_by_type(&self, repo_id: &Uuid, event_type: &str) -> Result<Vec<Event>, StorageError>;
    async fn query_by_workspace(&self, repo_id: &Uuid, workspace_id: &Uuid) -> Result<Vec<Event>, StorageError>;
    async fn query_by_time_range(&self, repo_id: &Uuid, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Vec<Event>, StorageError>;
    async fn query_since_id(&self, repo_id: &Uuid, last_id: i64) -> Result<Vec<Event>, StorageError>;
}

#[async_trait]
pub trait IssueStore: Send + Sync {
    async fn create(&self, repo_id: &Uuid, issue: NewIssue) -> Result<Issue, StorageError>;
    async fn get(&self, repo_id: &Uuid, id: &Uuid) -> Result<Issue, StorageError>;
    async fn list(&self, repo_id: &Uuid, filter: &IssueFilter) -> Result<Vec<Issue>, StorageError>;
    async fn update(&self, repo_id: &Uuid, id: &Uuid, update: IssueUpdate) -> Result<Issue, StorageError>;
    async fn close(&self, repo_id: &Uuid, id: &Uuid, resolution: &str) -> Result<Issue, StorageError>;
}

// Similar traits for:
// - EscalationStore
// - GraphStore (entities, relationships, stats)
// - VersionStore (create, get, list, read_head, advance_head)
// - WorkspaceStore (create, get, list, update, discard)
// - AuthStore (create_key, validate_key, list_keys, revoke_key)
```

Each trait method takes a `repo_id` parameter. In local SQLite mode, this is always the single local repo ID. In Postgres server mode, it scopes all queries.

### 9.2: SQLite Implementation

Wrap the existing SQLite storage code into trait implementations. This is mostly a refactor — the logic stays the same, it just moves behind trait methods. The SQLite impls ignore `repo_id` (single repo) or use it as a filter column for forward compatibility.

The existing module structure (`event_log/mod.rs`, `issue/mod.rs`, etc.) can keep their SQLite implementations as the default, with the trait extracted alongside.

### 9.3: Postgres Implementation

Create Postgres implementations of all storage traits. Use `sqlx` with the Postgres driver for async, compile-time checked queries.

Schema: shared tables with `repo_id UUID NOT NULL` column on every table. Indexes on `(repo_id, ...)` for all query patterns.

Core tables:
- `repos` — id, name, org_id, created_at
- `events` — id (BIGSERIAL), repo_id, event_type, payload (JSONB), workspace_id, timestamp
- `versions` — id, repo_id, version_id, parent_version_id, intent, created_by, created_at
- `workspaces` — id, repo_id, intent, base_version, status, issue_id, created_at, updated_at
- `issues` — id, repo_id, title, body, status, priority, labels (TEXT[]), creator, resolution, created_at, updated_at
- `escalations` — id, repo_id, type, severity, description, workspace_id, resolved, resolution, created_at
- `entities` — id, repo_id, kind, name, qualified_name, file_path, line_start, line_end, parent_entity_id
- `relationships` — id, repo_id, kind, from_entity_id, to_entity_id
- `api_keys` — id, repo_id (nullable for server-level keys), name, key_hash, key_prefix, role, created_at, revoked

### 9.4: FileStore Trait

```rust
#[async_trait]
pub trait FileStore: Send + Sync {
    async fn put(&self, repo_id: &Uuid, path: &str, content: &[u8]) -> Result<String, StorageError>;
    async fn get(&self, repo_id: &Uuid, path: &str) -> Result<Vec<u8>, StorageError>;
    async fn list(&self, repo_id: &Uuid, prefix: &str) -> Result<Vec<FileMetadata>, StorageError>;
    async fn delete(&self, repo_id: &Uuid, path: &str) -> Result<(), StorageError>;
    async fn exists(&self, repo_id: &Uuid, path: &str) -> Result<bool, StorageError>;
}
```

### 9.5: Filesystem Implementation

Wrap the current filesystem operations (reading/writing to project root and workspace overlays) into the `FileStore` trait. Path resolution: `{storage_root}/{repo_id}/{path}`.

### 9.6: S3 Implementation

Implement `FileStore` using the `aws-sdk-s3` crate (or `rusoto`). Bucket structure: `vai-{environment}/{repo_id}/{path}`. Content-addressable: store by SHA-256 hash, maintain a path→hash index in Postgres.

For local development, connect to MinIO which is S3-compatible.

### 9.7: Storage Factory

Create a factory/builder that constructs the right storage backend based on configuration:

```rust
pub enum StorageBackend {
    Local { vai_dir: PathBuf },
    Server { database_url: String, s3_config: S3Config },
}

impl StorageBackend {
    pub fn event_store(&self) -> Arc<dyn EventStore>;
    pub fn issue_store(&self) -> Arc<dyn IssueStore>;
    pub fn file_store(&self) -> Arc<dyn FileStore>;
    // ...
}
```

The CLI reads the backend from config. The server always uses the Server backend.

## Dependencies

- `sqlx` with `postgres` and `runtime-tokio` features — async Postgres driver
- `aws-sdk-s3` or `rust-s3` — S3 client

## Out of Scope

- Migration tooling (local → remote) — separate PRD
- RBAC implementation — separate PRD
- Schema migrations versioning (use sqlx migrations)

## Issues

1. **Define storage traits for all stores** — Create trait definitions for EventStore, IssueStore, EscalationStore, GraphStore, VersionStore, WorkspaceStore, AuthStore, and FileStore in a new `src/storage/` module. Priority: high.

2. **Refactor existing SQLite code into trait implementations** — Move current SQLite logic behind the new traits without changing behavior. All existing tests must continue to pass. Priority: high.

3. **Implement Postgres schema and migrations** — Create SQL migration files for all tables with repo_id columns, proper indexes, and constraints. Use sqlx migrations. Priority: high.

4. **Implement Postgres storage backends** — Write Postgres implementations of all storage traits using sqlx. Priority: high.

5. **Implement S3 FileStore backend** — Create S3-compatible FileStore implementation using aws-sdk-s3. Support content-addressable storage by SHA-256 hash. Priority: high.

6. **Implement filesystem FileStore backend** — Wrap existing filesystem operations into the FileStore trait. Priority: medium.

7. **Create storage factory and wire into CLI/server** — Build the StorageBackend enum and factory. CLI uses Local backend, server uses Server backend based on config. Priority: high.

8. **Add docker-compose.yml for local dev** — Postgres + MinIO containers with health checks, volume mounts, and default credentials. Include a README section on local setup. Priority: medium.

9. **Update server to use storage traits** — Replace direct SQLite calls in server handlers with trait-based storage. All server tests must pass against both backends. Priority: high.

10. **Add integration tests for Postgres backend** — Tests that run against a real Postgres instance (via testcontainers or docker-compose). Verify parity with SQLite backend behavior. Priority: medium.
