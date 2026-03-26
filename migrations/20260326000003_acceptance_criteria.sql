-- Add acceptance_criteria column to issues table.
--
-- An array of testable conditions that define when the issue is complete.
-- The work queue prefers issues with non-empty acceptance criteria.

ALTER TABLE issues
    ADD COLUMN IF NOT EXISTS acceptance_criteria TEXT[] NOT NULL DEFAULT '{}';
