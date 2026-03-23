-- S3 file index: maps (repo_id, path) to an S3 object key (the content SHA-256 hash).
--
-- When the S3 FileStore writes a file it stores the raw bytes in S3 at key
-- `{repo_id}/{sha256}` (content-addressable) and records the path-to-key
-- mapping here.  Multiple paths pointing to identical content share a single
-- S3 object — the ref-count query in S3FileStore::delete ensures the object
-- is only removed when no paths reference it any more.

CREATE TABLE IF NOT EXISTS file_index (
    id              UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    repo_id         UUID        NOT NULL,
    path            TEXT        NOT NULL,
    -- The S3 object key; currently `{repo_id}/{content_hash}`.
    s3_key          TEXT        NOT NULL,
    -- SHA-256 hex digest of the file content (64 chars).
    content_hash    TEXT        NOT NULL,
    -- File size in bytes, stored as BIGINT to avoid overflow for large files.
    size            BIGINT      NOT NULL,
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),

    CONSTRAINT file_index_repo_path UNIQUE (repo_id, path)
);

-- Supports prefix-scan queries used by FileStore::list.
-- text_pattern_ops enables prefix matching without a full-table scan.
CREATE INDEX IF NOT EXISTS file_index_prefix
    ON file_index (repo_id, path text_pattern_ops);

-- Supports the ref-count lookup in FileStore::delete.
CREATE INDEX IF NOT EXISTS file_index_s3_key
    ON file_index (repo_id, s3_key);
