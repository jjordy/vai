-- Add related issue links (issue #151).
-- Bidirectional: linking A blocks B also shows B is-blocked-by A.
-- Relationship types: 'blocks', 'relates-to', 'duplicates'.
CREATE TABLE IF NOT EXISTS issue_links (
    repo_id      UUID NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    source_id    UUID NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    target_id    UUID NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    relationship TEXT NOT NULL,
    PRIMARY KEY (source_id, target_id)
);

CREATE INDEX IF NOT EXISTS issue_links_source_id ON issue_links (source_id);
CREATE INDEX IF NOT EXISTS issue_links_target_id ON issue_links (target_id);
CREATE INDEX IF NOT EXISTS issue_links_repo_id   ON issue_links (repo_id);
