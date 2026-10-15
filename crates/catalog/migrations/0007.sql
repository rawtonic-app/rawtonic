-- Large recipe values (AI mask bitmaps, Relight analyses: data URLs of
-- hundreds of kilobytes) are stored once by their SHA-256; recipes, every
-- undo step included, refer to them ("rawtonic-blob:sha256:<hex>"). Without
-- it each history step copied them again.
CREATE TABLE IF NOT EXISTS recipe_blobs(hash TEXT PRIMARY KEY, data TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS recipe_blob_refs(
 recipe_id INTEGER NOT NULL REFERENCES recipes(id) ON DELETE CASCADE,
 hash TEXT NOT NULL REFERENCES recipe_blobs(hash),
 PRIMARY KEY(recipe_id, hash)
);
CREATE INDEX IF NOT EXISTS recipe_blob_refs_hash ON recipe_blob_refs(hash);
PRAGMA user_version=7;
