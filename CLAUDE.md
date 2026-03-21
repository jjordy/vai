# vai — Development Guidelines

## What is vai?

vai is a version control system built for AI agents. See `docs/prds/00-overview.md` for the full architecture.

## Project Structure

```
vai/
├── docs/prds/           # Product requirements documents
├── .sandcastle/          # Docker-based autonomous agent setup
├── src/
│   ├── main.rs           # CLI entrypoint
│   ├── lib.rs            # Library root
│   ├── cli/              # CLI command handlers (clap)
│   ├── event_log/        # Event log storage and querying
│   ├── graph/            # Semantic graph engine
│   ├── workspace/        # Workspace management
│   ├── merge/            # Semantic merge engine
│   └── version/          # Version history and rollback
└── tests/                # Integration tests
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

### Git
- Commit messages follow conventional format: `type: description`
- Autonomous agent commits are prefixed with `RALPH:`
- Keep commits small and focused

### Dependencies (approved)
- `clap` — CLI framework
- `serde`, `serde_json`, `toml` — serialization
- `thiserror` — error types
- `rusqlite` — SQLite for event log index and graph snapshots
- `tree-sitter`, `tree-sitter-rust` — AST parsing (add `tree-sitter-typescript`, `tree-sitter-python` when needed)
- `sha2` — content hashing
- `chrono` — timestamps
- `uuid` — entity and workspace IDs
- `colored` — terminal output
- `indicatif` — progress bars

Do not add dependencies outside this list without justification in the commit message.
