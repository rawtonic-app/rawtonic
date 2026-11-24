-- The camera naming the photographs' make and model were last given in
-- (`facts::one_camera_names`): the decoder catalogue's fingerprint. A
-- library whose names came from another catalogue (or before one name per
-- camera) has them named again when it opens. Library data only.
CREATE TABLE IF NOT EXISTS camera_naming(
 id INTEGER PRIMARY KEY CHECK(id = 1),
 fingerprint TEXT NOT NULL);
PRAGMA user_version=34;
