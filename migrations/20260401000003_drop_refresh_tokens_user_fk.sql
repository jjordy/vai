-- Drop the foreign key on refresh_tokens.user_id because Better Auth
-- user IDs are opaque strings, not UUIDs from vai's users table.
-- The user_id in refresh_tokens stores a deterministic UUID derived
-- from the Better Auth user ID via SHA-256.
ALTER TABLE refresh_tokens DROP CONSTRAINT IF EXISTS refresh_tokens_user_id_fkey;
