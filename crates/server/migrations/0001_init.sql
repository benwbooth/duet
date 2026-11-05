CREATE TABLE works (
    id INTEGER PRIMARY KEY,
    title TEXT NOT NULL,
    author TEXT,
    series TEXT,
    series_index REAL,
    description TEXT,
    cover_path TEXT,
    -- title_key + normalized author, used to link ebooks and audiobooks of the same book.
    match_key TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);
CREATE INDEX works_match ON works(match_key);

CREATE TABLE assets (
    id INTEGER PRIMARY KEY,
    work_id INTEGER NOT NULL REFERENCES works(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,           -- ebook | audiobook
    format TEXT NOT NULL,
    path TEXT NOT NULL UNIQUE,    -- file, or folder for multi-track audiobooks
    size INTEGER NOT NULL DEFAULT 0,
    duration_secs REAL,
    origin TEXT NOT NULL DEFAULT 'scan',
    added_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

CREATE TABLE tracks (
    asset_id INTEGER NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
    idx INTEGER NOT NULL,
    path TEXT NOT NULL,
    title TEXT NOT NULL,
    duration_secs REAL NOT NULL,
    PRIMARY KEY (asset_id, idx)
);

CREATE TABLE positions (
    work_id INTEGER PRIMARY KEY REFERENCES works(id) ON DELETE CASCADE,
    data TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE sync_maps (
    work_id INTEGER PRIMARY KEY REFERENCES works(id) ON DELETE CASCADE,
    ebook_asset INTEGER NOT NULL,
    audiobook_asset INTEGER NOT NULL,
    path TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

CREATE TABLE cast_members (
    id INTEGER PRIMARY KEY,
    work_id INTEGER NOT NULL REFERENCES works(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    voice TEXT,
    is_narrator INTEGER NOT NULL DEFAULT 0,
    UNIQUE (work_id, name)
);

CREATE TABLE jobs (
    id INTEGER PRIMARY KEY,
    kind TEXT NOT NULL,
    work_id INTEGER NOT NULL REFERENCES works(id) ON DELETE CASCADE,
    status TEXT NOT NULL DEFAULT 'queued',
    progress REAL NOT NULL DEFAULT 0,
    message TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

-- Direct (HTTP) downloads; torrents live in qBittorrent and are tracked by category.
CREATE TABLE downloads (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    source TEXT NOT NULL,
    state TEXT NOT NULL,
    progress REAL NOT NULL DEFAULT 0,
    size INTEGER,
    path TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

-- Torrents already imported, so auto-import runs once per torrent.
CREATE TABLE imported_torrents (
    hash TEXT PRIMARY KEY,
    imported_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

CREATE TABLE settings (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    data TEXT NOT NULL
);
