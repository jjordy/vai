-- Issue attachments: files uploaded and associated with an issue.
CREATE TABLE IF NOT EXISTS issue_attachments (
    id            UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    repo_id       UUID        NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    issue_id      UUID        NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    filename      TEXT        NOT NULL,
    content_type  TEXT        NOT NULL,
    size_bytes    BIGINT      NOT NULL,
    s3_key        TEXT        NOT NULL,
    uploaded_by   TEXT        NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (issue_id, filename)
);

CREATE INDEX IF NOT EXISTS idx_issue_attachments_issue_id ON issue_attachments (repo_id, issue_id);
