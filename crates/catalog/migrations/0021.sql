-- Eyes per face for culling (rawtonic_faces::eyes), read by the app's
-- background pass from the photograph as shot (upright, uncropped).
-- `eye_reads`: one row per photograph read, with the reader's version and
-- the file's fingerprint then, so a newer reader or a changed file reads it
-- again; `error` when it could not be read (no faces rows then). A
-- photograph without faces has a read row and no faces rows.
-- `face_eyes`: one row per face, left to right; the box as fractions of
-- the upright photo; `state` the reader's call, `ear` and `accessories`
-- its scores. `override_state` is the photographer's own call, which wins
-- everywhere and is carried to the same face when the photograph is read
-- again. No file is read or written here.
CREATE TABLE IF NOT EXISTS eye_reads(
 asset_id TEXT PRIMARY KEY REFERENCES assets(id) ON DELETE CASCADE,
 version INTEGER NOT NULL,
 fingerprint TEXT NOT NULL,
 error TEXT);
CREATE TABLE IF NOT EXISTS face_eyes(
 asset_id TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
 face INTEGER NOT NULL,
 x REAL NOT NULL, y REAL NOT NULL, w REAL NOT NULL, h REAL NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('open','closed','notVisible','uncertain')),
 ear REAL,
 accessories REAL,
 override_state TEXT CHECK(override_state IN ('open','closed','notVisible')),
 PRIMARY KEY(asset_id, face));
PRAGMA user_version=21;
