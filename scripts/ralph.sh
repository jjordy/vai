#!/bin/bash
set -eo pipefail

# ============================================================
# RALPH — Local Docker Loop
#
# Builds the sandcastle Docker image, mounts the repo, and
# runs Claude Code in a loop. Each iteration picks one issue,
# implements it, and commits. Commits land on the host repo
# via volume mount.
#
# Usage: ./scripts/ralph.sh [max_iterations]
# ============================================================

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
SANDCASTLE_DIR="$REPO_DIR/.sandcastle"
IMAGE_NAME="vai-ralph"
CONTAINER_NAME="vai-ralph-worker"

# Read max iterations from arg, config, or default to 100
MAX_ITERATIONS="${1:-$(jq -r '.defaultIterations // 100' "$SANDCASTLE_DIR/config.json" 2>/dev/null || echo 100)}"

# Check for required env
if [ ! -f "$SANDCASTLE_DIR/.env" ]; then
  echo "Error: .sandcastle/.env not found"
  echo "Copy .sandcastle/.env.example to .sandcastle/.env and fill in:"
  echo "  GH_TOKEN — GitHub PAT with repo scope"
  echo ""
  echo "CLAUDE_CODE_OAUTH_TOKEN is auto-read from ~/.claude/.credentials.json"
  exit 1
fi

source "$SANDCASTLE_DIR/.env"

# Auto-read Claude OAuth token from credentials file, fall back to .env
CLAUDE_CREDS="$HOME/.claude/.credentials.json"
if [ -f "$CLAUDE_CREDS" ]; then
  AUTO_TOKEN=$(jq -r '(.claudeAiOauth.accessToken // .oauthToken) // empty' "$CLAUDE_CREDS" 2>/dev/null || true)
  if [ -n "$AUTO_TOKEN" ]; then
    CLAUDE_CODE_OAUTH_TOKEN="$AUTO_TOKEN"
    echo "Using Claude OAuth token from ~/.claude/.credentials.json"
  fi
fi

if [ -z "$CLAUDE_CODE_OAUTH_TOKEN" ]; then
  echo "Error: Could not find Claude OAuth token."
  echo "Either set CLAUDE_CODE_OAUTH_TOKEN in .sandcastle/.env"
  echo "or ensure ~/.claude/.credentials.json exists (run 'claude' to authenticate)."
  exit 1
fi

if [ -z "$GH_TOKEN" ]; then
  echo "Error: GH_TOKEN must be set in .sandcastle/.env"
  exit 1
fi

# Ensure we're in a git repo with a GitHub remote
REPO=$(cd "$REPO_DIR" && gh repo view --json nameWithOwner -q .nameWithOwner 2>/dev/null)
if [ -z "$REPO" ]; then
  echo "Error: not in a GitHub-connected repo"
  exit 1
fi

echo "=== RALPH ==="
echo "Repo: $REPO"
echo "Max iterations: $MAX_ITERATIONS"
echo ""

# Build the Docker image
echo "Building Docker image..."
docker build -t "$IMAGE_NAME" "$SANDCASTLE_DIR"
echo ""

# Clean up any existing container
docker rm -f "$CONTAINER_NAME" 2>/dev/null || true

# Start container with repo mounted
echo "Starting container..."
docker run -d \
  --name "$CONTAINER_NAME" \
  --network host \
  -v "$REPO_DIR:/home/agent/repo" \
  -e CLAUDE_CODE_OAUTH_TOKEN="$CLAUDE_CODE_OAUTH_TOKEN" \
  -e GH_TOKEN="$GH_TOKEN" \
  -e VAI_TEST_DATABASE_URL="${VAI_TEST_DATABASE_URL:-postgres://vai:vai@localhost:5432/vai_test}" \
  "$IMAGE_NAME"

# Configure git and gh inside container
docker exec "$CONTAINER_NAME" bash -c "
  cd /home/agent/repo
  git config --global --add safe.directory /home/agent/repo
  echo '$GH_TOKEN' | gh auth login --with-token 2>/dev/null
  git config user.name 'ralph[bot]'
  git config user.email 'ralph[bot]@users.noreply.github.com'
"

echo "Container running. Starting loop..."
echo ""

# Main loop
for i in $(seq 1 "$MAX_ITERATIONS"); do
  echo "========================================="
  echo "  Iteration $i / $MAX_ITERATIONS"
  echo "========================================="
  echo ""

  # Fetch all open issues, sorted by priority label then issue number (lowest first)
  OPEN_ISSUES=$(docker exec "$CONTAINER_NAME" bash -c "
    cd /home/agent/repo
    gh issue list --repo '$REPO' --state open --json number,title,body,labels --limit 50
  " | jq 'sort_by(
    ((.labels // []) | map(.name) |
      if any(. == "priority:critical") then 0
      elif any(. == "priority:high") then 1
      elif any(. == "priority:medium") then 2
      elif any(. == "priority:low") then 3
      else 4 end),
    .number
  )')

  ISSUE_COUNT=$(echo "$OPEN_ISSUES" | jq length)

  if [ "$ISSUE_COUNT" -eq 0 ]; then
    echo "No open issues remaining. RALPH is done!"
    break
  fi

  echo "Open issues: $ISSUE_COUNT"
  echo ""

  # Read the prompt template
  PROMPT=$(cat "$SANDCASTLE_DIR/prompt.md")

  # Get recent RALPH commits
  RECENT_COMMITS=$(docker exec "$CONTAINER_NAME" bash -c "
    cd /home/agent/repo
    git log --oneline -10 --grep='RALPH:' 2>/dev/null || echo 'none'
  ")

  # Build the full prompt
  FULL_PROMPT="$PROMPT

## OPEN ISSUES
\`\`\`json
$OPEN_ISSUES
\`\`\`

## RECENT RALPH COMMITS
\`\`\`
$RECENT_COMMITS
\`\`\`"

  # Run Claude Code inside the container
  echo "Running Claude Code..."
  echo ""

  MAX_FIX_ATTEMPTS="${MAX_FIX_ATTEMPTS:-5}"
  PASSED=false
  ERRORS=""

  for attempt in $(seq 1 "$MAX_FIX_ATTEMPTS"); do
    echo "--- Attempt $attempt / $MAX_FIX_ATTEMPTS ---"
    echo ""

    if [ "$attempt" -eq 1 ]; then
      echo "$FULL_PROMPT" | docker exec -i "$CONTAINER_NAME" tee /tmp/ralph_prompt.md > /dev/null
      docker exec -i "$CONTAINER_NAME" bash -c "
        cd /home/agent/repo
        cat /tmp/ralph_prompt.md | claude -p \
          --allowedTools 'Read,Edit,Write,Bash,Glob,Grep'
      "
    else
      printf "Fix these errors. Run the failing checks yourself to verify they pass before finishing.\n\n%s" "$ERRORS" \
        | docker exec -i "$CONTAINER_NAME" bash -c "
          cd /home/agent/repo
          claude -p --allowedTools 'Read,Edit,Write,Bash,Glob,Grep'
        "
    fi

    CLAUDE_EXIT=$?
    if [ $CLAUDE_EXIT -ne 0 ]; then
      ERRORS="Claude exited with non-zero code $CLAUDE_EXIT. The implementation may be incomplete."
      echo "Claude exited with code $CLAUDE_EXIT."
      continue
    fi

    # ── Verify ──────────────────────────────────────────────────
    echo ""
    echo "Running quality checks..."
    ERRORS=""
    ALL_PASSED=true

    # 1. Clippy (full features)
    echo "  [1/3] Clippy..."
    CLIPPY_OUTPUT=$(docker exec "$CONTAINER_NAME" bash -c "cd /home/agent/repo && cargo clippy --features full -- -D warnings 2>&1")
    CLIPPY_EXIT=$?
    if [ $CLIPPY_EXIT -ne 0 ]; then
      ALL_PASSED=false
      ERRORS="${ERRORS}
=== cargo clippy --features full ===
${CLIPPY_OUTPUT}
"
      echo "  FAIL"
    else
      echo "  PASS"
    fi

    # 2. Test (full features)
    echo "  [2/3] Tests (full features)..."
    TEST_OUTPUT=$(docker exec "$CONTAINER_NAME" bash -c "cd /home/agent/repo && cargo test --features full 2>&1")
    TEST_EXIT=$?
    if [ $TEST_EXIT -ne 0 ]; then
      ALL_PASSED=false
      TEST_TAIL=$(echo "$TEST_OUTPUT" | tail -50)
      ERRORS="${ERRORS}
=== cargo test --features full ===
${TEST_TAIL}
"
      echo "  FAIL"
    else
      echo "  PASS"
    fi

    # 3. Test (CLI only)
    echo "  [3/3] Tests (CLI only)..."
    CLI_OUTPUT=$(docker exec "$CONTAINER_NAME" bash -c "cd /home/agent/repo && cargo test 2>&1")
    CLI_EXIT=$?
    if [ $CLI_EXIT -ne 0 ]; then
      ALL_PASSED=false
      CLI_TAIL=$(echo "$CLI_OUTPUT" | tail -50)
      ERRORS="${ERRORS}
=== cargo test ===
${CLI_TAIL}
"
      echo "  FAIL"
    else
      echo "  PASS"
    fi

    if [ "$ALL_PASSED" = true ]; then
      echo ""
      echo "All checks passed!"
      PASSED=true
      break
    else
      echo ""
      echo "Checks failed. Will retry (attempt $attempt of $MAX_FIX_ATTEMPTS)."
    fi
  done

  if [ "$PASSED" != true ]; then
    echo ""
    echo "Failed after $MAX_FIX_ATTEMPTS attempts. Reverting uncommitted changes."
    docker exec "$CONTAINER_NAME" bash -c "cd /home/agent/repo && git checkout -- . && git clean -fd"
    echo ""
    continue
  fi

  echo "Iteration $i complete."
  echo ""
done

# Cleanup
echo "Stopping container..."
docker rm -f "$CONTAINER_NAME" 2>/dev/null || true

echo ""
echo "=== RALPH complete ==="
echo "Check 'git log' for commits."
