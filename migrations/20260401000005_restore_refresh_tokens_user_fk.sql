-- vai Postgres schema — restore refresh_tokens.user_id foreign key
--
-- The FK was dropped in 20260401000003 because Better Auth user IDs weren't
-- yet in vai's users table. Now that user bridging is in place (migration
-- 20260401000004), every refresh token's user_id is guaranteed to resolve to
-- a row in users. Clean up any orphaned tokens and restore the constraint.

-- Remove any orphaned refresh tokens whose user_id no longer exists in users.
-- Under normal operation this should be a no-op, but guards against any rows
-- created during the window when the FK was absent.
DELETE FROM refresh_tokens
WHERE user_id NOT IN (SELECT id FROM users);

-- Restore the foreign key with ON DELETE CASCADE so that deleting a user
-- automatically revokes all of their refresh tokens.
ALTER TABLE refresh_tokens
    ADD CONSTRAINT refresh_tokens_user_id_fkey
    FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE CASCADE;
