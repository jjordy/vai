-- vai Postgres schema — refresh tokens
--
-- Stores opaque refresh tokens for session-exchange grants (PRD 18).
-- Access tokens (JWTs) are short-lived (15 min) and validated without a DB hit.
-- Refresh tokens are long-lived (7 days), stored here as SHA-256 hashes.
-- POST /api/auth/refresh exchanges a valid refresh token for a new access token.
-- POST /api/auth/revoke marks a refresh token as revoked.

CREATE TABLE IF NOT EXISTS refresh_tokens (
    id         UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id    UUID        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    token_hash TEXT        NOT NULL UNIQUE,
    expires_at TIMESTAMPTZ NOT NULL,
    revoked_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS refresh_tokens_user ON refresh_tokens (user_id);
CREATE INDEX IF NOT EXISTS refresh_tokens_hash ON refresh_tokens (token_hash);
