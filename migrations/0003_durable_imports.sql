ALTER TABLE background_tasks ADD COLUMN IF NOT EXISTS payload TEXT;
ALTER TABLE background_tasks ADD COLUMN IF NOT EXISTS attempt INT NOT NULL DEFAULT 0;
ALTER TABLE background_tasks ADD COLUMN IF NOT EXISTS updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW();
CREATE INDEX IF NOT EXISTS idx_background_tasks_pending ON background_tasks(kind,status,updated_at);
