#!/usr/bin/env python3
"""Filter for claude --output-format stream-json stdout.

Reads NDJSON from stdin, writes human-readable summary lines to stdout.
Strips raw token-stream events; logs tool calls, token usage, and errors.

Claude Code SDK stream-json event types we care about:
  assistant — assistant turn; content[] may contain tool_use blocks
  result    — final summary with token usage and cost
  system    — init event with session_id
  user      — tool results (dropped; too verbose)
"""
import json
import sys


def _truncate(s: str, n: int = 200) -> str:
    return s[:n] + "..." if len(s) > n else s


def _process(event: dict) -> None:
    t = event.get("type", "")

    if t == "assistant":
        msg = event.get("message", {})
        for block in msg.get("content", []):
            if block.get("type") == "tool_use":
                name = block.get("name", "?")
                inp = _truncate(json.dumps(block.get("input", {}), separators=(",", ":")))
                print(f"[claude] tool: {name} {inp}", flush=True)

    elif t == "result":
        subtype = event.get("subtype", "success")
        turns = event.get("num_turns", "?")
        cost = event.get("total_cost_usd", "?")
        usage = event.get("usage", {})
        inp = usage.get("input_tokens", "?")
        out = usage.get("output_tokens", "?")
        cache_read = usage.get("cache_read_input_tokens", 0)
        print(
            f"[claude] result: subtype={subtype} turns={turns}"
            f" input_tokens={inp} output_tokens={out}"
            f" cache_read={cache_read} cost_usd={cost}",
            flush=True,
        )

    elif t == "system" and event.get("subtype") == "init":
        session = event.get("session_id", "?")
        print(f"[claude] init: session_id={session}", flush=True)

    # "user" (tool results) and partial text deltas are intentionally dropped.


def main() -> None:
    for raw in sys.stdin:
        line = raw.rstrip("\n")
        if not line:
            continue
        try:
            _process(json.loads(line))
        except (json.JSONDecodeError, KeyError, TypeError):
            # Non-JSON line (plain text fallback, error messages) — pass through.
            print(line, flush=True)


if __name__ == "__main__":
    try:
        main()
    except BrokenPipeError:
        pass
    sys.exit(0)
