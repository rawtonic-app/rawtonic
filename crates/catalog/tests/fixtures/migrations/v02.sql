-- A schema 2 library with synthetic photographs (tests/migration_fixtures.rs).
CREATE TABLE roots(id TEXT PRIMARY KEY, path TEXT NOT NULL, volume_id TEXT, external INTEGER NOT NULL DEFAULT 0);
CREATE TABLE assets(
 id TEXT PRIMARY KEY, root_id TEXT NOT NULL REFERENCES roots(id), rel_path TEXT NOT NULL,
 size INTEGER NOT NULL, mtime INTEGER NOT NULL, fingerprint TEXT NOT NULL, capture_time TEXT,
 camera TEXT NOT NULL DEFAULT '', lens TEXT NOT NULL DEFAULT '', width INTEGER, height INTEGER,
 orientation INTEGER NOT NULL DEFAULT 1, kind TEXT NOT NULL, raw_jpeg_pair TEXT, stack TEXT,
 missing INTEGER NOT NULL DEFAULT 0, search_text TEXT NOT NULL DEFAULT '', UNIQUE(root_id, rel_path)
);
CREATE TABLE versions(id TEXT PRIMARY KEY, asset_id TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
 name TEXT NOT NULL, is_primary INTEGER NOT NULL DEFAULT 0, head INTEGER, legacy_state TEXT);
CREATE TABLE recipes(id INTEGER PRIMARY KEY, version_id TEXT NOT NULL REFERENCES versions(id) ON DELETE CASCADE,
 parent INTEGER REFERENCES recipes(id), schema_version INTEGER NOT NULL DEFAULT 1, payload TEXT NOT NULL,
 snapshot TEXT, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
CREATE TABLE metadata(asset_id TEXT PRIMARY KEY REFERENCES assets(id) ON DELETE CASCADE,
 rating INTEGER NOT NULL DEFAULT 0 CHECK(rating BETWEEN 0 AND 5), label TEXT NOT NULL DEFAULT '',
 flag INTEGER NOT NULL DEFAULT 0 CHECK(flag BETWEEN -1 AND 1), data TEXT NOT NULL DEFAULT '{}',
 xmp_hash TEXT, xmp_status TEXT NOT NULL DEFAULT 'pending');
CREATE TABLE masks(id TEXT PRIMARY KEY, version_id TEXT NOT NULL REFERENCES versions(id), vector_json TEXT, raster_path TEXT, hash TEXT);
CREATE TABLE collections(id TEXT PRIMARY KEY, name TEXT NOT NULL, parent TEXT REFERENCES collections(id), kind TEXT NOT NULL DEFAULT 'album' CHECK(kind IN ('album','group')), position INTEGER NOT NULL DEFAULT 0);
CREATE TABLE collection_assets(collection_id TEXT REFERENCES collections(id) ON DELETE CASCADE,asset_id TEXT REFERENCES assets(id) ON DELETE CASCADE,PRIMARY KEY(collection_id,asset_id));
CREATE TABLE smart_collections(id TEXT PRIMARY KEY, name TEXT NOT NULL, query_json TEXT NOT NULL);
CREATE TABLE presets(id TEXT PRIMARY KEY, name TEXT NOT NULL, recipe_json TEXT NOT NULL);
CREATE TABLE styles(id TEXT PRIMARY KEY, name TEXT NOT NULL, recipe_json TEXT NOT NULL);
CREATE TABLE export_recipes(id TEXT PRIMARY KEY, name TEXT NOT NULL, recipe_json TEXT NOT NULL);
CREATE TABLE naming_templates(id TEXT PRIMARY KEY, name TEXT NOT NULL, template TEXT NOT NULL);
CREATE TABLE workspaces(id TEXT PRIMARY KEY, name TEXT NOT NULL, settings_json TEXT NOT NULL);
CREATE TABLE keymaps(id TEXT PRIMARY KEY, name TEXT NOT NULL, bindings_json TEXT NOT NULL);
CREATE TABLE tasks(id TEXT PRIMARY KEY, kind TEXT NOT NULL, state TEXT NOT NULL, payload TEXT NOT NULL, progress REAL NOT NULL DEFAULT 0, error TEXT, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
CREATE TABLE file_ops_log(id TEXT PRIMARY KEY, kind TEXT NOT NULL, state TEXT NOT NULL, payload TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
CREATE VIRTUAL TABLE asset_search USING fts5(asset_id UNINDEXED, content);
INSERT INTO roots(id,path,volume_id,external) VALUES('session','.',NULL,0);
INSERT INTO roots(id,path,volume_id,external) VALUES('ext-archive','/Volumes/ARCHIVE/Photos','4C1A2B3D-0000-4000-8000-00000000A001',1);
INSERT INTO assets(id,root_id,rel_path,size,mtime,fingerprint,capture_time,camera,lens,width,height,orientation,kind,raw_jpeg_pair,stack,missing,search_text) VALUES('00000000-0000-4000-8000-000000000001','session','Capture/Day 1/DSC00123.ARW',20000001,1780000001,'xxh3:0000000000001eef','2026-06-01 10:00:00','ILCE-7RM4','FE 24-70mm F2.8 GM',6000,4000,1,'arw','DSC00123.JPG',NULL,0,'wedding ceremony');
INSERT INTO assets(id,root_id,rel_path,size,mtime,fingerprint,capture_time,camera,lens,width,height,orientation,kind,raw_jpeg_pair,stack,missing,search_text) VALUES('00000000-0000-4000-8000-000000000002','session','Capture/Day 1/DSC00124.ARW',20000002,1780000002,'xxh3:0000000000003dde','2026-06-01 18:30:00','ILCE-7RM4','FE 85mm F1.4 GM',6000,4000,1,'arw','DSC00124.JPG',NULL,0,'wedding   sunset ');
INSERT INTO assets(id,root_id,rel_path,size,mtime,fingerprint,capture_time,camera,lens,width,height,orientation,kind,raw_jpeg_pair,stack,missing,search_text) VALUES('00000000-0000-4000-8000-000000000003','session','Capture/Day 2/IMG_0001.CR3',20000003,1780000003,'xxh3:0000000000005ccd',NULL,'Canon EOS R5','RF 50mm F1.2 L USM',6000,4000,1,'cr3',NULL,NULL,0,'portrait');
INSERT INTO assets(id,root_id,rel_path,size,mtime,fingerprint,capture_time,camera,lens,width,height,orientation,kind,raw_jpeg_pair,stack,missing,search_text) VALUES('00000000-0000-4000-8000-000000000004','session','Capture/scan.jpg',20000004,1780000004,'xxh3:0000000000007bbc',NULL,'','',6000,4000,1,'jpg',NULL,NULL,0,'archive Wedding');
INSERT INTO assets(id,root_id,rel_path,size,mtime,fingerprint,capture_time,camera,lens,width,height,orientation,kind,raw_jpeg_pair,stack,missing,search_text) VALUES('00000000-0000-4000-8000-000000000005','session','Output/merge.dng',20000005,1780000005,'xxh3:0000000000009aab','2026-06-02 09:15:00','ILCE-7RM4','',6000,4000,1,'dng',NULL,NULL,0,'');
INSERT INTO assets(id,root_id,rel_path,size,mtime,fingerprint,capture_time,camera,lens,width,height,orientation,kind,raw_jpeg_pair,stack,missing,search_text) VALUES('00000000-0000-4000-8000-000000000006','ext-archive','2024/03/DSC_0042.NEF',20000006,1780000006,'xxh3:000000000000b99a','2024-03-10 12:00:00','NIKON Z 8','NIKKOR Z 24-120mm f/4 S',6000,4000,1,'nef',NULL,NULL,1,'');
INSERT INTO assets(id,root_id,rel_path,size,mtime,fingerprint,capture_time,camera,lens,width,height,orientation,kind,raw_jpeg_pair,stack,missing,search_text) VALUES('00000000-0000-4000-8000-000000000007','session','Trash/DSC00099.ARW',20000007,1780000007,'xxh3:000000000000d889','2026-06-01 11:00:00','ILCE-7RM4','',6000,4000,1,'arw',NULL,NULL,1,'');
INSERT INTO assets(id,root_id,rel_path,size,mtime,fingerprint,capture_time,camera,lens,width,height,orientation,kind,raw_jpeg_pair,stack,missing,search_text) VALUES('00000000-0000-4000-8000-000000000008','session','Selects/DSC00200.ARW',20000008,1780000008,'xxh3:000000000000f778',NULL,'','',6000,4000,1,'arw',NULL,NULL,0,'');
INSERT INTO assets(id,root_id,rel_path,size,mtime,fingerprint,capture_time,camera,lens,width,height,orientation,kind,raw_jpeg_pair,stack,missing,search_text) VALUES('00000000-0000-4000-8000-000000000009','session','Capture/Day 2/DSCF1234.RAF',20000009,1780000009,'xxh3:0000000000011667','2026-06-03 07:45:00','X-T5','',6000,4000,1,'raf',NULL,NULL,0,'landscape');
INSERT INTO versions(id,asset_id,name,is_primary,head,legacy_state) VALUES('00000000-0000-4000-9000-000000000001','00000000-0000-4000-8000-000000000001','Original',1,3,NULL);
INSERT INTO versions(id,asset_id,name,is_primary,head,legacy_state) VALUES('00000000-0000-4000-9000-000000000002','00000000-0000-4000-8000-000000000002','Original',1,4,NULL);
INSERT INTO versions(id,asset_id,name,is_primary,head,legacy_state) VALUES('00000000-0000-4000-9000-000000000003','00000000-0000-4000-8000-000000000003','Original',1,6,NULL);
INSERT INTO versions(id,asset_id,name,is_primary,head,legacy_state) VALUES('00000000-0000-4000-9000-000000000004','00000000-0000-4000-8000-000000000004','Original',1,NULL,NULL);
INSERT INTO versions(id,asset_id,name,is_primary,head,legacy_state) VALUES('00000000-0000-4000-9000-000000000005','00000000-0000-4000-8000-000000000005','Original',1,NULL,NULL);
INSERT INTO versions(id,asset_id,name,is_primary,head,legacy_state) VALUES('00000000-0000-4000-9000-000000000006','00000000-0000-4000-8000-000000000006','Original',1,NULL,NULL);
INSERT INTO versions(id,asset_id,name,is_primary,head,legacy_state) VALUES('00000000-0000-4000-9000-000000000007','00000000-0000-4000-8000-000000000007','Original',1,NULL,NULL);
INSERT INTO versions(id,asset_id,name,is_primary,head,legacy_state) VALUES('00000000-0000-4000-9000-000000000008','00000000-0000-4000-8000-000000000008','Original',1,NULL,NULL);
INSERT INTO versions(id,asset_id,name,is_primary,head,legacy_state) VALUES('00000000-0000-4000-9000-000000000009','00000000-0000-4000-8000-000000000009','Original',1,NULL,NULL);
INSERT INTO versions(id,asset_id,name,is_primary,head,legacy_state) VALUES('00000000-0000-4000-9000-000000000101','00000000-0000-4000-8000-000000000001','a1b2c3',0,NULL,'{"adjustments":{"exposure":1.0}}');
INSERT INTO versions(id,asset_id,name,is_primary,head,legacy_state) VALUES('00000000-0000-4000-9000-000000000102','00000000-0000-4000-8000-000000000001','0f0f0f',0,NULL,'{"adjustments":{"aiPatches":[{"id":"p1","patchData":{"color":"rawtonic-blob:sha256:none","model":"external"},"visible":true}]}}');
INSERT INTO versions(id,asset_id,name,is_primary,head,legacy_state) VALUES('00000000-0000-4000-9000-000000000103','00000000-0000-4000-8000-000000000002','beef01',0,NULL,'{"adjustments":{"saturation":-100}}');
INSERT INTO recipes(id,version_id,parent,schema_version,payload,snapshot,created_at) VALUES(1,'00000000-0000-4000-9000-000000000001',NULL,1,'{"adjustments":{"exposure":0.3}}',NULL,'2026-06-01 12:00:01');
INSERT INTO recipes(id,version_id,parent,schema_version,payload,snapshot,created_at) VALUES(2,'00000000-0000-4000-9000-000000000001',1,1,'{"adjustments":{"contrast":10,"exposure":0.5}}','Before AI','2026-06-01 12:00:02');
INSERT INTO recipes(id,version_id,parent,schema_version,payload,snapshot,created_at) VALUES(3,'00000000-0000-4000-9000-000000000001',2,1,'{"adjustments":{"aiDenoise":40,"aiDenoiseVersion":1,"contrast":10,"exposure":0.5,"masks":[{"id":"m1","image":"rawtonic-blob:sha256:5f1c0e6b2a9d4c3e8f7a6b5c4d3e2f1a0b9c8d7e6f5a4b3c2d1e0f9a8b7c6d5e"}]}}',NULL,'2026-06-01 12:00:03');
INSERT INTO recipes(id,version_id,parent,schema_version,payload,snapshot,created_at) VALUES(4,'00000000-0000-4000-9000-000000000002',NULL,1,'{"adjustments":{"temperature":5200}}',NULL,'2026-06-02 12:00:04');
INSERT INTO recipes(id,version_id,parent,schema_version,payload,snapshot,created_at) VALUES(5,'00000000-0000-4000-9000-000000000003',NULL,1,'{"adjustments":{"exposure":-0.2}}',NULL,'2026-06-03 12:00:05');
INSERT INTO recipes(id,version_id,parent,schema_version,payload,snapshot,created_at) VALUES(6,'00000000-0000-4000-9000-000000000003',5,1,'{"adjustments":{"exposure":-0.2,"vibrance":15}}',NULL,'2026-06-03 12:00:06');
INSERT INTO metadata(asset_id,rating,label,flag,data,xmp_hash,xmp_status) VALUES('00000000-0000-4000-8000-000000000001',5,'red',1,'{"creator":"Synthetic Photographer","flag":1,"keywords":["wedding","ceremony"],"label":"red","rating":5,"title":"First kiss"}','digest-1','synced');
INSERT INTO metadata(asset_id,rating,label,flag,data,xmp_hash,xmp_status) VALUES('00000000-0000-4000-8000-000000000002',4,'yellow',0,'{"flag":0,"keywords":["wedding","  sunset ",7],"label":"yellow","rating":4}','digest-2','pending');
INSERT INTO metadata(asset_id,rating,label,flag,data,xmp_hash,xmp_status) VALUES('00000000-0000-4000-8000-000000000003',3,'green',-1,'{"flag":-1,"keywords":["portrait"],"label":"green","rating":3}',NULL,'pending');
INSERT INTO metadata(asset_id,rating,label,flag,data,xmp_hash,xmp_status) VALUES('00000000-0000-4000-8000-000000000004',1,'',0,'{"flag":0,"keywords":["archive","Wedding"],"label":"","rating":1}',NULL,'pending');
INSERT INTO metadata(asset_id,rating,label,flag,data,xmp_hash,xmp_status) VALUES('00000000-0000-4000-8000-000000000005',0,'',0,'{"flag":0,"label":"","rating":0}',NULL,'pending');
INSERT INTO metadata(asset_id,rating,label,flag,data,xmp_hash,xmp_status) VALUES('00000000-0000-4000-8000-000000000006',2,'',0,'{"flag":0,"label":"","rating":2}','digest-6','synced');
INSERT INTO metadata(asset_id,rating,label,flag,data,xmp_hash,xmp_status) VALUES('00000000-0000-4000-8000-000000000007',0,'',0,'{"flag":0,"label":"","rating":0}',NULL,'pending');
INSERT INTO metadata(asset_id,rating,label,flag,data,xmp_hash,xmp_status) VALUES('00000000-0000-4000-8000-000000000008',0,'',0,'{}',NULL,'pending');
INSERT INTO metadata(asset_id,rating,label,flag,data,xmp_hash,xmp_status) VALUES('00000000-0000-4000-8000-000000000009',0,'',0,'{"flag":0,"keywords":["landscape"],"label":"","rating":0}',NULL,'pending');
INSERT INTO masks(id,version_id,vector_json,raster_path,hash) VALUES('mask-1','00000000-0000-4000-9000-000000000001','{"type":"radial"}','masks/mask-1.png','mask-hash-1');
INSERT INTO collections(id,name,parent,kind,position) VALUES('group-clients','Clients',NULL,'group',0);
INSERT INTO collections(id,name,parent,kind,position) VALUES('album-wedding','Wedding','group-clients','album',0);
INSERT INTO collections(id,name,parent,kind,position) VALUES('album-portraits','Portraits',NULL,'album',1);
INSERT INTO collection_assets(collection_id,asset_id) VALUES('album-wedding','00000000-0000-4000-8000-000000000001');
INSERT INTO collection_assets(collection_id,asset_id) VALUES('album-wedding','00000000-0000-4000-8000-000000000002');
INSERT INTO collection_assets(collection_id,asset_id) VALUES('album-wedding','00000000-0000-4000-8000-000000000004');
INSERT INTO collection_assets(collection_id,asset_id) VALUES('album-portraits','00000000-0000-4000-8000-000000000003');
INSERT INTO collection_assets(collection_id,asset_id) VALUES('album-portraits','00000000-0000-4000-8000-000000000007');
INSERT INTO smart_collections(id,name,query_json) VALUES('smart-five-stars','Four stars and up','{"combine":"all","rules":[{"field":"rating","op":"ge","value":4}]}');
INSERT INTO presets(id,name,recipe_json) VALUES('presets-1','Warm','{"temperature":5600}');
INSERT INTO styles(id,name,recipe_json) VALUES('styles-1','Warm','{"temperature":5600}');
INSERT INTO export_recipes(id,name,recipe_json) VALUES('export_recipes-1','Warm','{"temperature":5600}');
INSERT INTO naming_templates(id,name,template) VALUES('naming-1','Date and counter','{date}_{counter:4}');
INSERT INTO workspaces(id,name,settings_json) VALUES('ws-1','Culling','{}');
INSERT INTO keymaps(id,name,bindings_json) VALUES('km-1','Default','{}');
INSERT INTO tasks(id,kind,state,payload,progress,error,created_at) VALUES('job-1','ingest','interrupted','{"destination":"Capture"}',0.5,NULL,'2026-06-01 09:00:00');
INSERT INTO file_ops_log(id,kind,state,payload,created_at) VALUES('op-1','rename','committed','{}','2026-06-01 09:30:00');
INSERT INTO asset_search(rowid,asset_id,content) VALUES(1,'00000000-0000-4000-8000-000000000001','Capture/Day 1/DSC00123.ARW wedding ceremony');
INSERT INTO asset_search(rowid,asset_id,content) VALUES(2,'00000000-0000-4000-8000-000000000002','Capture/Day 1/DSC00124.ARW wedding   sunset ');
INSERT INTO asset_search(rowid,asset_id,content) VALUES(3,'00000000-0000-4000-8000-000000000003','Capture/Day 2/IMG_0001.CR3 portrait');
INSERT INTO asset_search(rowid,asset_id,content) VALUES(4,'00000000-0000-4000-8000-000000000004','Capture/scan.jpg archive Wedding');
INSERT INTO asset_search(rowid,asset_id,content) VALUES(5,'00000000-0000-4000-8000-000000000005','Output/merge.dng ');
INSERT INTO asset_search(rowid,asset_id,content) VALUES(6,'00000000-0000-4000-8000-000000000006','2024/03/DSC_0042.NEF ');
INSERT INTO asset_search(rowid,asset_id,content) VALUES(7,'00000000-0000-4000-8000-000000000007','Trash/DSC00099.ARW ');
INSERT INTO asset_search(rowid,asset_id,content) VALUES(8,'00000000-0000-4000-8000-000000000008','Selects/DSC00200.ARW ');
INSERT INTO asset_search(rowid,asset_id,content) VALUES(9,'00000000-0000-4000-8000-000000000009','Capture/Day 2/DSCF1234.RAF landscape');
CREATE INDEX assets_fingerprint ON assets(fingerprint);
CREATE UNIQUE INDEX primary_version ON versions(asset_id) WHERE is_primary=1;
CREATE TRIGGER assets_ai AFTER INSERT ON assets BEGIN INSERT INTO asset_search(asset_id,content) VALUES(new.id,new.rel_path || ' ' || new.search_text); END;
CREATE TRIGGER assets_au AFTER UPDATE OF rel_path,search_text ON assets BEGIN DELETE FROM asset_search WHERE asset_id=old.id; INSERT INTO asset_search(asset_id,content) VALUES(new.id,new.rel_path || ' ' || new.search_text); END;
CREATE TRIGGER assets_ad AFTER DELETE ON assets BEGIN DELETE FROM asset_search WHERE asset_id=old.id; END;
PRAGMA user_version=2;
