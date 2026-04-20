-- User onboarding state: records when a dashboard user completes the welcome walkthrough.
--
-- user_id is the vai user UUID (stored as TEXT to remain independent of any
-- foreign-key schema and to accept whatever identifier the JWT sub carries).

CREATE TABLE user_onboarding (
    user_id TEXT PRIMARY KEY,
    completed_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX idx_user_onboarding_completed_at ON user_onboarding (completed_at);
