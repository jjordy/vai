# vai — Product Requirements Overview

## What is vai?

vai is a version control system built for AI agents. It replaces git's human-centric model (branches, commits, text diffs) with an agent-native model (intents, workspaces, semantic graphs) designed for massive parallelism — 50 to 5000+ agents working on a single codebase simultaneously.

## Problem Statement

Git was designed for human developers collaborating on text files. As AI agents become primary contributors to codebases, git's model breaks down:

- **Merge conflicts** require human-like judgment that agents handle poorly
- **Branching models** assume a human context-switching, not hundreds of agents working simultaneously
- **Diffs are line-based** — agents think in terms of ASTs and semantic changes, not line numbers
- **No intent tracking** — git stores *what* changed but not *why* or *what was the intent*
- **No real-time awareness** — agents can't see what other agents are working on until after push/pull
- **No coordination** — git has no mechanism to prevent conflicting parallel work

## Vision

A future where the human's role shifts from writing code to directing and overseeing agent work. The human:

- Creates high-level intents ("add rate limiting to auth")
- Monitors agent progress via a dashboard
- Resolves high-severity intent conflicts when escalated
- Reviews completed work at the intent level, not the diff level

vai reduces the cognitive burden on the human while enabling agents to collaborate effectively.

## Core Concepts

### Intent
The fundamental unit of work. Replaces the git "commit message" concept. An intent describes *what* an agent is trying to accomplish, not what lines changed. Intents are first-class objects that drive the entire workflow.

### Semantic Graph
The codebase represented as a graph of language-level entities (functions, classes, types, modules) and their relationships (calls, imports, depends-on). Built from tree-sitter ASTs. Enables semantic-aware merging and conflict detection.

### Workspace
An isolated environment where an agent does its work. The agent gets a full checkout of the codebase plus a connection to the coordination layer. Workspaces track all changes as events.

### Version
A labeled state of the main codebase after a successful merge. Replaces git "commits." The version history reads as a sequence of completed intents.

### Event Log
The append-only source of truth. Every action in the system is an event. The current state of everything — semantic graph, workspaces, issues — is derived by replaying the log.

## Architecture

vai is a Rust library with two deployment modes:

- **Local mode:** Core library embedded directly, single-machine use
- **Server mode:** Core library behind a REST + WebSocket API, agents connect remotely

The server handles coordination, awareness, and integration. It does NOT manage agent compute — agents are responsible for their own execution environments (building, testing).

### Agent Communication
- WebSocket for real-time event streaming (workspace updates, conflict notifications)
- REST/gRPC API for commands (create workspace, submit changes, query graph)

### System Components
- **Intent Registry** — tracks all active, queued, and completed intents
- **Workspace Manager** — creates/manages isolated agent workspaces
- **Conflict Engine** — analyzes overlap between active workspaces, classifies severity
- **Semantic Graph Engine** — in-memory materialized view of codebase structure
- **Event Log** — append-only source of truth
- **Merge Engine** — semantic-level merging when workspaces complete
- **Issue System** — issue → intent → workspace → changes → merge lifecycle
- **Work Queue API** — exposes non-conflicting work for external orchestrators

## Phasing

- **Phase 1 — Foundation:** Core library, CLI, event log, semantic graph, workspaces, semantic merge. Local mode only.
- **Phase 2 — Coordination:** Server mode, multi-agent workspaces, conflict engine, real-time streaming.
- **Phase 3 — Issue System:** Issue lifecycle, intent pipeline, smart work queue, human escalation.
- **Phase 4 — Intelligence:** NLP scope inference, merge learning, agent-initiated issues, TUI dashboard.

## Technology Choices

- **Language:** Rust
- **AST Parsing:** tree-sitter (starting with Rust, TypeScript, Python)
- **Graph Storage:** In-memory materialized view backed by SQLite snapshot
- **Event Log:** Append-only files with SQLite index
- **Protocol:** WebSocket + REST/gRPC
- **Codebase Structure:** Vertical slices, clean API boundaries — optimized for AI-assisted development
