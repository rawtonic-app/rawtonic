-- A library's hot folder (G47): what it watches, so watching resumes when
-- the library is opened again, and the files it has taken (by path relative
-- to the watched folder, size and modification time), so files that arrived
-- while the app was closed are picked up once and the ones taken before are
-- not hashed again. No row: no hot folder.
CREATE TABLE IF NOT EXISTS hot_folder(
 id INTEGER PRIMARY KEY CHECK(id=1),
 folder TEXT NOT NULL,
 template TEXT NOT NULL,
 include_subfolders INTEGER NOT NULL DEFAULT 0,
 settings_json TEXT NOT NULL DEFAULT '{}',
 last_capture TEXT REFERENCES assets(id) ON DELETE SET NULL,
 started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE TABLE IF NOT EXISTS hot_folder_files(
 rel_path TEXT PRIMARY KEY,
 size INTEGER NOT NULL,
 mtime_ns INTEGER
);
PRAGMA user_version=8;
