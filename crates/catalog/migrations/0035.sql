-- A virtual copy's own pick flag (`versions.flag`, added before this
-- script), as in Lightroom (notes: benchmark-results/smr6-2026-11-25).
-- Copies had their photograph's until now, so each keeps the flag it
-- showed. Library data only.
UPDATE versions SET flag=coalesce((SELECT m.flag FROM metadata m WHERE m.asset_id=versions.asset_id), 0)
 WHERE is_primary=0;
-- The flag of a copy the library forgot (its sidecar went to the Trash),
-- given back when the copy is recorded again (Undo): `Library::remove_copy`.
CREATE TABLE IF NOT EXISTS forgotten_copies(
 asset_id TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
 copy TEXT NOT NULL,
 flag INTEGER NOT NULL DEFAULT 0 CHECK(flag BETWEEN -1 AND 1),
 PRIMARY KEY(asset_id, copy));
PRAGMA user_version=35;
