-- Unique partial index on (user_id, name) for non-revoked user-owned keys.
-- Enables ON CONFLICT DO UPDATE in create_key() so that re-running `vai login`
-- from the same machine rotates the existing key instead of accumulating duplicates.
CREATE UNIQUE INDEX IF NOT EXISTS api_keys_user_name_active
ON api_keys (user_id, name)
WHERE NOT revoked AND user_id IS NOT NULL;
