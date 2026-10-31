-- Find Duplicates (duplicates.rs, image_hash.rs): the hash of each
-- photograph's image data as its file stores it (metadata blocks, names and
-- edits left out), with the file size and modification time (nanoseconds)
-- it was read at; a hash counts only while the file still has them. A
-- change of the registered size, time or fingerprint (folder sync, relink)
-- drops it at once. Computed on demand by a background pass, never on open.
CREATE TABLE IF NOT EXISTS image_hashes(
 asset_id TEXT PRIMARY KEY REFERENCES assets(id) ON DELETE CASCADE,
 hash TEXT NOT NULL, size INTEGER NOT NULL, mtime INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS image_hashes_hash ON image_hashes(hash);
CREATE TRIGGER IF NOT EXISTS assets_image_hash_stale AFTER UPDATE OF size,mtime,fingerprint ON assets
 WHEN new.size IS NOT old.size OR new.mtime IS NOT old.mtime OR new.fingerprint IS NOT old.fingerprint
 BEGIN DELETE FROM image_hashes WHERE asset_id=new.id; END;
PRAGMA user_version=20;
