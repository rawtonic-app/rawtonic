-- Whether the photograph's file holds areas a generative model made because
-- Rawtonic rendered it so (`assets.generated_provenance`, 1): an Edit With
-- render of a photo with generated areas. Set only when such a render is
-- registered (`Library::mark_generated_provenance`), never cleared: not by
-- a metadata edit, Use XMP or a new read of the file's facts, since another
-- program's save need not keep the file's own marking. Exports of the
-- photograph are marked as containing generated content. The column is
-- added before this script runs (`migrations::add_missing_columns`).
PRAGMA user_version=37;
