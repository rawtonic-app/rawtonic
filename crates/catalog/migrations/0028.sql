-- Capture facts (G72): `assets.capture_subsec` and `assets.capture_offset`
-- hold the fraction of a second and the UTC offset the file states beside
-- its capture time (EXIF SubSecTimeOriginal, OffsetTimeOriginal), so an XMP
-- sidecar's capture time can be compared with the file's whole time: a
-- sub-second correction is one, an equal time none. Photographs read before
-- keep NULL (whole seconds) until their facts are read again. The columns
-- are added before this script runs (`migrations::add_missing_columns`).
PRAGMA user_version=28;
