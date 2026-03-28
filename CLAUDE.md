# vai — Development Guidelines

## What is vai?

vai is a version control system built for AI agents. See `docs/prds/00-overview.md` for the full architecture.

## Project Structure

```
vai/
├── docs/prds/           # Product requirements documents
├── .sandcastle/          # Docker-based autonomous agent setup
├── migrations/           # SQL migrations for Postgres (sqlx)
├── src/
│   ├── main.rs           # CLI entrypoint
│   ├── lib.rs            # Library root
│   ├── cli/              # CLI command handlers (clap)
│   ├── server/           # HTTP server (axum), REST + WebSocket handlers
│   ├── storage/          # Storage traits + implementations (SQLite, Postgres, S3)
│   ├── event_log/        # Event log storage and querying
│   ├── graph/            # Semantic graph engine
│   ├── workspace/        # Workspace management
│   ├── merge/            # Semantic merge engine
│   ├── merge_fs.rs       # MergeFs trait + DiskMergeFs/S3MergeFs implementations
│   ├── diff/             # File and entity diff computation
│   ├── version/          # Version history and rollback
│   ├── issue/            # Issue CRUD, comments, links
│   ├── escalation/       # Escalation management
│   ├── work_queue/       # Work queue with scope prediction
│   ├── auth/             # API key authentication
│   └── watcher/          # External system watchers
└── tests/                # Integration tests (including Postgres E2E)
```

## Conventions

### Code Style
- Idiomatic Rust — follow standard Rust conventions
- Use `thiserror` for error types, one error enum per module
- Use `serde` with `Serialize`/`Deserialize` for all data types that touch disk or network
- Use `clap` derive API for CLI
- Public API types and functions get doc comments
- Module-level doc comments explaining the module's purpose

### Architecture
- **Vertical slices**: each module owns its types, logic, and storage. Minimize cross-module dependencies.
- **Clean API boundaries**: modules expose a public API through their `mod.rs`. Internal types stay private.
- **Error propagation**: use `Result<T, E>` everywhere. No panics except in tests.
- **Testing**: unit tests in the module file, integration tests in `tests/`.
- **Storage traits**: all data operations go through traits in `src/storage/mod.rs`. Implementations: `SqliteStorage` (local), `PostgresStorage` (server), `S3FileStore` (server files).
- **Server mode = S3 + Postgres only**: no filesystem fallbacks in server handlers. If a storage trait method isn't implemented, fail loudly. Never use `std::fs::read/write` in server handlers for repo data.
- **MergeFs abstraction**: the merge/diff engines use `&dyn MergeFs` — `DiskMergeFs` for local mode, `S3MergeFs` for server mode. Never call `std::fs` directly from merge/diff code.
- **OpenAPI**: all server endpoints must have `#[utoipa::path]` annotations and all request/response types must derive `ToSchema`. The web dashboard auto-generates its API client from `GET /api/openapi.json`.

### Git
- Commit messages follow conventional format: `type: description`
- Autonomous agent commits are prefixed with `RALPH:`
- Keep commits small and focused

### Build Modes

- `cargo build` — CLI-only binary (default features: `cli`). No server, Postgres, or S3 dependencies.
- `cargo build --features full` — Full server binary with CLI, HTTP server, Postgres, and S3 support.
- `cargo build --release --features full` — Production server binary (use this for Docker/server deployments).

CI runs both: `cargo test` (CLI-only) and `cargo test --features full` (server with Postgres).

### Dependencies (approved)
- `clap` — CLI framework
- `serde`, `serde_json`, `toml` — serialization
- `thiserror` — error types
- `rusqlite` — SQLite for local mode storage
- `sqlx` — Postgres for server mode storage
- `axum`, `tower`, `tower-http` — HTTP server framework
- `utoipa`, `utoipa-swagger-ui` — OpenAPI spec generation
- `aws-sdk-s3`, `aws-config` — S3-compatible object storage
- `tree-sitter`, `tree-sitter-rust` — AST parsing (add `tree-sitter-typescript`, `tree-sitter-python` when needed)
- `sha2` — content hashing
- `chrono` — timestamps
- `uuid` — entity and workspace IDs
- `colored` — terminal output
- `indicatif` — progress bars
- `flate2`, `tar` — tarball creation for file download/upload
- `tokio` — async runtime
- `async-trait` — async trait support

Do not add dependencies outside this list without justification in the commit message.

### Security

Run `cargo audit` periodically to check for known vulnerabilities in dependencies. CI automatically runs `cargo audit --deny warnings` and fails the build on any advisory. If you add a new dependency, run `cargo audit` locally before committing.
