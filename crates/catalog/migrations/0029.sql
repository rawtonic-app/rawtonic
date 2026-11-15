-- Keywords without photographs (G36/G37): the keywords a person made in the
-- Keyword List (New Keyword) and that stay until they delete them, as
-- Lightroom keeps a keyword whose last photograph lost it. `path` is the
-- keyword as typed (levels joined by `|`), `folded` its lowercase form,
-- which makes a keyword unique ignoring case as the library compares them.
-- A keyword that photographs hold needs no row; a row for one that
-- photographs hold has no effect. No file is read or written here.
CREATE TABLE IF NOT EXISTS declared_keywords(
 folded TEXT PRIMARY KEY,
 path TEXT NOT NULL);
PRAGMA user_version=29;
