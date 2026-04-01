-- vai Postgres schema — Better Auth user bridge
--
-- Adds a `better_auth_id` column to the `users` table so vai users can be
-- linked to their corresponding Better Auth identity. This enables
-- auto-provisioning of vai user records on first session exchange.
--
-- The column is optional (NULL for users created directly via the vai API)
-- and unique (one vai user per Better Auth identity).

ALTER TABLE users ADD COLUMN IF NOT EXISTS better_auth_id TEXT UNIQUE;

CREATE INDEX IF NOT EXISTS users_better_auth_id ON users (better_auth_id) WHERE better_auth_id IS NOT NULL;
