-- Normalize issue status and priority values to lowercase.
--
-- The initial schema used title-case defaults ('Open', 'Medium', 'High', etc.)
-- but the application code writes and compares lowercase values ('open',
-- 'medium', 'high', etc.).  This caused `list_issues` with a status/priority
-- filter to return 0 results because the SQL WHERE clause did a case-sensitive
-- comparison ('Open' != 'open').
--
-- This migration:
--   1. Lowercases all existing status and priority column values in `issues`.
--   2. Updates the column defaults so newly-inserted rows also use lowercase.

UPDATE issues SET status   = LOWER(status)   WHERE status   != LOWER(status);
UPDATE issues SET priority = LOWER(priority) WHERE priority != LOWER(priority);

ALTER TABLE issues ALTER COLUMN status   SET DEFAULT 'open';
ALTER TABLE issues ALTER COLUMN priority SET DEFAULT 'medium';
