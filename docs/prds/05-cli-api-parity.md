# Phase 5: CLI–API Parity

## Summary

Every REST API endpoint on the vai server should have a corresponding CLI command. Users running vai locally should never need to start a server to access functionality. The CLI operates directly on the `.vai/` directory for local use.

## Motivation

The work queue, issue mutations, and escalation resolution are currently only available through the HTTP API. This creates a gap where CLI-only workflows (e.g., local agent orchestration scripts) cannot access core functionality without spinning up a server.

## Requirements

### 5.1: Work Queue CLI

Add `vai work-queue` subcommand group:

- `vai work-queue list` — equivalent to `GET /api/work-queue`. Shows available and blocked work ranked by priority. Uses the same scope prediction logic as the server endpoint.
- `vai work-queue claim <issue-id>` — equivalent to `POST /api/work-queue/claim`. Atomically marks the issue as in-progress and creates a workspace with the issue title as the intent. Returns the workspace ID. Links the issue to the workspace.

JSON output via `--json` flag for both commands.

### 5.2: Issue Mutation CLI

Extend `vai issue` subcommand group with missing mutations:

- `vai issue create --title <title> --body <body> --priority <priority> --label <label>` — add the missing `--body` and `--label` flags to the existing create command. Labels should accept comma-separated values.
- `vai issue update <id> --title <title> --priority <priority> --label <label> --body <body>` — equivalent to `PATCH /api/issues/:id`. All flags optional; only provided fields are updated.
- `vai issue close <id> --resolution <text>` — equivalent to `POST /api/issues/:id/close`. Resolution text is optional.

JSON output via `--json` flag for all commands.

### 5.3: Escalation CLI

Extend `vai escalations` subcommand group:

- `vai escalations resolve <id> --resolution <text>` — equivalent to `POST /api/escalations/:id/resolve`. Resolution text required.

JSON output via `--json` flag.

## Out of Scope

- Remote server proxying (see PRD 06)
- Watcher/discovery CLI commands (low priority — watchers are inherently server-side)

## Issues

1. **Implement `vai work-queue list` command** — Query open issues, predict scope against semantic graph and active workspaces, rank by priority and independence, display available and blocked work. Priority: high.

2. **Implement `vai work-queue claim` command** — Accept issue ID, atomically transition issue to in-progress, create workspace with issue title as intent, link issue to workspace, return workspace details. Priority: high.

3. **Add `--body` and `--label` flags to `vai issue create`** — Extend the existing create command to accept optional body text and comma-separated labels. Priority: high.

4. **Implement `vai issue update` command** — Accept issue ID and optional flags for title, priority, labels, and body. Update only provided fields. Priority: medium.

5. **Implement `vai issue close` command** — Accept issue ID and optional resolution text. Transition issue to closed status. Priority: medium.

6. **Implement `vai escalations resolve` command** — Accept escalation ID and resolution text. Mark escalation as resolved. Priority: medium.
