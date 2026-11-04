-- Where a policy version came from: warden.toml at startup, or a human on the
-- dashboard (rule 9). Startup only reseeds when the file itself changed, so a
-- dashboard edit survives restarts.
ALTER TABLE policies ADD COLUMN source TEXT NOT NULL DEFAULT 'config';
