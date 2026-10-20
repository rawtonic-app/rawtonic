-- A RAW's companion JPEG (ADR 0013) is identified by its size and content
-- fingerprint besides its name, so a file of that name that is not it (the
-- RAW moved or relinked next to another JPEG) is never taken for it.
ALTER TABLE assets ADD COLUMN companion_size INTEGER;
ALTER TABLE assets ADD COLUMN companion_fingerprint TEXT;
-- Ingest jobs (G66): the photographs of an ingest (job: `tasks`, kind
-- 'ingest', its options in `payload`) in import order, each with the copy
-- planned before it is made, so an ingest a quit, crash or card taken out
-- cut off is finished later without a copy made twice or a file left out.
CREATE TABLE IF NOT EXISTS ingest_items(
 job_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
 position INTEGER NOT NULL,
 number INTEGER NOT NULL,
 source TEXT NOT NULL,
 companion TEXT,
 state TEXT NOT NULL CHECK(state IN ('pending','copying','done','failed')),
 destination TEXT,
 companion_destination TEXT,
 error TEXT,
 PRIMARY KEY(job_id, position)
);
CREATE INDEX IF NOT EXISTS tasks_kind_state ON tasks(kind, state);
PRAGMA user_version=10;
