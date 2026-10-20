-- Named snapshots of a version's development (G26): the adjustments of one
-- moment, kept beside the version's history until deleted. `id` is the
-- snapshot's own (it travels in the editor sidecar), `key` orders them as
-- they were taken. Their long strings are stored once in `recipe_blobs`.
CREATE TABLE snapshots(
 key INTEGER PRIMARY KEY,
 id TEXT NOT NULL,
 version_id TEXT NOT NULL REFERENCES versions(id) ON DELETE CASCADE,
 name TEXT NOT NULL,
 created_at TEXT NOT NULL,
 adjustments TEXT NOT NULL,
 UNIQUE(version_id, id)
);
CREATE TABLE snapshot_blob_refs(
 snapshot_key INTEGER NOT NULL REFERENCES snapshots(key) ON DELETE CASCADE,
 hash TEXT NOT NULL REFERENCES recipe_blobs(hash),
 PRIMARY KEY(snapshot_key, hash)
);
CREATE INDEX snapshot_blob_refs_hash ON snapshot_blob_refs(hash);
PRAGMA user_version=12;
