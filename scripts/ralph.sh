#!/bin/bash
# RALPH — vai agent loop
#
# Runs Claude Code in a loop against vai issues via the vai agent CLI.
# Requires: vai (in PATH), claude (Claude Code CLI), GH_TOKEN env var.
#
# Usage:
#   ./scripts/ralph.sh                    # run up to 100 iterations
#   ./scripts/ralph.sh 10                 # run up to 10 iterations
#   MAX_ITERATIONS=50 ./scripts/ralph.sh  # via env var
#
# Docker (optional): wrap in a container with the repo volume-mounted:
#   docker run --rm -v "$(pwd):/home/agent/repo" \
#     -e CLAUDE_CODE_OAUTH_TOKEN -e GH_TOKEN \
#     vai-ralph ./scripts/ralph.sh

set -euo pipefail

MAX_ITERATIONS="${1:-${MAX_ITERATIONS:-100}}"
WORK_DIR="ralph-work"
COUNT=0

while [ "$COUNT" -lt "$MAX_ITERATIONS" ] && vai agent claim; do
  COUNT=$((COUNT + 1))
  echo "=== Iteration $COUNT / $MAX_ITERATIONS ==="
  vai agent download "$WORK_DIR"
  vai agent prompt | claude --dangerously-skip-permissions -p
  vai agent verify "$WORK_DIR" && vai agent submit "$WORK_DIR" || vai agent reset
  rm -rf "$WORK_DIR"
done

echo "RALPH complete after $COUNT iteration(s)."
