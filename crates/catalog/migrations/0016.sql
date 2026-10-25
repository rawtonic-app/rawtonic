-- Distinguish deliberate system-trash operations from disconnected originals.
-- Album/import memberships remain intact for undo.
-- Historical system-trash records only have missing=1, indistinguishable
-- from disconnected originals: leave them unchanged rather than guessing.
-- Undo, a found-file check or explicit re-add/relocate restores those rows.
ALTER TABLE assets ADD COLUMN trashed INTEGER NOT NULL DEFAULT 0 CHECK(trashed IN (0,1));
PRAGMA user_version=16;
