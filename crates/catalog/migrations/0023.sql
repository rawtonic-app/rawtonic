-- XMP digest window (G66): `metadata.xmp_publishing` holds the digest of
-- the sidecar the library is about to publish, committed before the file
-- is replaced and cleared with the commit of `xmp_hash` after it. A crash
-- between the publish and that commit leaves the new sidecar on disk with
-- this digest: the next check takes it for the library's own write (and
-- promotes it to `xmp_hash`) instead of a change by another program. A
-- sidecar matching neither digest is still a change elsewhere. The column
-- is added before this script runs (`Library::add_missing_columns`).
PRAGMA user_version=23;
