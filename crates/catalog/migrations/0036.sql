-- An album member is a photograph (`copy` '') or one of its virtual copies
-- (its id), a copy a member without its photograph as in Lightroom (notes:
-- benchmark-results/smr6-2026-11-25). `copy` is added before this script;
-- SQLite cannot change a primary key, so the table is rebuilt with its
-- rows. Nothing refers to it. Library data only.
CREATE TABLE collection_assets_v36(
 collection_id TEXT REFERENCES collections(id) ON DELETE CASCADE,
 asset_id TEXT REFERENCES assets(id) ON DELETE CASCADE,
 copy TEXT NOT NULL DEFAULT '',
 PRIMARY KEY(collection_id, asset_id, copy)
);
INSERT OR IGNORE INTO collection_assets_v36(collection_id, asset_id, copy)
 SELECT collection_id, asset_id, copy FROM collection_assets;
DROP TABLE collection_assets;
ALTER TABLE collection_assets_v36 RENAME TO collection_assets;
PRAGMA user_version=36;
