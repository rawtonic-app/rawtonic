//! Capture One sessions (`.cosessiondb`, Core Data SQLite of Capture One 16+).
//!
//! Photos: `ZIMAGE` with its folder in `ZPATHLOCATION` (relative to the
//! session, or absolute as Mac root + relative path) and file name. Each
//! image has variants (`ZVARIANT`); the first is the photograph, the others
//! are like Lightroom's virtual copies. A variant's effective settings are
//! its combined layer (`ZVARIANT.ZCOMBINEDSETTINGS` → `ZVARIANTLAYER`), with
//! metadata in `ZVARIANTMETADATA` (rating, colour tag, IPTC); only variants
//! Capture One marks as modified carry settings. The settings
//! columns are named like Capture One's style keys (`ZEXPOSURE` →
//! `Exposure`) and are returned as such, so the style translation applies.
//! Albums are `AlbumCollection` entities (`ZENTITIES`), smart albums are
//! counted. Local adjustment layers are `LocalAdjustmentsLayer` rows of
//! `ZVARIANTLAYER` (`ZVARIANT`, bottom first by `ZINDEX`, with name,
//! opacity, on/off and the same settings columns); their masks are in the
//! photograph's `.comask` (`capture_one_masks`). The reader works on a
//! session's `.cosessiondb` and a catalog's `.cocatalogdb` alike.
use crate::capture_one_masks::{self, Mask, MaskFile};
use crate::{Album, Develop, Library, NotRead, Photo, open_copy};
use anyhow::{Context, Result};
use rawtonic_fs::Canonical;
use rawtonic_metadata::Metadata;
use rawtonic_metadata::culling::Rating;
use rawtonic_metadata::labels::ColorLabel;
use rusqlite::types::ValueRef;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

/// `ZVARIANTLAYER` columns and the Capture One style keys they hold.
/// `ZHIGHLIGHTRECOVERY` holds the two-way Highlight slider (−100 seen in a
/// session), which styles call `HighlightRecoveryEx`.
const SETTINGS: &[(&str, &str)] = &[
    ("ZEXPOSURE", "Exposure"),
    ("ZCONTRAST", "Contrast"),
    ("ZBRIGHTNESS", "Brightness"),
    ("ZSATURATION", "Saturation"),
    ("ZHIGHLIGHTRECOVERY", "HighlightRecoveryEx"),
    ("ZSHADOWRECOVERY", "ShadowRecovery"),
    ("ZWHITERECOVERY", "WhiteRecovery"),
    ("ZBLACKRECOVERY", "BlackRecovery"),
    ("ZDEHAZEAMOUNT", "DehazeAmount"),
    ("ZCLARITY", "Clarity"),
    ("ZCLARITYSTRUCTURE", "ClarityStructure"),
    ("ZCOLORBALANCE", "ColorBalance"),
    ("ZCOLORBALANCESHADOW", "ColorBalanceShadow"),
    ("ZCOLORBALANCEMIDTONE", "ColorBalanceMidtone"),
    ("ZCOLORBALANCEHIGHLIGHT", "ColorBalanceHighlight"),
    ("ZGRADATIONCURVE", "GradationCurve"),
    ("ZGRADATIONCURVEY", "GradationCurveY"),
    ("ZLEVELSHIGHLIGHT", "Highlight"),
    ("ZLEVELSSHADOW", "Shadow"),
    ("ZMIDTONE", "Midtone"),
    ("ZLEVELSTARGETHIGHLIGHT", "TargetHighlight"),
    ("ZLEVELSTARGETSHADOW", "TargetShadow"),
    ("ZUSMAMOUNT", "UsmAmount"),
    ("ZUSMRADIUS", "UsmRadius"),
    ("ZFILMGRAINAMOUNT", "FilmGrainAmount"),
    ("ZFILMGRAINGRANULARITY", "FilmGrainGranularity"),
    ("ZBWENABLED", "BwEnabled"),
    ("ZLENSPURPLEFRINGINGAMOUNT", "PurpleFringingAmount"),
    ("ZMOIRE", "Moire"),
    ("ZLENSVIGNETTING", "Vignetting"),
    // Lens correction: `curve|amount|…` (only the amounts are read; the
    // curves are Capture One's own profile data) and the profile's id.
    ("ZLENSDISTORTION", "LensGeoAbr"),
    ("ZLENSLIGHTFALLOFF", "LensLightFallOff"),
    ("ZLENSIDGUID", "LensIdGuid"),
    // Chromatic Aberration on/off (null: Capture One's default, no choice
    // stored) and the curve its Analyse makes (`LensChromAbrDefaultCurve`
    // is the profile's; the analysed one's style key is not seen in any
    // style, so it is named after it).
    ("ZLENSCHROMABRENABLED", "ChromaticAberEnabled"),
    ("ZLENSCUSTOMCHROMABR", "LensChromAbrCustomCurve"),
];

/// A Capture One local adjustment layer.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Layer {
    pub name: String,
    pub enabled: bool,
    /// 0–100.
    pub opacity: f64,
    /// Its settings as style entries (`Exposure` …), as the photo's.
    pub settings: Vec<(String, String)>,
    pub mask: Mask,
    /// Width and height of the frame the mask's geometry is drawn in.
    pub frame: (f64, f64),
}

/// `ZVARIANTMETADATA` IPTC columns and Rawtonic's IPTC keys.
const IPTC_COLUMNS: &[(&str, &str)] = &[
    ("ZCONTENT_HEADLINE", "headline"),
    ("ZIMAGE_LOCATION", "location"),
    ("ZIMAGE_CITY", "city"),
    ("ZIMAGE_STATE", "state"),
    ("ZIMAGE_COUNTRY", "country"),
    ("ZIMAGE_ISOCOUNTRYCODE", "countryCode"),
    ("ZSTATUS_JOBIDENTIFIER", "jobId"),
    ("ZSTATUS_INSTRUCTIONS", "instructions"),
    ("ZSTATUS_PROVIDER", "credit"),
    ("ZSTATUS_SOURCE", "source"),
    ("ZSTATUS_USAGETERMS", "usageTerms"),
    ("ZCONTACT_CREATORSTITLE", "creatorJobTitle"),
    ("ZCONTACT_EMAILS", "creatorEmail"),
    ("ZCONTACT_WEBSITES", "creatorUrl"),
    ("ZCONTACT_PHONES", "creatorPhone"),
    ("ZCONTACT_ADDRESS", "creatorAddress"),
    ("ZCONTACT_CITY", "creatorCity"),
    ("ZCONTACT_STATE_PROVINCE", "creatorRegion"),
    ("ZCONTACT_POSTALCODE", "creatorPostalCode"),
    ("ZCONTACT_COUNTRY", "creatorCountry"),
];

/// Capture One's colour tags: 1 red, 2 orange, 3 yellow, 4 green, 5 blue,
/// 6 pink, 7 purple. Orange and pink have no Rawtonic label.
fn label_from_tag(tag: i64) -> String {
    match tag {
        1 => ColorLabel::Red.name(),
        3 => ColorLabel::Yellow.name(),
        4 => ColorLabel::Green.name(),
        5 => ColorLabel::Blue.name(),
        7 => ColorLabel::Purple.name(),
        _ => "",
    }
    .to_string()
}

pub fn read(session_db: &Path) -> Result<Library> {
    let session = session_db
        .parent()
        .context("The session database has no folder")?
        .to_path_buf();
    let scratch = tempfile::tempdir()?;
    let conn = open_copy(session_db, scratch.path())?;
    // Capture One has no global id for a variant: the session file and the
    // variant's id there stand in for one (`Photo::uid`).
    let origin = session_db
        .canonical()
        .unwrap_or_else(|_| session_db.to_path_buf());

    let entities: HashMap<String, i64> = {
        let mut q = conn.prepare("SELECT ZNAME, Z_ENT FROM ZENTITIES")?;
        q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
            .collect::<rusqlite::Result<_>>()?
    };
    let entity = |name: &str| entities.get(name).copied().unwrap_or(-1);

    let folders: HashMap<i64, PathBuf> = {
        let mut q =
            conn.prepare("SELECT Z_PK, ZMACROOT, ZRELATIVEPATH, ZISRELATIVE FROM ZPATHLOCATION")?;
        q.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<bool>>(3)?,
            ))
        })?
        .map(|row| {
            row.map(|(pk, root, relative, is_relative)| {
                let relative = relative.unwrap_or_default();
                let path = if is_relative.unwrap_or(false) {
                    session.join(&relative)
                } else {
                    PathBuf::from(root.unwrap_or_else(|| "/".into())).join(&relative)
                };
                (pk, path)
            })
        })
        .collect::<rusqlite::Result<_>>()?
    };

    // Settings columns this database has (older ones lack some).
    let present: std::collections::HashSet<String> = {
        let mut q = conn.prepare("SELECT name FROM pragma_table_info('ZVARIANTLAYER')")?;
        q.query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    let columns: Vec<String> = SETTINGS
        .iter()
        .map(|(c, _)| {
            if present.contains(*c) {
                format!("l.{c}")
            } else {
                "NULL".to_string()
            }
        })
        .collect();
    let sql = format!(
        "SELECT v.Z_PK, i.Z_PK, i.ZIMAGELOCATION, i.ZIMAGEFILENAME, coalesce(i.ZISTRASHED, 0) || ',' || coalesce(v.ZISMODIFIED, 0),
                m.ZBASIC_RATING, m.ZCOLOR_TAG_INDEX, m.ZCONTENT_KEYWORDS, m.ZSTATUS_TITLE,
                m.ZCONTENT_DESCRIPTION, m.ZCONTACT_CREATOR, m.ZSTATUS_COPYRIGHTNOTICE, {}, {}
         FROM ZVARIANT v
         JOIN ZIMAGE i ON i.Z_PK = v.ZIMAGE
         LEFT JOIN ZVARIANTLAYER l ON l.Z_PK = v.ZCOMBINEDSETTINGS
         LEFT JOIN ZVARIANTMETADATA m ON m.ZLAYER = v.ZCOMBINEDSETTINGS
         ORDER BY i.Z_PK, v.ZINDEX, v.Z_PK",
        columns.join(", "),
        IPTC_COLUMNS
            .iter()
            .map(|(c, _)| format!("m.{c}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let mut photos = Vec::new();
    let mut unlocated = 0;
    let mut first_variant: HashMap<i64, String> = HashMap::new();
    let mut q = conn.prepare(&sql)?;
    let mut rows = q.query([])?;
    while let Some(r) = rows.next()? {
        let variant: i64 = r.get(0)?;
        let image: i64 = r.get(1)?;
        let location: Option<i64> = r.get(2)?;
        let file: Option<String> = r.get(3)?;
        // "trashed,modified"; unmodified variants carry Capture One's
        // defaults, which are not edits.
        let flags: String = r.get::<_, Option<String>>(4)?.unwrap_or_default();
        let (trashed, modified) = flags.split_once(',').unwrap_or((&flags, "0"));
        if trashed == "1" {
            continue;
        }
        let modified = modified == "1";
        let (Some(folder), Some(file)) = (location.and_then(|l| folders.get(&l)), file) else {
            unlocated += 1;
            continue;
        };
        let text = |i: usize| -> rusqlite::Result<String> {
            Ok(r.get::<_, Option<String>>(i)?.unwrap_or_default())
        };
        // Capture One writes a hierarchy as `Parent|Child`, the library's
        // stored form (levels trimmed, `rawtonic_metadata::keywords`).
        let mut keywords: Vec<String> = Vec::new();
        for keyword in text(7)?
            .split([',', ';', '\n'])
            .map(rawtonic_metadata::keywords::normalize)
        {
            if !keyword.is_empty() && !keywords.contains(&keyword) {
                keywords.push(keyword);
            }
        }
        let metadata = Metadata {
            rating: r
                .get::<_, Option<i64>>(5)?
                .unwrap_or(0)
                .clamp(0, i64::from(Rating::MAX)) as u8,
            label: label_from_tag(r.get::<_, Option<i64>>(6)?.unwrap_or(0)),
            flag: 0,
            keywords,
            title: text(8)?,
            description: text(9)?,
            creator: text(10)?,
            copyright: text(11)?,
            iptc: Default::default(),
            capture: Default::default(),
        };
        let mut metadata = metadata;
        for (index, (_, key)) in IPTC_COLUMNS.iter().enumerate() {
            let value: Option<String> = r.get(12 + SETTINGS.len() + index)?;
            metadata.set_iptc(key, value.as_deref().unwrap_or(""));
        }
        let mut settings = Vec::new();
        for (index, (_, key)) in SETTINGS.iter().enumerate() {
            let value = match r.get_ref(12 + index)? {
                ValueRef::Null => continue,
                ValueRef::Integer(v) => v.to_string(),
                ValueRef::Real(v) => v.to_string(),
                ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned(),
                ValueRef::Blob(_) => continue,
            };
            settings.push((key.to_string(), value));
        }
        let key = variant.to_string();
        let copy_of = match first_variant.get(&image) {
            Some(first) => Some(first.clone()),
            None => {
                first_variant.insert(image, key.clone());
                None
            }
        };
        photos.push(Photo {
            key,
            path: folder.join(file),
            metadata,
            develop: if settings.is_empty() || !modified {
                Develop::None
            } else {
                Develop::CaptureOne(settings)
            },
            copy_of,
            copy_name: None,
            snapshots: Vec::new(),
            uid: Some(format!("{}#{variant}", origin.display())),
            capture_one_layers: Vec::new(),
        });
    }
    drop(rows);
    drop(q);

    let mut layers = read_layers(&conn, entity("LocalAdjustmentsLayer"))?;
    let mut mask_files: HashMap<PathBuf, Option<MaskFile>> = HashMap::new();
    let mut unread_layers = 0;
    for photo in &mut photos {
        let Some(found) = photo
            .key
            .parse::<i64>()
            .ok()
            .and_then(|v| layers.remove(&v))
        else {
            continue;
        };
        for (uuid, mut layer) in found {
            let file = capture_one_masks::candidates(&photo.path, &session)
                .into_iter()
                .find_map(|p| {
                    let parsed = mask_files.entry(p.clone()).or_insert_with(|| {
                        std::fs::read(&p).ok().and_then(|d| MaskFile::parse(&d))
                    });
                    parsed.as_ref().filter(|f| f.has(&uuid)).cloned()
                });
            if let Some(file) = file {
                layer.mask = file.mask(&uuid);
                layer.frame = file.frame;
            }
            if matches!(layer.mask, Mask::Unsupported { .. } | Mask::Missing) {
                unread_layers += 1;
            }
            photo.capture_one_layers.push(layer);
        }
    }

    let mut not_read = NotRead {
        local_adjustments: unread_layers,
        unlocated,
        ..Default::default()
    };
    not_read.smart_albums = conn.query_row(
        "SELECT count(*) FROM ZCOLLECTION WHERE Z_ENT = ? AND ZSMARTCRITERIA IS NOT NULL",
        [entity("SmartCollection")],
        |r| r.get::<_, i64>(0),
    )? as usize;

    // Albums hold images (all their variants) or single variants.
    let image_variants: HashMap<i64, Vec<String>> = {
        let mut map: HashMap<i64, Vec<String>> = HashMap::new();
        let mut q = conn.prepare("SELECT ZIMAGE, Z_PK FROM ZVARIANT ORDER BY ZINDEX, Z_PK")?;
        for row in q.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))? {
            let (image, variant) = row?;
            map.entry(image).or_default().push(variant.to_string());
        }
        map
    };
    let mut members: HashMap<i64, Vec<String>> = HashMap::new();
    {
        let mut q = conn.prepare("SELECT ZCOLLECTION, ZIMAGE FROM ZIMAGEINCOLLECTION")?;
        for row in q.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))? {
            let (collection, image) = row?;
            if let Some(variants) = image_variants.get(&image) {
                members
                    .entry(collection)
                    .or_default()
                    .extend(variants.iter().cloned());
            }
        }
        let mut q = conn.prepare("SELECT ZCOLLECTION, ZVARIANT FROM ZVARIANTINCOLLECTION")?;
        for row in q.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))? {
            let (collection, variant) = row?;
            members
                .entry(collection)
                .or_default()
                .push(variant.to_string());
        }
    }
    let (album, group) = (entity("AlbumCollection"), entity("ProjectCollection"));
    let mut albums = Vec::new();
    let mut q = conn.prepare(
        "SELECT Z_PK, Z_ENT, ZNAME, ZPARENT FROM ZCOLLECTION WHERE ZNAME IS NOT NULL AND ZNAME != ''
         AND ZPARENT IS NOT NULL AND Z_ENT IN (?, ?) ORDER BY ZCOLLECTIONINDEX, Z_PK",
    )?;
    for row in q.query_map([album, group], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, Option<i64>>(3)?,
        ))
    })? {
        let (pk, ent, name, parent) = row?;
        let mut photos = members.remove(&pk).unwrap_or_default();
        photos.dedup();
        albums.push(Album {
            key: pk.to_string(),
            name,
            parent: parent.map(|p| p.to_string()),
            is_group: ent == group,
            photos,
            smart: None,
        });
    }
    drop(q);
    // Projects without albums go (the document's root has no parent).
    let used: std::collections::HashSet<String> =
        albums.iter().filter_map(|a| a.parent.clone()).collect();
    albums.retain(|a| !a.is_group || used.contains(&a.key));
    let keys: std::collections::HashSet<String> = albums.iter().map(|a| a.key.clone()).collect();
    for a in &mut albums {
        if a.parent.as_ref().is_some_and(|p| !keys.contains(p)) {
            a.parent = None;
        }
    }

    Ok(Library {
        name: session_db
            .file_stem()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        photos,
        albums,
        stacks: Vec::new(),
        not_read,
    })
}

/// The local adjustment layers by variant (`Z_PK`), bottom first, each with
/// its mask UUID; masks are filled in from the `.comask` by the caller.
fn read_layers(
    conn: &rusqlite::Connection,
    entity: i64,
) -> Result<HashMap<i64, Vec<(String, Layer)>>> {
    let present: std::collections::HashSet<String> = {
        let mut q = conn.prepare("SELECT name FROM pragma_table_info('ZVARIANTLAYER')")?;
        q.query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    let column = |c: &str| {
        if present.contains(c) {
            format!("l.{c}")
        } else {
            "NULL".to_string()
        }
    };
    let settings: Vec<String> = SETTINGS.iter().map(|(c, _)| column(c)).collect();
    let sql = format!(
        "SELECT {}, {}, {}, {}, {}, {} FROM ZVARIANTLAYER l WHERE l.Z_ENT = ?
         ORDER BY {}, l.Z_PK",
        column("ZVARIANT"),
        column("ZNAME"),
        column("ZMASKUUID"),
        column("ZOPACITY"),
        column("ZENABLED"),
        settings.join(", "),
        column("ZINDEX"),
    );
    let mut out: HashMap<i64, Vec<(String, Layer)>> = HashMap::new();
    let mut q = conn.prepare(&sql)?;
    let mut rows = q.query([entity])?;
    while let Some(r) = rows.next()? {
        let Some(variant) = r.get::<_, Option<i64>>(0)? else {
            continue;
        };
        let uuid: Option<String> = r.get(2)?;
        let mut values = Vec::new();
        for (index, (_, key)) in SETTINGS.iter().enumerate() {
            let value = match r.get_ref(5 + index)? {
                ValueRef::Integer(v) => v.to_string(),
                ValueRef::Real(v) => v.to_string(),
                ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned(),
                ValueRef::Null | ValueRef::Blob(_) => continue,
            };
            values.push((key.to_string(), value));
        }
        let layer = Layer {
            name: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
            enabled: r.get::<_, Option<i64>>(4)?.unwrap_or(1) != 0,
            opacity: r
                .get::<_, Option<f64>>(3)?
                .unwrap_or(100.0)
                .clamp(0.0, 100.0),
            settings: values,
            mask: Mask::Missing,
            frame: (0.0, 0.0),
        };
        out.entry(variant)
            .or_default()
            .push((uuid.unwrap_or_default(), layer));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture_one_masks::tests as comask;

    /// A session with one photograph whose variant has four layers: a
    /// linear gradient, a radial one, a painted mask (compressed tiles) and a
    /// layer whose mask is not in the `.comask`.
    #[test]
    fn layers_read_with_their_masks_from_a_session() {
        let dir = tempfile::tempdir().unwrap();
        let session = dir.path().join("Shoot");
        std::fs::create_dir_all(session.join("Capture/CaptureOne/Settings1665")).unwrap();
        std::fs::write(session.join("Capture/a.NEF"), b"raw").unwrap();
        std::fs::write(
            session.join("Capture/CaptureOne/Settings1665/a.NEF.comask"),
            comask::file_with_tiles(
                &[
                    comask::linear("L", (100.0, 50.0), (100.0, 400.0)),
                    comask::radial("R", true),
                    comask::raster("P", 0, &[(117, 0)]),
                ],
                &[crate::capture_one_tiles::encode(&[90; 64 * 64], 16)],
            ),
        )
        .unwrap();
        let db = session.join("Shoot.cosessiondb");
        let conn = rusqlite::Connection::open(&db).unwrap();
        let settings: Vec<String> = SETTINGS
            .iter()
            .map(|(c, _)| format!("{c} VARCHAR"))
            .collect();
        conn.execute_batch(&format!(
            "CREATE TABLE ZENTITIES (Z_ENT INTEGER, ZNAME VARCHAR);
             INSERT INTO ZENTITIES VALUES(20,'VariantCombinedLayer'),(23,'LocalAdjustmentsLayer');
             CREATE TABLE ZPATHLOCATION (Z_PK INTEGER PRIMARY KEY, ZMACROOT VARCHAR, ZRELATIVEPATH VARCHAR, ZISRELATIVE BOOLEAN);
             INSERT INTO ZPATHLOCATION VALUES(1,NULL,'Capture',1);
             CREATE TABLE ZIMAGE (Z_PK INTEGER PRIMARY KEY, ZIMAGELOCATION INTEGER, ZIMAGEFILENAME VARCHAR, ZISTRASHED BOOLEAN);
             INSERT INTO ZIMAGE VALUES(1,1,'a.NEF',0);
             CREATE TABLE ZVARIANT (Z_PK INTEGER PRIMARY KEY, ZINDEX INTEGER, ZIMAGE INTEGER, ZCOMBINEDSETTINGS INTEGER, ZISMODIFIED BOOLEAN);
             INSERT INTO ZVARIANT VALUES(7,0,1,100,1);
             CREATE TABLE ZVARIANTMETADATA (Z_PK INTEGER PRIMARY KEY, ZLAYER INTEGER, ZBASIC_RATING INTEGER,
               ZCOLOR_TAG_INDEX INTEGER, ZCONTENT_KEYWORDS VARCHAR, ZSTATUS_TITLE VARCHAR, ZCONTENT_DESCRIPTION VARCHAR,
               ZCONTACT_CREATOR VARCHAR, ZSTATUS_COPYRIGHTNOTICE VARCHAR, {});
             CREATE TABLE ZCOLLECTION (Z_ENT INTEGER, Z_PK INTEGER PRIMARY KEY, ZNAME VARCHAR, ZPARENT INTEGER,
               ZCOLLECTIONINDEX INTEGER, ZSMARTCRITERIA VARCHAR);
             CREATE TABLE ZIMAGEINCOLLECTION (Z_PK INTEGER PRIMARY KEY, ZCOLLECTION INTEGER, ZIMAGE INTEGER);
             CREATE TABLE ZVARIANTINCOLLECTION (Z_PK INTEGER PRIMARY KEY, ZCOLLECTION INTEGER, ZVARIANT INTEGER);
             CREATE TABLE ZVARIANTLAYER (Z_ENT INTEGER, Z_PK INTEGER PRIMARY KEY, ZVARIANT INTEGER, ZINDEX INTEGER,
               ZNAME VARCHAR, ZMASKUUID VARCHAR, ZOPACITY FLOAT, ZENABLED INTEGER, {});
             INSERT INTO ZVARIANTLAYER (Z_ENT, Z_PK, ZVARIANT, ZEXPOSURE) VALUES(20,100,7,0.3);
             INSERT INTO ZVARIANTLAYER (Z_ENT, Z_PK, ZVARIANT, ZINDEX, ZNAME, ZMASKUUID, ZOPACITY, ZENABLED, ZEXPOSURE, ZCONTRAST)
               VALUES(23,201,7,1,'Sky','L',60.0,1,-1.2,30.0),(23,202,7,0,'Vignette','R',100.0,0,1.0,NULL),
                     (23,203,7,2,'Face','P',100.0,1,0.5,NULL),(23,204,7,3,'Lost','X',100.0,1,0.5,NULL);",
            IPTC_COLUMNS
                .iter()
                .map(|(c, _)| format!("{c} VARCHAR"))
                .collect::<Vec<_>>()
                .join(", "),
            settings.join(", ")
        ))
        .unwrap();
        drop(conn);
        let lib = read(&db).unwrap();
        let layers = &lib.photos[0].capture_one_layers;
        let names: Vec<&str> = layers.iter().map(|l| l.name.as_str()).collect();
        // Bottom first.
        assert_eq!(names, ["Vignette", "Sky", "Face", "Lost"]);
        assert!(!layers[0].enabled && layers[1].enabled);
        assert_eq!(layers[1].opacity, 60.0);
        assert!(
            layers[1]
                .settings
                .contains(&("Exposure".into(), "-1.2".into()))
        );
        assert!(
            layers[1]
                .settings
                .contains(&("Contrast".into(), "30.0".into()))
        );
        assert_eq!(layers[1].frame, (7360.0, 4912.0));
        assert!(matches!(layers[0].mask, Mask::Radial { outside: true, .. }));
        assert!(matches!(
            layers[1].mask,
            Mask::Linear {
                start: (100.0, 50.0),
                ..
            }
        ));
        let Mask::Raster { raster, ai: false } = &layers[2].mask else {
            panic!("{:?}", layers[2].mask);
        };
        let values = raster.values().unwrap();
        assert_eq!((values[64 * 7360 + 2 * 64], values[0]), (90, 0));
        assert_eq!(layers[3].mask, Mask::Missing);
        // The photo's own settings are not a layer's.
        let Develop::CaptureOne(own) = &lib.photos[0].develop else {
            panic!();
        };
        assert!(own.contains(&("Exposure".into(), "0.3".into())));
        assert_eq!(lib.not_read.local_adjustments, 1);
    }
}
