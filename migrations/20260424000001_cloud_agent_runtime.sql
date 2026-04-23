-- Cloud Agent Runtime schema (PRD 28)

CREATE TABLE plans (
  tier TEXT PRIMARY KEY,
  max_concurrent_workers_per_repo INT NOT NULL,
  max_issues_per_month INT,
  log_retention_days INT NOT NULL
);

ALTER TABLE repos ADD COLUMN cloud_agent_enabled BOOLEAN NOT NULL DEFAULT FALSE;

CREATE TABLE repo_agent_secrets (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  repo_id UUID NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
  key TEXT NOT NULL,
  encrypted_value BYTEA NOT NULL,
  nonce BYTEA NOT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  UNIQUE (repo_id, key)
);

CREATE TABLE agent_workers (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  repo_id UUID NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
  provider TEXT NOT NULL DEFAULT 'fly',
  machine_id TEXT,
  state TEXT NOT NULL CHECK (state IN ('spawning','running','completed','failed','dead')),
  workspace_id UUID REFERENCES workspaces(id),
  last_heartbeat_at TIMESTAMPTZ,
  started_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  ended_at TIMESTAMPTZ
);
CREATE INDEX idx_agent_workers_repo_state ON agent_workers(repo_id, state);

CREATE TABLE agent_worker_logs (
  id BIGSERIAL PRIMARY KEY,
  worker_id UUID NOT NULL REFERENCES agent_workers(id) ON DELETE CASCADE,
  ts TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  stream TEXT NOT NULL CHECK (stream IN ('stdout','stderr')),
  chunk TEXT NOT NULL
);
CREATE INDEX idx_agent_worker_logs_worker ON agent_worker_logs(worker_id, ts);

INSERT INTO plans (tier, max_concurrent_workers_per_repo, max_issues_per_month, log_retention_days) VALUES
  ('free', 3, 100, 1),
  ('pro',  5, NULL, 30),
  ('team', 10, NULL, 90);
