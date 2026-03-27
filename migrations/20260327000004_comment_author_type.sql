-- Add author_type and author_id to issue_comments (issue #178).
ALTER TABLE issue_comments ADD COLUMN IF NOT EXISTS author_type TEXT NOT NULL DEFAULT 'human';
ALTER TABLE issue_comments ADD COLUMN IF NOT EXISTS author_id TEXT;
