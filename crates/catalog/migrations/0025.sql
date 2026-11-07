-- Content search (G37): what each photograph shows, as an image embedding
-- of the search model (SigLIP 2 B/16-224), written by the app's background
-- pass from the photograph's cached thumbnail. One row per photograph:
-- `model` is the SHA-256 of the vision model file that made it, so another
-- model reads every photograph again; `fingerprint` is the file's then, so
-- a changed file is read again. `vector` holds the 768 values as signed
-- bytes, each times `scale` (one per photograph; the cosine to the f32
-- embedding stays above 0.996); `error` says why the photograph could not
-- be read (no vector then). Derived data only: dropping the table loses
-- nothing the pass cannot read again. No file is read or written here.
CREATE TABLE IF NOT EXISTS content_embeddings(
 asset_id TEXT PRIMARY KEY REFERENCES assets(id) ON DELETE CASCADE,
 model TEXT NOT NULL,
 fingerprint TEXT NOT NULL,
 scale REAL,
 vector BLOB,
 error TEXT,
 CHECK((vector IS NULL) = (error IS NOT NULL)),
 CHECK(vector IS NULL OR (length(vector)=768 AND scale IS NOT NULL)));
PRAGMA user_version=25;
