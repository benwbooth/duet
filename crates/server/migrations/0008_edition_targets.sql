-- A download fetched for a book that's already in the library ("Find another edition"):
-- when it finishes it joins that book instead of becoming a new one. Keyed by
-- "direct:<kind>:<title>" for HTTP downloads or "torrent:<hash>" for qBittorrent.
CREATE TABLE edition_targets (
    key TEXT PRIMARY KEY,
    work_id INTEGER NOT NULL REFERENCES works(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    replace INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

-- Editions taken out of the library when a new one replaced them. The files stay
-- where they are; scans and the inbox skip these paths so they don't come back.
CREATE TABLE hidden_paths (
    path TEXT PRIMARY KEY,
    hidden_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);
