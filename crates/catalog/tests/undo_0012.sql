-- Test helper: turns a schema 12 database back into schema 11 (as an older
-- Rawtonic left it), so tests can migrate it again. Snapshots are lost.
DROP TABLE snapshot_blob_refs;
DROP TABLE snapshots;
PRAGMA user_version=11;
