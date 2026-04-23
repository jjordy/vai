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
#   HEARTBEAT_INTERVAL   — seconds between heartbeat POSTs (default: 30)
#   LOG_BATCH_INTERVAL   — seconds between log batch POSTs (default: 5)
#   EMPTY_QUEUE_SLEEP    — seconds to sleep when no issues are available (default: 30)
#   MAX_HTTP_RETRIES     — max retries for transient HTTP failures (default: 5)
set -euo pipefail

# ── Bootstrap ─────────────────────────────────────────────────────────────────

: "${VAI_SERVER_URL:?Required: VAI_SERVER_URL (e.g. https://vai.example.com)}"
: "${VAI_REPO:?Required: VAI_REPO (repository name on the server)}"
: "${VAI_API_KEY:?Required: VAI_API_KEY (vai agent API key)}"
: "${VAI_WORKER_ID:?Required: VAI_WORKER_ID (UUID assigned by the orchestrator)}"
: "${ANTHROPIC_API_KEY:?Required: ANTHROPIC_API_KEY (injected by the orchestrator)}"

HEARTBEAT_INTERVAL="${HEARTBEAT_INTERVAL:-30}"
LOG_BATCH_INTERVAL="${LOG_BATCH_INTERVAL:-5}"
EMPTY_QUEUE_SLEEP="${EMPTY_QUEUE_SLEEP:-30}"
MAX_HTTP_RETRIES="${MAX_HTTP_RETRIES:-5}"

WORK_DIR="${HOME}/work"
# Capture all output to a log file so the background shipper can read it.
LOG_FILE="$(mktemp /tmp/vai-worker-XXXXXX.log)"
LOG_SHIPPED_LINES=0  # last line number successfully shipped
DONE_POSTED=0
HEARTBEAT_PID=""
LOG_SHIP_PID=""

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
    while true; do
        sleep "$HEARTBEAT_INTERVAL"
        api_post "/api/agent-workers/${VAI_WORKER_ID}/heartbeat" '{}' \
            || echo "[worker] heartbeat failed (continuing)" >&2
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

# ── SIGTERM / SIGINT handler ──────────────────────────────────────────────────

cleanup() {
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

trap cleanup SIGTERM SIGINT

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

    # Run Claude Code with the vai-generated prompt.
    vai agent prompt | claude -p \
        --allowedTools 'Read,Edit,Write,Bash,Glob,Grep' \
        || true

    # Verify quality checks (if configured in vai.toml).
    if vai agent verify "${WORK_DIR}"; then
        submit_exit=0
        vai agent submit "${WORK_DIR}" || submit_exit=$?
        case $submit_exit in
            0)
                echo "[worker] Submitted successfully"
                ;;
            3)
                # Workspace was empty — issue already resolved, close it.
                echo "[worker] Workspace empty — closing issue as already resolved"
                ISSUE_ID=$(vai --json agent status 2>/dev/null | jq -r '.issue_id // empty')
                if [ -n "$ISSUE_ID" ]; then
                    vai issue close "$ISSUE_ID" --resolution resolved || true
                fi
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

    rm -rf "${WORK_DIR}"
done
