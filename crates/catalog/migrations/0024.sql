-- Sony burst frame numbers (G36): the facts reader now reads Sony's maker
-- note SequenceNumber (tag 0xb04a) into `assets.sequence`, as it reads
-- Fujifilm's. Dated Sony photographs whose facts were read before are read
-- again by the background pass, which replaces only capture facts, never a
-- rating anyone gave (`facts::write_facts`). No table changes.
UPDATE assets SET facts_read=0
 WHERE facts_read=1 AND capture_time IS NOT NULL AND upper(make) LIKE 'SONY%';
PRAGMA user_version=24;
