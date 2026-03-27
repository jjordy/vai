-- Migrate issue_dependencies to issue_links (relationship = 'blocks').
--
-- Semantics: if issue B depends_on issue A (A must close before B starts),
-- then A *blocks* B.  In the issue_links model:
--   source_id = A (the blocker), target_id = B (the blocked), relationship = 'blocks'.
INSERT INTO issue_links (repo_id, source_id, target_id, relationship)
SELECT
    i.repo_id,
    d.depends_on_id AS source_id,
    d.issue_id      AS target_id,
    'blocks'
FROM issue_dependencies d
JOIN issues i ON i.id = d.issue_id
ON CONFLICT DO NOTHING;

DROP TABLE IF EXISTS issue_dependencies;
