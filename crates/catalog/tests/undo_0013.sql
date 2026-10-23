-- Test helper: turns a schema 13 database back into schema 12 (as an older
-- Rawtonic left it), so tests can migrate it again.
ALTER TABLE metadata DROP COLUMN from_camera;
PRAGMA user_version=12;
