-- Add per-conflict detail column to escalations.
--
-- Each escalation can carry a list of `EscalationConflict` records that
-- describe the file, entity, merge level, and content snippets for every
-- unresolvable conflict detected during a merge.  Stored as JSONB so the
-- schema is flexible while remaining queryable.

ALTER TABLE escalations
    ADD COLUMN IF NOT EXISTS conflicts JSONB NOT NULL DEFAULT '[]';
