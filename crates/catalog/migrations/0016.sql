-- Distinguish deliberate system-trash operations from disconnected originals.
-- Album/import memberships remain intact for undo.
ALTER TABLE assets ADD COLUMN trashed INTEGER NOT NULL DEFAULT 0 CHECK(trashed IN (0,1));
PRAGMA user_version=16;
