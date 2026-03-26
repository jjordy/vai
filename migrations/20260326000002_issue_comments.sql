-- Add issue comments (issue #148).
CREATE TABLE IF NOT EXISTS issue_comments (
    id         UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    repo_id    UUID        NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    issue_id   UUID        NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    author     TEXT        NOT NULL,
    body       TEXT        NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS issue_comments_issue_id ON issue_comments (issue_id);
CREATE INDEX IF NOT EXISTS issue_comments_repo_id  ON issue_comments (repo_id);
