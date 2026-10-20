-- Library search and smart albums (G36/G37).
--
-- Capture facts of each photograph, read from its file's EXIF (header only)
-- when it is added and by a background pass for photographs added before:
-- `capture_time` (local time as the camera recorded it,
-- `YYYY-MM-DD HH:MM:SS`), `make`, `camera` (the model), `lens`,
-- `focal_length` (mm), `aperture` (f-number), `iso` and `exposure`
-- (seconds). `facts_read` = 1 once the file was read (a file without EXIF
-- keeps NULLs), so the pass reads every file once.
ALTER TABLE assets ADD COLUMN make TEXT NOT NULL DEFAULT '';
ALTER TABLE assets ADD COLUMN focal_length REAL;
ALTER TABLE assets ADD COLUMN aperture REAL;
ALTER TABLE assets ADD COLUMN iso INTEGER;
ALTER TABLE assets ADD COLUMN exposure REAL;
ALTER TABLE assets ADD COLUMN facts_read INTEGER NOT NULL DEFAULT 0;
CREATE INDEX IF NOT EXISTS assets_facts_unread ON assets(facts_read) WHERE facts_read=0;
CREATE INDEX IF NOT EXISTS assets_capture_time ON assets(capture_time);
CREATE INDEX IF NOT EXISTS metadata_rating ON metadata(rating);

-- Keywords one row each (kept by triggers from the metadata JSON, whatever
-- writes it), so a keyword search does not parse every photograph's
-- metadata.
CREATE TABLE IF NOT EXISTS asset_keywords(
 asset_id TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
 keyword TEXT NOT NULL COLLATE NOCASE,
 PRIMARY KEY(asset_id, keyword)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS asset_keywords_keyword ON asset_keywords(keyword);
INSERT OR IGNORE INTO asset_keywords(asset_id, keyword)
 SELECT m.asset_id, j.value FROM metadata m, json_each(m.data, '$.keywords') j
 WHERE json_valid(m.data) AND j.type = 'text' AND j.value != '';
CREATE TRIGGER metadata_keywords_ai AFTER INSERT ON metadata BEGIN
 INSERT OR IGNORE INTO asset_keywords(asset_id, keyword)
  SELECT new.asset_id, value FROM json_each(new.data, '$.keywords')
  WHERE json_valid(new.data) AND type = 'text' AND value != '';
END;
CREATE TRIGGER metadata_keywords_au AFTER UPDATE OF data ON metadata BEGIN
 DELETE FROM asset_keywords WHERE asset_id = old.asset_id;
 INSERT OR IGNORE INTO asset_keywords(asset_id, keyword)
  SELECT new.asset_id, value FROM json_each(new.data, '$.keywords')
  WHERE json_valid(new.data) AND type = 'text' AND value != '';
END;

-- Text search finds a part of any word (`0123` in `DSC00123.ARW`, like
-- Lightroom's and Capture One's "contains"), so the index is a trigram one;
-- it also holds the camera and lens.
DROP TRIGGER IF EXISTS assets_ai;
DROP TRIGGER IF EXISTS assets_au;
DROP TRIGGER IF EXISTS assets_ad;
DROP TABLE IF EXISTS asset_search;
CREATE VIRTUAL TABLE asset_search USING fts5(asset_id UNINDEXED, content, tokenize='trigram');
INSERT INTO asset_search(asset_id, content)
 SELECT id, rel_path || ' ' || search_text || ' ' || make || ' ' || camera || ' ' || lens FROM assets;
CREATE TRIGGER assets_ai AFTER INSERT ON assets BEGIN
 INSERT INTO asset_search(asset_id, content)
  VALUES(new.id, new.rel_path || ' ' || new.search_text || ' ' || new.make || ' ' || new.camera || ' ' || new.lens);
END;
CREATE TRIGGER assets_au AFTER UPDATE OF rel_path, search_text, make, camera, lens ON assets BEGIN
 DELETE FROM asset_search WHERE asset_id = old.id;
 INSERT INTO asset_search(asset_id, content)
  VALUES(new.id, new.rel_path || ' ' || new.search_text || ' ' || new.make || ' ' || new.camera || ' ' || new.lens);
END;
CREATE TRIGGER assets_ad AFTER DELETE ON assets BEGIN
 DELETE FROM asset_search WHERE asset_id = old.id;
END;

-- Smart albums live among the albums (a group holds them too): kind
-- 'smart' with its rules (JSON, `crate::query::Rule`). SQLite cannot change
-- a CHECK constraint, so both tables are rebuilt with their rows, the child
-- first, so no cascade removes an album's photographs. The unused
-- `smart_collections` table of the first schema goes.
CREATE TABLE collections_v11(
 id TEXT PRIMARY KEY,
 name TEXT NOT NULL,
 parent TEXT REFERENCES collections_v11(id),
 kind TEXT NOT NULL DEFAULT 'album' CHECK(kind IN ('album','group','smart')),
 position INTEGER NOT NULL DEFAULT 0,
 rules TEXT,
 CHECK((kind = 'smart') = (rules IS NOT NULL))
);
INSERT INTO collections_v11(id, name, parent, kind, position)
 SELECT id, name, parent, kind, position FROM collections;
CREATE TABLE collection_assets_v11(
 collection_id TEXT REFERENCES collections_v11(id) ON DELETE CASCADE,
 asset_id TEXT REFERENCES assets(id) ON DELETE CASCADE,
 PRIMARY KEY(collection_id, asset_id)
);
INSERT INTO collection_assets_v11(collection_id, asset_id)
 SELECT collection_id, asset_id FROM collection_assets;
DROP TABLE collection_assets;
DROP TABLE collections;
ALTER TABLE collections_v11 RENAME TO collections;
ALTER TABLE collection_assets_v11 RENAME TO collection_assets;
DROP TABLE IF EXISTS smart_collections;
PRAGMA user_version=11;
