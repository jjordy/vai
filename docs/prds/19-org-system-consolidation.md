# PRD 19: Organization System Consolidation

> **Status:** Future — evaluate after MVP auth flow is stable and deployed.

## Problem

vai currently has two separate systems that manage users and organizations:

1. **Better Auth** (dashboard) — handles login, sessions, email/password auth, GitHub OAuth. Has a rich organization plugin available but NOT enabled.
2. **vai server** (Rust) — custom `organizations`, `org_members`, `repo_collaborators` tables with `OrgStore` trait. Handles API key auth, RBAC for agents, and repo-level permissions.

This creates friction:
- User bridging requires hashing Better Auth IDs into deterministic UUIDs
- No invitation system — members must be manually added via API
- No org switching in the dashboard — hardcoded to first repo
- Role management is split across two systems
- Better Auth's built-in org features (invitations, teams, lifecycle hooks, dynamic RBAC) are unused

## Options

### Option A: Enable Better Auth Organization Plugin (Dashboard-First)

Enable Better Auth's `organization()` plugin for human user flows. Keep vai's Rust RBAC for agent/API key auth.

**Better Auth handles:**
- Org creation, deletion, settings
- Member invitations (email-based, shareable URLs)
- Member management (add, remove, update roles)
- Teams within orgs
- Org switching (active org stored in session)
- Dynamic access control with custom roles

**vai server handles:**
- API key RBAC (agents, CLI)
- Repo-level collaborator permissions
- Work queue and workspace access control
- Mapping Better Auth org/member records to vai permissions

**Bridging:**
- Better Auth org → vai org (sync on creation via lifecycle hooks)
- Better Auth member → vai collaborator (sync on member add/remove via hooks)
- Better Auth roles → vai roles (map owner→admin, admin→write, member→read, or custom)

**Pros:**
- Rich invitation and team features for free
- Battle-tested auth flows
- Dashboard gets org UI primitives (org switcher, member management, invitation flows)
- Lifecycle hooks let us sync to vai's system automatically

**Cons:**
- Two sources of truth for org data (Better Auth tables + vai tables)
- Sync complexity — hooks must keep both systems in sync
- Agent auth still goes through vai's custom system
- Schema customization needed to match vai's existing field names

### Option B: Migrate vai's Org System to Better Auth Entirely

Replace vai's custom `OrgStore` with Better Auth's org tables as the single source of truth.

**Pros:**
- Single source of truth
- No sync complexity
- Full Better Auth feature set

**Cons:**
- vai server (Rust) would need to query Better Auth's tables (camelCase, different schema)
- Agent/API key auth can't use Better Auth sessions
- Major refactor of all OrgStore trait implementations
- Tight coupling to Better Auth's schema — hard to switch auth providers later

### Option C: Keep Systems Separate, Add Missing Features to vai

Don't use Better Auth's org plugin. Instead, add invitation system, team support, and org switching to vai's custom system.

**Pros:**
- Full control over the implementation
- No sync complexity
- Consistent schema (all snake_case, all UUID-based)
- Works identically for human users and agents

**Cons:**
- Significant implementation effort for features Better Auth provides for free
- Invitations, email sending, org lifecycle hooks all need custom code
- No benefit from Better Auth's ongoing development

## Recommendation

**Option A** — Enable Better Auth org plugin for dashboard, keep vai RBAC for agents, bridge via lifecycle hooks.

This gives us the best of both worlds:
- Human users get a polished org experience (invitations, teams, org switching) with minimal code
- Agents continue to use vai's API key RBAC which is purpose-built for programmatic access
- The bridging layer is straightforward — Better Auth hooks fire on org/member changes and sync to vai tables

## Implementation Plan (if Option A is chosen)

### Phase 1: Enable Better Auth Org Plugin
1. Add `organization()` plugin to `auth.ts` config
2. Add `organizationClient()` to client auth config
3. Run Better Auth migration to create org/member/invitation tables
4. Configure org creation limits, invitation settings

### Phase 2: Bridge Better Auth Orgs to vai
5. Add lifecycle hooks: `organization.afterCreate` → create vai org, `member.afterCreate` → add vai collaborator
6. Map Better Auth roles to vai roles (owner→admin, admin→write, member→read)
7. Update token exchange to include org context in JWT claims
8. Update vai's `resolve_repo_role` to check Better Auth membership as fallback

### Phase 3: Dashboard Org UI
9. Add org switcher to dashboard sidebar (using Better Auth's active org API)
10. Add member invitation flow (using Better Auth's invitation API)
11. Add team management UI (if teams enabled)
12. Replace custom org settings pages with Better Auth-powered versions

### Phase 4: Migration
13. Migrate existing vai org/member data into Better Auth tables
14. Verify all existing collaborator permissions are preserved
15. Remove redundant vai org management endpoints (keep RBAC resolution)

## Schema Considerations

Better Auth org tables use camelCase by default. Use schema customization to align:

```ts
organization({
  schema: {
    organization: {
      additionalFields: {
        vaiOrgId: { type: "string", required: false }, // Link to vai org UUID
      },
    },
    member: {
      additionalFields: {
        vaiUserId: { type: "string", required: false }, // Link to vai user UUID
      },
    },
  },
});
```

## Questions to Resolve

1. Should Better Auth orgs map 1:1 to vai orgs, or should one BA org contain multiple vai repos?
2. How do we handle agents that belong to an org but don't have a Better Auth session?
3. Should org-level settings (billing, plan limits) live in Better Auth's metadata or vai's custom tables?
4. Do we need Better Auth teams, or are vai's repo-level collaborators sufficient?
5. What happens if the lifecycle hook sync fails? Do we need a reconciliation job?
