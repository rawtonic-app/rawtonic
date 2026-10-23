-- Test helper: turns a schema 14 database back into schema 13 (as an older
-- Rawtonic left it), so tests can migrate it again.
PRAGMA user_version=13;
