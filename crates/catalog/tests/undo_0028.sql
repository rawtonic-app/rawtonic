ALTER TABLE assets DROP COLUMN capture_subsec;
ALTER TABLE assets DROP COLUMN capture_offset;
PRAGMA user_version=27;
