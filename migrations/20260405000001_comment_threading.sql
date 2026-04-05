-- Add threading and soft-delete support to issue comments (issue #246).
ALTER TABLE issue_comments ADD COLUMN IF NOT EXISTS parent_id UUID REFERENCES issue_comments(id) ON DELETE SET NULL;
ALTER TABLE issue_comments ADD COLUMN IF NOT EXISTS edited_at TIMESTAMPTZ;
ALTER TABLE issue_comments ADD COLUMN IF NOT EXISTS deleted_at TIMESTAMPTZ;

CREATE INDEX IF NOT EXISTS idx_issue_comments_parent_id ON issue_comments(parent_id);
