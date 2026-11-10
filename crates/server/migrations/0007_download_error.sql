-- Record why a download failed, so the UI can say more than "failed".
ALTER TABLE downloads ADD COLUMN error TEXT;
