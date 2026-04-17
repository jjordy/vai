-- CLI device code flow (PRD 26, V-3).
--
-- Stores short-lived pending device codes used by `vai login --device`.
-- The CLI exchanges the code for an API key after the user authorizes it
-- through the dashboard's /cli page.

CREATE TABLE cli_device_codes (
    code TEXT PRIMARY KEY,
    user_id UUID REFERENCES users(id),
    api_key TEXT,
    expires_at TIMESTAMP WITH TIME ZONE NOT NULL,
    created_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT now()
);

CREATE INDEX idx_cli_device_codes_expires ON cli_device_codes (expires_at);
