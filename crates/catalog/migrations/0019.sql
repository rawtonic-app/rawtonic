-- Corrected capture times (Shift Capture Time), kept apart from what the
-- file says (assets.capture_time), which the facts pass may write again: a
-- reread never undoes a correction, and deleting the row gives back the
-- file's time. `captured` is the corrected local wall time in the facts'
-- form (YYYY-MM-DD HH:MM:SS); `subsec` the fraction digits and `offset` the
-- UTC offset as the file wrote them (EXIF SubSecTimeOriginal,
-- OffsetTimeOriginal), NULL when it wrote none. No file is read or written.
CREATE TABLE capture_overrides(
 asset_id TEXT PRIMARY KEY REFERENCES assets(id) ON DELETE CASCADE,
 captured TEXT NOT NULL, subsec TEXT, offset TEXT);
PRAGMA user_version=19;
