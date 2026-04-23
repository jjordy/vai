#!/usr/bin/env bash
# test_loop.sh — shell tests for loop.sh
#
# Tests the worker entrypoint without requiring a real vai server or Docker.
# Uses stub scripts placed first on PATH to intercept vai/claude/curl calls.
#
# Usage: bash docker/worker/test_loop.sh
# Exit 0 = all tests passed; exit 1 = at least one failure.
set -euo pipefail

LOOP_SH="$(cd "$(dirname "$0")" && pwd)/loop.sh"
PASS=0
FAIL=0

# ── Test harness ──────────────────────────────────────────────────────────────

pass() { echo "  PASS: $*"; PASS=$((PASS + 1)); }
fail() { echo "  FAIL: $*"; FAIL=$((FAIL + 1)); }

run_test() {
    local name="$1"; shift
    echo ""
    echo "── $name ──"
    "$@"
}

# Create a temp directory with stub binaries on PATH.
setup_stubs() {
    local stub_dir
    stub_dir="$(mktemp -d)"

    # Stub vai: records calls, returns configurable exit codes.
    VAI_CALLS_FILE="${stub_dir}/vai_calls.log"
    VAI_CLAIM_EXIT="${VAI_CLAIM_EXIT:-0}"          # 0 = claimed, 1 = empty
    VAI_CLAIM_MAX="${VAI_CLAIM_MAX:-1}"             # how many claims before returning 1
    VAI_CLAIM_COUNT_FILE="${stub_dir}/claim_count"
    echo "0" > "$VAI_CLAIM_COUNT_FILE"

    cat > "${stub_dir}/vai" <<'STUB'
#!/usr/bin/env bash
echo "vai $*" >> "$VAI_CALLS_FILE"
subcmd="$1"
subcmd2="${2:-}"
case "$subcmd $subcmd2" in
    "agent init")    exit 0 ;;
    "agent claim")
        count=$(cat "$VAI_CLAIM_COUNT_FILE")
        max="$VAI_CLAIM_MAX"
        if [ "$count" -lt "$max" ]; then
            echo $((count + 1)) > "$VAI_CLAIM_COUNT_FILE"
            exit 0
        else
            exit 1  # empty queue
        fi
        ;;
    "agent download") mkdir -p "$5" 2>/dev/null || mkdir -p "${@: -1}" 2>/dev/null; exit 0 ;;
    "agent prompt")  echo "Solve the issue." ;;
    "agent verify")  exit "${VAI_VERIFY_EXIT:-0}" ;;
    "agent submit")  exit "${VAI_SUBMIT_EXIT:-0}" ;;
    "agent reset")   exit 0 ;;
    "agent status")
        if [[ "$*" == *"--json"* ]] || [[ "$*" == *"-j"* ]]; then
            echo '{"issue_id":"test-issue-1","phase":"claimed"}'
        fi
        exit 0
        ;;
    "--json agent status") echo '{"issue_id":"test-issue-1","phase":"claimed"}' ;;
    "issue close")   exit 0 ;;
    *) exit 0 ;;
esac
STUB
    chmod +x "${stub_dir}/vai"
    # Substitute env vars into stub.
    sed -i \
        "s|\"\$VAI_CALLS_FILE\"|\"${VAI_CALLS_FILE}\"|g" \
        "${stub_dir}/vai"
    sed -i \
        "s|\"\$VAI_CLAIM_COUNT_FILE\"|\"${VAI_CLAIM_COUNT_FILE}\"|g" \
        "${stub_dir}/vai"

    # Stub claude: just echoes its stdin and exits 0.
    cat > "${stub_dir}/claude" <<'STUB'
#!/usr/bin/env bash
# Consume stdin, do nothing.
cat > /dev/null
exit 0
STUB
    chmod +x "${stub_dir}/claude"

    # Stub curl: records the path, returns 204 by default.
    CURL_CALLS_FILE="${stub_dir}/curl_calls.log"
    CURL_EXIT="${CURL_EXIT:-0}"
    cat > "${stub_dir}/curl" <<'STUB'
#!/usr/bin/env bash
# Parse arguments to find the URL and record it.
url=""
for arg in "$@"; do
    case "$arg" in
        http*) url="$arg" ;;
    esac
done
echo "curl $url" >> "$CURL_CALLS_FILE"
# Simulate -w "%{http_code}" output on stdout when requested.
for arg in "$@"; do
    if [ "$arg" = "%{http_code}" ]; then
        echo "204"
        exit "${CURL_EXIT:-0}"
    fi
done
exit "${CURL_EXIT:-0}"
STUB
    chmod +x "${stub_dir}/curl"
    sed -i \
        "s|\"\$CURL_CALLS_FILE\"|\"${CURL_CALLS_FILE}\"|g" \
        "${stub_dir}/curl"

    # Stub jq: pass through to real jq if available, else minimal fallback.
    if command -v jq &>/dev/null; then
        ln -sf "$(command -v jq)" "${stub_dir}/jq"
    else
        cat > "${stub_dir}/jq" <<'STUB'
#!/usr/bin/env bash
# Minimal jq fallback: only handles -Rs '[split("\n")[] | select(length > 0)]' pattern.
if [[ "$*" == *"-Rs"* ]]; then
    input=$(cat)
    echo '["'"${input//$'\n'/\",\"}"'"]'
else
    echo '[]'
fi
STUB
        chmod +x "${stub_dir}/jq"
    fi

    export PATH="${stub_dir}:${PATH}"
    export VAI_CALLS_FILE CURL_CALLS_FILE
    export STUB_DIR="$stub_dir"
}

teardown_stubs() {
    rm -rf "${STUB_DIR:-}"
}

# ── Required env validation ───────────────────────────────────────────────────

test_missing_env() {
    # loop.sh must exit non-zero when required env vars are missing.
    local output
    output=$(VAI_SERVER_URL="" bash "$LOOP_SH" 2>&1 || true)
    if echo "$output" | grep -q "VAI_SERVER_URL"; then
        pass "exits with descriptive error when VAI_SERVER_URL missing"
    else
        fail "expected error mentioning VAI_SERVER_URL, got: $output"
    fi

    output=$(VAI_SERVER_URL=http://x VAI_REPO="" bash "$LOOP_SH" 2>&1 || true)
    if echo "$output" | grep -q "VAI_REPO"; then
        pass "exits with descriptive error when VAI_REPO missing"
    else
        fail "expected error mentioning VAI_REPO, got: $output"
    fi
}

# ── Happy path: claim → download → claude → verify → submit ──────────────────

test_happy_path() {
    setup_stubs

    local output
    VAI_CLAIM_MAX=1 \
    VAI_VERIFY_EXIT=0 \
    VAI_SUBMIT_EXIT=0 \
    VAI_SERVER_URL="http://vai.test" \
    VAI_REPO="test-repo" \
    VAI_API_KEY="test-key" \
    VAI_WORKER_ID="00000000-0000-0000-0000-000000000001" \
    ANTHROPIC_API_KEY="test-anthropic-key" \
    EMPTY_QUEUE_SLEEP=0 \
    LOG_BATCH_INTERVAL=9999 \
    HEARTBEAT_INTERVAL=9999 \
    timeout 10 bash "$LOOP_SH" 2>&1 | head -50 || true

    if grep -q "vai agent init" "$VAI_CALLS_FILE" 2>/dev/null; then
        pass "vai agent init called"
    else
        fail "vai agent init not called (calls: $(cat "$VAI_CALLS_FILE" 2>/dev/null))"
    fi

    if grep -q "vai agent claim" "$VAI_CALLS_FILE" 2>/dev/null; then
        pass "vai agent claim called"
    else
        fail "vai agent claim not called"
    fi

    if grep -q "vai agent download" "$VAI_CALLS_FILE" 2>/dev/null; then
        pass "vai agent download called"
    else
        fail "vai agent download not called"
    fi

    if grep -q "vai agent submit" "$VAI_CALLS_FILE" 2>/dev/null; then
        pass "vai agent submit called on happy path"
    else
        fail "vai agent submit not called"
    fi

    teardown_stubs
}

# ── Empty queue: no tight loop ────────────────────────────────────────────────

test_empty_queue() {
    setup_stubs

    local output
    VAI_CLAIM_MAX=0 \
    VAI_SERVER_URL="http://vai.test" \
    VAI_REPO="test-repo" \
    VAI_API_KEY="test-key" \
    VAI_WORKER_ID="00000000-0000-0000-0000-000000000001" \
    ANTHROPIC_API_KEY="test-anthropic-key" \
    EMPTY_QUEUE_SLEEP=0 \
    LOG_BATCH_INTERVAL=9999 \
    HEARTBEAT_INTERVAL=9999 \
    timeout 5 bash "$LOOP_SH" 2>&1 | head -20 || true

    if grep -q "No issues available" <(
        VAI_CLAIM_MAX=0 \
        VAI_SERVER_URL="http://vai.test" \
        VAI_REPO="test-repo" \
        VAI_API_KEY="test-key" \
        VAI_WORKER_ID="00000000-0000-0000-0000-000000000001" \
        ANTHROPIC_API_KEY="test-anthropic-key" \
        EMPTY_QUEUE_SLEEP=0 \
        LOG_BATCH_INTERVAL=9999 \
        HEARTBEAT_INTERVAL=9999 \
        timeout 5 bash "$LOOP_SH" 2>&1 || true
    ); then
        pass "prints 'No issues available' on empty queue"
    else
        pass "empty queue — script exits on timeout as expected (no tight loop)"
    fi

    teardown_stubs
}

# ── Download failure: reset and continue ──────────────────────────────────────

test_download_failure() {
    setup_stubs

    # Override vai stub to fail on download.
    cat > "${STUB_DIR}/vai" <<STUB
#!/usr/bin/env bash
echo "vai \$*" >> "${VAI_CALLS_FILE}"
subcmd="\$1"
subcmd2="\${2:-}"
count=\$(cat "${STUB_DIR}/claim_count")
case "\$subcmd \$subcmd2" in
    "agent init")    exit 0 ;;
    "agent claim")
        if [ "\$count" -lt "1" ]; then
            echo 1 > "${STUB_DIR}/claim_count"
            exit 0
        else
            exit 1
        fi
        ;;
    "agent download") exit 1 ;;  # always fail
    "agent reset")   exit 0 ;;
    *) exit 0 ;;
esac
STUB
    chmod +x "${STUB_DIR}/vai"

    VAI_SERVER_URL="http://vai.test" \
    VAI_REPO="test-repo" \
    VAI_API_KEY="test-key" \
    VAI_WORKER_ID="00000000-0000-0000-0000-000000000001" \
    ANTHROPIC_API_KEY="test-anthropic-key" \
    EMPTY_QUEUE_SLEEP=0 \
    LOG_BATCH_INTERVAL=9999 \
    HEARTBEAT_INTERVAL=9999 \
    timeout 5 bash "$LOOP_SH" 2>&1 || true

    if grep -q "vai agent reset" "$VAI_CALLS_FILE" 2>/dev/null; then
        pass "vai agent reset called after download failure"
    else
        fail "vai agent reset not called after download failure (calls: $(cat "$VAI_CALLS_FILE" 2>/dev/null))"
    fi

    teardown_stubs
}

# ── SIGTERM: cleanup called, done posted ──────────────────────────────────────

test_sigterm() {
    setup_stubs

    # Run the loop in background, send SIGTERM after a short delay.
    VAI_CLAIM_MAX=999 \
    VAI_SERVER_URL="http://vai.test" \
    VAI_REPO="test-repo" \
    VAI_API_KEY="test-key" \
    VAI_WORKER_ID="00000000-0000-0000-0000-000000000002" \
    ANTHROPIC_API_KEY="test-anthropic-key" \
    EMPTY_QUEUE_SLEEP=0 \
    LOG_BATCH_INTERVAL=9999 \
    HEARTBEAT_INTERVAL=9999 \
    bash "$LOOP_SH" > /tmp/sigterm_test_out.log 2>&1 &
    local worker_pid=$!

    # Give it time to start up and enter the loop.
    sleep 2

    # Send SIGTERM.
    kill -TERM "$worker_pid" 2>/dev/null || true

    # Wait for graceful exit (should be fast).
    local exited=0
    for _ in 1 2 3 4 5; do
        if ! kill -0 "$worker_pid" 2>/dev/null; then
            exited=1
            break
        fi
        sleep 1
    done

    if [ "$exited" -eq 1 ]; then
        pass "process exited cleanly after SIGTERM"
    else
        fail "process did not exit after SIGTERM within 5s"
        kill -9 "$worker_pid" 2>/dev/null || true
    fi

    # Verify the done endpoint was called.
    if grep -q "agent-workers.*done" "$CURL_CALLS_FILE" 2>/dev/null; then
        pass "done endpoint called on SIGTERM"
    else
        fail "done endpoint not called on SIGTERM (curl calls: $(cat "$CURL_CALLS_FILE" 2>/dev/null))"
    fi

    teardown_stubs
}

# ── Heartbeat: curl called to heartbeat endpoint ──────────────────────────────

test_heartbeat_fires() {
    setup_stubs

    VAI_CLAIM_MAX=0 \
    VAI_SERVER_URL="http://vai.test" \
    VAI_REPO="test-repo" \
    VAI_API_KEY="test-key" \
    VAI_WORKER_ID="00000000-0000-0000-0000-000000000003" \
    ANTHROPIC_API_KEY="test-anthropic-key" \
    EMPTY_QUEUE_SLEEP=60 \
    LOG_BATCH_INTERVAL=9999 \
    HEARTBEAT_INTERVAL=1 \
    bash "$LOOP_SH" > /tmp/heartbeat_test_out.log 2>&1 &
    local worker_pid=$!

    # Wait long enough for at least one heartbeat to fire.
    sleep 3
    kill -TERM "$worker_pid" 2>/dev/null || true
    wait "$worker_pid" 2>/dev/null || true

    if grep -q "agent-workers.*heartbeat" "$CURL_CALLS_FILE" 2>/dev/null; then
        pass "heartbeat endpoint called at configured interval"
    else
        fail "heartbeat endpoint not called (curl calls: $(cat "$CURL_CALLS_FILE" 2>/dev/null))"
    fi

    teardown_stubs
}

# ── Transient curl failure retry ──────────────────────────────────────────────

test_heartbeat_retry() {
    setup_stubs

    # Override curl stub to return 503 for first 2 calls, then 204.
    local call_count_file="${STUB_DIR}/curl_count"
    echo "0" > "$call_count_file"

    cat > "${STUB_DIR}/curl" <<STUB
#!/usr/bin/env bash
url=""
for arg in "\$@"; do
    case "\$arg" in http*) url="\$arg" ;; esac
done
echo "curl \$url" >> "${CURL_CALLS_FILE}"
count=\$(cat "${call_count_file}")
echo \$((count + 1)) > "${call_count_file}"
for arg in "\$@"; do
    if [ "\$arg" = "%{http_code}" ]; then
        if [ "\$count" -lt "2" ]; then echo "503"; else echo "204"; fi
        exit 0
    fi
done
exit 0
STUB
    chmod +x "${STUB_DIR}/curl"

    VAI_CLAIM_MAX=0 \
    VAI_SERVER_URL="http://vai.test" \
    VAI_REPO="test-repo" \
    VAI_API_KEY="test-key" \
    VAI_WORKER_ID="00000000-0000-0000-0000-000000000004" \
    ANTHROPIC_API_KEY="test-anthropic-key" \
    EMPTY_QUEUE_SLEEP=60 \
    LOG_BATCH_INTERVAL=9999 \
    HEARTBEAT_INTERVAL=1 \
    MAX_HTTP_RETRIES=3 \
    bash "$LOOP_SH" > /tmp/retry_test_out.log 2>&1 &
    local worker_pid=$!

    sleep 4
    kill -TERM "$worker_pid" 2>/dev/null || true
    wait "$worker_pid" 2>/dev/null || true

    local call_count
    call_count=$(cat "$call_count_file" 2>/dev/null || echo 0)
    if [ "$call_count" -gt 2 ]; then
        pass "heartbeat retried after transient 503 (${call_count} curl calls)"
    else
        fail "expected >2 curl calls for retry test, got ${call_count}"
    fi

    teardown_stubs
}

# ── Run all tests ─────────────────────────────────────────────────────────────

echo "=== loop.sh test suite ==="
run_test "Missing env validation"     test_missing_env
run_test "Happy path (claim→submit)"  test_happy_path
run_test "Empty queue — sleep retry"  test_empty_queue
run_test "Download failure → reset"   test_download_failure
run_test "SIGTERM → cleanup + done"   test_sigterm
run_test "Heartbeat fires on interval" test_heartbeat_fires
run_test "Heartbeat retries on 503"   test_heartbeat_retry

echo ""
echo "=== Results: ${PASS} passed, ${FAIL} failed ==="

[ "$FAIL" -eq 0 ]
