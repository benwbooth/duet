-- Podcasts you follow (the Podcasts tab; the daily briefing keeps its own files).
CREATE TABLE shows (
    id INTEGER PRIMARY KEY,
    feed_url TEXT NOT NULL UNIQUE,
    title TEXT NOT NULL DEFAULT '',
    author TEXT NOT NULL DEFAULT '',
    description TEXT NOT NULL DEFAULT '',
    image TEXT,
    link TEXT NOT NULL DEFAULT '',
    skip_ads INTEGER NOT NULL DEFAULT 1,
    added TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    refreshed TEXT
);

CREATE TABLE show_episodes (
    id INTEGER PRIMARY KEY,
    show_id INTEGER NOT NULL REFERENCES shows(id) ON DELETE CASCADE,
    guid TEXT NOT NULL,
    title TEXT NOT NULL DEFAULT '',
    notes TEXT NOT NULL DEFAULT '',
    published TEXT,
    duration REAL NOT NULL DEFAULT 0,
    audio_url TEXT NOT NULL,
    image TEXT,
    position REAL NOT NULL DEFAULT 0,
    finished INTEGER NOT NULL DEFAULT 0,
    played_at TEXT,
    UNIQUE (show_id, guid)
);
CREATE INDEX show_episodes_published ON show_episodes (show_id, published DESC);
CREATE INDEX show_episodes_played ON show_episodes (played_at DESC);
