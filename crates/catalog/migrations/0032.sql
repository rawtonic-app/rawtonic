-- The lens as the editor an import came from names it (`assets.lens_import`,
-- Lightroom's own lens name for the photograph). Smart albums imported from
-- Lightroom match their lens rules against it (field `lensImported`), as
-- Lightroom matched them, while `lens` stays the name the file states. Empty for
-- a photograph Lightroom records without a lens, NULL for one no import
-- named a lens for (the field then reads `lens`). The column is added before
-- this script runs (`migrations::add_missing_columns`).
PRAGMA user_version=32;
