-- "Older AI Denoise version" (AID, aiDenoiseVersion 4): version 4 became the
-- latest, so a recipe saved with version 3 by an earlier build carries
-- AI Denoise but not yet the older-version fact (`versions.ai_facts` is
-- computed when a recipe is written). The fact is computed again for every
-- version that has AI Denoise on, from the same recipe or editor state as
-- the triggers of schema 17 use. Facts only: no recipe, sidecar or file is
-- changed, and running it again gives the same facts. (An UPDATE of
-- `ai_facts` itself fires none of those triggers.)
UPDATE versions SET ai_facts = rawtonic_ai_facts(
 CASE WHEN head IS NOT NULL THEN (SELECT payload FROM recipes WHERE id=versions.head) ELSE legacy_state END)
 WHERE (ai_facts & 4) != 0;
PRAGMA user_version=26;
