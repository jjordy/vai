#!/usr/bin/env bash
# docker/worker/loop.sh — vai cloud worker entrypoint (PRD 28 Phase 2)
#
# Drives the agent lifecycle inside the canonical vai-worker image:
#   bootstrap → init → claim loop → heartbeat (bg) → log shipping (bg) → SIGTERM cleanup
#
# Required environment variables:
#   VAI_SERVER_URL   — vai server base URL (e.g. https://vai.example.com)
#   VAI_REPO         — repository name on the server
#   VAI_API_KEY      — vai agent API key (never written to disk)
#   VAI_WORKER_ID    — UUID assigned by the orchestrator at spawn time
#   ANTHROPIC_API_KEY — required by Claude Code; injected by the orchestrator
#
# Optional environment variables:
#   HEARTBEAT_INTERVAL      — seconds between heartbeat POSTs (default: 30)
#   HEARTBEAT_MAX_FAILURES  — consecutive heartbeat failures before self-terminating (default: 10)
#   LOG_BATCH_INTERVAL      — seconds between log batch POSTs (default: 5)
#   EMPTY_QUEUE_SLEEP       — seconds to sleep when no issues are available (default: 30)
#   MAX_HTTP_RETRIES        — max retries for transient HTTP failures (default: 5)
#   DEV_SERVER_TIMEOUT_SECS — seconds to wait for dev server ready before aborting (default: 180)
#   DEV_SERVER_MAX_CONSECUTIVE_FAILURES — exit after N consecutive dev-server-timeout iterations (default: 3)
#   VAI_WORKER_STALE_SECS   — server-side: seconds without heartbeat before worker is reaped (default: 900)
#                             bump this if verify steps (tsc, test suites) take longer than 15 min
set -euo pipefail

# ── Bootstrap ─────────────────────────────────────────────────────────────────

: "${VAI_SERVER_URL:?Required: VAI_SERVER_URL (e.g. https://vai.example.com)}"
: "${VAI_REPO:?Required: VAI_REPO (repository name on the server)}"
: "${VAI_API_KEY:?Required: VAI_API_KEY (vai agent API key)}"
: "${VAI_WORKER_ID:?Required: VAI_WORKER_ID (UUID assigned by the orchestrator)}"
: "${ANTHROPIC_API_KEY:?Required: ANTHROPIC_API_KEY (injected by the orchestrator)}"

HEARTBEAT_INTERVAL="${HEARTBEAT_INTERVAL:-30}"
HEARTBEAT_MAX_FAILURES="${HEARTBEAT_MAX_FAILURES:-10}"
LOG_BATCH_INTERVAL="${LOG_BATCH_INTERVAL:-5}"
EMPTY_QUEUE_SLEEP="${EMPTY_QUEUE_SLEEP:-30}"
MAX_HTTP_RETRIES="${MAX_HTTP_RETRIES:-5}"
DEV_SERVER_TIMEOUT_SECS="${DEV_SERVER_TIMEOUT_SECS:-180}"
DEV_SERVER_MAX_CONSECUTIVE_FAILURES="${DEV_SERVER_MAX_CONSECUTIVE_FAILURES:-3}"
DEV_SERVER_CONSECUTIVE_FAILURES=0

WORK_DIR="${HOME}/work"
# Capture all output to a log file so the background shipper can read it.
LOG_FILE="$(mktemp /tmp/vai-worker-XXXXXX.log)"
LOG_SHIPPED_LINES=0  # last line number successfully shipped
DONE_POSTED=0
HEARTBEAT_PID=""
LOG_SHIP_PID=""
DEV_PID=""

# Tee all stdout/stderr through the log file so the shipping loop can read it.
exec > >(tee -a "$LOG_FILE") 2>&1

echo "[worker] Starting vai cloud worker ${VAI_WORKER_ID}"
echo "[worker] Server: ${VAI_SERVER_URL}  Repo: ${VAI_REPO}"

# ── HTTP helper ───────────────────────────────────────────────────────────────

# api_post <path> [json_body]
# Returns 0 on 2xx. Retries on transient errors (408, 429, 5xx, network errors)
# with exponential backoff. Returns 1 on non-retryable errors or after exhausting
# retries.
api_post() {
    local path="$1"
    local body="${2:-\{\}}"
    local attempt=0
    local delay=1

    while [ "$attempt" -lt "$MAX_HTTP_RETRIES" ]; do
        local http_code
        http_code=$(curl -s -w "%{http_code}" -o /dev/null \
            -X POST \
            -H "Authorization: Bearer ${VAI_API_KEY}" \
            -H "Content-Type: application/json" \
            --max-time 10 \
            -d "$body" \
            "${VAI_SERVER_URL}${path}" 2>/dev/null) || http_code="0"

        case "$http_code" in
            2*)
                return 0
                ;;
            408|429|500|502|503|504)
                attempt=$((attempt + 1))
                echo "[worker] api_post ${path}: HTTP ${http_code}, retry ${attempt}/${MAX_HTTP_RETRIES} in ${delay}s" >&2
                sleep "$delay"
                delay=$((delay * 2))
                ;;
            0)
                # Network / timeout error — treat as transient.
                attempt=$((attempt + 1))
                echo "[worker] api_post ${path}: network error, retry ${attempt}/${MAX_HTTP_RETRIES} in ${delay}s" >&2
                sleep "$delay"
                delay=$((delay * 2))
                ;;
            *)
                # Non-retryable client error (4xx).
                echo "[worker] api_post ${path}: non-retryable HTTP ${http_code}" >&2
                return 1
                ;;
        esac
    done

    echo "[worker] api_post ${path}: exhausted ${MAX_HTTP_RETRIES} retries" >&2
    return 1
}

# ── Heartbeat loop (background) ───────────────────────────────────────────────

heartbeat_loop() {
    local failures=0
    while true; do
        sleep "$HEARTBEAT_INTERVAL"
        if api_post "/api/agent-workers/${VAI_WORKER_ID}/heartbeat" '{}'; then
            failures=0
        else
            failures=$((failures + 1))
            echo "[worker] heartbeat failed (${failures}/${HEARTBEAT_MAX_FAILURES} consecutive)" >&2
            if [ "$failures" -ge "$HEARTBEAT_MAX_FAILURES" ]; then
                echo "[worker] heartbeat failed ${HEARTBEAT_MAX_FAILURES} times — terminating worker" >&2
                kill "$$"
                return
            fi
        fi
    done
}

# ── Log shipping loop (background) ────────────────────────────────────────────

ship_logs() {
    local current_lines
    current_lines=$(wc -l < "$LOG_FILE" 2>/dev/null || echo 0)

    if [ "$current_lines" -le "$LOG_SHIPPED_LINES" ]; then
        return 0
    fi

    local new_lines
    # Extract lines after the last shipped line.
    new_lines=$(tail -n +$((LOG_SHIPPED_LINES + 1)) "$LOG_FILE" \
        | head -n 200 \
        | sed 's/\\/\\\\/g' | sed 's/"/\\"/g')

    if [ -z "$new_lines" ]; then
        return 0
    fi

    # Build JSON array of strings.  Each line becomes one chunk.
    local chunks
    chunks=$(printf '%s\n' "$new_lines" | jq -Rs '[split("\n")[] | select(length > 0)]')

    if [ "$chunks" = "[]" ] || [ -z "$chunks" ]; then
        LOG_SHIPPED_LINES="$current_lines"
        return 0
    fi

    local payload
    payload=$(jq -n --argjson chunks "$chunks" '{"stream":"stdout","chunks":$chunks}')

    if api_post "/api/agent-workers/${VAI_WORKER_ID}/logs" "$payload"; then
        LOG_SHIPPED_LINES="$current_lines"
    else
        echo "[worker] log shipping failed, will retry next interval" >&2
    fi
}

log_ship_loop() {
    while true; do
        sleep "$LOG_BATCH_INTERVAL"
        ship_logs || true
    done
}

# ── Dev server cleanup (per iteration) ───────────────────────────────────────

cleanup_iteration() {
    if [ -n "${DEV_PID:-}" ]; then
        echo "[worker] stopping dev server PID=${DEV_PID}"
        kill "$DEV_PID" 2>/dev/null || true
        wait "$DEV_PID" 2>/dev/null || true
        DEV_PID=""
    fi
}

# ── SIGTERM / SIGINT handler ──────────────────────────────────────────────────

cleanup() {
    # Stop the dev server before shutting down.
    cleanup_iteration

    if [ "$DONE_POSTED" -eq 0 ]; then
        DONE_POSTED=1
        echo "[worker] Received termination signal — flushing logs and posting done"

        # Final log flush.
        ship_logs || true

        # Notify the server this worker is terminating.
        api_post "/api/agent-workers/${VAI_WORKER_ID}/done" \
            '{"reason":"terminated"}' \
            || echo "[worker] done POST failed (continuing shutdown)" >&2
    fi

    # Stop background loops.
    [ -n "$HEARTBEAT_PID" ] && kill "$HEARTBEAT_PID" 2>/dev/null || true
    [ -n "$LOG_SHIP_PID" ] && kill "$LOG_SHIP_PID" 2>/dev/null || true

    exit 0
}

trap cleanup SIGTERM SIGINT EXIT

# ── MCP config (written once, reused each iteration) ─────────────────────────

cat > /tmp/mcp-config.json << 'MCPEOF'
{
  "mcpServers": {
    "playwright": {
      "command": "npx",
      "args": ["@playwright/mcp", "--headless", "--browser", "chromium"]
    }
  }
}
MCPEOF
echo "[worker] MCP config written to /tmp/mcp-config.json"

ALLOWED_TOOLS="Read,Edit,Write,Bash,Glob,Grep,\
mcp__playwright__browser_navigate,\
mcp__playwright__browser_screenshot,\
mcp__playwright__browser_click,\
mcp__playwright__browser_type,\
mcp__playwright__browser_fill,\
mcp__playwright__browser_select_option,\
mcp__playwright__browser_hover,\
mcp__playwright__browser_press_key,\
mcp__playwright__browser_snapshot,\
mcp__playwright__browser_wait_for,\
mcp__playwright__browser_close"

# ── Initialization ────────────────────────────────────────────────────────────

vai agent init --server "${VAI_SERVER_URL}" --repo "${VAI_REPO}"

# Start background loops.
heartbeat_loop &
HEARTBEAT_PID=$!

log_ship_loop &
LOG_SHIP_PID=$!

# ── Main claim loop ───────────────────────────────────────────────────────────

echo "[worker] Starting claim loop"

while true; do
    if ! vai agent claim; then
        echo "[worker] No issues available — sleeping ${EMPTY_QUEUE_SLEEP}s before retry"
        sleep "$EMPTY_QUEUE_SLEEP"
        continue
    fi

    echo "[worker] Issue claimed"

    # Download the repo snapshot.
    if ! vai agent download "${WORK_DIR}"; then
        echo "[worker] Download failed — resetting workspace" >&2
        vai agent reset || true
        rm -rf "${WORK_DIR}"
        continue
    fi

    # ── Pre-agent setup ───────────────────────────────────────────────────────

    # Run [agent].setup commands from vai.toml (e.g. pnpm install, cargo build).
    # Exit codes: 0 = setup ran and passed, 1 = setup ran but failed,
    # 2 = no [agent].setup configured in vai.toml.
    echo "[worker] Running vai.toml [agent].setup commands"
    setup_exit=0
    vai agent setup "${WORK_DIR}" || setup_exit=$?

    # Only run implicit pnpm install when the repo has NOT declared its own
    # [agent].setup (exit 2). If setup WAS declared, trust it — running pnpm
    # install a second time wastes 1-3 minutes on cold workers.
    if [ "$setup_exit" -eq 2 ] && [ -f "${WORK_DIR}/package.json" ]; then
        echo "[worker] No [agent].setup in vai.toml — installing JS dependencies (pnpm install)"
        (cd "${WORK_DIR}" && pnpm install --silent 2>/dev/null) || true
    fi

    # ── Dev server ────────────────────────────────────────────────────────────

    # Start a background dev server for repos that expose a 'dev' npm script.
    # The server runs inside WORK_DIR so hot-reload and e2e tests find files.
    DEV_PID=""
    if [ -f "${WORK_DIR}/package.json" ] && \
       jq -e '.scripts.dev' "${WORK_DIR}/package.json" >/dev/null 2>&1; then
        echo "[worker] Starting dev server (pnpm dev)"
        (cd "${WORK_DIR}" && pnpm dev > /tmp/dev-server.log 2>&1) &
        DEV_PID=$!
        echo "[worker] Dev server started, PID=${DEV_PID}"

        # Wait up to DEV_SERVER_TIMEOUT_SECS for the dev server to respond.
        dev_ready=0
        for i in $(seq 1 "$DEV_SERVER_TIMEOUT_SECS"); do
            if curl -sS -o /dev/null -w '%{http_code}' http://localhost:3000 2>/dev/null \
               | grep -qE '^(200|302|307)$'; then
                echo "[worker] Dev server ready after ${i}s"
                dev_ready=1
                break
            fi
            sleep 1
        done
        if [ "$dev_ready" -eq 0 ]; then
            DEV_SERVER_CONSECUTIVE_FAILURES=$((DEV_SERVER_CONSECUTIVE_FAILURES + 1))
            echo "[worker] Dev server did not respond within ${DEV_SERVER_TIMEOUT_SECS}s (failure ${DEV_SERVER_CONSECUTIVE_FAILURES}/${DEV_SERVER_MAX_CONSECUTIVE_FAILURES})" >&2
            cleanup_iteration
            vai agent reset || true
            rm -rf "${WORK_DIR}"
            if [ "$DEV_SERVER_CONSECUTIVE_FAILURES" -ge "$DEV_SERVER_MAX_CONSECUTIVE_FAILURES" ]; then
                echo "[worker] Too many consecutive dev server failures — exiting" >&2
                exit 2
            fi
            continue
        fi
        # Reset failure counter on successful dev server start.
        DEV_SERVER_CONSECUTIVE_FAILURES=0
    fi

    # ── Agent invocation ──────────────────────────────────────────────────────

    # Run Claude Code with the vai-generated prompt.
    # Playwright MCP browser tools are included so claude can explore and verify
    # UI changes. The --mcp-config file tells claude how to start the MCP server.
    vai agent prompt | claude -p \
        --model sonnet \
        --allowedTools "${ALLOWED_TOOLS}" \
        --mcp-config /tmp/mcp-config.json \
        || true

    # If Claude submitted via the Bash tool, agent state is already cleared.
    # Skip loop.sh's verify+submit to avoid a spurious "Submit failed" log line.
    if ! vai agent status >/dev/null 2>&1; then
        echo "[worker] Agent state already cleared by claude — skipping verify+submit"
        cleanup_iteration
        rm -rf "${WORK_DIR}"
        continue
    fi

    # Verify quality checks (if configured in vai.toml).
    if vai agent verify "${WORK_DIR}"; then
        submit_exit=0
        vai agent submit --close-if-empty "${WORK_DIR}" || submit_exit=$?
        case $submit_exit in
            0)
                echo "[worker] Submitted successfully"
                ;;
            *)
                echo "[worker] Submit failed (exit ${submit_exit}) — resetting" >&2
                vai agent reset || true
                ;;
        esac
    else
        echo "[worker] Verify failed — resetting workspace" >&2
        vai agent reset || true
    fi

    cleanup_iteration
    rm -rf "${WORK_DIR}"
done
