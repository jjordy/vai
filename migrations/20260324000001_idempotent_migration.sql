-- Idempotent migration support (issue #125)
--
-- Add a `local_event_id` column to events so that the bulk migration endpoint
-- can deduplicate events on re-run.  The column stores the local monotonic
-- event ID from the source SQLite repository.  A partial unique index on
-- (repo_id, local_event_id) enforces uniqueness only for migrated events;
-- events written natively by the server leave the column NULL, which is
-- excluded from the unique constraint.

ALTER TABLE events
    ADD COLUMN IF NOT EXISTS local_event_id BIGINT;

CREATE UNIQUE INDEX IF NOT EXISTS events_repo_local_id
    ON events (repo_id, local_event_id)
    WHERE local_event_id IS NOT NULL;
