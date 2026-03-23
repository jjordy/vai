-- vai Postgres schema — initial migration
--
-- All tables carry a `repo_id UUID NOT NULL` column so that multiple
-- repositories can share a single Postgres instance.  Every index is
-- scoped to `(repo_id, ...)` to keep per-tenant query plans tight.

-- ── repos ─────────────────────────────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS repos (
    id          UUID        PRIMARY KEY,
    name        TEXT        NOT NULL,
    org_id      UUID,                       -- null for personal / standalone repos
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS repos_name_org ON repos (org_id, name);

-- ── events ────────────────────────────────────────────────────────────────────
-- Append-only event log.  BIGSERIAL gives a monotonically increasing ID that
-- can be used as a cursor for streaming / replay.

CREATE TABLE IF NOT EXISTS events (
    id              BIGSERIAL   PRIMARY KEY,
    repo_id         UUID        NOT NULL REFERENCES repos (id) ON DELETE CASCADE,
    event_type      TEXT        NOT NULL,
    workspace_id    UUID,
    payload         JSONB       NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS events_repo_type        ON events (repo_id, event_type);
CREATE INDEX IF NOT EXISTS events_repo_workspace   ON events (repo_id, workspace_id);
CREATE INDEX IF NOT EXISTS events_repo_created     ON events (repo_id, created_at);
CREATE INDEX IF NOT EXISTS events_repo_id          ON events (repo_id, id);

-- ── versions ──────────────────────────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS versions (
    id                  UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    repo_id             UUID        NOT NULL REFERENCES repos (id) ON DELETE CASCADE,
    version_id          TEXT        NOT NULL,   -- e.g. "v3"
    parent_version_id   TEXT,
    intent              TEXT        NOT NULL,
    created_by          TEXT        NOT NULL,
    merge_event_id      BIGINT      REFERENCES events (id) ON DELETE SET NULL,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS versions_repo_version_id ON versions (repo_id, version_id);
CREATE INDEX        IF NOT EXISTS versions_repo_created    ON versions (repo_id, created_at);

-- HEAD pointer: one row per repo, updated atomically on each merge.
CREATE TABLE IF NOT EXISTS version_head (
    repo_id     UUID    PRIMARY KEY REFERENCES repos (id) ON DELETE CASCADE,
    version_id  TEXT    NOT NULL
);

-- ── workspaces ────────────────────────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS workspaces (
    id              UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    repo_id         UUID        NOT NULL REFERENCES repos (id) ON DELETE CASCADE,
    intent          TEXT        NOT NULL,
    base_version    TEXT        NOT NULL,
    status          TEXT        NOT NULL DEFAULT 'Created',
    issue_id        UUID,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS workspaces_repo_status  ON workspaces (repo_id, status);
CREATE INDEX IF NOT EXISTS workspaces_repo_issue   ON workspaces (repo_id, issue_id);

-- ── issues ────────────────────────────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS issues (
    id              UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    repo_id         UUID        NOT NULL REFERENCES repos (id) ON DELETE CASCADE,
    title           TEXT        NOT NULL,
    body            TEXT        NOT NULL DEFAULT '',
    status          TEXT        NOT NULL DEFAULT 'Open',
    priority        TEXT        NOT NULL DEFAULT 'Medium',
    labels          TEXT[]      NOT NULL DEFAULT '{}',
    creator         TEXT        NOT NULL,
    agent_source    JSONB,
    resolution      TEXT,
    workspace_id    UUID        REFERENCES workspaces (id) ON DELETE SET NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS issues_repo_status    ON issues (repo_id, status);
CREATE INDEX IF NOT EXISTS issues_repo_priority  ON issues (repo_id, priority);
CREATE INDEX IF NOT EXISTS issues_repo_creator   ON issues (repo_id, creator);
CREATE INDEX IF NOT EXISTS issues_repo_created   ON issues (repo_id, created_at);

-- ── escalations ───────────────────────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS escalations (
    id                  UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    repo_id             UUID        NOT NULL REFERENCES repos (id) ON DELETE CASCADE,
    escalation_type     TEXT        NOT NULL,
    severity            TEXT        NOT NULL,
    summary             TEXT        NOT NULL,
    intents             TEXT[]      NOT NULL DEFAULT '{}',
    agents              TEXT[]      NOT NULL DEFAULT '{}',
    workspace_ids       UUID[]      NOT NULL DEFAULT '{}',
    affected_entities   TEXT[]      NOT NULL DEFAULT '{}',
    resolution_options  JSONB       NOT NULL DEFAULT '[]',
    resolved            BOOLEAN     NOT NULL DEFAULT false,
    resolution          TEXT,
    resolved_by         TEXT,
    resolved_at         TIMESTAMPTZ,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS escalations_repo_resolved ON escalations (repo_id, resolved);
CREATE INDEX IF NOT EXISTS escalations_repo_created  ON escalations (repo_id, created_at);

-- ── graph: entities ───────────────────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS entities (
    id                  TEXT        NOT NULL,   -- stable hash-based ID from parser
    repo_id             UUID        NOT NULL REFERENCES repos (id) ON DELETE CASCADE,
    kind                TEXT        NOT NULL,
    name                TEXT        NOT NULL,
    qualified_name      TEXT        NOT NULL,
    file_path           TEXT        NOT NULL,
    line_start          INT,
    line_end            INT,
    parent_entity_id    TEXT,
    PRIMARY KEY (repo_id, id)
);

CREATE INDEX IF NOT EXISTS entities_repo_file       ON entities (repo_id, file_path);
CREATE INDEX IF NOT EXISTS entities_repo_kind       ON entities (repo_id, kind);
CREATE INDEX IF NOT EXISTS entities_repo_name       ON entities (repo_id, name);

-- ── graph: relationships ──────────────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS relationships (
    id              TEXT    NOT NULL,   -- stable hash-based ID
    repo_id         UUID    NOT NULL REFERENCES repos (id) ON DELETE CASCADE,
    kind            TEXT    NOT NULL,
    from_entity_id  TEXT    NOT NULL,
    to_entity_id    TEXT    NOT NULL,
    PRIMARY KEY (repo_id, id)
);

CREATE INDEX IF NOT EXISTS relationships_repo_from ON relationships (repo_id, from_entity_id);
CREATE INDEX IF NOT EXISTS relationships_repo_to   ON relationships (repo_id, to_entity_id);

-- ── api_keys ──────────────────────────────────────────────────────────────────
-- `repo_id` is NULL for server-level (admin) keys.

CREATE TABLE IF NOT EXISTS api_keys (
    id          TEXT        PRIMARY KEY,        -- UUID stored as text
    repo_id     UUID        REFERENCES repos (id) ON DELETE CASCADE,
    name        TEXT        NOT NULL,
    key_hash    TEXT        NOT NULL UNIQUE,    -- SHA-256 hex of the plaintext token
    key_prefix  TEXT        NOT NULL,           -- first 8 chars, shown in listings
    role        TEXT        NOT NULL DEFAULT 'reader',
    last_used_at TIMESTAMPTZ,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    revoked     BOOLEAN     NOT NULL DEFAULT false
);

CREATE INDEX IF NOT EXISTS api_keys_repo    ON api_keys (repo_id);
CREATE INDEX IF NOT EXISTS api_keys_hash    ON api_keys (key_hash);
CREATE INDEX IF NOT EXISTS api_keys_revoked ON api_keys (revoked) WHERE NOT revoked;
