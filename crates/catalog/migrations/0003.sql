-- Volume identity of external (catalog) roots: roots.volume_id holds the
-- volume's UUID; its name and mount point when the root was registered let
-- a disconnected drive be told apart from a missing file, and a drive
-- mounted under another mount point be followed without relinking.
ALTER TABLE roots ADD COLUMN volume_name TEXT;
ALTER TABLE roots ADD COLUMN volume_path TEXT;
PRAGMA user_version=3;
