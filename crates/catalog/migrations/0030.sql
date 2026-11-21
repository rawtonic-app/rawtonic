-- Assisted culling (G35): what the culling panel suggests per photograph.
-- `cull_scores` holds the misfire and document scores (0..1, SigLIP 2's
-- calibrated tag score of the best matching frozen prompt) computed from the
-- photograph's content embedding; `model` names the embedding model and the
-- prompt set that made them, `fingerprint` the file they were read from, so
-- a changed file or another model or prompt set is scored again. Derived
-- data only: dropping it loses nothing the pass cannot compute again.
-- `cull_dismissed` is the photographer's "not a problem" for one criterion
-- of one photograph: that suggestion is not shown again, whatever a new
-- reading says. Accepting a suggestion is the photograph's reject flag
-- itself (never stored here). No file is read or written here.
CREATE TABLE IF NOT EXISTS cull_scores(
 asset_id TEXT PRIMARY KEY REFERENCES assets(id) ON DELETE CASCADE,
 model TEXT NOT NULL,
 fingerprint TEXT NOT NULL,
 misfire REAL NOT NULL,
 document REAL NOT NULL);
CREATE TABLE IF NOT EXISTS cull_dismissed(
 asset_id TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
 criterion TEXT NOT NULL CHECK(criterion IN ('misfire','document','eyesClosed')),
 PRIMARY KEY(asset_id, criterion));
PRAGMA user_version=30;
