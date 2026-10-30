-- Photographs nobody rated, tagged or edited have nothing to write to an
-- XMP sidecar, yet were registered as pending and counted in the session
-- bar until someone edited them (O-03). Any recipe of the primary version
-- is an edit the sidecar lacks, so those stay pending. Only the state
-- changes; no file is read or written.
UPDATE metadata SET xmp_status='synced'
 WHERE xmp_status='pending' AND xmp_hash IS NULL AND (data='{}' OR from_camera=1)
   AND NOT EXISTS (SELECT 1 FROM versions v
                    WHERE v.asset_id=metadata.asset_id AND v.is_primary=1
                      AND v.head IS NOT NULL);
PRAGMA user_version=18;
