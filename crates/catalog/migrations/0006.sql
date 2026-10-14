-- Hot folder captures form import batches too ('hotfolder'). SQLite cannot
-- change a CHECK constraint, so both tables are rebuilt with their rows;
-- the child first, so no cascade deletes a photograph's batch.
CREATE TABLE import_batches_v6(
 id INTEGER PRIMARY KEY,
 kind TEXT NOT NULL CHECK(kind IN ('ingest','add','sync','hotfolder')),
 created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
INSERT INTO import_batches_v6(id,kind,created_at) SELECT id,kind,created_at FROM import_batches;
CREATE TABLE asset_imports_v6(
 asset_id TEXT PRIMARY KEY REFERENCES assets(id) ON DELETE CASCADE,
 batch_id INTEGER NOT NULL REFERENCES import_batches_v6(id) ON DELETE CASCADE
);
INSERT INTO asset_imports_v6(asset_id,batch_id) SELECT asset_id,batch_id FROM asset_imports;
DROP TABLE asset_imports;
DROP TABLE import_batches;
ALTER TABLE import_batches_v6 RENAME TO import_batches;
ALTER TABLE asset_imports_v6 RENAME TO asset_imports;
CREATE INDEX asset_imports_batch ON asset_imports(batch_id);
PRAGMA user_version=6;
