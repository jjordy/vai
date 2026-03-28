# Phase 13: Security Hardening

> **Execution order:** This PRD should be implemented AFTER PRD 13 (storage purity), PRD 15 (issue improvements), and PRD 18 (feature flags) are complete. Security hardening is most effective once the feature set is stable.

## Summary

Ensure the vai platform is secure for multi-tenant hosted deployment. Cover authentication, authorization, data isolation, input validation, secrets management, and protection against common attack vectors.

## Motivation

vai is transitioning from a local developer tool to a hosted platform where multiple organizations store source code and AI agents operate autonomously. A security breach could expose proprietary code, allow unauthorized merges, or let one tenant access another's data. Security must be treated as a first-class concern across every layer.

## Requirements

### 13.1: API Input Validation

All API endpoints must validate input before processing:

- **String length limits** — issue titles (500 chars), issue bodies (50KB), intent text (1000 chars), file paths (1000 chars), labels (100 chars each, max 20 per issue)
- **File content limits** — max file size per upload (10MB default, configurable), max total upload size per request (50MB), max files per upload request (100)
- **Path traversal prevention** — file paths must not contain `..`, must not be absolute, must not escape the repo root. Reject paths with null bytes.
- **JSON payload limits** — max request body size (50MB for migration endpoint, 10MB for all others)
- **Query parameter validation** — pagination limits (max 1000 per page), filter values sanitized

### 13.2: Data Isolation

Tenant data must be strictly isolated:

- **Every database query must include repo_id** — no query should ever return data from a repo the caller doesn't have access to. Enforce this at the storage trait level, not just in handlers.
- **Row-level security (optional)** — consider Postgres RLS policies as a defense-in-depth layer on top of application-level filtering.
- **File storage isolation** — S3 bucket paths are `{repo_id}/{path}`. No API should allow access to files outside the caller's repo.
- **Event isolation** — WebSocket connections only receive events for repos the authenticated user has access to.
- **Workspace isolation** — workspace overlays are scoped to repo_id. No cross-repo file access.

### 13.3: Authentication Hardening

- **API key hashing** — keys must be stored as SHA-256 hashes, never plaintext. Only the key prefix is stored for display. Already implemented — verify this is consistent.
- **Key rotation** — users can create new keys and revoke old ones without downtime. Add a `last_used_at` timestamp to key records for auditing.
- **Rate limiting on auth endpoints** — limit failed auth attempts (10 per minute per IP). Return 429 Too Many Requests.
- **Token expiry (optional)** — API keys don't expire by default (agents need long-lived tokens). But support optional `expires_at` on key creation.
- **Better Auth session security** — HttpOnly cookies, SameSite=Strict, Secure flag in production. CSRF protection on mutation endpoints.

### 13.4: Secrets Management

- **No secrets in config files** — API keys, database URLs, S3 credentials must come from environment variables, not `.vai/config.toml` or `server.toml`.
- **Audit sensitive config access** — log when keys are created, revoked, or used for the first time.
- **Sanitize error messages** — never expose database connection strings, internal paths, or stack traces in API error responses. Log them server-side, return generic messages to clients.
- **Redact secrets in logs** — API keys, passwords, and tokens must be redacted in all log output.

### 13.5: Transport Security

- **HTTPS in production** — document the nginx/caddy reverse proxy setup for TLS termination. The vai server itself doesn't need to handle TLS.
- **WebSocket security** — WSS (WebSocket over TLS) via the same reverse proxy. Validate the `key` query parameter on every WebSocket connection.
- **CORS policy** — in production, restrict `Access-Control-Allow-Origin` to the dashboard domain. The current `Any` origin is for local dev only. Make CORS origins configurable.

### 13.6: Agent Security

Agents operate autonomously and can create workspaces, submit code, create issues, and upload attachments. Guard against:

- **Malicious file content** — validate that uploaded files don't exceed size limits. Consider scanning for known patterns (e.g., embedded credentials, shell injection in filenames).
- **Tarball upload validation** — the `upload-snapshot` endpoint accepts full tarballs. Validate: max size (100MB), no symlinks, no absolute paths, no path traversal, no files outside repo scope. Reject tarballs with suspicious content types.
- **Attachment security** — agents can upload attachments to issues. Enforce file type allowlist (images, PDFs, text, JSON, YAML, CSV). Scan for embedded scripts in uploaded images (polyglot files). Max 10MB per attachment.
- **Workspace scope enforcement** — agents can only modify files within their workspace overlay. The merge engine must not apply changes outside the repo root. The `current/` prefix in S3 must only be writable by the submit handler, never directly by agents.
- **Issue spam** — rate limit issue creation per agent (configurable per watcher, currently implemented as `max_per_hour` in watcher policy).
- **Escalation flooding** — rate limit escalation creation to prevent an agent from overwhelming human reviewers.
- **Graph poisoning** — validate that entity names and relationships extracted from agent-submitted code don't contain injection payloads. The graph engine should parameterize all queries (already using sqlx params for Postgres, rusqlite params for SQLite — verify).
- **Comment abuse** — rate limit comments per agent. Validate comment body size (max 50KB). Future: when agent mentions are implemented, prevent agents from triggering infinite mention loops.

### 13.7: Dependency Security

- **Audit dependencies** — run `cargo audit` in CI to check for known vulnerabilities in Rust crate dependencies.
- **Pin dependency versions** — use `Cargo.lock` and commit it. Don't use wildcard version ranges.
- **Dashboard dependencies** — run `npm audit` in CI. Address critical and high severity vulnerabilities.

### 13.8: Logging and Audit Trail

- **Structured logging** — all security-relevant events should be logged with structured fields (user_id, repo_id, action, IP address, timestamp).
- **Auth events** — log successful and failed authentication attempts.
- **Permission denials** — log 403 responses with the user, repo, and required permission.
- **Data mutations** — log issue creation, workspace submission, escalation resolution with actor identity.
- **Admin actions** — log org/member/collaborator changes.

## Out of Scope

- Encryption at rest (delegate to Postgres and S3 encryption features)
- SOC 2 / compliance certifications (future)
- Penetration testing (future, but the hardening enables it)
- Code signing / workspace content verification
- Network segmentation / VPC configuration

## Issues

1. **Add input validation middleware for all API endpoints** — Enforce string length limits, file size limits, path traversal prevention, and JSON payload size limits. Return 400 with descriptive errors for invalid input. Priority: high.

2. **Enforce repo_id scoping at the storage trait level** — Ensure every storage trait method requires and uses repo_id. Add a wrapper or middleware that injects repo_id so handlers can't accidentally omit it. Priority: high.

3. **Add rate limiting for auth and mutation endpoints** — Implement per-IP rate limiting on authentication (10/min), issue creation (100/hour per key), and escalation creation (20/hour per key). Return 429 when exceeded. Priority: high.

4. **Add `last_used_at` tracking to API keys** — Update the key record on each successful authentication. Surface in `vai server keys list` and the dashboard. Priority: medium.

5. **Make CORS origins configurable** — Add `cors_origins` to server config. Default to `*` in development, require explicit origins in production. Priority: high.

6. **Sanitize error responses** — Ensure no internal paths, connection strings, or stack traces leak in API error responses. Log full details server-side. Priority: high.

7. **Add structured security logging** — Log auth attempts (success/failure), permission denials, data mutations, and admin actions with structured fields. Priority: medium.

8. **Add `cargo audit` and `npm audit` to CI** — Fail the build on critical/high severity vulnerabilities. Priority: medium.

9. **Document production deployment security checklist** — TLS setup with nginx/caddy, environment variable configuration, CORS restriction, recommended Postgres settings. Priority: medium.

10. **Add agent rate limiting and file validation** — Enforce file size limits on upload, validate file paths, rate limit workspace creation and issue creation per agent key. Priority: high.
