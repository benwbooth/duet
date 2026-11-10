-- One active download per book+kind: concurrent double-clicks collapse
-- into a single row (the loser gets a constraint error, treated as "already
-- on its way"). Finished rows are untouched, so retries still work.
CREATE UNIQUE INDEX IF NOT EXISTS idx_downloads_active
    ON downloads (name, kind) WHERE state = 'downloading';
