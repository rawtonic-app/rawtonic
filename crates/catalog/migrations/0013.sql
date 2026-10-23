-- Capture facts read once more (G36/G37 use test 2026-10-21): the reader
-- now finds a CR3's metadata wherever it is in the file (larger CR3s got
-- none from the first 16 MiB) and the rating the camera wrote into the
-- file. Photographs read without a capture date, whatever their metadata
-- (someone may have rated a CR3 read without any fact), and those whose
-- metadata was never set (the camera's rating may become theirs) are read
-- again by the background pass; what it records replaces only facts,
-- never a rating anyone gave (`facts::write_facts`).
--
-- `metadata.from_camera` = 1 while a photograph's metadata is only the
-- rating its camera wrote into the file: it still counts as untouched
-- (`add::untouched`), so a sidecar that appears later is adopted over it.
-- Any other metadata write clears it.
ALTER TABLE metadata ADD COLUMN from_camera INTEGER NOT NULL DEFAULT 0;
UPDATE assets SET facts_read=0 WHERE facts_read=1 AND (
 capture_time IS NULL
 OR id IN (SELECT asset_id FROM metadata WHERE rating=0 AND data='{}'));
PRAGMA user_version=13;
