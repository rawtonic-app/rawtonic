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
//! counted; local adjustment layers are counted, not read.
use crate::{Album, Develop, Library, NotRead, Photo, open_copy};
use anyhow::{Context, Result};
use rawtonic_metadata::Metadata;
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
];

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
        1 => "Red",
        3 => "Yellow",
        4 => "Green",
        5 => "Blue",
        7 => "Purple",
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
        .canonicalize()
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

    let columns: Vec<String> = SETTINGS.iter().map(|(c, _)| format!("l.{c}")).collect();
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
        let keywords = text(7)?
            .split([',', ';', '\n'])
            .map(str::trim)
            .filter(|k| !k.is_empty())
            .map(str::to_string)
            .collect();
        let metadata = Metadata {
            rating: r.get::<_, Option<i64>>(5)?.unwrap_or(0).clamp(0, 5) as u8,
            label: label_from_tag(r.get::<_, Option<i64>>(6)?.unwrap_or(0)),
            flag: 0,
            keywords,
            title: text(8)?,
            description: text(9)?,
            creator: text(10)?,
            copyright: text(11)?,
            iptc: Default::default(),
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
        });
    }
    drop(rows);
    drop(q);

    let mut not_read = NotRead {
        local_adjustments: conn.query_row(
            "SELECT count(*) FROM ZVARIANTLAYER WHERE Z_ENT = ?",
            [entity("LocalAdjustmentsLayer")],
            |r| r.get::<_, i64>(0),
        )? as usize,
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
