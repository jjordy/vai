-- Drop the better_auth_id bridge column now that Better Auth writes directly
-- to users.id (uuid) after the Phase 1 data migration (PRD 27).
-- Idempotent: IF EXISTS means a no-op if Phase 1 already dropped it.
ALTER TABLE users DROP COLUMN IF EXISTS better_auth_id;
