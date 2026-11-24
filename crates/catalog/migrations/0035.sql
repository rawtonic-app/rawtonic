-- A virtual copy's own pick flag (`versions.flag`, added before this
-- script), as in Lightroom (notes: benchmark-results/smr6-2026-11-25).
-- Copies had their photograph's until now, so each keeps the flag it
-- showed. Library data only.
UPDATE versions SET flag=coalesce((SELECT m.flag FROM metadata m WHERE m.asset_id=versions.asset_id), 0)
 WHERE is_primary=0;
PRAGMA user_version=35;
