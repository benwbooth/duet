-- The edition a book opens with: chosen on its page, or the last one switched to in the reader.
ALTER TABLE works ADD COLUMN default_ebook INTEGER REFERENCES assets(id) ON DELETE SET NULL;
ALTER TABLE works ADD COLUMN default_audio INTEGER REFERENCES assets(id) ON DELETE SET NULL;
