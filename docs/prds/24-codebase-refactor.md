# PRD: Codebase Refactor — Split Large Files

## Status

Proposed

## Context

Several files have grown unwieldy, making them hard for both humans and agents to work with effectively. The server module alone is 15,933 lines — 31.6% of the entire codebase in a single file, with 81 handler functions and 109 struct definitions.

This PRD systematically splits large files into smaller, focused modules while preserving all existing behavior. No functional changes — purely structural.

## Scope

### vai server (Rust)

| File | Lines | Action |
|------|-------|--------|
| `src/server/mod.rs` | 15,933 | Split into 10 submodules |
| `src/cli/mod.rs` | 3,621 | Split into subcommand groups |
| `src/storage/postgres.rs` | 3,180 | Split by trait implementation |

### vai-dashboard (TypeScript)

| File | Lines | Action |
|------|-------|--------|
| `src/hooks/use-vai.ts` | 1,618 | Split by resource group |
| `src/lib/vai-api.ts` | 844 | Split to match hooks |

### Out of Scope

- Generated code (`src/generated/`) — auto-generated, don't touch
- `IssueActivityTimeline.tsx` (787 lines) — component decomposition, separate concern
- Hook architecture consolidation (orval vs hand-written) — separate follow-up PRD
- `graph/mod.rs` (2,074) and `agent/mod.rs` (2,249) — borderline, defer

## Design Principles

1. **No functional changes** — pure file reorganization
2. **Preserve public API** — re-export everything from the parent module so external callers don't break
3. **One module at a time** — sequential extraction, each verified independently
4. **Smallest first** — start with the simplest extraction to establish the pattern

## Server Split Plan

### Target Structure

```
src/server/
├── mod.rs              # Router setup, AppState, shared types, middleware (~3,000 lines)
├── pagination.rs       # Already exists (119 lines)
├── workspace.rs        # Workspace CRUD + file operations
├── version.rs          # Version list, detail, diff, rollback
├── issue.rs            # Issue CRUD + comments + links + attachments
├── escalation.rs       # Escalation list, detail, resolve
├── work_queue.rs       # Work queue + claim
├── graph.rs            # Graph entities, blast radius, refresh
├── auth.rs             # Token exchange, refresh, revoke, middleware
├── admin.rs            # Repos, users, orgs, collaborators, API keys
├── watcher.rs          # Watcher registration + discoveries
└── ws.rs               # WebSocket events handler
```

### What stays in mod.rs

- `AppState` struct and initialization
- `AgentIdentity`, `AuthSource`, `RepoCtx` — shared across all handlers
- `ApiError` enum and error handling
- `ServerConfig` and server startup
- Rate limiting middleware and types
- Auth middleware (extracts identity, used by all routes)
- Router construction (`build_router`) — imports handlers from submodules
- OpenAPI schema registration
- Shared helper functions used by multiple handler groups

### What moves to each submodule

Each submodule gets:
- Its handler functions (`async fn xxx_handler`)
- Its request/response structs (e.g. `CreateIssueRequest`, `IssueResponse`)
- Its `#[utoipa::path]` annotations
- Helper functions only used by that handler group
- Tests specific to those handlers

## CLI Split Plan

### Target Structure

```
src/cli/
├── mod.rs              # Clap derive structs, main dispatch (~800 lines)
├── workspace.rs        # Workspace commands
├── version.rs          # Version commands
├── issue.rs            # Issue commands
├── escalation.rs       # Escalation commands
├── graph.rs            # Graph commands
├── remote.rs           # Remote config commands
├── migration.rs        # Migration commands
└── server_cmd.rs       # Server start command
```

## Storage Split Plan

### Target Structure

```
src/storage/
├── mod.rs              # Traits + shared types (keep as-is)
├── pagination.rs       # Already exists
├── sqlite.rs           # Keep as-is (1,665 lines — manageable)
├── s3.rs               # Keep as-is (366 lines)
├── filesystem.rs       # Keep as-is (272 lines)
├── postgres/
│   ├��─ mod.rs          # PostgresStorage struct + pool init
│   ├── workspace.rs    # WorkspaceStore impl
│   ├── version.rs      # VersionStore impl
│   ├── issue.rs        # IssueStore + CommentStore + LinkStore + AttachmentStore impl
│   ├── event.rs        # EventStore impl
│   ├── graph.rs        # GraphStore impl
│   ├── escalation.rs   # EscalationStore impl
│   └── auth.rs         # AuthStore impl
```

## Dashboard Split Plan

### Target Structure

```
src/hooks/
├── use-vai.ts          # Re-exports + shared types (QueryKeys, stale times)
├── use-workspaces.ts   # Workspace hooks
├── use-versions.ts     # Version hooks
├── use-issues.ts       # Issue + comment + attachment hooks
├── use-escalations.ts  # Escalation hooks
├── use-graph.ts        # Graph hooks
├── use-work-queue.ts   # Work queue hooks
├── use-status.ts       # Status + repos hooks
├── use-pagination.ts   # Already exists
├── use-events.ts       # Already exists
├── use-rbac.ts         # Already exists
├── use-notifications.ts # Already exists
└── use-force-layout.ts # Already exists

src/lib/
├── vai-api.ts          # Re-exports + shared request helper
├── vai-api-issues.ts   # Issue API functions
├── vai-api-workspaces.ts # Workspace API functions
├── vai-api-versions.ts # Version API functions
├── vai-api-admin.ts    # Repos, keys, orgs, collaborators
└── ... (existing files unchanged)
```

## Issue Breakdown

Issues are ordered sequentially — each depends on the previous to avoid merge conflicts on mod.rs.

### Server Extraction (Issues 1-10)

#### Issue 1: Extract server/escalation.rs (smallest — proof of concept)

**Priority:** high
**Blocks:** All other server extractions

Move escalation handlers + types from `src/server/mod.rs` to `src/server/escalation.rs`.

**Move:**
- `list_escalations_handler`, `get_escalation_handler`, `resolve_escalation_handler`
- `ListEscalationsQuery`, `EscalationResponse`, `ResolveEscalationRequest`
- Related `#[utoipa::path]` annotations

**Keep in mod.rs:**
- Re-export handler functions for router wiring
- Import in `build_router` unchanged

**Acceptance criteria:**
- `cargo test --features full` passes
- `cargo clippy --features full` clean
- Router still works — all escalation endpoints functional
- OpenAPI spec unchanged

---

#### Issue 2: Extract server/work_queue.rs

**Priority:** high
**Depends on:** Issue 1

Move work queue handlers: `get_work_queue_handler`, `claim_work_handler` + their types.

---

#### Issue 3: Extract server/version.rs

**Priority:** high
**Depends on:** Issue 2

Move version handlers: `list_versions_handler`, `get_version_handler`, `get_version_diff_handler`, `rollback_handler` + types.

---

#### Issue 4: Extract server/graph.rs

**Priority:** high
**Depends on:** Issue 3

Move graph handlers: `list_graph_entities_handler`, `get_graph_entity_handler`, `get_entity_deps_handler`, `get_blast_radius_handler`, `server_graph_refresh_handler` + types.

---

#### Issue 5: Extract server/watcher.rs

**Priority:** high
**Depends on:** Issue 4

Move watcher handlers: `register_watcher_handler`, `list_watchers_handler`, `pause_watcher_handler`, `resume_watcher_handler`, `submit_discovery_handler` + types.

---

#### Issue 6: Extract server/ws.rs

**Priority:** high
**Depends on:** Issue 5

Move WebSocket handler: `ws_events_handler`, `ClientMessage`, `SubscriptionFilter`, broadcast logic.

---

#### Issue 7: Extract server/workspace.rs

**Priority:** high
**Depends on:** Issue 6

Move workspace handlers: create, list, get, submit, discard, upload files, upload snapshot, get file + types. This is the largest extraction (~2,000 lines).

---

#### Issue 8: Extract server/issue.rs

**Priority:** high
**Depends on:** Issue 7

Move issue handlers: create, list, get, update, close + comments (create, list, edit, delete) + links (create, list, delete) + attachments (upload, list, download, delete) + all types. Largest extraction (~3,000 lines).

---

#### Issue 9: Extract server/auth.rs

**Priority:** high
**Depends on:** Issue 8

Move auth handlers: `token_exchange_handler`, `refresh_token_handler`, `revoke_token_handler` + types. Auth middleware stays in mod.rs (used by the router layer).

---

#### Issue 10: Extract server/admin.rs

**Priority:** high
**Depends on:** Issue 9

Move admin handlers: repos (create, list), users (create, get), orgs (create, list, get, delete, members), collaborators (add, list, update, remove), API keys (create, list, revoke, bulk revoke) + types.

After this, mod.rs should be ~3,000 lines: router setup, shared types, middleware, AppState.

---

### CLI Extraction (Issue 11)

#### Issue 11: Split cli/mod.rs into subcommand modules

**Priority:** medium
**Depends on:** Issue 10 (to avoid simultaneous mod.rs churn)

Split the CLI command handlers into separate files by subcommand group. The Clap derive structs and main dispatch stay in mod.rs.

**Acceptance criteria:**
- `cargo test` passes
- `cargo clippy` clean
- All CLI commands work unchanged

---

### Storage Extraction (Issue 12)

#### Issue 12: Split storage/postgres.rs into trait implementation modules

**Priority:** medium
**Depends on:** Issue 11

Split Postgres storage into `src/storage/postgres/` directory with one file per trait implementation.

**Acceptance criteria:**
- `cargo test --features full` passes
- `cargo clippy --features full` clean
- All storage operations work unchanged

---

### Dashboard Extraction (Issues 13-14)

#### Issue 13: Split hooks/use-vai.ts into resource-specific hook files

**Priority:** medium

Split the 1,618-line hooks file into separate files by resource group. Re-export everything from use-vai.ts so existing imports don't break.

**Acceptance criteria:**
- `pnpm test` passes
- `pnpm test:e2e` passes
- `npx tsc --noEmit` clean
- Existing imports from `use-vai` still work via re-exports

---

#### Issue 14: Split lib/vai-api.ts into resource-specific API files

**Priority:** medium

Split the 844-line API file into separate files. Re-export from vai-api.ts.

**Acceptance criteria:**
- `pnpm test` passes
- `npx tsc --noEmit` clean

---

## Follow-Up PRDs

- **Hook Architecture Consolidation** — decide whether to use orval-generated hooks everywhere with hand-written extensions, or keep the current hand-written approach. Currently two competing hook systems exist.

## Risk Mitigation

- Sequential execution prevents merge conflicts
- Re-exports from parent modules preserve backward compatibility
- Each extraction is independently verified with full test suite
- Smallest module first establishes the pattern before tackling large ones
- No functional changes — pure structural refactoring
