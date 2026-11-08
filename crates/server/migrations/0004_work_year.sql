-- Publication year, read from ebook metadata on import; existing books pick
-- it up on their next scan (COALESCE keeps the first edition that has one).
ALTER TABLE works ADD COLUMN year INTEGER;
