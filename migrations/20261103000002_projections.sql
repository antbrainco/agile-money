-- intents_view gains what the spend snapshot needs; spend is summed from it per
-- query, so the unused spend_view goes. Both tables are projections: rebuildable
-- from events.
DROP TABLE spend_view;
ALTER TABLE intents_view ADD COLUMN created_at TIMESTAMPTZ NOT NULL;
ALTER TABLE intents_view ADD COLUMN idempotency_key TEXT NOT NULL;
CREATE INDEX intents_view_spend ON intents_view (agent_id, asset, created_at);
CREATE INDEX policies_agent ON policies (agent_id, version DESC);
