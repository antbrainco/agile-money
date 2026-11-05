-- The owner's dashboard profile: wallets that may sign in, and the Telegram
-- account that confirmed it is the owner chat set in warden.toml.

-- A wallet signs in in place of the password; the authenticator code is still
-- asked for once codes are on. Linking one needs a signature from it and a code.
CREATE TABLE owner_wallets (
    agent_id  UUID        NOT NULL,
    address   TEXT        NOT NULL,
    label     TEXT        NOT NULL,
    linked_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (agent_id, address)
);

-- The owner chat itself comes from warden.toml; this records who confirmed it,
-- by opening the dashboard's link in that chat, and when.
CREATE TABLE owner_telegram (
    agent_id  UUID        PRIMARY KEY,
    name      TEXT        NOT NULL,
    username  TEXT,
    linked_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
