# PRD 13: Server Mode Storage Purity

## Overview

vai's server mode currently relies on a mix of local filesystem and remote storage (Postgres + S3). This creates a class of bugs where handlers silently fall back to stale filesystem data, workspace overlays don't capture deletions, and the download endpoint serves outdated files. This PRD eliminates all filesystem dependencies in server mode, establishing S3 + Postgres as the sole source of truth.

## Design Principles

1. **Server mode = S3 + Postgres only.** No filesystem fallbacks. If a storage trait method isn't implemented, fail loudly.
2. **Local mode unchanged.** The filesystem-based CLI workflow continues to work via the same trait abstraction.
3. **One trait call, backend dispatches.** No `if server_mode { ... } else { ... }` in handler code. No dual writes.
4. **Agent integration must be simple.** Six API calls for a complete agent workflow. SDKs hide complexity.

## Architecture

### Source of Truth

| Data | Local Mode | Server Mode |
|------|-----------|-------------|
| Metadata (events, issues, versions, workspaces) | SQLite | Postgres |
| File content (source, overlays, snapshots) | `.vai/files/` | S3 (content-addressable) |
| Current repo state | Filesystem (`repo_root`) | S3 `current/` prefix |
| Version history | `.vai/versions/` | S3 `versions/{id}/snapshot/` + Postgres |
| Graph (entities, relationships) | SQLite | Postgres |

### `current/` Prefix in S3

A `current/` prefix in S3 represents the complete, latest state of the repository. It is updated atomically after each workspace submit.

- `GET /files/download` serves `current/` (default)
- `GET /files/download?version=vN` reconstructs historical versions by replaying diffs
- Migration seeds `current/` from the initial upload
- Submits apply overlay additions/modifications and remove deleted files

### File Deletion Tracking

Add `deleted_paths TEXT[]` column to the `workspaces` table. The upload endpoint accepts:

```json
{
  "files": [{"path": "src/new.ts", "content_base64": "..."}],
  "deleted_paths": ["src/old.ts", "src/removed.ts"]
}
```

On submit, deleted paths are removed from `current/` in S3 and recorded in the version diff as `change_type: "deleted"`.

### Tarball Upload Endpoint

New endpoint for agent workflows:

```
POST /workspaces/:id/upload-snapshot
Content-Type: application/gzip
Body: <tarball of working directory>
```

The server extracts the tarball, diffs against `current/` in S3, stores changed files as workspace overlay, records deletions on the workspace row, and returns a summary. This replaces the fragile file-by-file upload + bash diffing in agent scripts.

Supports two modes:
- **Full tarball** (< 50MB repos) — complete working directory
- **Delta tarball** (large repos) — changed files only + deletions manifest

### Merge Engine Buffer Abstraction

The merge engine (`merge::submit`, `diff::compute`) currently reads/writes via `std::fs`. Introduce a trait:

```rust
trait MergeFs: Send + Sync {
    fn read_file(&self, path: &str) -> Result<Vec<u8>, MergeError>;
    fn write_file(&self, path: &str, content: &[u8]) -> Result<(), MergeError>;
    fn list_files(&self, prefix: &str) -> Result<Vec<String>, MergeError>;
    fn exists(&self, path: &str) -> Result<bool, MergeError>;
    fn delete_file(&self, path: &str) -> Result<(), MergeError>;
}
```

Implementations:
- **`DiskMergeFs`** — reads/writes local filesystem (local mode, current behavior)
- **`S3MergeFs`** — reads from S3, holds writes in memory, flushes to S3 after merge completes

The merge engine internals don't change — they call through the abstraction. The full semantic merge (AST-level, 3-way conflict resolution) is preserved in both modes.

Memory budget: ~15MB per concurrent merge (3 snapshots x ~5MB for a typical repo). With 50 concurrent agents: ~750MB. Add per-file size limit (1MB) for semantic merge — larger files get binary diff only.

### `repo_root` in Server Mode

In server mode, `repo_root` contains ONLY `.vai/config.toml` (repo_id mapping). No source files, no version snapshots, no workspace overlays. Any handler that attempts `std::fs::read` from `repo_root` in server mode is a bug.

`prepare_workspace_for_submit` is deleted entirely once the merge engine uses the buffer abstraction.

### Event Log Consolidation

Remove all `EventLog::open()` calls from server-mode code paths. The `EventStore` trait is the single write path:
- `SqliteStorage::append()` writes to local event log file
- `PostgresStorage::append()` writes to Postgres + triggers `pg_notify`

No dual writes. One call site, backend dispatches.

### Graph Refresh from S3

`server_graph_refresh_handler` reads source files from `current/` in S3 instead of `repo_root` on disk:
1. List all files under `current/` from S3
2. Filter to parseable extensions (.rs, .ts, .py)
3. Download content, parse ASTs in memory
4. Upsert entities/relationships to GraphStore

Graph refresh also runs automatically after each submit using the files already in memory from the merge.

### Conflict Detection from S3

The `ConflictEngine` takes a list of touched paths per workspace instead of scanning the filesystem. Paths are available from S3 — list `workspaces/{id}/` for each active workspace.

### Watcher Storage Trait

Add `WatcherStore` trait to the storage abstraction:
- `register_watcher(repo_id, watcher)` → Watcher
- `list_watchers(repo_id)` → Vec<Watcher>
- `pause_watcher(repo_id, id)` → ()
- `resume_watcher(repo_id, id)` → ()
- `submit_discovery(repo_id, discovery)` → Discovery

Implementations for SQLite (reads `.vai/watchers.db`) and Postgres.

### Migration Seeding

`vai remote migrate` seeds `current/` in S3:
1. Upload metadata to Postgres (existing, idempotent via `ON CONFLICT DO NOTHING`)
2. Upload all source files to S3 under `current/` prefix (new, idempotent via content-addressable keys)
3. Write ONLY `.vai/config.toml` to disk
4. Track progress via `migration_state` row in Postgres for resumability

### Testing Strategy

1. **Read-only `repo_root`** — E2E tests set `repo_root` to a read-only directory. Any filesystem write fails loudly instead of silently succeeding.
2. **CI grep check** — Fail the build if `src/server/mod.rs` handler functions contain `std::fs::read`, `std::fs::write`, `EventLog::open`, `workspace::get`, `workspace::overlay_dir`, or `repo::read_head` outside of explicitly allowed locations (marked with `// ALLOW_FS: reason`).
3. **Deletion round-trip test** — Upload file, submit, delete file in new workspace, submit, download — file must be absent.

## Agent Workflow (End State)

```
1. GET  /work-queue                    → pick highest priority issue
2. POST /work-queue/claim              → get workspace ID
3. GET  /files/download                → tarball of current repo
4. ... agent does work ...
5. POST /workspaces/:id/upload-snapshot → upload tarball, server diffs
6. POST /workspaces/:id/submit         → merge + create version
```

Six API calls. No filesystem knowledge required. SDKs for Python, TypeScript, and a future `vai-agent` CLI binary will wrap this into 4 function calls.

## Issue Breakdown

### Phase 1: Foundation (Critical Path)

1. **Add `deleted_paths` to workspace schema and upload endpoint** — Schema migration, update `upload_workspace_files_handler` to accept and store `deleted_paths`, update submit to remove from `current/`.
2. **Seed `current/` prefix in S3 during migration** — Update migration handler to write all source files to `current/`, make it idempotent and resumable.
3. **Download handler serves from `current/` only** — Remove filesystem fallback, fail if `current/` is empty.
4. **Tarball upload endpoint** — `POST /workspaces/:id/upload-snapshot`, server-side diffing against `current/`.

### Phase 2: Merge Engine (Core Refactor)

5. **Introduce `MergeFs` trait abstraction** — Define trait, implement `DiskMergeFs`, refactor merge engine to use it.
6. **Implement `S3MergeFs`** — In-memory buffer backed by S3, integrate with submit handler.
7. **Delete `prepare_workspace_for_submit`** — Remove all filesystem bridging code.
8. **Submit handler updates `current/` in S3** — Apply overlay + remove deletions atomically after merge.

### Phase 3: Handler Cleanup

9. **Remove all `EventLog::open()` from server handlers** — Single write path through `EventStore` trait.
10. **Graph refresh reads from `current/` in S3** — Auto-run after submit with in-memory files.
11. **Conflict detection from S3 path lists** — Remove filesystem scanning.
12. **Strip `repo_root` to config-only in server mode** — Remove source file writes during repo creation.

### Phase 4: Testing and Hardening

13. **E2E tests with read-only `repo_root`** — Catch any remaining filesystem leaks.
14. **CI grep check for `std::fs` in server handlers** — Automated guard against regressions.
15. **Deletion round-trip integration test** — Full lifecycle test for file deletions.

### Phase 5: Agent DX

16. **Tarball delta mode** — Support delta tarballs for large repos.
17. **Add `WatcherStore` trait and Postgres implementation** — Migrate watcher handlers off filesystem.
18. **Agent SDK (Python)** — Wrapper library for the 6-call agent workflow.
19. **Agent SDK (TypeScript)** — Same for Node.js agents.

### Future PRD

20. **`vai-agent` CLI binary** — Standalone Rust binary for agent integration. Four commands: `init`, `claim`, `download`, `submit`.
