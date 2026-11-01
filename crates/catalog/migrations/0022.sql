-- Stacks (G36): photographs grouped under one top photograph, library data
-- only (no standard XMP field holds a stack; files and sidecars are never
-- written for it). `stacks`: one row per stack, `collapsed` = 1 while the
-- folder and session views show it as its top photograph alone.
-- `stack_members`: one row per photograph (`copy` = '') or virtual copy
-- (`copy` = its id) in a stack; a photograph is in at most one stack.
-- `position` orders a stack, the lowest being its top: when the top leaves
-- (removed from the library, its copy forgotten) the next member is the
-- top, and a stack left with one member is dissolved by the triggers. The
-- `stack` column of `assets` stays what it was (Edit With's render link).
CREATE TABLE IF NOT EXISTS stacks(
 id TEXT PRIMARY KEY,
 collapsed INTEGER NOT NULL DEFAULT 1 CHECK(collapsed IN (0,1)));
CREATE TABLE IF NOT EXISTS stack_members(
 stack_id TEXT NOT NULL REFERENCES stacks(id) ON DELETE CASCADE,
 asset_id TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
 copy TEXT NOT NULL DEFAULT '',
 position INTEGER NOT NULL,
 PRIMARY KEY(asset_id, copy));
CREATE INDEX IF NOT EXISTS stack_members_stack ON stack_members(stack_id, position);
CREATE TRIGGER IF NOT EXISTS stack_members_dissolve AFTER DELETE ON stack_members
BEGIN
 DELETE FROM stacks WHERE id=OLD.stack_id
  AND (SELECT count(*) FROM stack_members WHERE stack_id=OLD.stack_id) < 2;
END;
CREATE TRIGGER IF NOT EXISTS stack_members_copy_gone AFTER DELETE ON versions
WHEN OLD.is_primary=0
BEGIN
 DELETE FROM stack_members WHERE asset_id=OLD.asset_id AND copy=OLD.name;
END;
-- Burst and bracket facts for Auto-Stack, read with the capture facts:
-- `exposure_mode` (EXIF ExposureMode: 0 auto, 1 manual, 2 auto bracket),
-- `exposure_bias` (EXIF ExposureBiasValue, EV) and `sequence` (the
-- maker's frame number within a continuous-drive burst, 1 for its first
-- frame, where the reader knows the maker's field; NULL otherwise). Read
-- again for every dated photograph (only those can be auto-stacked) by the
-- background pass, which replaces only
-- capture facts, never a rating anyone gave (`facts::write_facts`). The
-- columns are added before this script runs (`Library::add_missing_columns`:
-- only where missing, so the script can run again on a library a test
-- took back to an older schema).
UPDATE assets SET facts_read=0 WHERE facts_read=1 AND capture_time IS NOT NULL;
PRAGMA user_version=22;
