CREATE TABLE roots(id TEXT PRIMARY KEY, path TEXT NOT NULL, volume_id TEXT, external INTEGER NOT NULL DEFAULT 0);
CREATE TABLE assets(
 id TEXT PRIMARY KEY, root_id TEXT NOT NULL REFERENCES roots(id), rel_path TEXT NOT NULL,
 size INTEGER NOT NULL, mtime INTEGER NOT NULL, fingerprint TEXT NOT NULL, capture_time TEXT,
 camera TEXT NOT NULL DEFAULT '', lens TEXT NOT NULL DEFAULT '', width INTEGER, height INTEGER,
 orientation INTEGER NOT NULL DEFAULT 1, kind TEXT NOT NULL, raw_jpeg_pair TEXT, stack TEXT,
 missing INTEGER NOT NULL DEFAULT 0, search_text TEXT NOT NULL DEFAULT '', UNIQUE(root_id, rel_path)
);
CREATE INDEX assets_fingerprint ON assets(fingerprint);
CREATE TABLE versions(id TEXT PRIMARY KEY, asset_id TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
 name TEXT NOT NULL, is_primary INTEGER NOT NULL DEFAULT 0, head INTEGER, legacy_state TEXT);
CREATE UNIQUE INDEX primary_version ON versions(asset_id) WHERE is_primary=1;
CREATE TABLE recipes(id INTEGER PRIMARY KEY, version_id TEXT NOT NULL REFERENCES versions(id) ON DELETE CASCADE,
 parent INTEGER REFERENCES recipes(id), schema_version INTEGER NOT NULL DEFAULT 1, payload TEXT NOT NULL,
 snapshot TEXT, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
CREATE TABLE metadata(asset_id TEXT PRIMARY KEY REFERENCES assets(id) ON DELETE CASCADE,
 rating INTEGER NOT NULL DEFAULT 0 CHECK(rating BETWEEN 0 AND 5), label TEXT NOT NULL DEFAULT '',
 flag INTEGER NOT NULL DEFAULT 0 CHECK(flag BETWEEN -1 AND 1), data TEXT NOT NULL DEFAULT '{}',
 xmp_hash TEXT, xmp_status TEXT NOT NULL DEFAULT 'pending');
CREATE TABLE masks(id TEXT PRIMARY KEY, version_id TEXT NOT NULL REFERENCES versions(id), vector_json TEXT, raster_path TEXT, hash TEXT);
CREATE TABLE collections(id TEXT PRIMARY KEY, name TEXT NOT NULL, parent TEXT REFERENCES collections(id));
CREATE TABLE collection_assets(collection_id TEXT REFERENCES collections(id) ON DELETE CASCADE,asset_id TEXT REFERENCES assets(id) ON DELETE CASCADE,PRIMARY KEY(collection_id,asset_id));
CREATE TABLE smart_collections(id TEXT PRIMARY KEY, name TEXT NOT NULL, query_json TEXT NOT NULL);
CREATE TABLE presets(id TEXT PRIMARY KEY, name TEXT NOT NULL, recipe_json TEXT NOT NULL);
CREATE TABLE styles(id TEXT PRIMARY KEY, name TEXT NOT NULL, recipe_json TEXT NOT NULL);
CREATE TABLE export_recipes(id TEXT PRIMARY KEY, name TEXT NOT NULL, recipe_json TEXT NOT NULL);
CREATE TABLE naming_templates(id TEXT PRIMARY KEY, name TEXT NOT NULL, template TEXT NOT NULL);
CREATE TABLE workspaces(id TEXT PRIMARY KEY, name TEXT NOT NULL, settings_json TEXT NOT NULL);
CREATE TABLE keymaps(id TEXT PRIMARY KEY, name TEXT NOT NULL, bindings_json TEXT NOT NULL);
CREATE TABLE tasks(id TEXT PRIMARY KEY, kind TEXT NOT NULL, state TEXT NOT NULL, payload TEXT NOT NULL, progress REAL NOT NULL DEFAULT 0, error TEXT, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
CREATE TABLE file_ops_log(id TEXT PRIMARY KEY, kind TEXT NOT NULL, state TEXT NOT NULL, payload TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
CREATE VIRTUAL TABLE asset_search USING fts5(asset_id UNINDEXED, content);
CREATE TRIGGER assets_ai AFTER INSERT ON assets BEGIN INSERT INTO asset_search(asset_id,content) VALUES(new.id,new.rel_path || ' ' || new.search_text); END;
CREATE TRIGGER assets_au AFTER UPDATE OF rel_path,search_text ON assets BEGIN DELETE FROM asset_search WHERE asset_id=old.id; INSERT INTO asset_search(asset_id,content) VALUES(new.id,new.rel_path || ' ' || new.search_text); END;
CREATE TRIGGER assets_ad AFTER DELETE ON assets BEGIN DELETE FROM asset_search WHERE asset_id=old.id; END;
PRAGMA user_version=1;
