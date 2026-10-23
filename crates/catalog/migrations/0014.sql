-- Capture facts read once more (G37): the reader now follows a TIFF-based
-- file's IFDs past its header when the header holds no date (a DNG that
-- keeps its IFDs after its image data, as merges write them, got none
-- from the first 16 MiB). Photographs read without a capture date are
-- read again by the background pass, whatever their format (the reader
-- decides what it can follow; an undated file is read once more and keeps
-- what it had); what it records replaces only facts, never a rating
-- anyone gave (`facts::write_facts`).
UPDATE assets SET facts_read=0 WHERE facts_read=1 AND capture_time IS NULL;
PRAGMA user_version=14;
