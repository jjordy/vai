# PRD: Stateless Server Mode — Eliminate Filesystem Dependencies

## Status

Proposed

## Context

The vai server in multi-repo Postgres+S3 mode still depends on three filesystem artifacts that prevent it from running as a stateless container:

1. **`registry.json`** — maps repo names to filesystem paths. Redundant with the `repos` table in Postgres.
2. **`.vai/config.toml`** (per repo) — stores `repo_id`. Redundant with `repos.id` in Postgres.
3. **`.vai/head`** (per repo) — stores current version pointer. Derivable from the `versions` table.

Additionally, `~/.vai/server.toml` must be generated at container startup to pass `database_url` to the server, because the server doesn't read `DATABASE_URL` from the environment.

All real data (issues, workspaces, versions, events, graph, auth) is already in Postgres. All file content is in S3. The filesystem artifacts are redundant metadata that prevents clean container deployments.

## Design Decisions

### Server mode: Postgres is the source of truth

In server mode (Postgres+S3), ALL metadata comes from the database:

- **Repo registry** → `SELECT name, id FROM repos WHERE name = $1`
- **Repo ID** → returned by the repo lookup query above
- **Head version** → `SELECT version_id FROM versions WHERE repo_id = $1 ORDER BY created_at DESC LIMIT 1`

No filesystem reads or writes for these operations.

### Local mode: Filesystem stays as-is

Local mode (SQLite) continues to use:

- `registry.json` for multi-repo local setups
- `.vai/config.toml` for repo_id
- `.vai/head` for version pointer

No changes to local mode behavior.

### Environment variables for server config

The server startup reads configuration with this precedence (highest → lowest):

1. CLI flags (`--host`, `--port`, `--database-url`)
2. Environment variables (`DATABASE_URL`, `VAI_STORAGE_ROOT`, `VAI_HOST`, `VAI_PORT`, `VAI_CORS_ORIGINS`, `VAI_S3_*`)
3. `~/.vai/server.toml` (optional file, primarily for local dev)
4. Built-in defaults

`DATABASE_URL` (not `VAI_DATABASE_URL`) matches the convention used by Fly.io, Heroku, Railway, and every major PaaS.

### Stateless container

The production Dockerfile becomes:

```dockerfile
FROM debian:trixie-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/vai /usr/local/bin/vai
EXPOSE 7865
ENTRYPOINT ["vai"]
CMD ["server", "start", "--host", "0.0.0.0"]
```

No volumes, no config files, no entrypoint scripts. All configuration via env vars.

## Issue Breakdown

### Issue 1: Server startup reads DATABASE_URL and other config from env vars

**Priority:** high
**Blocks:** Issues 2-4

Update `src/cli/server_cmd.rs` to read environment variables before falling back to `server.toml`:

```rust
// Precedence: CLI flag > env var > server.toml > default
let database_url = cli_database_url
    .or_else(|| std::env::var("DATABASE_URL").ok())
    .or(global_cfg.database_url);

let host = cli_host
    .or_else(|| std::env::var("VAI_HOST").ok())
    .or(global_cfg.host)
    .unwrap_or_else(|| "127.0.0.1".to_string());

let port = cli_port
    .or_else(|| std::env::var("VAI_PORT").ok().and_then(|p| p.parse().ok()))
    .or(global_cfg.port)
    .unwrap_or(7865);
```

Also read `VAI_STORAGE_ROOT`, `VAI_CORS_ORIGINS` from environment.

**Acceptance criteria:**
- `DATABASE_URL=postgres://... vai server start` connects to Postgres without server.toml
- `VAI_HOST=0.0.0.0 vai server start` binds to all interfaces
- Existing server.toml config still works as fallback
- `cargo test --features full` passes

---

### Issue 2: Repo registry queries Postgres instead of registry.json in server mode

**Priority:** high
**Depends on:** Issue 1

Update `repo_resolve_middleware` to query the `repos` table when the storage backend is Postgres.

**Changes:**
- `src/server/mod.rs` — `repo_resolve_middleware`:
  - If storage is Postgres: `SELECT id, name FROM repos WHERE name = $1`
  - If storage is local: load `registry.json` (existing behavior)
- Pass `repo_id` from the query result directly into `RepoCtx` — no more `repo_id_from_vai_dir()`
- Add a `get_repo_by_name(name: &str)` method to the storage trait
- Implement for PostgresStorage (query `repos` table)
- Implement for SqliteStorage (return error or scan local registry)

**Acceptance criteria:**
- Server starts with Postgres, no `registry.json` needed
- `POST /api/repos` creates repo in Postgres only (no filesystem writes in server mode)
- All repo-scoped endpoints work without filesystem state
- Local mode still uses registry.json
- `cargo test --features full` passes

---

### Issue 3: Head version derived from Postgres instead of .vai/head file in server mode

**Priority:** high
**Depends on:** Issue 2

Derive the current head version from the `versions` table instead of reading `.vai/head`.

**Changes:**
- Add `get_head_version(repo_id: &Uuid) -> Option<String>` to the version storage trait
- PostgresStorage: `SELECT version_id FROM versions WHERE repo_id = $1 ORDER BY created_at DESC LIMIT 1`
- SqliteStorage: read from `.vai/head` file (existing behavior)
- Update `status_handler` to use the storage method
- Update `submit_workspace_handler` to skip writing `.vai/head` in server mode
- Update any other handlers that read/write `head`

**Acceptance criteria:**
- Status endpoint shows correct head_version from Postgres
- Version submission updates Postgres, no `.vai/head` file write
- Local mode still uses `.vai/head` file
- `cargo test --features full` passes

---

### Issue 4: Eliminate .vai/config.toml dependency in server mode

**Priority:** high
**Depends on:** Issue 2

Since issue 2 provides `repo_id` from the Postgres lookup, `config.toml` is no longer needed in server mode.

**Changes:**
- `repo_id_from_vai_dir()` is no longer called in server mode (repo_id comes from RepoCtx populated by the middleware)
- `POST /api/repos` (create_repo_handler) skips creating `.vai/config.toml` in server mode
- `repo_storage()` no longer needs `vai_dir` in server mode — the Postgres backend is shared and repo_id-scoped

**Acceptance criteria:**
- Server starts with no `.vai/` directories on disk
- Creating a repo via API doesn't touch the filesystem in server mode
- All endpoints work with repo_id from Postgres
- Local mode still creates and reads config.toml
- `cargo test --features full` passes

---

### Issue 5: Simplify Dockerfile and fly.toml — remove all filesystem scaffolding

**Priority:** medium
**Depends on:** Issues 1-4

Update deployment files for stateless operation.

**Dockerfile.server:**
```dockerfile
FROM rust:latest AS builder
WORKDIR /app
RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY migrations ./migrations
RUN cargo build --release --features full

FROM debian:trixie-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/vai /usr/local/bin/vai
EXPOSE 7865
ENTRYPOINT ["vai"]
CMD ["server", "start", "--host", "0.0.0.0"]
```

**fly.toml:** Remove `[mounts]` section (no volume needed).

**Acceptance criteria:**
- `fly deploy` succeeds
- Server starts and connects to Postgres via `DATABASE_URL`
- All API endpoints work
- No volumes, no config files, no entrypoint scripts
- Health check passes

---

### Issue 6: Add integration test for stateless server startup

**Priority:** medium
**Depends on:** Issues 1-4

Add a test that starts the server with only environment variables (no filesystem state) and verifies:

1. Server starts and health check passes
2. Create a repo via API
3. Repo appears in list
4. Create workspace, upload files, submit — version created
5. Head version correct from status endpoint
6. All without any `.vai/` directory existing

**Acceptance criteria:**
- Test passes in `cargo test --features full`
- Validates the full stateless server lifecycle
