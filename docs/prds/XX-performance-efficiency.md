# Phase 14: Performance and Efficiency

> **Execution order:** This PRD should be implemented AFTER PRD 13 (storage purity), PRD 15 (issue improvements), and PRD 13 (security) are complete. Performance optimization is most effective once the architecture is stable.

## Summary

Ensure vai operates efficiently at scale — fast queries, low memory usage, minimal latency, and smart resource management. This PRD defines performance targets, optimization strategies, and monitoring for both local CLI and hosted server modes.

## Motivation

vai's value proposition depends on being faster and smarter than git for AI agent workflows. Agents shouldn't wait seconds for scope inference, workspace creation should be instant, and merge operations should be fast enough that 50+ agents can submit concurrently without queueing. Performance degradation would make agents prefer raw git, defeating the purpose.

## Requirements

### 14.1: Performance Targets

| Operation | Target | Current | Mode |
|-----------|--------|---------|------|
| `vai init` (10K files) | < 30s | ~30s | Local |
| `vai graph refresh` (10K files) | < 30s | Untested at scale | Local |
| `vai workspace create` | < 100ms | ~50ms | Both |
| `vai workspace submit` (fast-forward) | < 500ms | ~200ms | Both |
| `vai workspace submit` (semantic merge, 50 files) | < 10s | Untested at scale | Both |
| Scope inference | < 2s | ~500ms | Both |
| `GET /api/status` | < 10ms | ~5ms | Server |
| `GET /api/issues` (1000 issues) | < 100ms | Untested at scale | Server |
| `GET /api/work-queue` (500 issues, 50 active workspaces) | < 500ms | Untested at scale | Server |
| `GET /api/graph/entities` (100K entities) | < 200ms | Untested at scale | Server |
| WebSocket event delivery | < 50ms from creation | ~20ms (in-memory) | Server |
| File upload (10MB) | < 2s | Untested | Server |
| File download | < 500ms | Untested | Server |
| Migration (1000 events, 50 issues) | < 30s | Not implemented | Server |

### 14.2: Database Query Optimization

**Postgres (server mode):**
- **Connection pooling** — use `sqlx::PgPool` with configurable pool size (default: 10 connections, max: 50). Measure and tune.
- **Prepared statements** — ensure all frequent queries use prepared statements via sqlx's compile-time checking.
- **Pagination** — all list endpoints must support cursor-based pagination (`?after=<id>&limit=50`). Never load unbounded result sets.
- **Selective columns** — list endpoints should `SELECT` only the columns needed for the list view, not `SELECT *`. Detail endpoints can fetch all columns.
- **Explain analyze** — for the top 10 most frequent queries, run `EXPLAIN ANALYZE` and verify indexes are being used. Document the query plans.
- **Partial indexes** — for issue queries filtered by status (most common pattern: `WHERE status = 'open'`), add a partial index.

**SQLite (local mode):**
- **WAL mode** — ensure all SQLite databases use WAL mode for concurrent read/write. Already set in event log — verify for all stores.
- **PRAGMA optimizations** — set `journal_mode=WAL`, `synchronous=NORMAL`, `cache_size=-64000` (64MB cache) on all SQLite connections.

### 14.3: Graph Engine Optimization

The semantic graph is the most query-intensive component:

- **Incremental parsing** — when a single file changes, only re-parse that file. Don't rebuild the entire graph. Already implemented in `update_file` — verify it's used consistently.
- **Batch entity insertion** — when parsing multiple files (init, refresh), use batch INSERT instead of individual statements. Use Postgres `COPY` or multi-value INSERT.
- **Graph query caching** — cache blast radius computations and scope predictions for active workspaces. Invalidate when the workspace scope changes.
- **Lazy relationship loading** — entity list queries should not join relationships. Load relationships only for detail/blast-radius queries.

### 14.4: File Storage Optimization

- **Streaming uploads** — large file uploads should stream to S3 without buffering the entire file in memory. Use multipart upload for files > 5MB.
- **Streaming downloads** — file downloads should stream from S3 to the HTTP response, not buffer in memory.
- **Content deduplication** — files stored by SHA-256 hash. If two versions have the same file content, it's stored once. The `put` operation checks if the hash exists before uploading.
- **Compression** — consider gzip/zstd compression for stored file content. S3 supports `Content-Encoding: gzip`.
- **Presigned URLs** — for large file downloads, return a presigned S3 URL instead of proxying through the vai server. Reduces server load.

### 14.5: Merge Engine Optimization

- **Parallel file processing** — when merging a workspace with many files, process non-conflicting files in parallel using `tokio::spawn` or rayon.
- **Tree-sitter parser reuse** — reuse parser instances across files instead of creating new ones for each parse. Parsers are not thread-safe but can be reused sequentially.
- **Early exit on conflicts** — if Level 1 (textual) detects no overlap, skip Level 2 and Level 3 analysis for that file.
- **Merge result caching** — if the same workspace is submitted multiple times (retry after transient failure), cache the merge analysis.
- **S3MergeFs memory management** — the in-memory buffer for server-mode merges holds ~15MB per concurrent merge (3 snapshots x 5MB). Files > 1MB skip semantic merge (binary diff only). Monitor memory usage under concurrent load and consider streaming large files to temp storage if memory pressure is detected.
- **Tarball processing** — the `upload-snapshot` endpoint extracts tarballs in memory. For large repos, consider streaming extraction with size limits. Current limit: 100MB per tarball.

### 14.6: Event System Optimization

- **Batched NOTIFY** — when a single operation creates multiple events (e.g., workspace submit creates MergeCompleted + VersionCreated + EntityAdded), batch them into a single NOTIFY signal. The WebSocket handler fetches all new events in one query.
- **Event table partitioning** — partition the events table by month for efficient archival and query performance on recent events.
- **Event pruning** — provide a configurable retention policy (default: 90 days). Older events are archived to cold storage or deleted. Keep a summary/count for analytics.

### 14.7: Server Concurrency

- **Per-repo locking granularity** — replace the single global `repo_lock` with per-repo locks. Multiple agents working on different repos should never block each other.
- **Read/write lock separation** — use `RwLock` instead of `Mutex` where possible. Read-only operations (status, list, graph query) should not block each other.
- **Connection limits** — configurable max WebSocket connections per server (default: 1000). Gracefully reject new connections when at capacity.
- **Request timeout** — add a configurable request timeout (default: 30s). Kill long-running requests to prevent resource exhaustion.

### 14.8: CLI Performance

- **Lazy initialization** — don't open the graph database or event log for commands that don't need them (e.g., `vai issue list` doesn't need the graph).
- **Parallel file collection** — use `rayon` or `walkdir` with parallel iteration for `collect_source_files` on large repos.
- **Progress indicators** — show progress bars for operations > 1s (already implemented for init — extend to refresh, migration).

### 14.9: Monitoring and Observability

- **Request metrics** — track request count, latency (p50/p95/p99), and error rate per endpoint. Expose via `GET /api/server/metrics` (Prometheus format).
- **Database metrics** — track connection pool utilization, query duration, and slow queries (> 100ms).
- **WebSocket metrics** — track connected clients, events delivered per second, replay requests.
- **Storage metrics** — track S3 operations per second, upload/download latency, storage size per repo.
- **Health check detail** — expand `GET /health` to include subsystem status (database reachable, S3 reachable, event system healthy).

## Out of Scope

- CDN / edge caching (future — for global deployment)
- Read replicas (future — for horizontal read scaling)
- Sharding (future — for extreme multi-tenancy)
- Client-side caching in the CLI (beyond what the filesystem provides)

## Issues

1. **Add cursor-based pagination to all list endpoints** — Replace offset-based pagination with cursor-based (`?after=<id>&limit=N`). Apply to issues, workspaces, versions, entities, escalations, events. Priority: high.

2. **Implement per-repo locking instead of global repo_lock** — Replace the single `Mutex<()>` with a `DashMap<Uuid, Arc<Mutex<()>>>` keyed by repo_id. Agents on different repos never block each other. Priority: high.

3. **Optimize Postgres connection pooling** — Configure `sqlx::PgPool` with appropriate min/max connections. Add pool utilization to server stats. Priority: high.

4. **Add streaming file upload/download for S3** — Use multipart upload for files > 5MB. Stream downloads directly from S3 to HTTP response without buffering. Priority: high.

5. ~~**Implement content deduplication in FileStore**~~ — ALREADY DONE. Files stored by SHA-256 hash in S3 with path→hash mapping in Postgres `file_index` table.

6. **Add batch entity insertion for graph operations** — Use multi-value INSERT or Postgres COPY for bulk entity/relationship insertion during init and refresh. Priority: medium.

7. **Add partial indexes for common query patterns** — Add partial index on issues `WHERE status = 'open'`, events `WHERE created_at > now() - interval '7 days'`, workspaces `WHERE status NOT IN ('merged', 'discarded')`. Priority: medium.

8. **Implement parallel file processing in merge engine** — Process non-conflicting files in parallel during semantic merge. Use tokio::spawn for I/O-bound work. Priority: medium.

9. **Add Prometheus-format metrics endpoint** — `GET /api/server/metrics` exposing request count, latency histograms, connection pool stats, WebSocket client count. Priority: medium.

10. **Implement event table partitioning and retention** — Partition events table by month. Add configurable retention policy with archival/deletion of old events. Priority: low.

11. **Add request timeout middleware** — Configurable timeout (default 30s) that kills long-running requests. Return 504 Gateway Timeout. Priority: medium.

12. **Optimize CLI lazy initialization** — Only open databases needed for the current command. Profile startup time and reduce to < 50ms for simple commands. Priority: low.
