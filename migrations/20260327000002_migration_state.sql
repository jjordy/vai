-- Track per-file upload progress during `vai remote migrate`.
--
-- Each row records that a specific file (path + content hash) has been
-- successfully uploaded to S3 under the `current/` prefix for the given repo.
-- The endpoint is idempotent by design (content-addressable keys mean
-- re-uploading the same bytes is a no-op), but this table allows the client
-- to skip files that were already confirmed uploaded, providing fast
-- resumability when a large migration is interrupted mid-batch.
CREATE TABLE IF NOT EXISTS migration_state (
    id          UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    repo_id     UUID        NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    path        TEXT        NOT NULL,
    hash        TEXT        NOT NULL,
    uploaded_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (repo_id, path)
);

CREATE INDEX IF NOT EXISTS migration_state_repo_id_idx ON migration_state (repo_id);
