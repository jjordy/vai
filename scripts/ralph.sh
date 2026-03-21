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
  echo "  CLAUDE_CODE_OAUTH_TOKEN  — run: claude config get oauthToken"
  echo "  GH_TOKEN                 — GitHub PAT with repo scope"
  exit 1
fi

source "$SANDCASTLE_DIR/.env"

if [ -z "$CLAUDE_CODE_OAUTH_TOKEN" ] || [ -z "$GH_TOKEN" ]; then
  echo "Error: CLAUDE_CODE_OAUTH_TOKEN and GH_TOKEN must be set in .sandcastle/.env"
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
  -v "$REPO_DIR:/home/agent/repo" \
  -e CLAUDE_CODE_OAUTH_TOKEN="$CLAUDE_CODE_OAUTH_TOKEN" \
  -e GH_TOKEN="$GH_TOKEN" \
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

  # Fetch open issues inside container
  OPEN_ISSUES=$(docker exec "$CONTAINER_NAME" bash -c "
    cd /home/agent/repo
    gh issue list --repo '$REPO' --label 'phase-1' --state open --json number,title,body,labels --limit 50
  ")

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

  docker exec "$CONTAINER_NAME" bash -c "
    cd /home/agent/repo
    echo $(printf '%q' "$FULL_PROMPT") | claude -p \
      --allowedTools 'Read,Edit,Write,Bash(git commit:*),Bash(cargo:*),Bash(gh issue:*),Glob,Grep'
  "

  CLAUDE_EXIT=$?

  echo ""

  if [ $CLAUDE_EXIT -ne 0 ]; then
    echo "Claude exited with code $CLAUDE_EXIT. Continuing to next iteration..."
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
