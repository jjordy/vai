# vai — Ubiquitous Language

Canonical terminology for the vai project. Every term has one definition. Use these terms in code, docs, issues, and conversation.

---

## Core Concepts

| Term | Definition | Aliases to avoid |
|------|-----------|-----------------|
| **Intent** | A natural-language description of what an agent is trying to accomplish, serving as the fundamental unit of work. | goal, objective, task description, commit message |
| **Workspace** | An isolated environment where an agent makes changes against a snapshot of the codebase, tracked as events. | sandbox, branch, working copy, checkout |
| **Version** | A labeled state of the main codebase after a successful merge, identified by a monotonic ID (v1, v2, ...). | commit, revision, snapshot, checkpoint |
| **Semantic Graph** | The codebase represented as a graph of language-level entities and their relationships, built from tree-sitter ASTs and stored in the database (SQLite locally, Postgres on server). | code graph, dependency graph, symbol table |
| **Event Log** | Append-only, immutable record of every action in the system; the source of truth from which all state is derived. Stored as NDJSON segments locally or as Postgres rows on the server. | transaction log, audit log, changelog |
| **Repository** | A vai-initialized directory containing a `.vai/` metadata directory and a `vai.toml` config file. On the hosted platform, a repo is registered under an **Organization** and stored on the server. | repo, project |

## Workspace Lifecycle

| Term | Definition | Aliases to avoid |
|------|-----------|-----------------|
| **Created** | Workspace exists but no files have been modified yet. | initialized, new |
| **Active** | Agent has started making changes in the workspace overlay. | in-progress, working |
| **Submitted** | Workspace has been sent to the merge engine for integration. | pushed, sent |
| **Merged** | Workspace changes have been successfully integrated into a new version. | completed, closed, done |
| **Discarded** | Workspace was abandoned without merging. | deleted, cancelled, dropped |
| **Overlay** | The copy-on-write directory within a workspace that stores only the files the agent has changed. | working tree, changeset, patch |

## Semantic Graph

| Term | Definition | Aliases to avoid |
|------|-----------|-----------------|
| **Entity** | A language-level element in the semantic graph (function, struct, class, component, etc.) identified by a stable hash of its qualified name. | symbol, definition, node, declaration |
| **Entity Kind** | Classification of an entity's type: Function, Method, Struct, Enum, Trait, Impl, Module, UseStatement (Rust); Class, Interface, TypeAlias, Component, Hook, ExportStatement (TypeScript). | entity type, node type |
| **Qualified Name** | The full hierarchical name of an entity including its scope (e.g., `auth::AuthService::validate_token`). | full name, scoped name, path |
| **Relationship** | A directed connection between two entities in the semantic graph. | edge, dependency, link |
| **Relationship Kind** | Classification of a relationship: Contains, Calls, Imports, Implements, Extends. | edge type, connection type |
| **Blast Radius** | The transitive closure of semantic dependencies for an entity or set of entities. | impact scope, dependency chain, ripple effect |
| **Snapshot** | The materialized view of the semantic graph persisted as a SQLite database at `.vai/graph/snapshot.db`. | graph database, graph cache |
| **Graph Refresh** | Re-scanning all source files and rebuilding the semantic graph snapshot. | graph rebuild, reparse, reindex |

## Merge Engine

| Term | Definition | Aliases to avoid |
|------|-----------|-----------------|
| **Fast-Forward Merge** | A merge where HEAD has not advanced since the workspace was created, so changes are applied directly. | direct merge, clean merge |
| **Semantic Merge** | A three-level merge performed when HEAD has advanced past the workspace's base version. | three-way merge, conflict merge |
| **Level 1 (Textual)** | First merge level: checks whether workspace and HEAD changes touch the same lines in the same file. | line-level merge |
| **Level 2 (Structural)** | Second merge level: checks whether changes modify the same AST entities even if lines overlap. | AST merge, entity-level merge |
| **Level 3 (Referential)** | Third merge level: checks whether one change references or depends on what the other modified; produces conflicts if unresolvable. | semantic merge, dependency merge |
| **Conflict** | Incompatible changes detected between a workspace and HEAD during merge that cannot be auto-resolved. | collision, clash, incompatibility |
| **Conflict Severity** | Assessment of a conflict's impact: Low, Medium, High, or Critical. | conflict level, conflict priority |

## Merge Intelligence

| Term | Definition | Aliases to avoid |
|------|-----------|-----------------|
| **Merge Pattern** | A learned recurring pattern of overlapping changes with an associated resolution strategy and success rate. | conflict pattern, resolution template |
| **Pattern Library** | The collection of all learned merge patterns, stored in SQLite with success rates and instance counts. | pattern store, resolution database |
| **Auto-Promotion** | Automatic enablement of a merge pattern for auto-resolution after it exceeds 90% success rate with 10+ instances. | auto-enable, pattern graduation |
| **Demotion** | Disabling a previously promoted merge pattern after a rollback indicates the resolution was incorrect. | auto-disable, pattern degradation |

## Conflict Detection

| Term | Definition | Aliases to avoid |
|------|-----------|-----------------|
| **Conflict Engine** | The system that monitors active workspace scope footprints and detects overlapping work in real time. | overlap detector, conflict monitor |
| **Scope Footprint** | The set of semantic entities a workspace has read or modified, maintained by the conflict engine. | work scope, touched entities, scope signature |
| **Overlap** | The intersection of scope footprints between two or more active workspaces. | conflict area, shared scope |
| **Overlap Level** | Classification of overlap severity: None, Low (same file, different entities), Medium (same entity, different aspects), High (same entity with dependencies), Critical (contradictory intents). | overlap severity |

## Scope Inference

| Term | Definition | Aliases to avoid |
|------|-----------|-----------------|
| **Scope Inference** | Automatic prediction of which semantic entities will be affected by a natural-language intent, using keyword extraction and graph traversal. | scope prediction, impact prediction |
| **Scope Prediction** | The output of scope inference: a set of predicted entities with confidence levels (High, Medium, Low). | predicted scope, estimated scope |
| **Scope History** | Historical record of past intents and the entities they actually touched, used to improve future predictions. | prediction history, learning data |
| **Scope Accuracy** | Measured correctness of scope predictions via recall (% of actual entities predicted) and precision (% of predictions that were correct). | prediction accuracy |

## Issues

| Term | Definition | Aliases to avoid |
|------|-----------|-----------------|
| **Issue** | A unit of work that flows through the pipeline: creation → assignment → workspace → merge. | task, ticket, work item, story |
| **Issue Status** | Current state of an issue: Open, InProgress, Resolved, or Closed. | issue state, issue phase |
| **Priority** | Importance ranking for an issue: Critical, High, Medium, or Low. | severity, urgency |
| **Label** | A categorical tag applied to an issue for filtering and classification. | tag, category |
| **Resolution** | The reason an issue was closed (e.g., "resolved", "won't fix", "duplicate"). | close reason, outcome |
| **Issue Link** | A directional relationship between two issues: Blocks, RelatesTo, or Duplicates. The single mechanism for both informational relationships and work queue blocking. | dependency, relation, reference |
| **Blocks** (link type) | Issue A blocks issue B — B is unavailable in the work queue until A is closed. Replaces the legacy `depends_on` field. | depends on, blocked by |
| **Acceptance Criteria** | A list of testable conditions on an issue that define "done." Helps agents verify completeness and helps the work queue prioritize well-specified issues. | definition of done, checklist, requirements |
| **Issue Comment** | A timestamped message on an issue with `author_type` (human or agent) and `author_id`. Supports markdown. Part of the activity timeline. | note, reply, message |
| **Issue Attachment** | A file attached to an issue, stored in S3 at `issues/{id}/attachments/{filename}`. Agents can download attachments for context (screenshots, logs, designs). | file, upload, artifact |
| **Activity Timeline** | Chronological interleaving of comments and system events on an issue detail page. | history, feed, log |

## Work Queue

| Term | Definition | Aliases to avoid |
|------|-----------|-----------------|
| **Work Queue** | The system that returns issues safe to work on in parallel, ranked by priority and filtered by conflict analysis against active workspaces. | task queue, job queue, backlog |
| **Available Work** | Issues in the work queue with no predicted scope conflicts against any active workspace. | ready work, unblocked work |
| **Blocked Work** | Issues in the work queue that cannot be safely started due to predicted conflicts with active workspaces, with blocking reasons listed. | queued work, waiting work |
| **Claim** | The atomic operation of marking an issue as in-progress and creating a linked workspace for it. | assign, take, pick up |
| **Orchestrator** | An external system that queries the work queue, claims issues, and assigns them to agents; vai does not provide an orchestrator. | scheduler, dispatcher, coordinator |

## Escalations

| Term | Definition | Aliases to avoid |
|------|-----------|-----------------|
| **Escalation** | A request for human judgment when conflicts are too severe for automated resolution. | alert, incident, human review |
| **Escalation Type** | Category of escalation: MergeConflict, IntentConflict, ReviewRequest, or ValidationFailure. | escalation category |
| **Resolution Option** | A suggested resolution path auto-generated from conflict context (e.g., "keep Agent A's changes"). | suggested fix, recommendation |

## Watchers

| Term | Definition | Aliases to avoid |
|------|-----------|-----------------|
| **Watcher** | An agent that monitors external systems and creates issues in vai based on discoveries. | monitor, observer, sensor |
| **Discovery Event** | An observation from a watcher that may trigger issue creation (e.g., test failure, security vulnerability). | finding, alert, detection |
| **Duplicate Suppression** | The system that prevents creating duplicate issues for recurring discoveries. | deduplication, duplicate detection |
| **Issue Creation Policy** | Configuration on a watcher that controls automatic issue creation (auto_create, max_per_hour, approval threshold). | auto-create rules |

## Server & Remote

| Term | Definition | Aliases to avoid |
|------|-----------|-----------------|
| **Local Mode** | Operating vai directly on the filesystem via CLI without a server. | standalone, offline, embedded |
| **Server Mode** | Operating vai behind a REST + WebSocket API that agents connect to remotely. | remote mode, hosted mode, service mode |
| **Remote** | A configured vai server URL and API key in `.vai/config.toml` that CLI commands proxy through transparently. | origin, upstream |
| **Transparent Proxying** | CLI behavior where commands automatically route to a remote server when configured, with `--local` flag to override. | remote forwarding |
| **API Key** | A token used to authenticate agents and UI clients with the vai server. | auth token, secret, credentials |
| **Event Streaming** | Real-time delivery of filtered events to connected clients via WebSocket with at-least-once delivery. | event feed, live updates, push notifications |

## Versioning

| Term | Definition | Aliases to avoid |
|------|-----------|-----------------|
| **HEAD** | Pointer to the current active version, stored in `.vai/head`. | current version, tip, latest |
| **Version History** | Linear sequence of versions (completed intents), not a DAG. | commit history, log |
| **Rollback** | Reverting the codebase to a previous version by creating a new version with inverse changes, after impact analysis. | revert, undo, reset |
| **Impact Analysis** | Identification of downstream versions that depend on changes being rolled back. | risk assessment, dependency check |

## Language Support

| Term | Definition | Aliases to avoid |
|------|-----------|-----------------|
| **Tree-sitter** | The parser generator used to produce ASTs from source code for entity and relationship extraction. | parser, lexer, analyzer |
| **Component** (EntityKind) | A TypeScript/React function that returns JSX, detected by the presence of `jsx_element` in its body. | widget, view, element |
| **Hook** (EntityKind) | A TypeScript/React function following the `use*` naming convention. | utility, helper |

## Storage & Infrastructure

| Term | Definition | Aliases to avoid |
|------|-----------|-----------------|
| **Event Segment** | An append-only file in the event log directory (local mode), rotated when exceeding the size threshold (default 64MB). | log file, event file |
| **Event Index** | SQLite database providing fast queries into the event log by type, workspace, entity, or time range; fully rebuildable from segments. Used in local mode only. | event database, log index |
| **Storage Trait** | A Rust trait defining all database operations (EventStore, IssueStore, GraphStore, etc.), with separate implementations for SQLite (local) and Postgres (server). | storage interface, storage abstraction, backend |
| **Storage Backend** | The configured storage implementation: Local (SQLite + filesystem) or Server (Postgres + S3). Determined by config and mode. | storage driver, storage provider |
| **FileStore** | Trait for file content storage with implementations for local filesystem and S3-compatible object storage. | blob store, content store, file backend |
| **Object Storage** | S3-compatible storage for file content in server mode. MinIO locally, Cloudflare R2 in production. | S3, blob storage, file storage, MinIO, R2 |
| **Content-Addressable Storage** | Files stored by their SHA-256 hash, enabling deduplication across versions. The FileStore maps paths to hashes via the `file_index` table in Postgres. | CAS, hash-based storage |
| **`current/` Prefix** | The S3 key prefix that holds the complete, latest state of a repository. Updated atomically after each workspace submit. The download handler serves from here. | latest, head files, repo state |
| **MergeFs** | A Rust trait abstracting file I/O for the merge and diff engines, with logical key namespaces (`overlay/`, `base/`, `snapshot/`). Implementations: `DiskMergeFs` (local filesystem) and `S3MergeFs` (in-memory buffer backed by S3). | merge abstraction, file layer |
| **Deleted Paths** | A list of file paths on a workspace that were present in the base but removed by the agent. Stored as `deleted_paths` on the workspace row. Applied to `current/` prefix on submit. | tombstones, removal manifest |
| **Tarball Upload** | `POST /workspaces/:id/upload-snapshot` — accepts a gzipped tarball of the agent's working directory. The server diffs it against `current/` to determine added, modified, and deleted files. The primary upload path for agent workflows. | snapshot upload, bulk upload |

## Multi-Tenancy

| Term | Definition | Aliases to avoid |
|------|-----------|-----------------|
| **Organization** | A group of users that owns repositories. The top-level entity in the hosted platform's hierarchy. | org, team, account, tenant |
| **Member** | A user who belongs to an organization, with an org-level role (Owner, Admin, or Member). | org user, team member |
| **Collaborator** | A user with access to a specific repository, with a repo-level role (Admin, Write, or Read). May or may not be an org member. | contributor, repo user |
| **Role** | A fixed set of permissions assigned to a user at the org or repo level. Not customizable. | permission level, access level |
| **Owner** (role) | Org-level role with full access including billing, repo deletion, and member management. | super admin |
| **Admin** (role) | Org or repo-level role with full access except billing. Can manage members/collaborators and all repo operations. | manager |
| **Write** (role) | Repo-level role that can create workspaces, submit changes, manage issues, and resolve escalations. Cannot manage access. | contributor, editor, developer |
| **Read** (role) | Repo-level role with view-only access to all repo data. Cannot create or modify anything. | viewer, observer |
| **Effective Role** | The computed permission level for a user on a specific repo, resolved by checking org membership first, then repo collaborator role. | resolved role, actual permissions |
| **Scoped API Key** | An API key created by a user, tied to a specific repo and role. Cannot exceed the creator's own permissions. | repo key, agent key |
| **Bootstrap Admin Key** | A server-level admin key set via `VAI_ADMIN_KEY` environment variable for initial server setup before any orgs/users exist. | root key, setup key, master key |

## Deployment Modes

| Term | Definition | Aliases to avoid |
|------|-----------|-----------------|
| **Local Mode** | Operating vai directly on the filesystem via CLI without a server. Uses SQLite and local filesystem. Zero external dependencies. | standalone, offline, embedded |
| **Server Mode** | Operating vai behind a REST + WebSocket API using Postgres and S3. Supports multi-repo and multi-tenant access. | remote mode, hosted mode, platform mode |
| **Single-Repo Mode** | Server started from within a vai repo directory, serving only that repo's data. Legacy mode for backward compatibility. | local server, dev server |
| **Multi-Repo Mode** | Server started with a `storage_root` configured, hosting multiple repositories under a single instance. The hosted platform mode. | platform mode, hosted mode |
| **Storage Root** | Directory where multi-repo mode stores all repository data, configured in `~/.vai/server.toml`. | repo root, data directory |

## Migration

| Term | Definition | Aliases to avoid |
|------|-----------|-----------------|
| **Migration** (local → remote) | The one-time transfer of a local repo's history (events, issues, versions) to the hosted server via a bulk endpoint. Source files are uploaded to S3 and the `current/` prefix is seeded. Idempotent and resumable. | upload, sync, push, import |
| **Bulk Migration Endpoint** | `POST /api/repos/:repo/migrate` — accepts all local data in a single request, inserts in one Postgres transaction. Uses `ON CONFLICT DO NOTHING` for idempotent retry. | import endpoint, upload endpoint |
| **Migration Marker** | A `.vai/migrated_at` file written after successful migration, recording the timestamp and remote URL. | migration flag, migration stamp |

---

## Relationships

- A **Workspace** is created with exactly one **Intent**.
- A **Workspace** is based on exactly one **Version** (its base version).
- A **Workspace** contains zero or more **Overlay** files.
- A **Version** is created by exactly one successful **Merge** of a **Workspace**.
- A **Version** has exactly one parent **Version** (except v1).
- The **Semantic Graph** contains zero or more **Entities** connected by **Relationships**.
- An **Entity** belongs to exactly one file and has exactly one **Entity Kind**.
- An **Entity** may have one **Parent Entity** (e.g., method inside a class).
- A **Conflict** is detected between a **Workspace** and **HEAD** during a **Semantic Merge**.
- A **Conflict** may trigger an **Escalation** if it cannot be auto-resolved.
- An **Escalation** is resolved by selecting one **Resolution Option**.
- An **Issue** transitions through **Issue Status** states and may be linked to one or more **Workspaces**.
- An **Issue** may have **Issue Links** to other issues (Blocks, RelatesTo, Duplicates).
- An **Issue** may have **Acceptance Criteria**, **Comments**, and **Attachments**.
- A **Blocks** link prevents the target issue from appearing in **Available Work** until the source issue is closed.
- A **Claim** atomically transitions an **Issue** to InProgress and creates a linked **Workspace**.
- The **Work Queue** ranks **Issues** using **Scope Prediction**, filters by **Overlap** with active **Workspaces**, and excludes issues blocked by **Issue Links**.
- A **Watcher** submits **Discovery Events** that may create **Issues** according to its **Issue Creation Policy**.
- A **Merge Pattern** is recorded from a resolved **Conflict** and may be auto-promoted after reaching the success threshold.
- The **Conflict Engine** tracks **Scope Footprints** for all active **Workspaces** and computes **Overlap Levels**.
- An **Organization** owns zero or more **Repositories**.
- A **User** belongs to zero or more **Organizations** as a **Member** with a **Role**.
- A **User** may be a **Collaborator** on a **Repository** with a repo-level **Role**.
- A **Scoped API Key** belongs to exactly one **User** and optionally one **Repository**.
- The **Effective Role** is resolved from **Organization** membership first, then **Collaborator** role.
- A **Storage Backend** provides implementations of all **Storage Traits** (Local or Server).
- The **FileStore** stores content in the local filesystem (Local mode) or **Object Storage** (Server mode) using **Content-Addressable Storage**.
- In server mode, the **`current/` Prefix** in S3 holds the latest repo state. It is updated after each **Workspace** submit.
- The **MergeFs** trait is used by the merge and diff engines. **DiskMergeFs** backs local mode; **S3MergeFs** backs server mode with in-memory buffering.
- A **Migration** transfers data from Local **Storage Backend** to Server **Storage Backend** via the **Bulk Migration Endpoint** and seeds the **`current/` Prefix**.

---

## Example Dialogue

> **Developer:** "An agent just pushed changes that conflict with another agent's work."
>
> **Domain Expert:** "Agents don't push — they **submit** a **workspace**. When the **merge engine** detects incompatible changes at **Level 3**, it records a **conflict**. If the conflict can't be auto-resolved, it creates an **escalation** for human review."
>
> **Developer:** "Got it. So what happens to the other agent's branch?"
>
> **Domain Expert:** "There are no branches — there are **workspaces**. The other agent's workspace is still active. The **conflict engine** already detected the **overlap** and sent a real-time notification via **event streaming**. The **work queue** also marks related issues as **blocked work** until the conflict is resolved."
>
> **Developer:** "And the version history is linear?"
>
> **Domain Expert:** "Yes. Each **version** has exactly one parent. The **version history** reads as a sequence of completed **intents**, not a graph of merged branches. **Rollback** creates a new version with inverse changes — it never rewrites history."

---

## Flagged Ambiguities

| Ambiguity | Recommendation |
|-----------|---------------|
| "Conflict" is used for both merge conflicts (detected during workspace submit) and scope overlaps (detected in real-time by the conflict engine). | Use **Conflict** only for merge-time incompatibilities. Use **Overlap** for real-time scope intersection detection. An overlap may or may not become a conflict at merge time. |
| "Scope" appears in Scope Footprint, Scope Inference, Scope Prediction, Scope History, and Scope Accuracy. | Always qualify: **scope footprint** (what a workspace touched), **scope inference** (prediction process), **scope prediction** (the output), **scope history** (past data), **scope accuracy** (metrics). Never use bare "scope." |
| "Resolution" is used for both conflict resolution (merge) and issue resolution (close). | Use **conflict resolution** and **issue resolution** explicitly. In escalation context, use **resolution option** for the suggested paths. |
| "Snapshot" could mean graph snapshot (SQLite DB) or pre-change file snapshot (saved before merge for rollback). | Use **graph snapshot** for the semantic graph DB. Use **pre-change snapshot** for the file backup saved before merge. |
| "Status" is used for both workspace status and issue status. | Always qualify: **workspace status** (Created/Active/Submitted/Merged/Discarded) or **issue status** (Open/InProgress/Resolved/Closed). |
| "Server" can mean the vai HTTP server process or the remote server a CLI connects to. | Use **vai server** for the process. Use **remote** for the configured server URL in `.vai/config.toml`. |
| "Dashboard" refers to both the existing Rust TUI and the planned web UI. | Use **TUI dashboard** for the terminal interface. Use **web dashboard** for the browser-based UI. |
| "Storage" can mean the storage trait, the storage backend, or the physical storage (SQLite/Postgres/S3). | Use **storage trait** for the Rust interface, **storage backend** for the configured implementation (Local/Server), and name the physical system explicitly (SQLite, Postgres, S3, MinIO). |
| "Admin" is used for org admin role and repo admin role. | Always qualify: **org admin** or **repo admin**. The permissions differ — org admins can manage all repos in the org, repo admins can only manage their repo's collaborators. |
| "Key" can mean API key, admin key, or Better Auth session token. | Use **API key** for agent/user tokens sent as `Bearer <key>`. Use **bootstrap admin key** for the `VAI_ADMIN_KEY` env var. Use **session** for browser auth managed by Better Auth. |
| "Mode" is overloaded: local mode, server mode, single-repo mode, multi-repo mode. | Use **local mode** (CLI, no server) vs **server mode** (HTTP API running). Within server mode, use **single-repo** vs **multi-repo** to specify hosting model. |
| "Migration" could mean database schema migration (sqlx) or local-to-remote data migration. | Use **schema migration** for database DDL changes. Use **remote migration** for the `vai remote migrate` data transfer flow. |
| "depends_on" is a legacy field on issues that has been replaced by **Issue Links** with the **Blocks** relationship type. | Never use `depends_on`. Use **blocks** link to express that one issue must be completed before another can start. The work queue checks links, not the legacy field. |
