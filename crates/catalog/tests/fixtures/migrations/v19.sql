-- A schema 19 library with synthetic photographs (tests/migration_fixtures.rs).
CREATE TABLE roots(id TEXT PRIMARY KEY, path TEXT NOT NULL, volume_id TEXT, external INTEGER NOT NULL DEFAULT 0, volume_name TEXT, volume_path TEXT, volume_kind TEXT CHECK(volume_kind IN ('startup','local','removable','network')));
CREATE TABLE assets(
 id TEXT PRIMARY KEY, root_id TEXT NOT NULL REFERENCES roots(id), rel_path TEXT NOT NULL,
 size INTEGER NOT NULL, mtime INTEGER NOT NULL, fingerprint TEXT NOT NULL, capture_time TEXT,
 camera TEXT NOT NULL DEFAULT '', lens TEXT NOT NULL DEFAULT '', width INTEGER, height INTEGER,
 orientation INTEGER NOT NULL DEFAULT 1, kind TEXT NOT NULL, raw_jpeg_pair TEXT, stack TEXT,
 missing INTEGER NOT NULL DEFAULT 0, search_text TEXT NOT NULL DEFAULT '', sha256 TEXT, companion_size INTEGER, companion_fingerprint TEXT, companion_sha256 TEXT, make TEXT NOT NULL DEFAULT '', focal_length REAL, aperture REAL, iso INTEGER, exposure REAL, facts_read INTEGER NOT NULL DEFAULT 0, trashed INTEGER NOT NULL DEFAULT 0 CHECK(trashed IN (0,1)), source_type TEXT NOT NULL DEFAULT '', UNIQUE(root_id, rel_path)
);
CREATE TABLE versions(id TEXT PRIMARY KEY, asset_id TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
 name TEXT NOT NULL, is_primary INTEGER NOT NULL DEFAULT 0, head INTEGER, legacy_state TEXT, ai_facts INTEGER NOT NULL DEFAULT 0);
CREATE TABLE recipes(id INTEGER PRIMARY KEY, version_id TEXT NOT NULL REFERENCES versions(id) ON DELETE CASCADE,
 parent INTEGER REFERENCES recipes(id), schema_version INTEGER NOT NULL DEFAULT 1, payload TEXT NOT NULL,
 snapshot TEXT, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
CREATE TABLE metadata(asset_id TEXT PRIMARY KEY REFERENCES assets(id) ON DELETE CASCADE,
 rating INTEGER NOT NULL DEFAULT 0 CHECK(rating BETWEEN 0 AND 5), label TEXT NOT NULL DEFAULT '',
 flag INTEGER NOT NULL DEFAULT 0 CHECK(flag BETWEEN -1 AND 1), data TEXT NOT NULL DEFAULT '{}',
 xmp_hash TEXT, xmp_status TEXT NOT NULL DEFAULT 'pending', from_camera INTEGER NOT NULL DEFAULT 0);
CREATE TABLE masks(id TEXT PRIMARY KEY, version_id TEXT NOT NULL REFERENCES versions(id), vector_json TEXT, raster_path TEXT, hash TEXT);
CREATE TABLE presets(id TEXT PRIMARY KEY, name TEXT NOT NULL, recipe_json TEXT NOT NULL);
CREATE TABLE styles(id TEXT PRIMARY KEY, name TEXT NOT NULL, recipe_json TEXT NOT NULL);
CREATE TABLE export_recipes(id TEXT PRIMARY KEY, name TEXT NOT NULL, recipe_json TEXT NOT NULL);
CREATE TABLE naming_templates(id TEXT PRIMARY KEY, name TEXT NOT NULL, template TEXT NOT NULL);
CREATE TABLE workspaces(id TEXT PRIMARY KEY, name TEXT NOT NULL, settings_json TEXT NOT NULL);
CREATE TABLE keymaps(id TEXT PRIMARY KEY, name TEXT NOT NULL, bindings_json TEXT NOT NULL);
CREATE TABLE tasks(id TEXT PRIMARY KEY, kind TEXT NOT NULL, state TEXT NOT NULL, payload TEXT NOT NULL, progress REAL NOT NULL DEFAULT 0, error TEXT, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
CREATE TABLE file_ops_log(id TEXT PRIMARY KEY, kind TEXT NOT NULL, state TEXT NOT NULL, payload TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
CREATE TABLE recipe_blobs(hash TEXT PRIMARY KEY, data TEXT NOT NULL);
CREATE TABLE recipe_blob_refs(
 recipe_id INTEGER NOT NULL REFERENCES recipes(id) ON DELETE CASCADE,
 hash TEXT NOT NULL REFERENCES recipe_blobs(hash),
 PRIMARY KEY(recipe_id, hash)
);
CREATE TABLE hot_folder(
 id INTEGER PRIMARY KEY CHECK(id=1),
 folder TEXT NOT NULL,
 template TEXT NOT NULL,
 include_subfolders INTEGER NOT NULL DEFAULT 0,
 settings_json TEXT NOT NULL DEFAULT '{}',
 last_capture TEXT REFERENCES assets(id) ON DELETE SET NULL,
 started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE TABLE hot_folder_files(
 rel_path TEXT PRIMARY KEY,
 size INTEGER NOT NULL,
 mtime_ns INTEGER
);
CREATE TABLE "import_batches"(
 id INTEGER PRIMARY KEY,
 kind TEXT NOT NULL CHECK(kind IN ('ingest','add','sync','hotfolder','tether')),
 created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE TABLE "asset_imports"(
 asset_id TEXT PRIMARY KEY REFERENCES assets(id) ON DELETE CASCADE,
 batch_id INTEGER NOT NULL REFERENCES "import_batches"(id) ON DELETE CASCADE
);
CREATE TABLE ingest_items(
 job_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
 position INTEGER NOT NULL,
 number INTEGER NOT NULL,
 source TEXT NOT NULL,
 source_size INTEGER,
 source_mtime_ns INTEGER,
 source_fingerprint TEXT,
 companion TEXT,
 companion_size INTEGER,
 companion_mtime_ns INTEGER,
 companion_fingerprint TEXT,
 state TEXT NOT NULL CHECK(state IN ('pending','copying','done','failed')),
 destination TEXT,
 companion_destination TEXT,
 error TEXT,
 PRIMARY KEY(job_id, position)
);
CREATE TABLE asset_keywords(
 asset_id TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
 keyword TEXT NOT NULL COLLATE NOCASE,
 PRIMARY KEY(asset_id, keyword)
) WITHOUT ROWID;
CREATE VIRTUAL TABLE asset_search USING fts5(asset_id UNINDEXED, content, tokenize='trigram case_sensitive 1');
CREATE TABLE "collections"(
 id TEXT PRIMARY KEY,
 name TEXT NOT NULL,
 parent TEXT REFERENCES "collections"(id),
 kind TEXT NOT NULL DEFAULT 'album' CHECK(kind IN ('album','group','smart')),
 position INTEGER NOT NULL DEFAULT 0,
 rules TEXT,
 CHECK((kind = 'smart') = (rules IS NOT NULL))
);
CREATE TABLE "collection_assets"(
 collection_id TEXT REFERENCES "collections"(id) ON DELETE CASCADE,
 asset_id TEXT REFERENCES assets(id) ON DELETE CASCADE,
 PRIMARY KEY(collection_id, asset_id)
);
CREATE TABLE snapshots(
 key INTEGER PRIMARY KEY,
 id TEXT NOT NULL,
 version_id TEXT NOT NULL REFERENCES versions(id) ON DELETE CASCADE,
 name TEXT NOT NULL,
 created_at TEXT NOT NULL,
 adjustments TEXT NOT NULL,
 UNIQUE(version_id, id)
);
CREATE TABLE snapshot_blob_refs(
 snapshot_key INTEGER NOT NULL REFERENCES snapshots(key) ON DELETE CASCADE,
 hash TEXT NOT NULL REFERENCES recipe_blobs(hash),
 PRIMARY KEY(snapshot_key, hash)
);
CREATE TABLE folder_root_volumes(path TEXT PRIMARY KEY, volume TEXT NOT NULL);
CREATE TABLE capture_overrides(
 asset_id TEXT PRIMARY KEY REFERENCES assets(id) ON DELETE CASCADE,
 captured TEXT NOT NULL, subsec TEXT, offset TEXT);
INSERT INTO roots(id,path,volume_id,external,volume_name,volume_path,volume_kind) VALUES('session','.',NULL,0,NULL,NULL,NULL);
INSERT INTO roots(id,path,volume_id,external,volume_name,volume_path,volume_kind) VALUES('ext-archive','/Volumes/ARCHIVE/Photos','4C1A2B3D-0000-4000-8000-00000000A001',1,'ARCHIVE','/Volumes/ARCHIVE','removable');
INSERT INTO assets(id,root_id,rel_path,size,mtime,fingerprint,capture_time,camera,lens,width,height,orientation,kind,raw_jpeg_pair,stack,missing,search_text,sha256,companion_size,companion_fingerprint,companion_sha256,make,focal_length,aperture,iso,exposure,facts_read,trashed,source_type) VALUES('00000000-0000-4000-8000-000000000001','session','Capture/Day 1/DSC00123.ARW',20000001,1780000001,'xxh3:0000000000001eef','2026-06-01 10:00:00','ILCE-7RM4','FE 24-70mm F2.8 GM',6000,4000,1,'arw','DSC00123.JPG',NULL,0,'wedding ceremony','0000000000000000000000000000000000000000000000000000000000000001',5000000,'xxh3:00000000000c0001','00000000000000000000000000000000000000000000000000000000000003e9','SONY',50.0,2.8,100,0.004,1,0,'');
INSERT INTO assets(id,root_id,rel_path,size,mtime,fingerprint,capture_time,camera,lens,width,height,orientation,kind,raw_jpeg_pair,stack,missing,search_text,sha256,companion_size,companion_fingerprint,companion_sha256,make,focal_length,aperture,iso,exposure,facts_read,trashed,source_type) VALUES('00000000-0000-4000-8000-000000000002','session','Capture/Day 1/DSC00124.ARW',20000002,1780000002,'xxh3:0000000000003dde','2026-06-01 18:30:00','ILCE-7RM4','FE 85mm F1.4 GM',6000,4000,1,'arw','DSC00124.JPG',NULL,0,'wedding   sunset ','0000000000000000000000000000000000000000000000000000000000000002',5000000,'xxh3:00000000000c0001','00000000000000000000000000000000000000000000000000000000000003ea','SONY',50.0,2.8,3200,0.004,1,0,'');
INSERT INTO assets(id,root_id,rel_path,size,mtime,fingerprint,capture_time,camera,lens,width,height,orientation,kind,raw_jpeg_pair,stack,missing,search_text,sha256,companion_size,companion_fingerprint,companion_sha256,make,focal_length,aperture,iso,exposure,facts_read,trashed,source_type) VALUES('00000000-0000-4000-8000-000000000003','session','Capture/Day 2/IMG_0001.CR3',20000003,1780000003,'xxh3:0000000000005ccd',NULL,'Canon EOS R5','RF 50mm F1.2 L USM',6000,4000,1,'cr3',NULL,NULL,0,'portrait','0000000000000000000000000000000000000000000000000000000000000003',NULL,NULL,NULL,'Canon',50.0,2.8,800,0.004,1,0,'');
INSERT INTO assets(id,root_id,rel_path,size,mtime,fingerprint,capture_time,camera,lens,width,height,orientation,kind,raw_jpeg_pair,stack,missing,search_text,sha256,companion_size,companion_fingerprint,companion_sha256,make,focal_length,aperture,iso,exposure,facts_read,trashed,source_type) VALUES('00000000-0000-4000-8000-000000000004','session','Capture/scan.jpg',20000004,1780000004,'xxh3:0000000000007bbc',NULL,'','',6000,4000,1,'jpg',NULL,NULL,0,'archive Wedding','0000000000000000000000000000000000000000000000000000000000000004',NULL,NULL,NULL,'',NULL,NULL,NULL,NULL,1,0,'trainedAlgorithmicMedia');
INSERT INTO assets(id,root_id,rel_path,size,mtime,fingerprint,capture_time,camera,lens,width,height,orientation,kind,raw_jpeg_pair,stack,missing,search_text,sha256,companion_size,companion_fingerprint,companion_sha256,make,focal_length,aperture,iso,exposure,facts_read,trashed,source_type) VALUES('00000000-0000-4000-8000-000000000005','session','Output/merge.dng',20000005,1780000005,'xxh3:0000000000009aab','2026-06-02 09:15:00','ILCE-7RM4','',6000,4000,1,'dng',NULL,NULL,0,'','0000000000000000000000000000000000000000000000000000000000000005',NULL,NULL,NULL,'SONY',NULL,NULL,NULL,NULL,1,0,'');
INSERT INTO assets(id,root_id,rel_path,size,mtime,fingerprint,capture_time,camera,lens,width,height,orientation,kind,raw_jpeg_pair,stack,missing,search_text,sha256,companion_size,companion_fingerprint,companion_sha256,make,focal_length,aperture,iso,exposure,facts_read,trashed,source_type) VALUES('00000000-0000-4000-8000-000000000006','ext-archive','2024/03/DSC_0042.NEF',20000006,1780000006,'xxh3:000000000000b99a','2024-03-10 12:00:00','NIKON Z 8','NIKKOR Z 24-120mm f/4 S',6000,4000,1,'nef',NULL,NULL,1,'','0000000000000000000000000000000000000000000000000000000000000006',NULL,NULL,NULL,'NIKON CORPORATION',50.0,2.8,64,0.004,1,0,'');
INSERT INTO assets(id,root_id,rel_path,size,mtime,fingerprint,capture_time,camera,lens,width,height,orientation,kind,raw_jpeg_pair,stack,missing,search_text,sha256,companion_size,companion_fingerprint,companion_sha256,make,focal_length,aperture,iso,exposure,facts_read,trashed,source_type) VALUES('00000000-0000-4000-8000-000000000007','session','Trash/DSC00099.ARW',20000007,1780000007,'xxh3:000000000000d889','2026-06-01 11:00:00','ILCE-7RM4','',6000,4000,1,'arw',NULL,NULL,1,'','0000000000000000000000000000000000000000000000000000000000000007',NULL,NULL,NULL,'SONY',NULL,NULL,NULL,NULL,1,1,'');
INSERT INTO assets(id,root_id,rel_path,size,mtime,fingerprint,capture_time,camera,lens,width,height,orientation,kind,raw_jpeg_pair,stack,missing,search_text,sha256,companion_size,companion_fingerprint,companion_sha256,make,focal_length,aperture,iso,exposure,facts_read,trashed,source_type) VALUES('00000000-0000-4000-8000-000000000008','session','Selects/DSC00200.ARW',20000008,1780000008,'xxh3:000000000000f778',NULL,'','',6000,4000,1,'arw',NULL,NULL,0,'','0000000000000000000000000000000000000000000000000000000000000008',NULL,NULL,NULL,'',NULL,NULL,NULL,NULL,1,0,'');
INSERT INTO assets(id,root_id,rel_path,size,mtime,fingerprint,capture_time,camera,lens,width,height,orientation,kind,raw_jpeg_pair,stack,missing,search_text,sha256,companion_size,companion_fingerprint,companion_sha256,make,focal_length,aperture,iso,exposure,facts_read,trashed,source_type) VALUES('00000000-0000-4000-8000-000000000009','session','Capture/Day 2/DSCF1234.RAF',20000009,1780000009,'xxh3:0000000000011667','2026-06-03 07:45:00','X-T5','',6000,4000,1,'raf',NULL,NULL,0,'landscape','0000000000000000000000000000000000000000000000000000000000000009',NULL,NULL,NULL,'FUJIFILM',NULL,NULL,160,0.004,1,0,'');
INSERT INTO versions(id,asset_id,name,is_primary,head,legacy_state,ai_facts) VALUES('00000000-0000-4000-9000-000000000001','00000000-0000-4000-8000-000000000001','Original',1,3,NULL,516);
INSERT INTO versions(id,asset_id,name,is_primary,head,legacy_state,ai_facts) VALUES('00000000-0000-4000-9000-000000000002','00000000-0000-4000-8000-000000000002','Original',1,4,NULL,0);
INSERT INTO versions(id,asset_id,name,is_primary,head,legacy_state,ai_facts) VALUES('00000000-0000-4000-9000-000000000003','00000000-0000-4000-8000-000000000003','Original',1,6,NULL,0);
INSERT INTO versions(id,asset_id,name,is_primary,head,legacy_state,ai_facts) VALUES('00000000-0000-4000-9000-000000000004','00000000-0000-4000-8000-000000000004','Original',1,NULL,NULL,0);
INSERT INTO versions(id,asset_id,name,is_primary,head,legacy_state,ai_facts) VALUES('00000000-0000-4000-9000-000000000005','00000000-0000-4000-8000-000000000005','Original',1,NULL,NULL,0);
INSERT INTO versions(id,asset_id,name,is_primary,head,legacy_state,ai_facts) VALUES('00000000-0000-4000-9000-000000000006','00000000-0000-4000-8000-000000000006','Original',1,NULL,NULL,0);
INSERT INTO versions(id,asset_id,name,is_primary,head,legacy_state,ai_facts) VALUES('00000000-0000-4000-9000-000000000007','00000000-0000-4000-8000-000000000007','Original',1,NULL,NULL,0);
INSERT INTO versions(id,asset_id,name,is_primary,head,legacy_state,ai_facts) VALUES('00000000-0000-4000-9000-000000000008','00000000-0000-4000-8000-000000000008','Original',1,NULL,NULL,0);
INSERT INTO versions(id,asset_id,name,is_primary,head,legacy_state,ai_facts) VALUES('00000000-0000-4000-9000-000000000009','00000000-0000-4000-8000-000000000009','Original',1,NULL,NULL,0);
INSERT INTO versions(id,asset_id,name,is_primary,head,legacy_state,ai_facts) VALUES('00000000-0000-4000-9000-000000000101','00000000-0000-4000-8000-000000000001','a1b2c3',0,NULL,'{"adjustments":{"exposure":1.0}}',0);
INSERT INTO versions(id,asset_id,name,is_primary,head,legacy_state,ai_facts) VALUES('00000000-0000-4000-9000-000000000102','00000000-0000-4000-8000-000000000001','0f0f0f',0,NULL,'{"adjustments":{"aiPatches":[{"id":"p1","patchData":{"color":"rawtonic-blob:sha256:none","model":"external"},"visible":true}]}}',33);
INSERT INTO versions(id,asset_id,name,is_primary,head,legacy_state,ai_facts) VALUES('00000000-0000-4000-9000-000000000103','00000000-0000-4000-8000-000000000002','beef01',0,NULL,'{"adjustments":{"saturation":-100}}',0);
INSERT INTO recipes(id,version_id,parent,schema_version,payload,snapshot,created_at) VALUES(1,'00000000-0000-4000-9000-000000000001',NULL,1,'{"adjustments":{"exposure":0.3}}',NULL,'2026-06-01 12:00:01');
INSERT INTO recipes(id,version_id,parent,schema_version,payload,snapshot,created_at) VALUES(2,'00000000-0000-4000-9000-000000000001',1,1,'{"adjustments":{"contrast":10,"exposure":0.5}}','Before AI','2026-06-01 12:00:02');
INSERT INTO recipes(id,version_id,parent,schema_version,payload,snapshot,created_at) VALUES(3,'00000000-0000-4000-9000-000000000001',2,1,'{"adjustments":{"aiDenoise":40,"aiDenoiseVersion":1,"contrast":10,"exposure":0.5,"masks":[{"id":"m1","image":"rawtonic-blob:sha256:5f1c0e6b2a9d4c3e8f7a6b5c4d3e2f1a0b9c8d7e6f5a4b3c2d1e0f9a8b7c6d5e"}]}}',NULL,'2026-06-01 12:00:03');
INSERT INTO recipes(id,version_id,parent,schema_version,payload,snapshot,created_at) VALUES(4,'00000000-0000-4000-9000-000000000002',NULL,1,'{"adjustments":{"temperature":5200}}',NULL,'2026-06-02 12:00:04');
INSERT INTO recipes(id,version_id,parent,schema_version,payload,snapshot,created_at) VALUES(5,'00000000-0000-4000-9000-000000000003',NULL,1,'{"adjustments":{"exposure":-0.2}}',NULL,'2026-06-03 12:00:05');
INSERT INTO recipes(id,version_id,parent,schema_version,payload,snapshot,created_at) VALUES(6,'00000000-0000-4000-9000-000000000003',5,1,'{"adjustments":{"exposure":-0.2,"vibrance":15}}',NULL,'2026-06-03 12:00:06');
INSERT INTO metadata(asset_id,rating,label,flag,data,xmp_hash,xmp_status,from_camera) VALUES('00000000-0000-4000-8000-000000000001',5,'red',1,'{"creator":"Synthetic Photographer","flag":1,"keywords":["wedding","ceremony"],"label":"red","rating":5,"title":"First kiss"}','digest-1','synced',0);
INSERT INTO metadata(asset_id,rating,label,flag,data,xmp_hash,xmp_status,from_camera) VALUES('00000000-0000-4000-8000-000000000002',4,'yellow',0,'{"flag":0,"keywords":["wedding","  sunset ",7],"label":"yellow","rating":4}','digest-2','pending',0);
INSERT INTO metadata(asset_id,rating,label,flag,data,xmp_hash,xmp_status,from_camera) VALUES('00000000-0000-4000-8000-000000000003',3,'green',-1,'{"flag":-1,"keywords":["portrait"],"label":"green","rating":3}',NULL,'pending',1);
INSERT INTO metadata(asset_id,rating,label,flag,data,xmp_hash,xmp_status,from_camera) VALUES('00000000-0000-4000-8000-000000000004',1,'',0,'{"flag":0,"keywords":["archive","Wedding"],"label":"","rating":1}',NULL,'pending',0);
INSERT INTO metadata(asset_id,rating,label,flag,data,xmp_hash,xmp_status,from_camera) VALUES('00000000-0000-4000-8000-000000000005',0,'',0,'{"flag":0,"label":"","rating":0}',NULL,'pending',0);
INSERT INTO metadata(asset_id,rating,label,flag,data,xmp_hash,xmp_status,from_camera) VALUES('00000000-0000-4000-8000-000000000006',2,'',0,'{"flag":0,"label":"","rating":2}','digest-6','synced',0);
INSERT INTO metadata(asset_id,rating,label,flag,data,xmp_hash,xmp_status,from_camera) VALUES('00000000-0000-4000-8000-000000000007',0,'',0,'{"flag":0,"label":"","rating":0}',NULL,'pending',0);
INSERT INTO metadata(asset_id,rating,label,flag,data,xmp_hash,xmp_status,from_camera) VALUES('00000000-0000-4000-8000-000000000008',0,'',0,'{}',NULL,'pending',0);
INSERT INTO metadata(asset_id,rating,label,flag,data,xmp_hash,xmp_status,from_camera) VALUES('00000000-0000-4000-8000-000000000009',0,'',0,'{"flag":0,"keywords":["landscape"],"label":"","rating":0}',NULL,'pending',0);
INSERT INTO masks(id,version_id,vector_json,raster_path,hash) VALUES('mask-1','00000000-0000-4000-9000-000000000001','{"type":"radial"}','masks/mask-1.png','mask-hash-1');
INSERT INTO presets(id,name,recipe_json) VALUES('presets-1','Warm','{"temperature":5600}');
INSERT INTO styles(id,name,recipe_json) VALUES('styles-1','Warm','{"temperature":5600}');
INSERT INTO export_recipes(id,name,recipe_json) VALUES('export_recipes-1','Warm','{"temperature":5600}');
INSERT INTO naming_templates(id,name,template) VALUES('naming-1','Date and counter','{date}_{counter:4}');
INSERT INTO workspaces(id,name,settings_json) VALUES('ws-1','Culling','{}');
INSERT INTO keymaps(id,name,bindings_json) VALUES('km-1','Default','{}');
INSERT INTO tasks(id,kind,state,payload,progress,error,created_at) VALUES('job-1','ingest','interrupted','{"destination":"Capture"}',0.5,NULL,'2026-06-01 09:00:00');
INSERT INTO file_ops_log(id,kind,state,payload,created_at) VALUES('op-1','rename','committed','{}','2026-06-01 09:30:00');
INSERT INTO recipe_blobs(hash,data) VALUES('5f1c0e6b2a9d4c3e8f7a6b5c4d3e2f1a0b9c8d7e6f5a4b3c2d1e0f9a8b7c6d5e','data:image/png;base64,iVBORw0KGgo=');
INSERT INTO recipe_blob_refs(recipe_id,hash) VALUES(3,'5f1c0e6b2a9d4c3e8f7a6b5c4d3e2f1a0b9c8d7e6f5a4b3c2d1e0f9a8b7c6d5e');
INSERT INTO hot_folder(id,folder,template,include_subfolders,settings_json,last_capture,started_at) VALUES(1,'/Users/photographer/Tethered','{original}',1,'{"rating":0}','00000000-0000-4000-8000-000000000008','2026-06-03 08:00:00');
INSERT INTO hot_folder_files(rel_path,size,mtime_ns) VALUES('DSC00200.ARW',20000008,1780000008000000000);
INSERT INTO import_batches(id,kind,created_at) VALUES(1,'ingest','2026-06-01 08:00:00');
INSERT INTO import_batches(id,kind,created_at) VALUES(2,'add','2026-06-02 08:00:00');
INSERT INTO import_batches(id,kind,created_at) VALUES(3,'hotfolder','2026-06-03 08:00:00');
INSERT INTO import_batches(id,kind,created_at) VALUES(4,'tether','2026-06-04 08:00:00');
INSERT INTO asset_imports(asset_id,batch_id) VALUES('00000000-0000-4000-8000-000000000001',1);
INSERT INTO asset_imports(asset_id,batch_id) VALUES('00000000-0000-4000-8000-000000000002',1);
INSERT INTO asset_imports(asset_id,batch_id) VALUES('00000000-0000-4000-8000-000000000003',2);
INSERT INTO asset_imports(asset_id,batch_id) VALUES('00000000-0000-4000-8000-000000000005',2);
INSERT INTO asset_imports(asset_id,batch_id) VALUES('00000000-0000-4000-8000-000000000008',3);
INSERT INTO asset_imports(asset_id,batch_id) VALUES('00000000-0000-4000-8000-000000000009',4);
INSERT INTO ingest_items(job_id,position,number,source,source_size,source_mtime_ns,source_fingerprint,companion,companion_size,companion_mtime_ns,companion_fingerprint,state,destination,companion_destination,error) VALUES('job-1',0,1,'/Volumes/CARD/DCIM/100MSDCF/DSC00123.ARW',20000001,1780000000000000001,'xxh3:0000000000001eef','/Volumes/CARD/DCIM/100MSDCF/DSC00123.JPG',NULL,NULL,NULL,'done','Capture/Day 1/DSC00123.ARW',NULL,NULL);
INSERT INTO asset_keywords(asset_id,keyword) VALUES('00000000-0000-4000-8000-000000000001','ceremony');
INSERT INTO asset_keywords(asset_id,keyword) VALUES('00000000-0000-4000-8000-000000000001','wedding');
INSERT INTO asset_keywords(asset_id,keyword) VALUES('00000000-0000-4000-8000-000000000002','sunset');
INSERT INTO asset_keywords(asset_id,keyword) VALUES('00000000-0000-4000-8000-000000000002','wedding');
INSERT INTO asset_keywords(asset_id,keyword) VALUES('00000000-0000-4000-8000-000000000003','portrait');
INSERT INTO asset_keywords(asset_id,keyword) VALUES('00000000-0000-4000-8000-000000000004','archive');
INSERT INTO asset_keywords(asset_id,keyword) VALUES('00000000-0000-4000-8000-000000000004','Wedding');
INSERT INTO asset_keywords(asset_id,keyword) VALUES('00000000-0000-4000-8000-000000000009','landscape');
INSERT INTO asset_search(rowid,asset_id,content) VALUES(1,'00000000-0000-4000-8000-000000000001','capture/day 1/dsc00123.arw wedding ceremony sony ilce-7rm4 fe 24-70mm f2.8 gm');
INSERT INTO asset_search(rowid,asset_id,content) VALUES(2,'00000000-0000-4000-8000-000000000002','capture/day 1/dsc00124.arw wedding   sunset  sony ilce-7rm4 fe 85mm f1.4 gm');
INSERT INTO asset_search(rowid,asset_id,content) VALUES(3,'00000000-0000-4000-8000-000000000003','capture/day 2/img_0001.cr3 portrait canon canon eos r5 rf 50mm f1.2 l usm');
INSERT INTO asset_search(rowid,asset_id,content) VALUES(4,'00000000-0000-4000-8000-000000000004','capture/scan.jpg archive wedding   ');
INSERT INTO asset_search(rowid,asset_id,content) VALUES(5,'00000000-0000-4000-8000-000000000005','output/merge.dng  sony ilce-7rm4 ');
INSERT INTO asset_search(rowid,asset_id,content) VALUES(6,'00000000-0000-4000-8000-000000000006','2024/03/dsc_0042.nef  nikon corporation nikon z 8 nikkor z 24-120mm f/4 s');
INSERT INTO asset_search(rowid,asset_id,content) VALUES(7,'00000000-0000-4000-8000-000000000007','trash/dsc00099.arw  sony ilce-7rm4 ');
INSERT INTO asset_search(rowid,asset_id,content) VALUES(8,'00000000-0000-4000-8000-000000000008','selects/dsc00200.arw    ');
INSERT INTO asset_search(rowid,asset_id,content) VALUES(9,'00000000-0000-4000-8000-000000000009','capture/day 2/dscf1234.raf landscape fujifilm x-t5 ');
INSERT INTO collections(id,name,parent,kind,position,rules) VALUES('group-clients','Clients',NULL,'group',0,NULL);
INSERT INTO collections(id,name,parent,kind,position,rules) VALUES('album-wedding','Wedding','group-clients','album',0,NULL);
INSERT INTO collections(id,name,parent,kind,position,rules) VALUES('album-portraits','Portraits',NULL,'album',1,NULL);
INSERT INTO collections(id,name,parent,kind,position,rules) VALUES('smart-five-stars','Four stars and up',NULL,'smart',2,'{"combine":"all","rules":[{"field":"rating","op":"ge","value":4}]}');
INSERT INTO collection_assets(collection_id,asset_id) VALUES('album-wedding','00000000-0000-4000-8000-000000000001');
INSERT INTO collection_assets(collection_id,asset_id) VALUES('album-wedding','00000000-0000-4000-8000-000000000002');
INSERT INTO collection_assets(collection_id,asset_id) VALUES('album-wedding','00000000-0000-4000-8000-000000000004');
INSERT INTO collection_assets(collection_id,asset_id) VALUES('album-portraits','00000000-0000-4000-8000-000000000003');
INSERT INTO collection_assets(collection_id,asset_id) VALUES('album-portraits','00000000-0000-4000-8000-000000000007');
INSERT INTO snapshots(key,id,version_id,name,created_at,adjustments) VALUES(1,'snap-1','00000000-0000-4000-9000-000000000001','Before retouch','2026-06-01T12:00:00Z','{"exposure":0.3,"mask":"rawtonic-blob:sha256:5f1c0e6b2a9d4c3e8f7a6b5c4d3e2f1a0b9c8d7e6f5a4b3c2d1e0f9a8b7c6d5e"}');
INSERT INTO snapshots(key,id,version_id,name,created_at,adjustments) VALUES(2,'snap-2','00000000-0000-4000-9000-000000000101','Copy look','2026-06-01T12:05:00Z','{"exposure":1.0}');
INSERT INTO snapshot_blob_refs(snapshot_key,hash) VALUES(1,'5f1c0e6b2a9d4c3e8f7a6b5c4d3e2f1a0b9c8d7e6f5a4b3c2d1e0f9a8b7c6d5e');
INSERT INTO folder_root_volumes(path,volume) VALUES('/Volumes/ARCHIVE/Photos','4C1A2B3D-0000-4000-8000-00000000A001');
INSERT INTO capture_overrides(asset_id,captured,subsec,offset) VALUES('00000000-0000-4000-8000-000000000009','2026-06-03 08:45:00','12','+02:00');
CREATE INDEX assets_fingerprint ON assets(fingerprint);
CREATE UNIQUE INDEX primary_version ON versions(asset_id) WHERE is_primary=1;
CREATE INDEX recipe_blob_refs_hash ON recipe_blob_refs(hash);
CREATE INDEX asset_imports_batch ON asset_imports(batch_id);
CREATE INDEX tasks_kind_state ON tasks(kind, state);
CREATE INDEX assets_facts_unread ON assets(facts_read) WHERE facts_read=0;
CREATE INDEX assets_capture_time ON assets(capture_time);
CREATE INDEX metadata_rating ON metadata(rating);
CREATE INDEX asset_keywords_keyword ON asset_keywords(keyword);
CREATE TRIGGER metadata_keywords_ai AFTER INSERT ON metadata BEGIN
 INSERT OR IGNORE INTO asset_keywords(asset_id, keyword)
  SELECT new.asset_id, rawtonic_trim(value) FROM json_each(new.data, '$.keywords')
  WHERE json_valid(new.data) AND type = 'text' AND rawtonic_trim(value) != '';
END;
CREATE TRIGGER metadata_keywords_au AFTER UPDATE OF data ON metadata BEGIN
 DELETE FROM asset_keywords WHERE asset_id = old.asset_id;
 INSERT OR IGNORE INTO asset_keywords(asset_id, keyword)
  SELECT new.asset_id, rawtonic_trim(value) FROM json_each(new.data, '$.keywords')
  WHERE json_valid(new.data) AND type = 'text' AND rawtonic_trim(value) != '';
END;
CREATE TRIGGER assets_ai AFTER INSERT ON assets BEGIN
 INSERT INTO asset_search(asset_id, content)
  VALUES(new.id, rawtonic_fold(new.rel_path || ' ' || new.search_text || ' ' || new.make || ' ' || new.camera || ' ' || new.lens));
END;
CREATE TRIGGER assets_au AFTER UPDATE OF rel_path, search_text, make, camera, lens ON assets BEGIN
 DELETE FROM asset_search WHERE asset_id = old.id;
 INSERT INTO asset_search(asset_id, content)
  VALUES(new.id, rawtonic_fold(new.rel_path || ' ' || new.search_text || ' ' || new.make || ' ' || new.camera || ' ' || new.lens));
END;
CREATE TRIGGER assets_ad AFTER DELETE ON assets BEGIN
 DELETE FROM asset_search WHERE asset_id = old.id;
END;
CREATE INDEX snapshot_blob_refs_hash ON snapshot_blob_refs(hash);
CREATE INDEX versions_ai_facts ON versions(ai_facts, asset_id) WHERE ai_facts != 0;
CREATE TRIGGER versions_ai_facts_ai AFTER INSERT ON versions BEGIN
 UPDATE versions SET ai_facts = rawtonic_ai_facts(
  CASE WHEN new.head IS NOT NULL THEN (SELECT payload FROM recipes WHERE id=new.head) ELSE new.legacy_state END)
 WHERE id = new.id;
END;
CREATE TRIGGER versions_ai_facts_au AFTER UPDATE OF head, legacy_state ON versions BEGIN
 UPDATE versions SET ai_facts = rawtonic_ai_facts(
  CASE WHEN new.head IS NOT NULL THEN (SELECT payload FROM recipes WHERE id=new.head) ELSE new.legacy_state END)
 WHERE id = new.id;
END;
CREATE TRIGGER recipes_ai_facts_au AFTER UPDATE OF payload ON recipes BEGIN
 UPDATE versions SET ai_facts = rawtonic_ai_facts(new.payload) WHERE head = new.id;
END;
PRAGMA user_version=19;
