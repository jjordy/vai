-- Add issue dependency tracking (issue #145).
CREATE TABLE IF NOT EXISTS issue_dependencies (
    issue_id      UUID NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    depends_on_id UUID NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    PRIMARY KEY (issue_id, depends_on_id)
);

CREATE INDEX IF NOT EXISTS issue_deps_issue_id      ON issue_dependencies (issue_id);
CREATE INDEX IF NOT EXISTS issue_deps_depends_on_id ON issue_dependencies (depends_on_id);
