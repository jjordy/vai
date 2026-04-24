-- Fix rows where resolution is set but status is not 'closed'.
-- These are pathological rows created by the discard handler bug:
-- discard always reopened linked issues, even after an operator had closed them.
-- Trustworthy assumption: if resolution IS NOT NULL, the operator intended closure.
UPDATE issues SET status = 'closed' WHERE resolution IS NOT NULL AND status != 'closed';
