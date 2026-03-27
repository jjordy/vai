-- Watcher registration and discovery event tables.
--
-- Previously watchers were stored in a per-repo SQLite file (.vai/watchers.db).
-- These tables provide a Postgres-native alternative for server mode, scoped
-- by repo_id for multi-tenant isolation.

CREATE TABLE IF NOT EXISTS watchers (
    repo_id           UUID        NOT NULL,
    agent_id          TEXT        NOT NULL,
    watch_type        TEXT        NOT NULL,
    description       TEXT        NOT NULL,
    policy_json       JSONB       NOT NULL DEFAULT '{}',
    status            TEXT        NOT NULL DEFAULT 'active',
    registered_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_discovery_at TIMESTAMPTZ,
    discovery_count   INTEGER     NOT NULL DEFAULT 0,
    PRIMARY KEY (repo_id, agent_id)
);

CREATE TABLE IF NOT EXISTS watcher_discoveries (
    id               UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    repo_id          UUID        NOT NULL,
    agent_id         TEXT        NOT NULL,
    event_type       TEXT        NOT NULL,
    event_json       JSONB       NOT NULL,
    dedup_key        TEXT        NOT NULL,
    received_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_issue_id UUID,
    suppressed       BOOLEAN     NOT NULL DEFAULT FALSE,
    CONSTRAINT fk_watcher
        FOREIGN KEY (repo_id, agent_id) REFERENCES watchers(repo_id, agent_id)
);

CREATE INDEX IF NOT EXISTS idx_watcher_discoveries_dedup
    ON watcher_discoveries(repo_id, agent_id, dedup_key, suppressed);

-- Per-watcher hourly rate-limit counters.
-- hour_bucket is formatted as 'YYYY-MM-DDTHH' (UTC).
CREATE TABLE IF NOT EXISTS watcher_rate_limits (
    repo_id     UUID    NOT NULL,
    agent_id    TEXT    NOT NULL,
    hour_bucket TEXT    NOT NULL,
    count       INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (repo_id, agent_id, hour_bucket)
);
