//! Both readers against small libraries built with the tables and columns
//! of real ones (Lightroom Classic 14 catalog schema, Capture One 16 session).
use rawtonic_interop::{Develop, capture_one, lightroom};
use rusqlite::Connection;
use std::{fs, path::Path};

fn lightroom_catalog(dir: &Path, photos: &Path) -> std::path::PathBuf {
    let cat = dir.join("Test.lrcat");
    let conn = Connection::open(&cat).unwrap();
    conn.execute_batch(
        "CREATE TABLE Adobe_images (id_local INTEGER PRIMARY KEY, id_global UNIQUE NOT NULL,
           captureTime, colorLabels NOT NULL DEFAULT '', copyName, masterImage INTEGER,
           pick NOT NULL DEFAULT 0, rating, rootFile INTEGER NOT NULL DEFAULT 0);
         CREATE TABLE AgLibraryFile (id_local INTEGER PRIMARY KEY, id_global UNIQUE NOT NULL,
           baseName NOT NULL DEFAULT '', extension NOT NULL DEFAULT '', folder INTEGER NOT NULL DEFAULT 0);
         CREATE TABLE AgLibraryFolder (id_local INTEGER PRIMARY KEY, id_global UNIQUE NOT NULL,
           parentId INTEGER, pathFromRoot NOT NULL DEFAULT '', rootFolder INTEGER NOT NULL DEFAULT 0);
         CREATE TABLE AgLibraryRootFolder (id_local INTEGER PRIMARY KEY, id_global UNIQUE NOT NULL,
           absolutePath UNIQUE NOT NULL DEFAULT '', name NOT NULL DEFAULT '');
         CREATE TABLE Adobe_AdditionalMetadata (id_local INTEGER PRIMARY KEY, id_global UNIQUE NOT NULL,
           image INTEGER, xmp NOT NULL DEFAULT '');
         CREATE TABLE Adobe_imageDevelopSettings (id_local INTEGER PRIMARY KEY, hasDevelopAdjustments INTEGER,
           hasDevelopAdjustmentsEx, image INTEGER, text);
         CREATE TABLE AgLibraryIPTC (id_local INTEGER PRIMARY KEY, caption, copyright, image INTEGER NOT NULL DEFAULT 0);
         CREATE TABLE AgLibraryKeyword (id_local INTEGER PRIMARY KEY, id_global UNIQUE NOT NULL, name, parent INTEGER);
         CREATE TABLE AgLibraryKeywordImage (id_local INTEGER PRIMARY KEY, image INTEGER NOT NULL DEFAULT 0, tag INTEGER NOT NULL DEFAULT 0);
         CREATE TABLE AgLibraryCollection (id_local INTEGER PRIMARY KEY, creationId NOT NULL DEFAULT '',
           name NOT NULL DEFAULT '', parent INTEGER, systemOnly NOT NULL DEFAULT '');
         CREATE TABLE AgLibraryCollectionImage (id_local INTEGER PRIMARY KEY, collection INTEGER NOT NULL DEFAULT 0,
           image INTEGER NOT NULL DEFAULT 0, pick NOT NULL DEFAULT 0, positionInCollection);",
    )
    .unwrap();
    let root = format!("{}/", photos.display());
    conn.execute(
        "INSERT INTO AgLibraryRootFolder VALUES(1,'r1',?,'Photos')",
        [root],
    )
    .unwrap();
    conn.execute_batch(
        r#"INSERT INTO AgLibraryFolder VALUES(1,'f1',NULL,'2026/',1);
           INSERT INTO AgLibraryFile VALUES(1,'a',"A",'NEF',1),(2,'b',"B",'NEF',1);
           INSERT INTO Adobe_images VALUES(1,'i1',NULL,'Rot',NULL,NULL,1,3,1),
                                          (2,'i2',NULL,'',NULL,NULL,-1,0,2),
                                          (3,'i3',NULL,'',"Copy 1",1,0,5,1);
           INSERT INTO Adobe_imageDevelopSettings VALUES(1,1,1.0,1,'s = {}'),(2,0,0.0,2,'s = {}');
           INSERT INTO Adobe_AdditionalMetadata VALUES(2,'x2',2,'<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:Exposure2012="0.00"/></rdf:RDF></x:xmpmeta>');
           INSERT INTO AgLibraryIPTC VALUES(1,'Grandpa at the table','(c) N',1);
           INSERT INTO Adobe_AdditionalMetadata VALUES(1,'x1',1,'<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" xmlns:dc="http://purl.org/dc/elements/1.1/" crs:Exposure2012="+0.50" crs:Contrast2012="+10" xmlns:photoshop="http://ns.adobe.com/photoshop/1.0/" photoshop:City="Rome"><dc:title><rdf:Alt><rdf:li xml:lang="x-default">Summer</rdf:li></rdf:Alt></dc:title><dc:creator><rdf:Seq><rdf:li>Alex Example</rdf:li></rdf:Seq></dc:creator></rdf:Description></rdf:RDF></x:xmpmeta>');
           INSERT INTO AgLibraryKeyword VALUES(1,'k0',NULL,NULL),(2,'k1','Places',1),(3,'k2','Rome',2);
           INSERT INTO AgLibraryKeywordImage VALUES(1,1,3);
           INSERT INTO AgLibraryCollection VALUES(7,'com.adobe.ag.library.collection','quick collection',NULL,''),
             (10,'com.adobe.ag.library.group','Trips',NULL,''),
             (11,'com.adobe.ag.library.collection','Italy',10,''),
             (12,'com.adobe.ag.library.smart_collection','Five stars',NULL,''),
             (13,'com.adobe.ag.library.group','Smart only',NULL,''),
             (14,'com.adobe.ag.library.smart_collection','Recent',13,'');
           INSERT INTO AgLibraryCollectionImage VALUES(1,11,1,0,'a'),(2,11,2,0,'b');"#,
    )
    .unwrap();
    cat
}

#[test]
fn lightroom_photos_metadata_settings_and_collections() {
    let dir = tempfile::tempdir().unwrap();
    let photos = dir.path().join("Photos");
    fs::create_dir_all(photos.join("2026")).unwrap();
    fs::write(photos.join("2026/A.NEF"), b"a").unwrap();
    let cat = lightroom_catalog(dir.path(), &photos);
    let before = fs::read(&cat).unwrap();
    let mut lib = lightroom::read(&cat).unwrap();
    assert_eq!(fs::read(&cat).unwrap(), before, "catalog untouched");

    assert_eq!(lib.photos.len(), 3);
    let a = &lib.photos[0];
    assert_eq!(a.path, photos.join("2026/A.NEF"));
    assert_eq!(a.metadata.rating, 3);
    assert_eq!(a.metadata.label, "Red", "German label name");
    assert_eq!(a.metadata.flag, 1);
    assert_eq!(a.metadata.keywords, vec!["Places|Rome".to_string()]);
    assert_eq!(a.metadata.title, "Summer");
    assert_eq!(a.metadata.creator, "Alex Example");
    assert_eq!(
        a.metadata.iptc.get("city").map(String::as_str),
        Some("Rome")
    );
    assert_eq!(a.metadata.description, "Grandpa at the table");
    assert_eq!(a.metadata.copyright, "(c) N");
    assert!(
        matches!(&a.develop, Develop::LightroomXmp(x) if x.contains("crs:Exposure2012=\"+0.50\""))
    );
    let b = &lib.photos[1];
    assert_eq!(b.metadata.flag, -1, "rejected");
    assert_eq!(
        b.develop,
        Develop::None,
        "defaults in its XMP are not edits"
    );
    assert_eq!(lib.photos[2].copy_of.as_deref(), Some("1"), "virtual copy");

    // Trips ▸ Italy; the empty Quick Collection, smart collections and a
    // set holding only smart collections are left out.
    let names: Vec<_> = lib
        .albums
        .iter()
        .map(|a| (a.name.as_str(), a.is_group))
        .collect();
    assert_eq!(names, vec![("Trips", true), ("Italy", false)]);
    assert_eq!(lib.albums[1].parent.as_deref(), Some("10"));
    assert_eq!(lib.albums[1].photos, vec!["1".to_string(), "2".to_string()]);
    assert_eq!(lib.not_read.smart_albums, 2);

    let roots = lib.roots();
    assert_eq!(roots.len(), 1);
    assert_eq!((roots[0].photos, roots[0].found), (2, 1));
    // The drive moved: relink.
    let moved = dir.path().join("Moved");
    fs::create_dir_all(moved.join("2026")).unwrap();
    fs::write(moved.join("2026/B.NEF"), b"b").unwrap();
    assert_eq!(lib.relink(&photos, &moved), 3);
    assert_eq!(lib.roots()[0].found, 1);
    assert_eq!(lib.photos[1].path, moved.join("2026/B.NEF"));
}

#[test]
fn an_open_lightroom_catalog_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let cat = lightroom_catalog(dir.path(), dir.path());
    fs::write(dir.path().join("Test.lrcat.lock"), b"").unwrap();
    let error = lightroom::read(&cat).unwrap_err();
    assert!(error.to_string().contains("Quit Lightroom"), "{error}");
}

#[test]
fn capture_one_session_variants_settings_and_albums() {
    let dir = tempfile::tempdir().unwrap();
    let session = dir.path().join("Shoot");
    fs::create_dir_all(session.join("Capture")).unwrap();
    fs::write(session.join("Capture/DSCF0001.RAF"), b"raf").unwrap();
    let db = session.join("Shoot.cosessiondb");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE ZENTITIES (Z_ENT INTEGER, ZNAME VARCHAR);
         INSERT INTO ZENTITIES VALUES(2,'AlbumCollection'),(5,'SmartCollection'),(7,'ProjectCollection'),
           (16,'VariantLayer'),(20,'VariantCombinedLayer'),(23,'LocalAdjustmentsLayer');
         CREATE TABLE ZPATHLOCATION (Z_ENT INTEGER, Z_PK INTEGER PRIMARY KEY, ZWINROOT VARCHAR, ZMACROOT VARCHAR,
           ZRELATIVEPATH VARCHAR, ZISRELATIVE BOOLEAN, ZVOLUME VARCHAR);
         INSERT INTO ZPATHLOCATION VALUES(38,1,NULL,NULL,'Capture',1,NULL),(38,2,NULL,'/','nowhere/else',0,'/');
         CREATE TABLE ZIMAGE (Z_ENT INTEGER, Z_PK INTEGER PRIMARY KEY, ZIMAGELOCATION INTEGER,
           ZIMAGEFILENAME VARCHAR, ZISTRASHED BOOLEAN);
         INSERT INTO ZIMAGE VALUES(12,1,1,'DSCF0001.RAF',0),(12,2,2,'DSCF0002.RAF',0),(12,3,1,'DSCF0003.RAF',1),
           (12,4,99,'DSCF0004.RAF',0);
         CREATE TABLE ZVARIANT (Z_ENT INTEGER, Z_PK INTEGER PRIMARY KEY, ZINDEX INTEGER, ZIMAGE INTEGER,
           ZCOMBINEDSETTINGS INTEGER, ZISMODIFIED BOOLEAN);
         INSERT INTO ZVARIANT VALUES(15,10,0,1,100,1),(15,11,1,1,101,0),(15,12,0,2,NULL,0),(15,13,0,3,NULL,0),
           (15,14,0,4,NULL,0);
         CREATE TABLE ZVARIANTMETADATA (Z_ENT INTEGER, Z_PK INTEGER PRIMARY KEY, ZLAYER INTEGER, ZBASIC_RATING INTEGER,
           ZCOLOR_TAG_INDEX INTEGER, ZCONTENT_KEYWORDS VARCHAR, ZSTATUS_TITLE VARCHAR, ZCONTENT_DESCRIPTION VARCHAR,
           ZCONTACT_CREATOR VARCHAR, ZSTATUS_COPYRIGHTNOTICE VARCHAR,
           ZCONTENT_HEADLINE VARCHAR, ZIMAGE_LOCATION VARCHAR, ZIMAGE_CITY VARCHAR, ZIMAGE_STATE VARCHAR,
           ZIMAGE_COUNTRY VARCHAR, ZIMAGE_ISOCOUNTRYCODE VARCHAR, ZSTATUS_JOBIDENTIFIER VARCHAR, ZSTATUS_INSTRUCTIONS VARCHAR,
           ZSTATUS_PROVIDER VARCHAR, ZSTATUS_SOURCE VARCHAR, ZSTATUS_USAGETERMS VARCHAR, ZCONTACT_CREATORSTITLE VARCHAR,
           ZCONTACT_EMAILS VARCHAR, ZCONTACT_WEBSITES VARCHAR, ZCONTACT_PHONES VARCHAR, ZCONTACT_ADDRESS VARCHAR,
           ZCONTACT_CITY VARCHAR, ZCONTACT_STATE_PROVINCE VARCHAR, ZCONTACT_POSTALCODE VARCHAR, ZCONTACT_COUNTRY VARCHAR);
         INSERT INTO ZVARIANTMETADATA (Z_ENT, Z_PK, ZLAYER, ZBASIC_RATING, ZCOLOR_TAG_INDEX, ZCONTENT_KEYWORDS,
           ZSTATUS_TITLE, ZCONTENT_DESCRIPTION, ZCONTACT_CREATOR, ZSTATUS_COPYRIGHTNOTICE, ZIMAGE_CITY, ZIMAGE_COUNTRY,
           ZCONTACT_EMAILS)
           VALUES(13,1,100,4,1,'beach, sunset','Sun','Evening','Alex Example','(c) A','Ascona','Switzerland','alex@example.org'),
                 (13,2,101,2,2,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL);
         CREATE TABLE ZCOLLECTION (Z_ENT INTEGER, Z_PK INTEGER PRIMARY KEY, ZNAME VARCHAR, ZPARENT INTEGER,
           ZCOLLECTIONINDEX INTEGER, ZSMARTCRITERIA VARCHAR);
         INSERT INTO ZCOLLECTION VALUES(7,1,'root',NULL,0,NULL),(2,6,'Picks',1,0,NULL),(5,7,'Five stars',1,1,'x'),
           (5,8,'All Images',1,2,NULL);
         CREATE TABLE ZIMAGEINCOLLECTION (Z_ENT INTEGER, Z_PK INTEGER PRIMARY KEY, ZCOLLECTION INTEGER, ZIMAGE INTEGER);
         INSERT INTO ZIMAGEINCOLLECTION VALUES(47,1,6,1);
         CREATE TABLE ZVARIANTINCOLLECTION (Z_ENT INTEGER, Z_PK INTEGER PRIMARY KEY, ZCOLLECTION INTEGER, ZVARIANT INTEGER);",
    )
    .unwrap();
    let columns: Vec<String> = [
        "ZEXPOSURE",
        "ZCONTRAST",
        "ZBRIGHTNESS",
        "ZSATURATION",
        "ZHIGHLIGHTRECOVERY",
        "ZSHADOWRECOVERY",
        "ZWHITERECOVERY",
        "ZBLACKRECOVERY",
        "ZDEHAZEAMOUNT",
        "ZCLARITY",
        "ZCLARITYSTRUCTURE",
        "ZCOLORBALANCE",
        "ZCOLORBALANCESHADOW",
        "ZCOLORBALANCEMIDTONE",
        "ZCOLORBALANCEHIGHLIGHT",
        "ZGRADATIONCURVE",
        "ZGRADATIONCURVEY",
        "ZLEVELSHIGHLIGHT",
        "ZLEVELSSHADOW",
        "ZMIDTONE",
        "ZLEVELSTARGETHIGHLIGHT",
        "ZLEVELSTARGETSHADOW",
        "ZUSMAMOUNT",
        "ZUSMRADIUS",
        "ZFILMGRAINAMOUNT",
        "ZFILMGRAINGRANULARITY",
        "ZBWENABLED",
        "ZLENSPURPLEFRINGINGAMOUNT",
        "ZMOIRE",
        "ZLENSVIGNETTING",
    ]
    .iter()
    .map(|c| c.to_string())
    .collect();
    conn.execute_batch(&format!(
        "CREATE TABLE ZVARIANTLAYER (Z_ENT INTEGER, Z_PK INTEGER PRIMARY KEY, ZVARIANT INTEGER, {});
         INSERT INTO ZVARIANTLAYER (Z_ENT, Z_PK, ZVARIANT, ZEXPOSURE, ZHIGHLIGHTRECOVERY, ZCOLORBALANCE)
           VALUES(20,100,10,0.5,-100.0,'1;0.9;0.9'),(20,101,11,-0.3,0.0,NULL),(23,102,10,NULL,NULL,NULL);",
        columns.iter().map(|c| format!("{c} VARCHAR")).collect::<Vec<_>>().join(", ")
    ))
    .unwrap();
    drop(conn);
    let before = fs::read(&db).unwrap();
    let lib = capture_one::read(&db).unwrap();
    assert_eq!(fs::read(&db).unwrap(), before, "session untouched");

    // Trashed images and those without a recorded folder are left out.
    assert_eq!(lib.photos.len(), 3);
    assert_eq!(lib.not_read.unlocated, 1);
    let first = &lib.photos[0];
    assert_eq!(first.path, session.join("Capture/DSCF0001.RAF"));
    assert_eq!(
        (first.metadata.rating, first.metadata.label.as_str()),
        (4, "Red")
    );
    assert_eq!(
        first.metadata.keywords,
        vec!["beach".to_string(), "sunset".to_string()]
    );
    assert_eq!(first.metadata.title, "Sun");
    assert_eq!(
        first.metadata.iptc.get("city").map(String::as_str),
        Some("Ascona")
    );
    assert_eq!(
        first.metadata.iptc.get("country").map(String::as_str),
        Some("Switzerland")
    );
    assert_eq!(
        first.metadata.iptc.get("creatorEmail").map(String::as_str),
        Some("alex@example.org")
    );
    assert!(lib.photos[1].metadata.iptc.is_empty());
    let Develop::CaptureOne(settings) = &first.develop else {
        panic!("no settings: {:?}", first.develop);
    };
    assert!(settings.contains(&("Exposure".into(), "0.5".into())));
    assert!(settings.contains(&("HighlightRecoveryEx".into(), "-100.0".into())));
    assert!(settings.contains(&("ColorBalance".into(), "1;0.9;0.9".into())));
    // The second variant is a copy with Capture One's defaults only (not
    // modified); orange has no Rawtonic label.
    assert_eq!(lib.photos[1].copy_of.as_deref(), Some("10"));
    assert_eq!(lib.photos[1].develop, Develop::None);
    assert_eq!(lib.photos[1].metadata.label, "");
    assert_eq!(lib.photos[2].path, Path::new("/nowhere/else/DSCF0002.RAF"));
    // The album holds both variants of its image; the smart album is counted.
    assert_eq!(lib.albums.len(), 1);
    assert_eq!(lib.albums[0].name, "Picks");
    assert_eq!(
        lib.albums[0].photos,
        vec!["10".to_string(), "11".to_string()]
    );
    assert_eq!(lib.not_read.smart_albums, 1);
    assert_eq!(lib.not_read.local_adjustments, 1);
}
