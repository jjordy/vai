# PRD 18: Auth Token Exchange

## Problem

The dashboard currently stores a static `vaiApiKey` on the user record and sends it as a Bearer token with every vai server request. This has issues:

- **No expiry** — if a key is leaked, it's valid forever until manually revoked
- **No session binding** — logging out of Better Auth doesn't invalidate the vai API key
- **Static secret** — the same key is used for the lifetime of the account
- **No token refresh** — no mechanism to rotate credentials without user action

## Architecture

### For human users (dashboard):

```
Browser → Better Auth → Session cookie
Dashboard SSR → Exchange session for short-lived access token
Dashboard client → Use access token for vai API calls
                → Refresh token when access token expires
Logout → Revoke refresh token, access token expires naturally
```

**Access token:** JWT signed by vai server, 15 min expiry. Contains user_id, repo_id, role. Validated without database hit (signature + expiry check only).

**Refresh token:** Opaque token stored in Postgres, 7 day expiry. Used to mint new access tokens. Revoked on logout.

**Token exchange endpoint:**
```
POST /api/auth/token
{
  "grant_type": "session_exchange",
  "session_token": "<better-auth-session-id>"
}
→ { "access_token": "eyJ...", "refresh_token": "rt_...", "expires_in": 900 }
```

**Refresh endpoint:**
```
POST /api/auth/refresh
{ "refresh_token": "rt_..." }
→ { "access_token": "eyJ...", "expires_in": 900 }
```

### For agents (CLI/SDK):

Each agent gets its own API key. Shared keys are a security anti-pattern — individual keys provide revocation granularity, audit trails, least privilege, and independent rotation.

**Agent key features:**

- **Identity** — keys have a `name` and optional `agent_type` field (e.g., "ralph-1", "security-scanner") for audit trail visibility
- **Role scoping** — each key has a repo-level role (admin, write, read) controlling what the agent can do
- **Action scoping** (future) — keys can be restricted to specific actions (e.g., issues-only, read-only, workspace-only) beyond blanket roles
- **Optional expiry** — `expires_at` on creation for keys that should auto-expire (CI/CD pipelines, temporary agent runs)
- **Bulk revocation** — revoke all keys for a repo, all keys created by a user, or all keys matching a name pattern in one call

**Agent JWT exchange:**

Agents should exchange their API key for a short-lived JWT to minimize exposure of the long-lived key:

```
# Agent starts up with a long-lived API key (stored securely)
vai-agent init --key vai_sk_abc123

# SDK exchanges it for a 15-min JWT on startup
POST /api/auth/token
{
  "grant_type": "api_key",
  "api_key": "vai_sk_abc123"
}
→ { "access_token": "eyJ...", "expires_in": 900 }

# All subsequent API calls use the short-lived JWT
Authorization: Bearer eyJ...

# SDK auto-refreshes using the original API key before expiry
# The long-lived API key only hits the wire once per 15 minutes
```

If the JWT leaks, it's valid for at most 15 minutes. The original API key is stored in the agent's secure environment and rarely transmitted.

### API Key Management Endpoints

```
POST   /api/keys                    — create key (name, repo_id, role, agent_type?, expires_at?)
GET    /api/keys                    — list all keys (for admin) or keys by user
GET    /api/keys?repo_id=<id>       — list keys for a specific repo
DELETE /api/keys/:id                — revoke single key
DELETE /api/keys?repo_id=<id>       — revoke all keys for a repo
DELETE /api/keys?created_by=<user>  — revoke all keys created by a user
```

### Key creation response:
```json
{
  "id": "key-uuid",
  "name": "ralph-1",
  "agent_type": "development",
  "repo_id": "repo-uuid",
  "role": "write",
  "expires_at": "2026-04-01T00:00:00Z",
  "created_by": "user-uuid",
  "created_at": "2026-03-31T...",
  "token": "vai_sk_abc123..."  // only shown once at creation
}
```

## Server Changes

### 1. JWT Infrastructure
- Add `jsonwebtoken` crate
- Generate HMAC signing key on first startup, store in `~/.vai/server.toml` or environment variable `VAI_JWT_SECRET`
- Support key rotation: keep previous key for verification during overlap period (configurable, default 1 hour)

### 2. Token Exchange Endpoint
`POST /api/auth/token` — accepts `grant_type`:
- `session_exchange` — validates Better Auth session (query session table), mints JWT
- `api_key` — validates API key, mints JWT with same permissions
- Returns `{ access_token, refresh_token (session only), expires_in }`

### 3. Refresh Token Store
```sql
CREATE TABLE refresh_tokens (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    revoked_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
```

### 4. Auth Middleware Update
Accept three auth methods (checked in order):
1. **JWT** — validate signature + expiry, extract claims (no DB hit)
2. **API key** — existing behavior, hash and lookup in api_keys table
3. **Bootstrap admin key** — `VAI_ADMIN_KEY` env var for initial setup

### 5. Revocation
- `POST /api/auth/revoke` — revoke a refresh token
- Bulk key revocation: `DELETE /api/keys?repo_id=<id>` or `?created_by=<user>`
- On Better Auth logout, dashboard calls revoke endpoint

### 6. API Key Schema Updates
```sql
ALTER TABLE api_keys ADD COLUMN agent_type TEXT;
ALTER TABLE api_keys ADD COLUMN expires_at TIMESTAMPTZ;
```
Auth middleware checks `expires_at` on each validation — reject expired keys with 401.

## Dashboard Changes

### 1. Token Manager
Replace `setSessionApiKey` with a token manager that:
- On login: exchanges Better Auth session for JWT + refresh token
- Stores tokens in memory (not localStorage — prevents XSS access)
- Auto-refreshes JWT 60 seconds before expiry
- On refresh failure (401): redirects to login

### 2. Request Interceptor
Update `orval-fetch.ts`:
- Attach JWT as `Authorization: Bearer <jwt>`
- On 401 response: attempt refresh, retry original request
- On refresh failure: clear tokens, redirect to login

### 3. Logout
- Call `POST /api/auth/revoke` with refresh token
- Call Better Auth `signOut()`
- Clear in-memory tokens
- Redirect to login

### 4. Key Management UI
Settings page should show:
- Key name, agent_type, role, created_at, last_used_at, expires_at
- Expiry status (active, expired, revoked)
- Create key form with name, agent_type (optional), role, expires_at (optional)
- Revoke button per key
- Bulk revoke by repo

## Considerations

- **Shared database:** Better Auth and vai server share Postgres, so session validation is a simple `SELECT` on the session table
- **Backward compatibility:** Raw API keys continue to work alongside JWTs — additive change
- **Clock skew:** JWT expiry has a 30s grace period
- **Key rotation:** JWT signing key rotatable without invalidating existing tokens (keep previous key for verification during 1-hour overlap)
- **Agent SDK:** The `vai-agent` CLI (future PRD) should handle JWT exchange transparently — agents just provide their API key

## Issue Breakdown

### Server (vai)
1. Add JWT signing and validation infrastructure (`jsonwebtoken` crate, signing key management)
2. Implement token exchange endpoint (`POST /api/auth/token` with session_exchange and api_key grants)
3. Implement refresh token store, refresh endpoint, and revocation endpoint
4. Update auth middleware to accept JWTs alongside API keys (check JWT first, then API key, then admin key)
5. Add agent_type and expires_at to API key schema, enforce expiry in auth middleware
6. Add bulk key revocation endpoints (by repo_id, by created_by)

### Dashboard (vai-dashboard)
7. Implement token manager with auto-refresh, replacing setSessionApiKey
8. Update orval-fetch.ts request interceptor for JWT auth with 401 → refresh → retry
9. Add logout flow: revoke refresh token, Better Auth signOut, redirect to login
10. Update key management UI: show agent_type, expires_at, add create form with new fields
