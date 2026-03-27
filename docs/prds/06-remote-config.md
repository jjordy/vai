# Phase 6: Remote Configuration — Transparent Local/Remote CLI

## Summary

Add remote server configuration to `.vai/config.toml` so that CLI commands automatically proxy to a remote vai server when configured. The CLI becomes a universal interface — same commands work locally or against a remote, like `git` with remotes.

## Motivation

Today, using vai against a remote server requires direct HTTP/WebSocket calls. A developer or orchestration script should be able to run `vai issue list` and have it transparently hit the remote server if one is configured, or operate on the local `.vai/` directory if not.

This mirrors the git model: `git log` works locally, `git push` works against a remote, and the user doesn't think about the transport layer.

## Requirements

### 6.1: Remote Configuration

Add a `[remote]` section to `.vai/config.toml`:

```toml
[remote]
url = "https://vai.example.com"
api_key = "vai_key_..."
```

Add CLI commands to manage the remote:

- `vai remote add <url> --key <api-key>` — set the remote server URL and API key
- `vai remote remove` — remove the remote configuration
- `vai remote status` — show current remote config and test connectivity (ping `/api/status`)

### 6.2: Transparent Proxying

When a remote is configured, CLI commands that have API equivalents should proxy to the remote server instead of operating on the local `.vai/` directory. Commands affected:

- `vai issue list/create/show/update/close`
- `vai workspace create/list/submit/discard`
- `vai work-queue list/claim`
- `vai status`
- `vai log` / `vai show` / `vai diff`
- `vai graph show/query/infer`
- `vai escalations list/resolve`

Commands that always operate locally:
- `vai init`
- `vai remote add/remove/status`
- `vai dashboard` (connects to remote WebSocket if configured)

### 6.3: Local Override

Add a `--local` flag that forces any command to operate on the local `.vai/` directory, ignoring the remote config. Useful for debugging or when the remote is down.

```bash
vai issue list           # hits remote if configured
vai issue list --local   # always reads local .vai/
```

### 6.4: API Key Storage

The API key should support:
1. Direct in config: `api_key = "vai_key_..."`
2. Environment variable reference: `api_key_env = "VAI_API_KEY"`
3. Command reference: `api_key_cmd = "pass show vai/api-key"`

Only one of the three should be set. Evaluated in order: env var, command, direct.

## Out of Scope

- Multiple remotes (only one remote supported, like `origin`)
- Automatic sync/push (user explicitly runs `vai sync`)
- Remote authentication beyond API keys (OAuth, SSO)
- Conflict resolution between local and remote state

## Issues

1. **Add `[remote]` section to config and `vai remote` CLI commands** — Parse remote config from `.vai/config.toml`, implement `vai remote add/remove/status` commands. Priority: high.

2. **Implement HTTP client for remote API proxying** — Create a shared HTTP client module that CLI commands can use to forward requests to the remote server. Handle auth, errors, and JSON serialization. Priority: high.

3. **Wire CLI commands to proxy through remote when configured** — For each command that has an API equivalent, check for remote config and route through the HTTP client instead of local `.vai/` operations. Priority: high.

4. **Add `--local` flag to force local operation** — Global CLI flag that bypasses remote config. Priority: medium.

5. **Implement flexible API key storage (env var, command, direct)** — Support `api_key`, `api_key_env`, and `api_key_cmd` in the remote config section. Priority: medium.
