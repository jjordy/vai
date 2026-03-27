# Phase 10: RBAC and Organization-Based Access Control

## Summary

Add organization-based role-based access control (RBAC) to the vai server. Organizations own repositories, users belong to organizations with roles, and API keys inherit the permissions of their creator.

## Motivation

The current auth model is a flat list of API keys per repo with no permission differentiation. A hosted platform needs to control who can read, write, admin, and own repositories. Agents need scoped keys that limit what they can do.

## Requirements

### 10.1: Data Model

```
Organization
  ├── id, name, slug, created_at
  ├── Members (user_id, org_id, role: owner | admin | member)
  └── Repositories
       ├── repo_id, org_id, name, created_at
       └── Collaborators (user_id, repo_id, role: admin | write | read)

User
  ├── id, email, name, created_at
  └── API Keys
       ├── id, user_id, repo_id (nullable), name, key_hash, role_override (nullable)
       └── Scoped to: user's effective role on the repo, or role_override if set
```

**Effective permission resolution:**
1. If the user is an org owner/admin → full access to all repos in the org
2. Else check repo-level collaborator role
3. API key inherits user's effective role, or `role_override` if explicitly scoped down

### 10.2: Permission Matrix

| Permission | Owner | Admin | Write | Read |
|-----------|-------|-------|-------|------|
| Delete repo | Yes | Yes | — | — |
| Manage collaborators | Yes | Yes | Yes (invite only) | — |
| Create/submit workspaces | Yes | Yes | Yes | — |
| Create/update/close issues | Yes | Yes | Yes | — |
| Resolve escalations | Yes | Yes | Yes | — |
| Manage API keys | Yes | Yes | Yes (own keys only) | — |
| Read all repo data | Yes | Yes | Yes | Yes |
| Manage org members | Yes | Yes | — | — |
| Create repos in org | Yes | Yes | — | — |
| Billing/plan management | Yes | — | — | — |

### 10.3: API Endpoints

Organization management:
- `POST /api/orgs` — create organization
- `GET /api/orgs` — list user's organizations
- `GET /api/orgs/:org` — org details
- `POST /api/orgs/:org/members` — invite member
- `PATCH /api/orgs/:org/members/:user` — change member role
- `DELETE /api/orgs/:org/members/:user` — remove member

Repository access:
- `POST /api/orgs/:org/repos/:repo/collaborators` — add collaborator
- `PATCH /api/orgs/:org/repos/:repo/collaborators/:user` — change role
- `DELETE /api/orgs/:org/repos/:repo/collaborators/:user` — remove collaborator

API key management:
- `POST /api/keys` — create API key (scoped to repo + role)
- `GET /api/keys` — list user's keys
- `DELETE /api/keys/:id` — revoke key

### 10.4: Auth Middleware Update

Update the server auth middleware to:
1. Validate the Bearer token (API key)
2. Look up the user and their effective role for the target repo
3. Check the required permission for the endpoint
4. Return 403 Forbidden if insufficient permissions

The middleware should inject the authenticated user and their role into the request context so handlers don't need to re-query.

### 10.5: Server-Level Admin

For initial server setup (before any orgs/users exist), support a bootstrap admin key:
- Set via environment variable: `VAI_ADMIN_KEY=<secret>`
- Or generated on first startup and printed to stdout
- Admin key has full access to all endpoints including org/user management
- Used to create the first organization and user

## Out of Scope

- OAuth/session auth for humans (handled by dashboard with Better Auth)
- Fine-grained per-entity permissions (e.g., "can only edit files in src/auth/")
- Audit logging of permission changes (can be added via event log later)
- Rate limiting per key/role

## Issues

1. **Add organization, user, and membership tables to Postgres schema** — Create tables for orgs, users, org_members, repo_collaborators. Add migrations. Priority: high.

2. **Implement organization CRUD endpoints** — Create, list, get orgs. Invite/update/remove members. Priority: high.

3. **Implement repository collaborator management** — Add/update/remove collaborators on repos. Collaborators can be org members or external users. Priority: high.

4. **Implement permission resolution logic** — Given a user ID and repo ID, compute effective role by checking org membership then repo collaborators. Priority: high.

5. **Update auth middleware for RBAC** — Validate token, resolve user, compute permissions, inject into request context. Return 403 for insufficient permissions. Priority: high.

6. **Implement scoped API key creation** — Users can create keys scoped to a specific repo and role. Keys cannot exceed the creator's own permissions. Priority: high.

7. **Add bootstrap admin key support** — `VAI_ADMIN_KEY` environment variable for initial setup. Full access to all endpoints. Priority: high.

8. **Update all existing endpoints with permission checks** — Each handler checks the required permission level before executing. Priority: high.
