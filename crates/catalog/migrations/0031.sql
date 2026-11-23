-- What an import from another editor's catalog or session (Lightroom,
-- Capture One) last wrote to a version, by SHA-256 digest: `source` of the
-- other editor's settings as read, `translated` of the recipe made of them,
-- `stored` of the recipe as the library held it right after. A repeated
-- import tells from them whether the photographer changed the edit here
-- since (then it is never replaced) and whether the other editor's settings
-- changed. No file is read or written.
CREATE TABLE IF NOT EXISTS imported_recipes(
 version_id TEXT PRIMARY KEY REFERENCES versions(id) ON DELETE CASCADE,
 source TEXT NOT NULL,
 translated TEXT NOT NULL,
 stored TEXT NOT NULL);
PRAGMA user_version=31;
