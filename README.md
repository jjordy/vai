# vai

Version control built for AI agents.

vai replaces git's human-centric model (branches, commits, text diffs) with an agent-native model (intents, workspaces, semantic graphs) designed for massive parallelism — 50 to 5000+ agents working on a single codebase simultaneously.

## Why?

Git was designed for human developers. As AI agents become primary contributors to codebases, git's model breaks down:

- **Merge conflicts** require human judgment that agents handle poorly
- **Line-based diffs** miss semantic meaning — agents think in ASTs, not line numbers
- **No coordination** — agents can't see what other agents are working on
- **No intent tracking** — git stores *what* changed but not *why*
- **Branching doesn't scale** — merging 500 branches is combinatorial hell

vai solves this with a semantic-aware version control system where the human oversees intents, not diffs.

## Core Concepts

- **Intent** — the fundamental unit of work. "Add rate limiting to auth" instead of a commit hash.
- **Semantic Graph** — the codebase as a graph of functions, structs, traits, and their relationships. Built from tree-sitter ASTs.
- **Workspace** — an isolated environment where an agent works. Agents get full checkouts and can build/test locally.
- **Version** — a labeled state of the codebase after a successful merge. History reads as a sequence of completed intents.
- **Semantic Merge** — three-level merge analysis (textual, structural, referential) that auto-resolves what git can't.

## Quick Start

```bash
# Build from source
cargo build --release

# Initialize a repository
vai init

# Check status
vai status

# Create a workspace
vai workspace create --intent "add rate limiting to auth"

# Make changes, then check the diff
vai workspace diff

# Submit changes (triggers semantic merge)
vai workspace submit

# View version history
vai log

# Show what changed in a version
vai show v2

# Rollback with impact analysis
vai rollback v2
```

## Server Mode

vai runs as a central coordination server for multi-agent workflows:

```bash
# Start the server
vai server start --port 7832

# Create API keys for agents
vai server keys create --name "agent-alpha"

# Agents clone and work remotely
vai clone vai://localhost:7832/myproject
vai workspace create --intent "fix auth bug"
# ... make changes ...
vai workspace submit

# See what other agents are doing
vai status --others
```

The server provides:
- REST API for workspace management, graph queries, and version history
- WebSocket streaming for real-time event notifications
- Conflict engine that detects overlapping work and classifies severity
- API key authentication with per-agent identity

## Semantic Merge

vai's merge engine operates at three levels, going beyond git's line-based approach:

| Level | What it checks | Example |
|-------|---------------|---------|
| **Textual** | Same lines touched? | Two agents edit different parts of a file — auto-merge |
| **Structural** | Same AST nodes touched? | Two agents add different functions to the same file — auto-merge |
| **Referential** | Does one change reference something the other modified? | Agent A renames a variable, Agent B uses the old name — conflict |

When conflicts are detected, they're sent back to an involved agent with full context. Only severe conflicts escalate to the human.

## Architecture

```
┌─────────────────────────────────────────────┐
│              Human (TUI/CLI)                 │
│  Reviews intents, resolves escalations       │
└──────────────────────┬──────────────────────┘
                       │
┌──────────────────────▼──────────────────────┐
│               vai Server                     │
│                                              │
│  Intent Registry · Workspace Manager         │
│  Conflict Engine · Semantic Graph Engine     │
│  Event Log · Merge Engine                    │
└──────────────────────┬──────────────────────┘
                       │
        ┌──────────────┼──────────────┐
   ┌────▼────┐   ┌────▼────┐   ┌────▼────┐
   │ Agent 1  │   │ Agent 2  │   │ Agent N  │
   └──────────┘   └──────────┘   └──────────┘
```

- **Local mode**: core library runs embedded, single-machine use
- **Server mode**: REST + WebSocket API, agents connect remotely
- Same core library powers both modes

## Language Support

vai uses [tree-sitter](https://tree-sitter.github.io/) for parsing. Currently supported:

- **Rust** — functions, structs, enums, traits, impl blocks, modules, use statements

TypeScript and Python support planned.

## Project Status

vai is in active development. Phase 1 (foundation) and Phase 2 (coordination) are complete:

- [x] Repository initialization and on-disk format
- [x] Append-only event log with SQLite indexing
- [x] Semantic graph engine (tree-sitter)
- [x] Workspace management (create, diff, submit, discard)
- [x] Three-level semantic merge engine
- [x] Version history with rollback and impact analysis
- [x] HTTP/WebSocket server with REST API
- [x] API key authentication
- [x] Remote clone, sync, and workspace workflow
- [x] Conflict engine with overlap detection
- [x] Real-time event streaming with buffering
- [ ] Issue tracking system (Phase 3)
- [ ] Smart work queue for orchestrators (Phase 3)
- [ ] Automatic scope inference (Phase 4)
- [ ] TUI dashboard (Phase 4)

## Local Development Setup (Postgres + MinIO)

The hosted server backend requires Postgres and an S3-compatible object store.
A `docker-compose.yml` at the repository root starts both with a single command.

```bash
# Start Postgres and MinIO in the background
docker compose up -d

# Verify both containers are healthy
docker compose ps
```

Default connection details:

| Service | URL / address | Credentials |
|---------|--------------|-------------|
| Postgres | `localhost:5432` db `vai` | user `vai` / password `vai` |
| MinIO S3 API | `http://localhost:9000` | key `vaidev` / secret `vaidevpass123` |
| MinIO console | `http://localhost:9001` | key `vaidev` / secret `vaidevpass123` |

Set `DATABASE_URL` before starting the server in Postgres mode:

```bash
export DATABASE_URL=postgres://vai:vai@localhost:5432/vai
vai server start
```

Stop and clean up:

```bash
docker compose down        # stop containers, keep volumes
docker compose down -v     # stop containers AND delete all data
```

## Development

vai is written in Rust and structured for AI-assisted development (vertical slices, clean API boundaries).

```bash
# Build
cargo build

# Test
cargo test

# Lint
cargo clippy
```

See [CLAUDE.md](CLAUDE.md) for development conventions and [docs/prds/](docs/prds/) for product requirements.

## License

TBD
