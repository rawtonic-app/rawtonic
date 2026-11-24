-- A catalog import's wall time awaits the camera facts. It is not an
-- effective correction until compared with the camera date and fraction;
-- a real shift keeps the camera's UTC offset. No source file is written.
CREATE TABLE IF NOT EXISTS pending_capture_times(
 asset_id TEXT PRIMARY KEY REFERENCES assets(id) ON DELETE CASCADE,
 wall_time TEXT NOT NULL);
PRAGMA user_version=33;
