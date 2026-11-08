-- Details looked up online for each book: its subjects, how many pages, and when the lookup
-- was done (so it's done once).
ALTER TABLE works ADD COLUMN subjects TEXT;
ALTER TABLE works ADD COLUMN pages INTEGER;
ALTER TABLE works ADD COLUMN looked_up_at TEXT;
