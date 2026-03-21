# Phase 1 — Foundation

## Goal

Build the core vai library and CLI that can initialize a repository, parse source code into a semantic graph, manage isolated workspaces, perform semantic-aware merges, and track history via an event log. Local mode only — no server, no networking.

By the end of Phase 1, a single developer with one or more local agents can use vai as their version control system.

---

## PRD 1.1: Repository Initialization & On-Disk Format

### Summary
`vai init` creates a `.vai/` directory that houses all vai metadata — event log, semantic graph, workspace data, version history, and caches.

### Requirements

**Functional:**
- `vai init` creates the `.vai/` directory structure in the current directory
- `vai init` performs an initial parse of all supported source files and builds the first semantic graph snapshot
- `vai init` creates the first event (`RepoInitialized`) in the event log
- A `vai.toml` file is created at the project root for project-level configuration (languages, ignore patterns)
- Running `vai init` in an already-initialized directory produces a clear error

**On-Disk Layout:**
```
project/
├── .vai/
│   ├── config.toml              # repo config
│   ├── head                     # current version pointer
│   ├── event_log/
│   │   ├── 000001.events        # append-only event log segments
│   │   └── index.db             # SQLite index for fast event queries
│   ├── graph/
│   │   ├── snapshot.db          # materialized semantic graph (SQLite)
│   │   └── entities/            # serialized entity data by hash
│   ├── workspaces/              # active workspace directories
│   ├── versions/
│   │   └── v1.toml              # initial version metadata
│   └── cache/
│       └── treesitter/          # cached AST parses
└── vai.toml                     # project-level config
```

**Configuration (`vai.toml`):**
- `languages`: list of languages to parse (default: auto-detect)
- `ignore`: glob patterns to exclude (default: common build artifacts, `.vai/`, `.git/`)

**Non-Functional:**
- `vai init` on a 10,000-file repository should complete in under 30 seconds
- On-disk format must be forward-compatible — older versions of vai should be able to read repos created by newer versions (within the same major version)

### Out of Scope
- Server configuration
- Remote/clone functionality

---

## PRD 1.2: Event Log

### Summary
The event log is vai's source of truth. Every action in the system is recorded as an immutable, append-only event. The current state of the repository is derived by replaying the log.

### Requirements

**Functional:**
- Events are appended to segment files in order
- New segments are created when the current segment exceeds a configurable size threshold (default: 64MB)
- Each event has: `id` (monotonic), `timestamp`, `event_type`, `payload`
- A SQLite index provides fast queries by event type, workspace ID, entity ID, and time range
- The index is rebuildable from the raw event files (crash recovery)

**Event Types (Phase 1):**
```
RepoInitialized { id, name, timestamp }
VersionCreated { version_id, parent_version_id, intent, timestamp }
WorkspaceCreated { id, intent, base_version, timestamp }
WorkspaceSubmitted { id, changes_summary, timestamp }
WorkspaceDiscarded { id, reason, timestamp }
EntityAdded { workspace_id, entity }
EntityModified { workspace_id, entity_id, change_description }
EntityRemoved { workspace_id, entity_id }
FileAdded { workspace_id, path, hash }
FileModified { workspace_id, path, old_hash, new_hash }
FileRemoved { workspace_id, path }
MergeCompleted { workspace_id, new_version_id, auto_resolved_conflicts }
MergeConflictDetected { workspace_id, conflict }
MergeConflictResolved { conflict_id, resolution, resolved_by }
```

**Non-Functional:**
- Event writes must be atomic — no partial events on crash
- Event log must be appendable by concurrent workspaces (file-level locking or per-workspace logs that compact)
- Sub-millisecond append latency

### Out of Scope
- Event streaming over network (Phase 2)
- Compaction or archival strategies

---

## PRD 1.3: Semantic Graph Engine

### Summary
The semantic graph represents the codebase as a graph of language-level entities and their relationships. It is built from tree-sitter ASTs and stored as an in-memory materialized view backed by a SQLite snapshot on disk.

### Requirements

**Functional:**
- Parse source files using tree-sitter for supported languages (Rust, TypeScript, Python)
- Extract entities: functions, methods, classes/structs, traits/interfaces, modules, type definitions, constants
- Extract relationships: calls, imports, inherits/implements, contains (parent-child), depends-on
- Store the graph as a SQLite database on disk, load into memory for queries
- Support incremental updates — when a file changes, reparse only that file and update affected entities/edges
- Provide a query API:
  - Get entity by ID or name
  - Get all entities in a file
  - Get all relationships for an entity (incoming and outgoing)
  - Get the transitive closure of dependencies for an entity (blast radius)
  - Get all entities of a given type

**Entity Schema:**
```
Entity {
    id: Hash,
    kind: Function | Method | Class | Struct | Trait | Interface | Module | Type | Constant,
    name: String,
    qualified_name: String,    // e.g., "auth::AuthService::validate_token"
    file_path: String,
    byte_range: (usize, usize),
    line_range: (usize, usize),
    language: Language,
    parent_entity: Option<Hash>,
}

Relationship {
    source: Hash,
    target: Hash,
    kind: Calls | Imports | Inherits | Implements | Contains | DependsOn,
}
```

**Non-Functional:**
- Graph queries must return in under 10ms for repos up to 100,000 entities
- Initial graph build for a 10,000-file repo should complete in under 30 seconds
- Incremental update for a single file change should complete in under 100ms

**Language Support (Phase 1):**
- Rust: functions, structs, enums, traits, impl blocks, modules, use statements
- TypeScript: functions, classes, interfaces, type aliases, imports, exports
- Python: functions, classes, imports, module-level definitions

### Out of Scope
- Type-level analysis (function signatures, generic parameters)
- Cross-language relationships
- Higher-level concept groupings ("the auth system")

---

## PRD 1.4: Workspace Management

### Summary
A workspace is an isolated environment where changes are made against a snapshot of the codebase. Workspaces track all changes as events and can be submitted for merging or discarded.

### Requirements

**Functional:**
- `vai workspace create --intent "<description>"` creates a new workspace
  - Records the base version (current HEAD)
  - Creates workspace directory under `.vai/workspaces/<id>/`
  - Creates a working copy of the project files in the workspace overlay
  - Records `WorkspaceCreated` event
- `vai workspace list` shows all active workspaces with their intents and status
- `vai workspace switch <id>` switches the working directory to use a workspace's overlay
- `vai workspace diff` shows changes in the current workspace (both file-level and entity-level)
- `vai workspace submit` submits the workspace for merging
  - Captures all file changes
  - Rebuilds semantic graph for changed files
  - Computes entity-level diff (added/modified/removed entities)
  - Records `WorkspaceSubmitted` event with changes
  - Triggers merge process
- `vai workspace discard <id>` discards a workspace and cleans up files
  - Records `WorkspaceDiscarded` event

**Workspace Directory Structure:**
```
.vai/workspaces/<id>/
├── meta.toml          # intent, base_version, status, created_at
├── overlay/           # copy-on-write changed files
└── events.log         # workspace-local event log
```

**Workspace States:**
```
Created → Active → Submitted → Merging → Merged
                              → Conflict → Resolved → Merged
              → Discarded
```

**Non-Functional:**
- Workspace creation should complete in under 5 seconds for repos up to 100,000 files
- Multiple workspaces can exist simultaneously
- Workspace overlay should only store changed files, not the entire repo

### Out of Scope
- Remote workspace sync (Phase 2)
- Conflict detection between workspaces during active work (Phase 2)
- Scope inference from intent text (Phase 4)

---

## PRD 1.5: Semantic Merge Engine

### Summary
The merge engine integrates workspace changes back into the main version. Unlike git's line-based merge, vai's merge engine operates at three levels: textual, structural (AST), and referential (semantic graph edges). It auto-resolves non-conflicting changes and flags true semantic conflicts.

### Requirements

**Functional:**
- When a workspace is submitted, the merge engine compares workspace changes against the current HEAD version
- If HEAD has not advanced since the workspace was created (no concurrent changes), fast-forward merge — apply changes directly
- If HEAD has advanced, perform three-level merge analysis:

**Level 1 — Textual:**
- Did the workspace and HEAD changes touch the same lines in the same file?
- If no: auto-merge (interleave changes)
- If yes: proceed to Level 2

**Level 2 — Structural (AST):**
- Did the changes modify the same AST nodes (functions, classes, statements)?
- If changes are in different AST nodes within the same file: auto-merge by combining AST-level changes
- If same AST node: proceed to Level 3

**Level 3 — Referential (Semantic Graph):**
- Does one change reference or depend on something the other change modified?
- Examples:
  - Change A renames an identifier, Change B uses the old name → CONFLICT
  - Change A modifies a function signature, Change B calls that function → CONFLICT
  - Change A adds a new function, Change B adds a different new function to the same file → NO CONFLICT
- If referential conflict detected: flag as merge conflict

**Conflict Resolution Flow:**
- Auto-resolved merges record `MergeCompleted` with resolution details
- Conflicts record `MergeConflictDetected` with:
  - Which entities conflict
  - What each side changed
  - The semantic relationship that creates the conflict
  - Severity assessment (low/medium/high)
- For Phase 1 (local only): conflicts are presented to the user via CLI for manual resolution
- `vai merge resolve <conflict_id>` marks a conflict as resolved after the user fixes it

**Output:**
- `vai merge status` shows pending merges and conflicts
- Successful merge creates a new version with the intent from the workspace

**Non-Functional:**
- Merge analysis for a workspace with 50 changed files should complete in under 10 seconds
- Auto-resolved merges should not produce code that fails to parse (validated by re-parsing merged output)

### Out of Scope
- Sending conflicts back to agents automatically (Phase 2)
- Human escalation workflow (Phase 3)
- Type/contract level analysis — Level 4 (future)

---

## PRD 1.6: Version History & Rollback

### Summary
Versions are labeled states of the codebase after successful merges. The version history is a linear sequence of intent-labeled states (not a DAG like git). Rollback allows reverting to previous versions with full impact analysis.

### Requirements

**Functional:**
- `vai log` displays version history:
  ```
  v3  "add rate limiting to auth"    2h ago
  v2  "fix token expiry check"       5h ago
  v1  "initial repository"           1d ago
  ```
- Each version stores:
  - Version ID (monotonic)
  - Parent version ID
  - Intent description
  - Agent/user who performed the work
  - Timestamp
  - Pointer to the merge event in the event log
- `vai show <version>` displays the entity-level changes in a version (what was added/modified/removed)
- `vai rollback <version>` reverts the codebase to a previous version:
  - Performs impact analysis: identifies downstream versions that depend on changes being rolled back
  - Displays impact analysis to the user with risk assessment
  - On confirmation: generates inverse events and applies them, creating a new version
  - Supports surgical rollback: `vai rollback <version> --entity <entity_name>` to revert specific entity changes only
- `vai diff <version_a> <version_b>` shows semantic diff between two versions

**Non-Functional:**
- `vai log` should return instantly for repos with up to 10,000 versions
- Impact analysis should complete in under 5 seconds

### Out of Scope
- Branching (vai uses a linear version history with parallel workspaces instead)
- Version signing or verification

---

## PRD 1.7: CLI Interface

### Summary
The vai CLI is the primary interface for interacting with vai. It should be fast, informative, and feel familiar to git users while introducing vai-specific concepts.

### Requirements

**Commands (Phase 1):**

| Command | Description |
|---------|-------------|
| `vai init` | Initialize a new vai repository |
| `vai status` | Show repository status, active workspaces, graph stats |
| `vai workspace create --intent "<text>"` | Create a new workspace |
| `vai workspace list` | List all active workspaces |
| `vai workspace switch <id>` | Switch to a workspace |
| `vai workspace diff` | Show changes in current workspace |
| `vai workspace submit` | Submit workspace for merging |
| `vai workspace discard <id>` | Discard a workspace |
| `vai merge status` | Show pending merges and conflicts |
| `vai merge resolve <conflict_id>` | Mark a conflict as resolved |
| `vai log` | Show version history |
| `vai show <version>` | Show details of a version |
| `vai rollback <version>` | Revert to a previous version |
| `vai diff <a> <b>` | Semantic diff between versions |
| `vai graph query <entity>` | Query the semantic graph |
| `vai graph show` | Display graph statistics |

**UX Requirements:**
- Colored terminal output with clear visual hierarchy
- Progress bars for long operations (init, merge)
- Confirmation prompts for destructive operations (rollback, discard)
- `--json` flag on all commands for machine-readable output (agent consumption)
- `--quiet` flag to suppress non-essential output
- Helpful error messages that suggest the correct command

**Non-Functional:**
- CLI startup time under 50ms
- `--json` output must be stable and parseable — this is the agent API in local mode

### Out of Scope
- Server commands (Phase 2)
- Issue commands (Phase 3)
- TUI dashboard (Phase 4)
