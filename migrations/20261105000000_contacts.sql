-- Saved payees. A contact the owner adds is proposed, confirmed with a code,
-- and active only after a cooling-off period; the active set is what the
-- policy's recipient allowlist holds. Rows are never deleted: closed ones
-- (cancelled, removed, expired) stay as the record of who was payable when.
CREATE TABLE contacts (
    contact_id   UUID        PRIMARY KEY,
    agent_id     UUID        NOT NULL,
    label        TEXT        NOT NULL,
    address      JSONB       NOT NULL,
    state        TEXT        NOT NULL,
    source       TEXT        NOT NULL,
    -- The active contact this one takes over from when it goes live.
    replaces     UUID        REFERENCES contacts (contact_id),
    proposed_at  TIMESTAMPTZ NOT NULL,
    confirmed_at TIMESTAMPTZ,
    active_at    TIMESTAMPTZ,
    closed_at    TIMESTAMPTZ
);
CREATE UNIQUE INDEX contacts_active_label ON contacts (agent_id, lower(label)) WHERE state = 'active';
CREATE UNIQUE INDEX contacts_active_address ON contacts (agent_id, address) WHERE state = 'active';
CREATE INDEX contacts_open ON contacts (agent_id, state) WHERE state IN ('proposed', 'confirmed');

-- The pause switch, so a restart can't quietly resume payments.
CREATE TABLE switches (
    agent_id   UUID        PRIMARY KEY,
    paused     BOOLEAN     NOT NULL,
    changed_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
