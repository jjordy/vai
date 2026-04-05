-- Add comment_mentions table for @mention tracking in issue comments (issue #247).
CREATE TABLE IF NOT EXISTS comment_mentions (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    comment_id       UUID NOT NULL REFERENCES issue_comments(id) ON DELETE CASCADE,
    repo_id          UUID NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    mentioned_user_id UUID,
    mentioned_key_id  UUID,
    mentioned_name   TEXT NOT NULL,
    mention_type     TEXT NOT NULL DEFAULT 'human',
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_comment_mentions_user    ON comment_mentions(mentioned_user_id);
CREATE INDEX IF NOT EXISTS idx_comment_mentions_comment ON comment_mentions(comment_id);
