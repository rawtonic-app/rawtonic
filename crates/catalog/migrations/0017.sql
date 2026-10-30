-- AI and provenance facts for the library's search (N5).
--
-- `versions.ai_facts`: what each version's recipe says about AI
-- (`rawtonic_metadata::provenance::ai_facts` bits: generated areas and the
-- model that filled them, AI selections, AI Denoise, AI results still
-- missing), computed by `rawtonic_ai_facts` (registered on every connection
-- the library opens, as `rawtonic_fold`) from the version's head recipe or,
-- for a virtual copy, its editor state. Triggers keep it whatever writes a
-- recipe (an edit, undo, an import, a sidecar adopted, a copy recorded), so
-- old sessions are indexed here once and no recipe is changed.
ALTER TABLE versions ADD COLUMN ai_facts INTEGER NOT NULL DEFAULT 0;
UPDATE versions SET ai_facts = rawtonic_ai_facts(
 CASE WHEN head IS NOT NULL THEN (SELECT payload FROM recipes WHERE id=versions.head) ELSE legacy_state END);
CREATE INDEX IF NOT EXISTS versions_ai_facts ON versions(ai_facts, asset_id) WHERE ai_facts != 0;
CREATE TRIGGER versions_ai_facts_ai AFTER INSERT ON versions BEGIN
 UPDATE versions SET ai_facts = rawtonic_ai_facts(
  CASE WHEN new.head IS NOT NULL THEN (SELECT payload FROM recipes WHERE id=new.head) ELSE new.legacy_state END)
 WHERE id = new.id;
END;
CREATE TRIGGER versions_ai_facts_au AFTER UPDATE OF head, legacy_state ON versions BEGIN
 UPDATE versions SET ai_facts = rawtonic_ai_facts(
  CASE WHEN new.head IS NOT NULL THEN (SELECT payload FROM recipes WHERE id=new.head) ELSE new.legacy_state END)
 WHERE id = new.id;
END;
-- A head recipe rewritten in place (resources moved with a library).
CREATE TRIGGER recipes_ai_facts_au AFTER UPDATE OF payload ON recipes BEGIN
 UPDATE versions SET ai_facts = rawtonic_ai_facts(new.payload) WHERE head = new.id;
END;

-- The IPTC Digital Source Type a file brings in its own XMP (an image a
-- generator or another editor marked), as its IPTC code
-- (`provenance::source_type_code`); '' when it says none. Read with the
-- capture facts; files that may carry one (every type but camera RAWs)
-- are read once again for it.
ALTER TABLE assets ADD COLUMN source_type TEXT NOT NULL DEFAULT '';
UPDATE assets SET facts_read=0 WHERE facts_read=1 AND lower(kind) IN
 ('dng','jpg','jpeg','jpe','heic','heif','hif','avif','tif','tiff','png','webp','jxl','psd');
PRAGMA user_version=17;
