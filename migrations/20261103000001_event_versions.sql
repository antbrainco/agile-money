-- Per-stream version for optimistic concurrency: two writers appending to the same
-- stream position collide on the unique constraint instead of both succeeding.
ALTER TABLE events ADD COLUMN version BIGINT NOT NULL;
ALTER TABLE events ADD CONSTRAINT events_stream_version UNIQUE (stream, version);
DROP INDEX events_stream_seq;
