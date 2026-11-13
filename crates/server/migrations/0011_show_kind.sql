-- YouTube channels and playlists followed like podcasts (their sound pulled out with yt-dlp).
ALTER TABLE shows ADD COLUMN kind TEXT NOT NULL DEFAULT 'rss';
