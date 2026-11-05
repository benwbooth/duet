-- A sync map per ebook/audiobook pairing, so the original and generated editions can each be
-- read along with either audiobook.
CREATE TABLE sync_maps_new (
    work_id INTEGER NOT NULL REFERENCES works(id) ON DELETE CASCADE,
    ebook_asset INTEGER NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
    audiobook_asset INTEGER NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    PRIMARY KEY (ebook_asset, audiobook_asset)
);
INSERT INTO sync_maps_new (work_id, ebook_asset, audiobook_asset, path, created_at)
    SELECT work_id, ebook_asset, audiobook_asset, path, created_at FROM sync_maps
    WHERE ebook_asset IN (SELECT id FROM assets) AND audiobook_asset IN (SELECT id FROM assets);
DROP TABLE sync_maps;
ALTER TABLE sync_maps_new RENAME TO sync_maps;
CREATE INDEX sync_maps_work ON sync_maps(work_id);

-- Which editions a job works on (empty: the job picks).
ALTER TABLE jobs ADD COLUMN ebook_asset INTEGER;
ALTER TABLE jobs ADD COLUMN audiobook_asset INTEGER;
