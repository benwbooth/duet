-- Syncing, the full cast and text from audiobooks all happen as you read or listen now; the old
-- batch jobs for them are gone.
DELETE FROM jobs WHERE kind IN ('align', 'full_cast', 'transcribe');
