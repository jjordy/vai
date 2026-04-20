# vai agent CLI — Developer Guide

The `vai agent` subcommands handle all vai server interaction for autonomous agent loops. Your coding agent (Claude, Codex, a custom script) stays completely separate — `vai agent` is only concerned with claiming work, downloading repo state, and submitting results.

## Quick Start (5 minutes)

### Prerequisites

- `vai` binary installed and on your `PATH`
- A running vai server (see `docs/prds/08-server-deployment.md`)
- A vai API key in the `VAI_API_KEY` environment variable

### 1. Initialize configuration

```bash
vai agent init --server https://vai.example.com --repo myapp
```

This creates `.vai/agent.toml`. Review and edit it if needed (see [Configuration Reference](#configuration-reference)).

### 2. Claim an issue

```bash
vai agent claim
# Claimed [high] Fix auth middleware (#42) → workspace ws-abc123
```

Exit 0 means work was claimed. Exit 1 means the queue is empty — the loop is done.

### 3. Download the repo

```bash
vai agent download ./work
# Downloaded 320 files to ./work
```

### 4. Run your coding agent

```bash
vai agent prompt | claude -p --allowedTools 'Read,Edit,Write,Bash,Glob,Grep'
```

### 5. Submit the result

```bash
vai agent submit ./work
# Uploaded 12 files (8 modified, 3 added, 1 deleted) → submitted
rm -rf ./work
```

That's it. Run `vai agent claim` again for the next issue.

---

## Command Reference

### `vai agent init`

Initialize agent configuration for the current directory.

```
vai agent init [OPTIONS]
```

**Options:**

| Flag | Description |
|------|-------------|
| `--server <URL>` | Base URL of the vai server (e.g. `https://vai.example.com`) |
| `--repo <NAME>` | Repository name on the server |
| `--prompt-template <PATH>` | Path to prompt template file (default: `.vai/prompt.md`) |

Creates `.vai/agent.toml`. Falls back to `VAI_SERVER_URL` / `VAI_REPO` environment variables if flags are omitted.

The API key is **never** written to disk — set `VAI_API_KEY` in your environment.

---

### `vai agent claim`

Query the work queue and atomically claim the highest-priority available issue.

```
vai agent claim [OPTIONS]
```

**Options:**

| Flag | Description |
|------|-------------|
| `--server <URL>` | Override server URL from config/env |
| `--repo <NAME>` | Override repository name from config/env |

**Exit codes:**
- `0` — work was claimed (or resumed from a previous crashed iteration)
- `1` — no work available; the loop should exit

On success, writes `.vai/agent-state.json`:

```json
{
  "issue_id": "550e8400-...",
  "issue_title": "Fix auth middleware",
  "workspace_id": "6ba7b810-...",
  "phase": "claimed",
  "claimed_at": "2026-04-01T12:00:00Z"
}
```

If the state file already exists (a previous iteration crashed mid-work), the existing issue is printed and the command exits 0 without re-claiming.

---

### `vai agent download <dir>`

Download the current repo snapshot from the server and extract it into a local directory.

```
vai agent download <DIR>
```

- Fetches the current repository tarball from the server
- Extracts all files into `<DIR>` (created if it does not exist)
- Saves a file manifest to `.vai/download-manifest.json` for use during submit
- Advances agent phase to `downloaded`

---

### `vai agent issue`

Display the details of the currently claimed issue.

```
vai agent issue [--json]
```

**Options:**

| Flag | Description |
|------|-------------|
| `--json` | Print raw JSON response (useful for piping to agents) |

Without `--json`, prints a human-readable summary: title, status, priority, description, acceptance criteria, and recent comments.

Exits 1 if no issue is currently claimed.

---

### `vai agent status`

Show the current state of the agent iteration.

```
vai agent status
```

Example output:

```
Issue:     Fix auth middleware (#42) [high]
Workspace: ws-abc123
Phase:     downloaded
Claimed:   5 minutes ago
```

Exits 1 if no agent state exists.

---

### `vai agent prompt`

Build a complete prompt from a template and the current issue details.

```
vai agent prompt [--template <PATH>]
```

**Options:**

| Flag | Description |
|------|-------------|
| `--template <PATH>` | Override the template file path |

Reads the template from `.vai/prompt.md` (or the path in `agent.toml`), replaces `{{issue}}` with the full JSON issue details, and prints the result to stdout.

If no template file exists, a sensible built-in default is used.

Pipe directly to any coding agent:

```bash
vai agent prompt | claude -p --allowedTools 'Read,Edit,Write,Bash,Glob,Grep'
```

---

### `vai agent verify <dir>`

Run quality checks defined in `.vai/agent.toml` against the working directory.

```
vai agent verify <DIR>
```

**Exit codes:**
- `0` — all checks passed (or no checks are configured)
- `1` — one or more checks failed

On failure, prints structured output to stderr formatted for AI consumption.
Every failing check is wrapped in a labelled section:

```
=== cargo clippy --features full -- -D warnings ===
error: this match arm has an identical body to the `_` wildcard arm
  --> src/server/mod.rs:42:9

  exit code: 1

=== cargo audit --deny warnings ===
error[RUSTSEC-2026-0099]: ...

  exit code: 1
```

**RALPH / Rust CI verify contract**

For vai itself, `.vai/agent.toml` configures verify to mirror the full CI
check matrix exactly:

| Step | Command | Mirrors CI job |
|------|---------|----------------|
| 1 | `cargo clippy -- -D warnings` | `Test (CLI only)` — Clippy |
| 2 | `cargo test` | `Test (CLI only)` — Tests |
| 3 | `cargo clippy --features full -- -D warnings` | `Test (full features)` — Clippy |
| 4 | `cargo test --features full -- --skip server_postgres_e2e` | `Test (full features)` — Tests (Postgres E2E skipped locally; CI runs them via `VAI_TEST_DATABASE_URL`) |
| 5 | `cargo audit --deny warnings` | `Security audit` |

> **Gap**: Postgres E2E tests (`tests/server_postgres_e2e.rs`) are excluded in
> step 4 because the sandcastle container has no Postgres service.  CI still
> runs them.  If step 4 passes locally but CI fails with a Postgres E2E error,
> check that test file manually.

Configure checks in `.vai/agent.toml`:

```toml
[checks]
commands = [
    "cargo clippy -- -D warnings",
    "cargo test",
    "cargo clippy --features full -- -D warnings",
    "cargo test --features full -- --skip server_postgres_e2e",
    "cargo audit --deny warnings",
]
```

---

### `vai agent submit <dir>`

Upload changes and submit the workspace in one step.

```
vai agent submit <DIR>
```

Steps performed in order:

1. Build a gzip tarball of `<DIR>` (standard ignore patterns applied)
2. `POST /api/workspaces/:id/upload-snapshot` — upload tarball
3. `POST /api/workspaces/:id/submit` — trigger server-side merge
4. `POST /api/issues/:id/close` — close the issue as resolved
5. Clear `.vai/agent-state.json`

If any step fails, the state file is preserved so you can retry with the same command.

**Exit codes:**
- `0` — success
- `1` — error (state preserved; retry or run `vai agent reset`)

---

### `vai agent reset`

Abandon the current work, discard the workspace, and reopen the issue.

```
vai agent reset
```

- Calls `DELETE /api/workspaces/:id` (marks workspace as `Discarded`)
- Reopens the linked issue so it can be claimed by another iteration
- Clears `.vai/agent-state.json`

Use this after a failed iteration to return the issue to the work queue.

---

## Example Loop Scripts

### Claude Code

```bash
#!/bin/bash
# loop-claude.sh — minimal RALPH loop using Claude Code
set -eo pipefail

export VAI_API_KEY="your-api-key"

while vai agent claim; do
    vai agent download ./work

    # Run Claude Code on the issue prompt
    vai agent prompt | claude -p --allowedTools 'Read,Edit,Write,Bash,Glob,Grep' \
        --output-format text \
        -- ./work

    # Run quality checks; if they fail, send errors back to Claude for a fix
    if ! vai agent verify ./work 2>/tmp/verify-errors.txt; then
        cat /tmp/verify-errors.txt | claude -p \
            --allowedTools 'Read,Edit,Write,Bash,Glob,Grep' \
            "Fix the following errors in ./work:"
    fi

    vai agent submit ./work || vai agent reset
    rm -rf ./work
done

echo "No more work available."
```

### OpenAI Codex / codex CLI

```bash
#!/bin/bash
# loop-codex.sh — agent loop using OpenAI Codex CLI
set -eo pipefail

export VAI_API_KEY="your-vai-api-key"

while vai agent claim; do
    vai agent download ./work

    # Build prompt and pass to codex
    PROMPT=$(vai agent prompt)
    codex --model o4-mini \
          --approval-policy auto-edit \
          --cwd ./work \
          "$PROMPT"

    if ! vai agent verify ./work 2>/tmp/verify-errors.txt; then
        ERRORS=$(cat /tmp/verify-errors.txt)
        codex --model o4-mini \
              --approval-policy auto-edit \
              --cwd ./work \
              "Fix these errors: $ERRORS"
    fi

    vai agent submit ./work || vai agent reset
    rm -rf ./work
done
```

### Custom Python Agent

```python
#!/usr/bin/env python3
"""loop-python.py — agent loop using a custom Python agent."""

import subprocess
import sys

def run(cmd, **kwargs):
    return subprocess.run(cmd, shell=True, check=True, **kwargs)

def run_nocheck(cmd, **kwargs):
    return subprocess.run(cmd, shell=True, **kwargs)

def your_agent(prompt: str, workdir: str):
    """Replace this with your own agent invocation."""
    # Example: call your agent SDK here
    import openai
    client = openai.OpenAI()
    response = client.chat.completions.create(
        model="gpt-4o",
        messages=[{"role": "user", "content": prompt}],
    )
    print(response.choices[0].message.content)

while True:
    # Claim work; exit when queue is empty
    result = run_nocheck("vai agent claim")
    if result.returncode != 0:
        print("No more work available.")
        break

    run("vai agent download ./work")

    # Build prompt and invoke agent
    prompt_result = subprocess.run(
        "vai agent prompt", shell=True, capture_output=True, text=True, check=True
    )
    your_agent(prompt_result.stdout, workdir="./work")

    # Verify and optionally fix
    verify = run_nocheck("vai agent verify ./work", capture_output=True, text=True)
    if verify.returncode != 0:
        your_agent(
            f"Fix these errors in ./work:\n{verify.stderr}",
            workdir="./work",
        )

    submit = run_nocheck("vai agent submit ./work")
    if submit.returncode != 0:
        run("vai agent reset")

    run("rm -rf ./work")
```

---

## Configuration Reference

### `.vai/agent.toml`

```toml
# Required: vai server base URL
server = "https://vai.example.com"

# Required: repository name on the server
repo = "myapp"

# Optional: path to prompt template (default: .vai/prompt.md)
prompt_template = ".vai/prompt.md"

# Optional: quality check commands run by `vai agent verify`
[checks]
commands = [
    "cargo build",
    "cargo test",
    "cargo clippy -- -D warnings",
]

# Optional: additional tarball ignore patterns for `vai agent submit`
[ignore]
patterns = ["*.log", "tmp/", "coverage/"]
```

### Environment Variables

| Variable | Description |
|----------|-------------|
| `VAI_API_KEY` | **Required.** API key for authenticating with the vai server. Never stored on disk. |
| `VAI_SERVER_URL` | Override the server URL from `agent.toml`. |
| `VAI_REPO` | Override the repository name from `agent.toml`. |

### Configuration Precedence

1. CLI flags (`--server`, `--repo`)
2. Environment variables (`VAI_SERVER_URL`, `VAI_REPO`)
3. `.vai/agent.toml`

### Default Ignore Patterns (submit tarball)

The following are always excluded from the submission tarball:

- `.vai/`
- `.git/`
- `node_modules/`
- `dist/`, `.next/`, `.output/`
- `target/` (Rust)
- `__pycache__/`, `.venv/` (Python)

Add custom patterns in `[ignore]` or via a `.vaignore` file (gitignore format).

### Prompt Template

Create `.vai/prompt.md` with `{{issue}}` as the placeholder for issue details:

```markdown
# Agent Instructions

You are working on a TypeScript web application.

## Current Issue

{{issue}}

## Instructions

1. Read the issue description and acceptance criteria carefully.
2. Implement the required changes.
3. Run `pnpm test` to verify your work.
4. Do not modify files unrelated to the issue.
```

`vai agent prompt` replaces `{{issue}}` with the full JSON issue object and prints to stdout.

---

## Error Recovery

| Failure point | What happens | Recovery |
|---------------|-------------|---------|
| `claim` — no work available | Exits 1 | Loop exits naturally |
| `claim` — server error | Exits 1 with error message | Fix connectivity, retry |
| Previous iteration crashed | State file exists with old issue | `vai agent claim` resumes it automatically |
| `download` — network error | Phase stays at `claimed` | Re-run `vai agent download ./work` |
| Agent crashes mid-work | Files partially modified in `./work` | Re-run agent, or `vai agent reset` |
| `submit` — merge conflict | Exits 1, state preserved | Fix conflict and retry `submit`, or `reset` |
| `submit` — server down | Exits 1, state preserved | Retry `submit` later |
| Want to abandon an issue | — | `vai agent reset` reopens it for another agent |

### Manual Recovery Steps

If the loop ends up in an inconsistent state:

```bash
# Check current state
vai agent status

# Option 1: retry submit if work is done
vai agent submit ./work

# Option 2: abandon and return to queue
vai agent reset
rm -rf ./work
```

---

## Docker Usage

Running the agent loop inside a Docker container isolates the working directory and keeps credentials out of the host environment.

### Dockerfile (minimal)

```dockerfile
FROM debian:bookworm-slim

# Install vai
COPY --from=vai-builder /usr/local/bin/vai /usr/local/bin/vai

# Install your coding agent (example: Claude Code)
RUN apt-get update && apt-get install -y curl nodejs npm && \
    npm install -g @anthropic-ai/claude-code

WORKDIR /agent

# Copy agent configuration (no secrets)
COPY .vai/agent.toml .vai/agent.toml
COPY .vai/prompt.md .vai/prompt.md
COPY scripts/loop.sh loop.sh

ENTRYPOINT ["/bin/bash", "loop.sh"]
```

### Running the container

```bash
docker run --rm \
  -e VAI_API_KEY="$VAI_API_KEY" \
  -e ANTHROPIC_API_KEY="$ANTHROPIC_API_KEY" \
  my-agent-image
```

No volume mounts are needed — the agent loop downloads fresh repo state for each issue and uploads the result. Each container run can handle multiple issues until the queue is empty.

### Example `loop.sh` for Docker

```bash
#!/bin/bash
set -eo pipefail

while vai agent claim; do
    vai agent download ./work
    vai agent prompt | claude -p \
        --allowedTools 'Read,Edit,Write,Bash,Glob,Grep' \
        -- ./work
    vai agent submit ./work || vai agent reset
    rm -rf ./work
done
```

---

## See Also

- `docs/prds/20-agent-cli.md` — PRD with design rationale
- `scripts/ralph.sh` — reference loop script (Docker-based, Claude Code)
- `src/agent/mod.rs` — implementation source
