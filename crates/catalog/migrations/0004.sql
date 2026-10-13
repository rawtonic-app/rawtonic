-- Import batches (Lightroom's Previous Import): every ingest, Add Folder or
-- Synchronize Folder that adds photographs records one batch; each asset
-- remembers the batch that added it.
CREATE TABLE import_batches(
 id INTEGER PRIMARY KEY,
 kind TEXT NOT NULL CHECK(kind IN ('ingest','add','sync')),
 created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE TABLE asset_imports(
 asset_id TEXT PRIMARY KEY REFERENCES assets(id) ON DELETE CASCADE,
 batch_id INTEGER NOT NULL REFERENCES import_batches(id) ON DELETE CASCADE
);
CREATE INDEX asset_imports_batch ON asset_imports(batch_id);
PRAGMA user_version=4;
