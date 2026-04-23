-- Change user_onboarding.user_id from TEXT to UUID to match the users table.
--
-- This formalises the manual migration applied to production on 2026-04-22
-- (PRD 27 Phase 1). The USING clause casts existing TEXT values to uuid; the
-- column already contained valid UUID strings so the cast is safe.

ALTER TABLE user_onboarding
    ALTER COLUMN user_id TYPE uuid USING user_id::uuid;
