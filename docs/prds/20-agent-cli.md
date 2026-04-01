# PRD 20: Agent CLI (`vai agent`)

## Overview

Replace the fragile 436-line bash RALPH script with a set of `vai agent` subcommands that handle all vai server interaction for autonomous agent workflows. The coding agent (Claude, Codex, custom) is invoked by the user's loop script — the CLI is agnostic to which agent is used.

## Design Principles

1. **Agent-agnostic** — the CLI manages vai server interaction only. It doesn't invoke, configure, or know about the coding agent.
2. **Environment-agnostic** — works the same locally, in Docker, or in cloud sandboxes.
3. **Resumable** — each command is idempotent. If a step fails, re-running it picks up from the state file.
4. **Minimal loop** — a complete agent loop is ~12 lines of bash.

## Commands

### `vai agent init`

Initialize agent configuration for a repository.

```bash
vai agent init --server https://vai.example.com --repo myapp
# Creates .vai/agent.toml
```

Config file (`.vai/agent.toml`):
```toml
server = "https://vai.example.com"
repo = "myapp"

# Optional: prompt template path (default: .vai/prompt.md)
prompt_template = ".vai/prompt.md"

# Optional: quality check commands
[checks]
commands = [
    "npx biome check --write src/",
    "npx tsc --noEmit",
    "pnpm run test",
]
```

API key comes from `VAI_API_KEY` environment variable (never stored in config files).

Config precedence: CLI flags > environment variables > `.vai/agent.toml`.

### `vai agent claim`

Query the work queue, pick the highest priority available issue, claim it, and create a workspace.

```bash
vai agent claim
# Output: Claimed [high] Fix auth middleware (#42) → workspace ws-abc123
# Exit 0: work claimed
# Exit 1: no work available (loop should exit)
```

Saves state to `.vai/agent-state.json`:
```json
{
  "issue_id": "uuid",
  "issue_title": "Fix auth middleware",
  "workspace_id": "uuid",
  "phase": "claimed",
  "claimed_at": "2026-04-01T..."
}
```

If state already exists (previous iteration crashed after claim), prints the current issue and continues without re-claiming.

### `vai agent download <dir>`

Download the current repo state from the server and extract to the given directory.

```bash
vai agent download ./work
# Output: Downloaded 320 files to ./work
```

- Fetches tarball from `GET /files/download`
- Extracts to `<dir>`
- Saves a copy of the original state for diff comparison during submit
- Updates state: `"phase": "downloaded"`

### `vai agent issue`

Print the current issue details.

```bash
vai agent issue           # Human-readable summary
vai agent issue --json    # Full JSON (for piping to agents)
```

Reads issue ID from state file, fetches from `GET /issues/:id`. Includes title, description, acceptance criteria, links, attachments, comments.

### `vai agent prompt`

Build a complete prompt from a template file and current issue details.

```bash
vai agent prompt                     # Uses .vai/prompt.md template
vai agent prompt --template my.md    # Custom template
```

The template uses `{{issue}}` as a placeholder:
```markdown
# Agent Instructions
You are working on a TypeScript project...

## Current Issue
{{issue}}

## Instructions
Implement the issue. Run quality checks before finishing.
```

`vai agent prompt` replaces `{{issue}}` with the JSON issue details from `vai agent issue --json` and prints to stdout. Pipe directly to the coding agent:

```bash
vai agent prompt | claude -p --allowedTools 'Read,Edit,Write,Bash,Glob,Grep'
```

### `vai agent verify <dir>`

Run quality checks defined in config against the working directory.

```bash
vai agent verify ./work
# Exit 0: all checks passed
# Exit 1: checks failed, structured error output on stderr
```

Reads `checks.commands` from `.vai/agent.toml`. Runs each command in sequence. If any fails, outputs a structured summary of all failures formatted for feeding to an agent:

```
=== TypeScript errors ===
src/lib/auth.ts(42,5): error TS2339: Property 'foo' does not exist...

=== Test failures ===
FAIL src/lib/auth.test.ts > validateToken > should reject expired tokens
  Expected: true, Received: false
```

If no checks are configured, exits 0 with a warning.

### `vai agent submit <dir>`

Upload changes and submit the workspace in one atomic step.

```bash
vai agent submit ./work
# Output: Uploaded 12 files (8 modified, 3 added, 1 deleted) → version v42
```

Steps:
1. Create tarball of `<dir>` (excluding node_modules, .git, dist, etc.)
2. POST tarball to `upload-snapshot` endpoint — server diffs against `current/` to detect adds, modifications, and deletions
3. Submit workspace via `POST /workspaces/:id/submit`
4. Close the linked issue via `POST /issues/:id/close`
5. Clear state file

If upload or submit fails, the state file is preserved so the user can retry with the same command.

### `vai agent status`

Show the current agent state.

```bash
vai agent status
# Output:
# Issue: Fix auth middleware (#42) [high]
# Workspace: ws-abc123
# Phase: downloaded
# Claimed: 5 minutes ago
```

### `vai agent reset`

Abandon the current work, discard the workspace, and reopen the issue.

```bash
vai agent reset
# Output: Discarded workspace ws-abc123, reopened issue #42
```

- Calls `DELETE /workspaces/:id` or discard endpoint
- Reopens the issue so another agent can pick it up
- Clears state file

## The Complete Agent Loop

```bash
#!/bin/bash
# Minimal RALPH loop — 12 lines

while vai agent claim; do
  vai agent download ./work

  # Run coding agent (your choice — Claude, Codex, custom)
  vai agent prompt | claude -p --allowedTools 'Read,Edit,Write,Bash,Glob,Grep'

  # Optional: verify and retry
  ERRORS=$(vai agent verify ./work 2>&1)
  if [ $? -ne 0 ]; then
    echo "$ERRORS" | claude -p "Fix these errors in ./work"
  fi

  vai agent submit ./work || vai agent reset
  rm -rf ./work
done
```

## Error Recovery

| Failure | Behavior | Recovery |
|---------|----------|---------|
| `claim` — no work | Returns exit 1 | Loop exits naturally |
| `claim` — conflict | Skips, tries next iteration | Automatic |
| `download` — network error | State stays at "claimed" | Re-run `download` |
| Agent crashes mid-work | Files partially modified on disk | Re-run agent, or `reset` |
| `submit` — merge conflict | Prints error, state preserved | Fix and re-run `submit`, or `reset` |
| `submit` — server down | State preserved | Retry `submit` later |

## Configuration Precedence

1. CLI flags (`--server`, `--repo`, `--key`)
2. Environment variables (`VAI_SERVER_URL`, `VAI_API_KEY`, `VAI_REPO`)
3. Config file (`.vai/agent.toml`)

API key is NEVER stored in the config file — environment variable or CLI flag only.

## Ignore Patterns

The `submit` command excludes these paths from the tarball:
- `node_modules/`
- `.git/`
- `dist/`, `.next/`, `.output/`
- `target/` (Rust)
- `__pycache__/`, `.venv/` (Python)
- `.vai/`

Additional patterns can be configured in `.vai/agent.toml`:
```toml
[ignore]
patterns = ["*.log", "tmp/"]
```

Or via `.vaignore` file (same format as `.gitignore`).

## Issue Breakdown

### Core Commands
1. **Implement `vai agent init`** — config file creation, validation, env var reading
2. **Implement `vai agent claim`** — work queue query, priority sorting, atomic claim, state file management
3. **Implement `vai agent download <dir>`** — tarball download, extraction, original copy for diffing
4. **Implement `vai agent issue`** — fetch and display issue details, JSON output mode
5. **Implement `vai agent submit <dir>`** — tarball creation, upload-snapshot, submit workspace, close issue, state cleanup
6. **Implement `vai agent status` and `vai agent reset`** — state inspection and cleanup

### Enhancement Commands
7. **Implement `vai agent prompt`** — template reading, `{{issue}}` replacement, stdout output
8. **Implement `vai agent verify <dir>`** — read check commands from config, run sequentially, structured error output

### Integration
9. **Rewrite RALPH loop scripts using `vai agent` CLI** — replace both vai and vai-dashboard bash scripts with ~12 line versions
10. **Add agent CLI documentation** — usage guide with examples for Claude, Codex, and custom agents

## Future Enhancements

- `vai agent watch` — long-running mode that polls for work and auto-claims (replaces the while loop entirely)
- `vai agent log` — append a comment to the current issue (for progress reporting)
- `vai agent attach <file>` — upload a file as an attachment to the current issue
- Webhook mode — vai server pushes work to agents instead of agents polling
