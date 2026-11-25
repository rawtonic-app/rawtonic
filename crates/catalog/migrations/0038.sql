-- The sensor layout of each RAW photograph as read from its file's own
-- bytes (`sensor_kinds`, `sensor_kind::SensorKind`): 'bayer' or 'xTrans',
-- never a guess (a read that cannot tell stores nothing). `fingerprint` is
-- the file's when it was read; the kind counts only while the registered
-- file still has it, and a change of the registered size, time or
-- fingerprint (folder sync, relink, a save in place) drops it at once.
-- Only Develop's controls use it while the file is offline: renders and
-- exports read the sensor from the file.
CREATE TABLE IF NOT EXISTS sensor_kinds(
 asset_id TEXT PRIMARY KEY REFERENCES assets(id) ON DELETE CASCADE,
 kind TEXT NOT NULL CHECK(kind IN ('bayer','xTrans')),
 fingerprint TEXT NOT NULL);
CREATE TRIGGER IF NOT EXISTS assets_sensor_kind_stale AFTER UPDATE OF size,mtime,fingerprint ON assets
 WHEN new.size IS NOT old.size OR new.mtime IS NOT old.mtime OR new.fingerprint IS NOT old.fingerprint
 BEGIN DELETE FROM sensor_kinds WHERE asset_id=new.id; END;
PRAGMA user_version=38;
