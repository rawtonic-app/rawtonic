-- The sensor layout of each RAW photograph as read from its file's own
-- bytes (`sensor_kinds`, `sensor_kind::SensorKind`): 'bayer' or 'xTrans',
-- never a guess (a read that cannot tell stores nothing). `fingerprint` is
-- the file's when it was read; a later read of the photograph's file
-- replaces the row, and a change of the registered size, time or
-- fingerprint (folder sync, a save in place) drops it at once. A new
-- location (relink, move) drops a row read from another file than the
-- registered one: the photograph's file there is the registered one.
-- Only Develop's controls use it while the file is offline: renders and
-- exports read the sensor from the file.
CREATE TABLE IF NOT EXISTS sensor_kinds(
 asset_id TEXT PRIMARY KEY REFERENCES assets(id) ON DELETE CASCADE,
 kind TEXT NOT NULL CHECK(kind IN ('bayer','xTrans')),
 fingerprint TEXT NOT NULL);
CREATE TRIGGER IF NOT EXISTS assets_sensor_kind_stale AFTER UPDATE OF size,mtime,fingerprint ON assets
 WHEN new.size IS NOT old.size OR new.mtime IS NOT old.mtime OR new.fingerprint IS NOT old.fingerprint
 BEGIN DELETE FROM sensor_kinds WHERE asset_id=new.id; END;
CREATE TRIGGER IF NOT EXISTS assets_sensor_kind_moved AFTER UPDATE OF root_id,rel_path ON assets
 WHEN new.root_id IS NOT old.root_id OR new.rel_path IS NOT old.rel_path
 BEGIN DELETE FROM sensor_kinds WHERE asset_id=new.id AND fingerprint IS NOT new.fingerprint; END;
PRAGMA user_version=38;
