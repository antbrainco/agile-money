-- Append-only event log (rule 12): no UPDATE or DELETE grants for the app role.
CREATE TABLE events (
    seq     BIGSERIAL PRIMARY KEY,
    stream  TEXT        NOT NULL,
    type    TEXT        NOT NULL,
    body    JSONB       NOT NULL,
    at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX events_stream_seq ON events (stream, seq);

CREATE OR REPLACE FUNCTION events_append_only() RETURNS trigger AS $$
BEGIN
    RAISE EXCEPTION 'events is append-only';
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER events_no_update BEFORE UPDATE OR DELETE ON events
    FOR EACH ROW EXECUTE FUNCTION events_append_only();

-- Commands for the executor, written in the same transaction as their events (rule 11).
CREATE TABLE outbox (
    id           BIGSERIAL PRIMARY KEY,
    intent_id    UUID        NOT NULL,
    kind         TEXT        NOT NULL,
    body         JSONB       NOT NULL,
    attempts     INT         NOT NULL DEFAULT 0,
    next_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    locked_until TIMESTAMPTZ,
    done_at      TIMESTAMPTZ
);
CREATE INDEX outbox_ready ON outbox (next_at) WHERE done_at IS NULL;

CREATE TABLE idempotency (
    key       TEXT PRIMARY KEY,
    intent_id UUID NOT NULL
);

CREATE TABLE intents_view (
    intent_id    UUID PRIMARY KEY,
    agent_id     UUID        NOT NULL,
    state        TEXT        NOT NULL,
    amount_minor NUMERIC(39) NOT NULL,
    asset        TEXT        NOT NULL,
    rail         TEXT        NOT NULL,
    updated_at   TIMESTAMPTZ NOT NULL
);

CREATE TABLE spend_view (
    agent_id       UUID        NOT NULL,
    asset          TEXT        NOT NULL,
    settled_minor  NUMERIC(39) NOT NULL DEFAULT 0,
    reserved_minor NUMERIC(39) NOT NULL DEFAULT 0,
    window_start   TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (agent_id, asset)
);

CREATE TABLE policies (
    policy_id  UUID        NOT NULL,
    version    INT         NOT NULL,
    agent_id   UUID        NOT NULL,
    body       JSONB       NOT NULL,
    created_by UUID        NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (policy_id, version)
);
