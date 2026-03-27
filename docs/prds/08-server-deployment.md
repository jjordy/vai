# Phase 8: Server Deployment & Multi-Repo Hosting

## Summary

Make the vai server deployable as a standalone hosted service that manages multiple repositories — similar to how GitHub hosts repos. This transforms vai from a local dev tool into a platform that teams and AI orchestrators connect to remotely.

## Motivation

Today the vai server is a per-repo local process bound to `127.0.0.1`. To serve as a remote platform (the "GitHub for AI agents" vision), it needs to:
- Bind to public network interfaces
- Host multiple repositories under a single server instance
- Persist across restarts
- Support configurable storage and server options

## Requirements

### 8.1: Configurable Bind Address and Port

The server currently binds to `127.0.0.1` with a random port (for testing) or a hardcoded default. Add:

- `vai server start --host <addr> --port <port>` — CLI flags to set bind address and port
- `[server]` section in a server config file for persistent settings:
  ```toml
  [server]
  host = "0.0.0.0"
  port = 7865
  ```
- Default: `127.0.0.1:7865` (localhost only, backward compatible)

### 8.2: Multi-Repo Support

The server should host multiple repositories. Route structure:

```
POST   /api/repos                           — create/register a repo
GET    /api/repos                           — list repos
GET    /api/repos/:repo/status              — repo status
GET    /api/repos/:repo/workspaces          — list workspaces
POST   /api/repos/:repo/workspaces          — create workspace
POST   /api/repos/:repo/workspaces/:id/submit — submit workspace
GET    /api/repos/:repo/versions            — version history
GET    /api/repos/:repo/issues              — list issues
POST   /api/repos/:repo/issues              — create issue
GET    /api/repos/:repo/graph/entities      — graph entities
GET    /api/repos/:repo/work-queue          — work queue
GET    /api/repos/:repo/escalations         — escalations
WS     /api/repos/:repo/ws/events           — WebSocket stream
```

Each repo has its own `.vai/` directory under a server-managed storage root:
```
/var/vai/repos/
  owner/repo-name/
    .vai/
    <source files>
```

The existing single-repo endpoints should continue to work for backward compatibility when the server is started with `vai server start` from within a repo directory (legacy mode).

### 8.3: Server Storage Configuration

Add a server-level config file at `~/.vai/server.toml` (or `/etc/vai/server.toml`):

```toml
[server]
host = "0.0.0.0"
port = 7865
storage_root = "/var/vai/repos"

[auth]
# Future: admin keys, org management
```

### 8.4: Repository Registration

When running in multi-repo mode, repos must be registered before they can be used:

- `POST /api/repos` with `{ "name": "my-project" }` — creates the directory structure and initializes `.vai/`
- `vai clone vai://<host>:<port>/<repo-name>` — should work against multi-repo servers
- The server manages the storage lifecycle (create, archive, delete)

### 8.5: Health Check and Monitoring

Add operational endpoints:
- `GET /health` — simple health check (returns 200 OK)
- `GET /api/server/stats` — server-level statistics (repo count, total workspaces, uptime, memory usage)

### 8.6: Graceful Shutdown and Process Management

- Handle SIGTERM/SIGINT gracefully — finish in-flight requests, close WebSocket connections
- Log startup/shutdown to stdout
- Support running as a systemd service (provide example unit file)
- PID file support for process management (`--pid-file <path>`)

## Out of Scope

- User/organization management (future PRD — for now, API keys are per-server)
- TLS termination (use a reverse proxy like nginx/caddy)
- Horizontal scaling / replication
- Repo deletion / garbage collection
- Rate limiting
- Billing / quotas

## Issues

1. **Add `--host` and `--port` flags to `vai server start`** — Allow binding to a configurable address and port. Default to `127.0.0.1:7865`. Read from `[server]` config if present. Priority: high.

2. **Implement multi-repo routing** — Add `/api/repos/:repo/` prefix to all existing endpoints. Each repo resolves to its own `.vai/` directory under the storage root. Keep legacy single-repo mode working when started from within a repo. Priority: high.

3. **Add server-level config file** — Support `~/.vai/server.toml` with host, port, and storage_root settings. Parse on startup, CLI flags override config values. Priority: high.

4. **Implement repository registration endpoint** — `POST /api/repos` creates a new repo with `vai init` under the storage root. `GET /api/repos` lists all registered repos. Priority: high.

5. **Add health check and server stats endpoints** — `GET /health` returns 200. `GET /api/server/stats` returns repo count, total workspaces, uptime. Priority: medium.

6. **Add graceful shutdown and systemd support** — Handle SIGTERM/SIGINT, finish in-flight requests, provide example systemd unit file and `--pid-file` flag. Priority: medium.
