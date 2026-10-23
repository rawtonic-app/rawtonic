ALTER TABLE roots DROP COLUMN volume_kind;
DROP TABLE folder_root_volumes;
PRAGMA user_version=14;
