-- The Telegram message carrying each open approval card, so an answer on the
-- dashboard or an expiry can take its buttons off, even after a restart.
CREATE TABLE approval_cards (
    approval_id UUID PRIMARY KEY,
    message_id  INTEGER NOT NULL,
    sent_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);
