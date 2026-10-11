-- Album groups (Capture One) / collection sets (Lightroom) and a manual order.
ALTER TABLE collections ADD COLUMN kind TEXT NOT NULL DEFAULT 'album' CHECK(kind IN ('album','group'));
ALTER TABLE collections ADD COLUMN position INTEGER NOT NULL DEFAULT 0;
PRAGMA user_version=2;
