# Phase 2 — Coordination

## Goal

Add server mode, multi-agent coordination, real-time awareness, and conflict detection. By the end of Phase 2, multiple remote agents can work on the same codebase simultaneously with real-time visibility into each other's work and automatic conflict detection.

**Depends on:** Phase 1 (Foundation) complete.

---

## PRD 2.1: Server Architecture

### Summary
The vai server wraps the core library with a network API, enabling remote agents to create workspaces, submit changes, query the semantic graph, and receive real-time event streams. The server is the single source of truth for the codebase in multi-agent mode.

### Requirements

**Functional:**
- `vai server start` launches the vai server for the current repository
- The server exposes two interfaces:
  - **REST API** for command operations (create workspace, submit changes, query graph)
  - **WebSocket API** for real-time event streaming
- Server configuration via `.vai/config.toml`:
  - Bind address and port
  - Authentication settings
  - Max concurrent workspaces
  - Event stream buffer size
- The server manages the same `.vai/` directory structure as local mode — the core library is shared
- Server logs all operations with structured logging

**REST API Endpoints:**
```
POST   /api/workspaces                    # create workspace
GET    /api/workspaces                    # list workspaces
GET    /api/workspaces/:id                # workspace details
POST   /api/workspaces/:id/submit         # submit workspace
DELETE /api/workspaces/:id                # discard workspace
POST   /api/workspaces/:id/files          # upload changed files
GET    /api/workspaces/:id/files/:path    # get file from workspace

GET    /api/versions                      # version history
GET    /api/versions/:id                  # version details
POST   /api/versions/rollback             # rollback

GET    /api/graph/entities                # query entities
GET    /api/graph/entities/:id            # entity details
GET    /api/graph/entities/:id/deps       # entity dependencies
GET    /api/graph/blast-radius            # blast radius for entity set

GET    /api/status                        # server status, active workspaces
```

**WebSocket API:**
```
CONNECT /ws/events
  → Subscribe to event types and entity filters
  → Receive real-time events matching subscription
  → Bidirectional: agent can send workspace events upstream
```

**Authentication:**
- API key-based authentication for agents
- Each agent has a unique identity (agent ID, name)
- Keys are managed via `vai server keys create/list/revoke`

**Non-Functional:**
- Server must handle 500+ concurrent WebSocket connections
- REST API response time under 100ms for graph queries
- Event stream latency under 50ms from event creation to delivery
- Graceful shutdown: complete in-flight merges before stopping

### Out of Scope
- Horizontal scaling / multi-server (future)
- TLS termination (use a reverse proxy)

---

## PRD 2.2: Remote Agent Workflow

### Summary
Define how a remote agent interacts with the vai server — from cloning the codebase to submitting changes. The agent uses the vai CLI, which communicates with the server API.

### Requirements

**Functional:**
- `vai clone vai://<host>:<port>/<repo>` clones a repository from a vai server
  - Downloads the full codebase (all source files)
  - Creates a local `.vai/` with server connection config
  - Does NOT download the full event log or graph — these are queried from the server on demand
- `vai workspace create --intent "<text>"` in a cloned repo:
  - Registers the workspace with the server (REST API)
  - The agent works locally on its full checkout
  - Workspace events are streamed to the server in real-time via WebSocket
- `vai sync` pulls latest changes from the server into the local checkout
  - Updates files that have changed since last sync
  - Updates local graph cache
  - Notifies the agent of changes relevant to its current workspace
- `vai workspace submit` in a cloned repo:
  - Uploads changed files to the server
  - Server performs merge (same engine as local mode)
  - Result streamed back to agent
- `vai status --others` queries the server for all active workspaces and their intents

**Agent-Local Directory Structure:**
```
project/
├── .vai/
│   ├── config.toml        # includes server URL, agent API key
│   ├── workspace/          # current active workspace
│   │   ├── meta.toml
│   │   └── events.log
│   └── cache/
│       └── treesitter/
├── src/                    # full checkout
└── vai.toml
```

**Connection Resilience:**
- If WebSocket disconnects, agent buffers events locally and replays on reconnect
- If server is unreachable during submit, retry with exponential backoff
- Agent can continue local work while disconnected — workspace events sync when connection restores

**Non-Functional:**
- `vai clone` for a 1GB repository should complete in under 60 seconds on a fast connection
- `vai sync` should be incremental — only transfer changed files
- Agent should be able to work offline and sync later

### Out of Scope
- Partial clone (only relevant files)
- Agent-to-agent direct communication (all communication goes through server)

---

## PRD 2.3: Conflict Engine

### Summary
The conflict engine continuously monitors all active workspaces and detects overlapping work. It classifies overlap severity and triggers appropriate responses: notifications, warnings, or escalation.

### Requirements

**Functional:**
- The conflict engine runs as a background process on the server
- For each active workspace, it maintains a **scope footprint**: the set of semantic entities the agent has read or modified
- When a workspace's scope footprint overlaps with another workspace's, the engine classifies the overlap:

**Overlap Classification:**
| Level | Criteria | Action |
|-------|----------|--------|
| None | No shared entities | Nothing |
| Low | Same file but different entities | Informational notification |
| Medium | Same entity, different aspects (e.g., one reads, other writes) | Warning notification with details |
| High | Same entity modified by multiple workspaces, with referential dependencies | Alert — recommend coordination |
| Critical | Directly contradictory intents detected | Escalation — block submission until resolved |

**Notifications:**
- Sent via WebSocket to affected agents
- Include: overlap level, which entities overlap, which other workspace(s), the other workspace's intent
- Notification format:
  ```json
  {
    "type": "overlap_detected",
    "severity": "medium",
    "your_workspace": "ws-abc123",
    "other_workspace": "ws-def456",
    "other_intent": "refactor auth service",
    "overlapping_entities": ["AuthService", "validate_token"],
    "recommendation": "Your changes to validate_token may conflict with an ongoing refactor. Consider syncing."
  }
  ```

**Scope Tracking:**
- When an agent reads a file: entities in that file are added to the workspace's "read scope"
- When an agent modifies a file: entities in that file are added to the workspace's "write scope"
- The blast radius is computed from write scope using the semantic graph (transitive dependencies)
- Scope is updated in real-time as the agent streams workspace events

**Non-Functional:**
- Overlap analysis should run within 500ms of a scope change
- Must scale to 500 concurrent workspaces without degradation
- False positive rate for "high" and "critical" classifications should be under 10%

### Out of Scope
- Automatic conflict resolution (this is done by the merge engine)
- Intent-level NLP analysis for contradiction detection (Phase 4)

---

## PRD 2.4: Real-Time Event Streaming

### Summary
Agents subscribe to event streams filtered by relevance. The event system enables real-time awareness of other agents' work and powers the conflict engine.

### Requirements

**Functional:**
- Agents connect via WebSocket and subscribe to event streams with filters:
  ```json
  {
    "subscribe": {
      "entities": ["AuthService", "validate_token"],
      "paths": ["src/auth/*"],
      "event_types": ["EntityModified", "WorkspaceCreated", "OverlapDetected"],
      "workspaces": ["ws-def456"]
    }
  }
  ```
- The server delivers matching events in real-time
- Events include full context: what changed, who changed it, which workspace, which intent
- Agents can update their subscriptions at any time
- The server tracks per-agent delivery state — if an agent disconnects and reconnects, it receives missed events (up to a configurable buffer)

**Event Delivery Guarantees:**
- At-least-once delivery: events may be delivered more than once, agents must handle idempotently
- Ordered per workspace: events within a single workspace are delivered in order
- No global ordering guarantee across workspaces

**Server-Side Filtering:**
- The server must efficiently match events to subscriptions
- With 500 agents each subscribing to different entity sets, the fan-out must be efficient
- Use an inverted index: entity_id → [subscribed agent IDs]

**Non-Functional:**
- Event delivery latency under 50ms from event creation
- Support 500+ concurrent WebSocket connections with active subscriptions
- Event buffer for disconnected agents: configurable, default 1 hour or 10,000 events

### Out of Scope
- Persistent event subscriptions (subscriptions are session-scoped)
- Event replay from arbitrary points in history (use REST API for historical queries)
