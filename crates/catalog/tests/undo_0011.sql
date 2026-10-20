-- Test helper: turns a schema 11 database back into schema 10 (as an older
-- Rawtonic left it), so tests can migrate it again. Albums keep their rows.
DROP TRIGGER metadata_keywords_ai;
DROP TRIGGER metadata_keywords_au;
DROP TABLE asset_keywords;
DROP TRIGGER assets_ai;
DROP TRIGGER assets_au;
DROP TRIGGER assets_ad;
DROP TABLE asset_search;
CREATE VIRTUAL TABLE asset_search USING fts5(asset_id UNINDEXED, content);
INSERT INTO asset_search(asset_id,content) SELECT id, rel_path || ' ' || search_text FROM assets;
CREATE TRIGGER assets_ai AFTER INSERT ON assets BEGIN INSERT INTO asset_search(asset_id,content) VALUES(new.id,new.rel_path || ' ' || new.search_text); END;
CREATE TRIGGER assets_au AFTER UPDATE OF rel_path,search_text ON assets BEGIN DELETE FROM asset_search WHERE asset_id=old.id; INSERT INTO asset_search(asset_id,content) VALUES(new.id,new.rel_path || ' ' || new.search_text); END;
CREATE TRIGGER assets_ad AFTER DELETE ON assets BEGIN DELETE FROM asset_search WHERE asset_id=old.id; END;
DROP INDEX assets_facts_unread;
DROP INDEX assets_capture_time;
DROP INDEX metadata_rating;
ALTER TABLE assets DROP COLUMN make;
ALTER TABLE assets DROP COLUMN focal_length;
ALTER TABLE assets DROP COLUMN aperture;
ALTER TABLE assets DROP COLUMN iso;
ALTER TABLE assets DROP COLUMN exposure;
ALTER TABLE assets DROP COLUMN facts_read;
PRAGMA foreign_keys=OFF;
CREATE TABLE c10(id TEXT PRIMARY KEY, name TEXT NOT NULL, parent TEXT REFERENCES collections(id),
 kind TEXT NOT NULL DEFAULT 'album' CHECK(kind IN ('album','group')), position INTEGER NOT NULL DEFAULT 0);
INSERT INTO c10 SELECT id,name,parent,kind,position FROM collections WHERE kind!='smart';
DROP TABLE collections;
ALTER TABLE c10 RENAME TO collections;
CREATE TABLE smart_collections(id TEXT PRIMARY KEY, name TEXT NOT NULL, query_json TEXT NOT NULL);
PRAGMA foreign_keys=ON;
PRAGMA user_version=10;
