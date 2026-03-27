-- Add deleted_paths column to workspaces table.
--
-- Stores the accumulated list of repository-relative paths that the agent
-- deleted during its work session.  Applied at submit time to remove files
-- from the `current/` S3 prefix and recorded as FileRemoved events.
ALTER TABLE workspaces
    ADD COLUMN IF NOT EXISTS deleted_paths TEXT[] NOT NULL DEFAULT '{}';
