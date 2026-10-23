-- Keep the drive classification when it is disconnected. Existing records
-- are classified when their mounted volume is next observed.
ALTER TABLE roots ADD COLUMN volume_kind TEXT CHECK(volume_kind IN ('startup','local','removable','network'));
-- A configured root may have no registered photographs yet. Keep its last
-- observed drive as well, rather than guessing from an offline path.
CREATE TABLE folder_root_volumes(path TEXT PRIMARY KEY, volume TEXT NOT NULL);
PRAGMA user_version=15;
