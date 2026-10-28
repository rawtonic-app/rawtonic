-- Photographs nobody rated, tagged or edited have nothing to write to an
-- XMP sidecar, yet were registered as pending and counted in the session
-- bar until someone edited them (O-03). Only the state changes; no file is
-- read or written.
UPDATE metadata SET xmp_status='synced'
 WHERE xmp_status='pending' AND xmp_hash IS NULL AND (data='{}' OR from_camera=1)
   AND NOT EXISTS (SELECT 1 FROM versions v JOIN recipes p ON p.id=v.head
                    WHERE v.asset_id=metadata.asset_id AND v.is_primary=1
                      AND json_type(p.payload,'$.editor_state') IS NOT NULL);
PRAGMA user_version=18;
