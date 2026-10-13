-- Full-content SHA-256 of each asset's file as registered (same size and
-- modification time): what proves a file found elsewhere is a missing
-- photograph byte for byte (Synchronize Folder's verified moves). Filled by
-- verified ingest and a throttled background pass; NULL until then.
ALTER TABLE assets ADD COLUMN sha256 TEXT;
PRAGMA user_version=5;
