-- The owner's authenticator secret when it was enrolled from the dashboard.
-- TOTP_SECRET in the environment wins when both exist. One row per agent and
-- never overwritten here: replacing a device means deleting the row on the
-- server, so a stolen dashboard session can't swap in its own authenticator.
CREATE TABLE owner_codes (
    agent_id    UUID        PRIMARY KEY,
    secret      TEXT        NOT NULL,
    enrolled_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
