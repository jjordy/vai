-- vai Postgres schema — RBAC migration
--
-- Adds organizations, users, org membership, and repo collaborator tables
-- to support role-based access control on the hosted server.

-- ── organizations ──────────────────────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS organizations (
    id          UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    name        TEXT        NOT NULL,
    slug        TEXT        NOT NULL UNIQUE,    -- URL-safe identifier, e.g. "acme-corp"
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS orgs_slug ON organizations (slug);

-- ── users ──────────────────────────────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS users (
    id          UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    email       TEXT        NOT NULL UNIQUE,
    name        TEXT        NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS users_email ON users (email);

-- ── org_members ────────────────────────────────────────────────────────────────
-- Maps users to organizations with a role.
-- Roles: owner | admin | member

CREATE TABLE IF NOT EXISTS org_members (
    org_id      UUID        NOT NULL REFERENCES organizations (id) ON DELETE CASCADE,
    user_id     UUID        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    role        TEXT        NOT NULL DEFAULT 'member',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (org_id, user_id)
);

CREATE INDEX IF NOT EXISTS org_members_user  ON org_members (user_id);
CREATE INDEX IF NOT EXISTS org_members_org   ON org_members (org_id);

-- ── repo_collaborators ─────────────────────────────────────────────────────────
-- Grants a user a specific role on a repository, overriding their org role.
-- Roles: owner | admin | write | read

CREATE TABLE IF NOT EXISTS repo_collaborators (
    repo_id     UUID        NOT NULL REFERENCES repos (id) ON DELETE CASCADE,
    user_id     UUID        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    role        TEXT        NOT NULL DEFAULT 'read',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (repo_id, user_id)
);

CREATE INDEX IF NOT EXISTS repo_collaborators_repo  ON repo_collaborators (repo_id);
CREATE INDEX IF NOT EXISTS repo_collaborators_user  ON repo_collaborators (user_id);

-- ── Update api_keys ────────────────────────────────────────────────────────────
-- Add user_id and role_override columns so keys can be scoped to a user and
-- optionally down-scoped to a lower permission level.

ALTER TABLE api_keys
    ADD COLUMN IF NOT EXISTS user_id        UUID REFERENCES users (id) ON DELETE CASCADE,
    ADD COLUMN IF NOT EXISTS role_override  TEXT;

CREATE INDEX IF NOT EXISTS api_keys_user ON api_keys (user_id) WHERE user_id IS NOT NULL;
