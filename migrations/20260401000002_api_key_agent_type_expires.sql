-- Add agent_type and expires_at columns to api_keys.
--
-- agent_type: optional label for the kind of agent this key belongs to
--             (e.g. "ci", "worker", "human"). NULL means unspecified.
-- expires_at: optional expiry timestamp. NULL means the key never expires.
--             The auth middleware rejects keys where expires_at <= now().

ALTER TABLE api_keys
    ADD COLUMN IF NOT EXISTS agent_type TEXT,
    ADD COLUMN IF NOT EXISTS expires_at TIMESTAMPTZ;

CREATE INDEX IF NOT EXISTS api_keys_expires ON api_keys (expires_at)
    WHERE expires_at IS NOT NULL AND NOT revoked;
