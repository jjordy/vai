# PRD 16: Binary Split via Cargo Feature Flags

## Overview

The `vai` binary currently compiles everything — CLI, server, Postgres, S3, auth middleware — into a single 400MB+ binary. Clients running `vai workspace create` don't need server code, and shipping server internals to client machines is a security concern.

Rust's Cargo feature flags solve this with zero code duplication. One crate, conditional compilation.

## Design

### Feature Flags

```toml
[features]
default = ["cli"]
cli = []
server = ["dep:axum", "dep:tower", "dep:tower-http", "dep:utoipa", "dep:utoipa-swagger-ui"]
postgres = ["server", "dep:sqlx"]
s3 = ["server", "dep:aws-sdk-s3", "dep:aws-config"]
full = ["cli", "server", "postgres", "s3"]
```

### Build Targets

| Command | Includes | Use Case |
|---------|----------|----------|
| `cargo build` | CLI only | Agent/developer machines |
| `cargo build --features server` | CLI + server (SQLite) | Local self-hosted |
| `cargo build --features full` | Everything | Production server |

### Code Changes

1. **Wrap server module** — `#[cfg(feature = "server")] mod server;`
2. **Wrap storage backends** — `#[cfg(feature = "postgres")] mod postgres;`, `#[cfg(feature = "s3")] mod s3;`
3. **Wrap CLI server subcommand** — `vai server` only available with `server` feature
4. **Conditional dependencies** — move axum, sqlx, aws-sdk-s3, tower, utoipa to optional deps
5. **Keep shared types unconditional** — storage traits, workspace/issue/version types stay in all builds

### What Stays in All Builds

- CLI commands (workspace, issue, version, merge, diff, graph)
- SQLite storage backend (local mode)
- Event log
- Merge engine + MergeFs trait
- Tree-sitter parsing
- All core types and traits

### What Becomes Server-Only

- `src/server/mod.rs` — all HTTP handlers, routes, middleware
- `src/storage/postgres.rs` — Postgres storage backend
- `src/storage/s3.rs` — S3 file store
- Auth middleware, API key management
- OpenAPI spec generation
- WebSocket event streaming
- Multi-repo management

## Expected Impact

| Metric | Current | CLI-only | Full |
|--------|---------|----------|------|
| Debug binary | ~400MB | ~80MB (est.) | ~400MB |
| Release binary | ~50MB (est.) | ~15MB (est.) | ~50MB |
| Compile time | ~50s | ~15s (est.) | ~50s |
| Dependencies | All | Core only | All |

## Issue Breakdown

1. **Add feature flags to Cargo.toml** — Define `cli`, `server`, `postgres`, `s3`, `full` features. Move heavy dependencies to optional.
2. **Gate server module behind feature flag** — `#[cfg(feature = "server")]` on server mod, server CLI subcommand, and all server-only imports.
3. **Gate Postgres storage behind feature flag** — `#[cfg(feature = "postgres")]` on postgres module and StorageBackend::Server variant.
4. **Gate S3 storage behind feature flag** — `#[cfg(feature = "s3")]` on s3 module and StorageBackend::ServerWithS3 variant.
5. **Verify CLI-only build compiles and passes tests** — `cargo build --no-default-features --features cli && cargo test --no-default-features --features cli`
6. **Update CI to test both feature combinations** — CLI-only and full builds both pass.
7. **Update Dockerfile and deployment scripts** — Server builds use `--features full`.

## Priority

Medium. Not blocking any current work. Do after PRD 13 (storage purity) is complete since the feature gating is easier once the module boundaries are clean.
