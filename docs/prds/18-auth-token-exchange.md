# PRD 18: Auth Token Exchange

> **Status:** Future — implement after MVP is stable. Current static API key approach works for development.

## Problem

The dashboard currently stores a static `vaiApiKey` on the user record and sends it as a Bearer token with every vai server request. This has issues:

- **No expiry** — if a key is leaked, it's valid forever until manually revoked
- **No session binding** — logging out of Better Auth doesn't invalidate the vai API key
- **Static secret** — the same key is used for the lifetime of the account
- **No token refresh** — no mechanism to rotate credentials without user action

## Proposed Architecture

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

Agents continue to use static API keys. They don't have browser sessions and need long-lived credentials. API keys remain the primary auth mechanism for programmatic access.

Optionally, agents can exchange an API key for a short-lived access token to reduce exposure if the agent environment is less trusted:

```
POST /api/auth/token
{
  "grant_type": "api_key",
  "api_key": "vai_..."
}
→ { "access_token": "eyJ...", "expires_in": 900 }
```

### vai server changes:

1. **JWT signing** — add `jsonwebtoken` crate. Server generates a signing key on first startup, stores in config.
2. **Token exchange endpoint** — validates Better Auth session (query session table in shared Postgres), mints JWT.
3. **Refresh token store** — new `refresh_tokens` table with user_id, token_hash, expires_at, revoked_at.
4. **Auth middleware update** — accept both API keys (existing) and JWTs (new). JWT validation is stateless (fast).
5. **Revocation on logout** — Better Auth logout webhook or dashboard calls revoke endpoint.

### Dashboard changes:

1. **Token manager** — replaces `setSessionApiKey`. Exchanges session for tokens on login, auto-refreshes before expiry.
2. **Interceptor** — attaches access token to all requests. On 401, attempts refresh. On refresh failure, redirects to login.
3. **Logout** — calls revoke endpoint then Better Auth signout.

## Considerations

- **Shared database:** Better Auth and vai server can share the same Postgres database, making session exchange a simple `SELECT` on the session table.
- **Backward compatibility:** API keys continue to work. Token exchange is additive.
- **Clock skew:** JWT expiry should have a small grace period (30s) for clock differences.
- **Key rotation:** JWT signing key should be rotatable without invalidating existing tokens (keep previous key for verification during overlap period).

## Issue Breakdown

1. Add JWT signing and validation to vai server
2. Implement token exchange endpoint (session → JWT)
3. Implement refresh token store and endpoint
4. Update auth middleware to accept JWTs alongside API keys
5. Add token revocation endpoint
6. Dashboard: implement token manager with auto-refresh
7. Dashboard: update request interceptor for JWT auth
8. Dashboard: revoke tokens on logout

## Priority

Low — the current API key approach works for MVP. Implement when:
- Preparing for production deployment with real users
- After security PRD (XX-security) audit
- When Better Auth session → vai access becomes a user-facing flow
