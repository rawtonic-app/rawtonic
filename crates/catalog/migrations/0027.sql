-- Corrected capture time in XMP (G72): `metadata.xmp_capture` holds the
-- capture time (XMP date) the library's correction put into the photo's XMP
-- sidecar or took from it, so a reset removes exactly that value
-- (`exif:DateTimeOriginal`, `photoshop:DateCreated`) and no date another
-- editor wrote. NULL: the sidecar holds no correction of the library's.
-- The column is added before this script runs (`migrations::add_missing_columns`).
PRAGMA user_version=27;
