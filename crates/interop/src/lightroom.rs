//! Lightroom Classic catalogs (`.lrcat`, schema of Lightroom Classic 6–14).
//!
//! Photos: `Adobe_images` → `AgLibraryFile` → `AgLibraryFolder` →
//! `AgLibraryRootFolder` (path = root + folder + base name + extension).
//! Rating, pick and colour label from `Adobe_images`; caption and copyright
//! from `AgLibraryIPTC`; keywords from `AgLibraryKeyword(Image)` with their
//! hierarchy; title and creator from the XMP packet Lightroom keeps for
//! every photo in `Adobe_AdditionalMetadata.xmp`. Develop settings, for
//! photos `Adobe_imageDevelopSettings` marks as adjusted (`1`; `-1` is a
//! photo at its defaults), come from that table's Lua settings (`text`,
//! written as sidecar XMP by `lightroom_settings::to_xmp`); catalogs whose
//! XMP packet carries the `crs:` settings instead are read from there.
//! Collections and collection sets become albums and album groups; smart
//! collections carry their rules (`AgLibraryCollectionContent`), which the
//! app translates where it can. Folder stacks (with the virtual copies
//! Lightroom stacks with their photograph) come with their top, order and
//! collapsed state.
use crate::{
    Album, Develop, Library, NotRead, Photo, SmartRule, SmartRules, SmartValue, Stack,
    label_from_name,
    lightroom_settings::{self, Lua},
    open_copy,
};
use anyhow::{Result, bail};
use rawtonic_metadata::{Metadata, read_xmp};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

const COLLECTION: &str = "com.adobe.ag.library.collection";
const GROUP: &str = "com.adobe.ag.library.group";
const SMART: &str = "com.adobe.ag.library.smart_collection";

/// Reads a catalog. Refuses while Lightroom has it open (its lock file
/// exists), because the catalog may be half-written then.
pub fn read(catalog: &Path) -> Result<Library> {
    let mut lock = catalog.as_os_str().to_owned();
    lock.push(".lock");
    if PathBuf::from(lock).exists() {
        bail!("Lightroom has this catalog open. Quit Lightroom and try again.");
    }
    let scratch = tempfile::tempdir()?;
    let conn = open_copy(catalog, scratch.path())?;

    let list = keyword_list(&conn)?;
    let keywords = &list.paths;
    let mut photo_keywords: HashMap<i64, Vec<String>> = HashMap::new();
    {
        let mut q = conn.prepare("SELECT image, tag FROM AgLibraryKeywordImage")?;
        for row in q.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))? {
            let (image, tag) = row?;
            if let Some(path) = keywords.get(&tag) {
                photo_keywords.entry(image).or_default().push(path.clone());
            }
        }
    }

    // Develop snapshots (Lua settings like the photo's own), oldest first; a
    // catalog without the table has none.
    let mut snapshots: HashMap<i64, Vec<(String, Develop)>> = HashMap::new();
    if let Ok(mut q) = conn.prepare(
        "SELECT image, name, text FROM Adobe_libraryImageDevelopSnapshot ORDER BY id_local",
    ) {
        let rows = q.query_map([], |r| {
            Ok((
                r.get::<_, Option<i64>>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })?;
        for row in rows {
            let (Some(image), name, Some(text)) = row? else {
                continue;
            };
            let Some(xmp) = lightroom_settings::to_xmp(&text) else {
                continue;
            };
            let name = name
                .map(|n| n.trim().to_string())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| "Lightroom".to_string());
            snapshots
                .entry(image)
                .or_default()
                .push((name, Develop::LightroomXmp(xmp)));
        }
    }

    // Lightroom's own name for each photograph's lens: empty where its
    // catalog records the photograph without one, absent where the catalog
    // has no record (or none of these tables).
    let mut lenses: HashMap<i64, String> = HashMap::new();
    if let Ok(mut q) = conn.prepare(
        "SELECT e.image, l.value FROM AgHarvestedExifMetadata e
         LEFT JOIN AgInternedExifLens l ON l.id_local = e.lensRef",
    ) && let Ok(rows) = q.query_map([], |r| {
        Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, Option<String>>(1)?))
    }) {
        for (image, name) in rows.flatten() {
            if let Some(image) = image {
                lenses.insert(image, name.unwrap_or_default().trim().to_string());
            }
        }
    }

    let mut photos = Vec::new();
    let mut q = conn.prepare(
        "SELECT i.id_local, r.absolutePath, fo.pathFromRoot, f.baseName, f.extension,
                i.rating, i.colorLabels, i.pick, i.masterImage,
                x.xmp, ip.caption, ip.copyright,
                coalesce(d.hasDevelopAdjustmentsEx, d.hasDevelopAdjustments, 0), d.text,
                i.copyName, i.id_global
         FROM Adobe_images i
         JOIN AgLibraryFile f ON f.id_local = i.rootFile
         JOIN AgLibraryFolder fo ON fo.id_local = f.folder
         JOIN AgLibraryRootFolder r ON r.id_local = fo.rootFolder
         LEFT JOIN Adobe_AdditionalMetadata x ON x.image = i.id_local
         LEFT JOIN AgLibraryIPTC ip ON ip.image = i.id_local
         LEFT JOIN Adobe_imageDevelopSettings d ON d.image = i.id_local
         ORDER BY i.id_local",
    )?;
    let rows = q.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, Option<f64>>(5)?,
            r.get::<_, Option<String>>(6)?,
            r.get::<_, Option<f64>>(7)?,
            r.get::<_, Option<i64>>(8)?,
            packet(r.get_ref(9)?),
            r.get::<_, Option<String>>(10)?,
            r.get::<_, Option<String>>(11)?,
            r.get::<_, Option<f64>>(12)?.unwrap_or(0.0) > 0.0,
            r.get::<_, Option<String>>(13)?,
            r.get::<_, Option<String>>(14)?,
            r.get::<_, Option<String>>(15)?,
        ))
    })?;
    for row in rows {
        let (
            id,
            root,
            folder,
            base,
            ext,
            rating,
            label,
            pick,
            master,
            xmp,
            caption,
            copyright,
            adjusted,
            settings,
            copy_name,
            uid,
        ) = row?;
        let file = if ext.is_empty() {
            base
        } else {
            format!("{base}.{ext}")
        };
        let path = PathBuf::from(root).join(folder).join(file);
        let from_xmp = xmp
            .as_deref()
            .filter(|x| !x.trim().is_empty())
            .and_then(|x| read_xmp(x).ok())
            .map(|(meta, _)| meta)
            .unwrap_or_default();
        let metadata = Metadata {
            rating: rating.unwrap_or(0.0).clamp(0.0, 5.0) as u8,
            label: label_from_name(label.as_deref().unwrap_or("")),
            flag: match pick.unwrap_or(0.0) {
                p if p > 0.0 => 1,
                p if p < 0.0 => -1,
                _ => 0,
            },
            keywords: photo_keywords.remove(&id).unwrap_or_default(),
            title: from_xmp.title,
            description: caption
                .filter(|c| !c.is_empty())
                .unwrap_or(from_xmp.description),
            creator: from_xmp.creator,
            copyright: copyright
                .filter(|c| !c.is_empty())
                .unwrap_or(from_xmp.copyright),
            // City, country, headline, contact … from the catalog's XMP.
            iptc: from_xmp.iptc,
            capture: Default::default(),
        };
        // The settings list every value, defaults included; only photos
        // Lightroom marks as adjusted carry them.
        let develop = match (
            settings.as_deref().and_then(lightroom_settings::to_xmp),
            xmp,
        ) {
            _ if !adjusted => Develop::None,
            (Some(settings), _) => Develop::LightroomXmp(settings),
            (None, Some(xmp)) if xmp.contains("crs:") => Develop::LightroomXmp(xmp),
            _ => Develop::None,
        };
        photos.push(Photo {
            key: id.to_string(),
            path,
            metadata,
            develop,
            lens: lenses.remove(&id),
            copy_of: master.filter(|m| *m != id).map(|m| m.to_string()),
            copy_name: copy_name.filter(|n| !n.trim().is_empty()),
            uid: uid.filter(|u| !u.trim().is_empty()),
            snapshots: snapshots.remove(&id).unwrap_or_default(),
            capture_one_layers: Vec::new(),
        });
    }
    drop(q);

    let mut albums = Vec::new();
    let mut not_read = NotRead::default();
    let keyword_library = keyword_library(&list, &mut not_read);
    let mut members: HashMap<i64, Vec<String>> = HashMap::new();
    {
        let mut q = conn.prepare(
            "SELECT collection, image FROM AgLibraryCollectionImage ORDER BY positionInCollection, id_local",
        )?;
        for row in q.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))? {
            let (collection, image) = row?;
            members
                .entry(collection)
                .or_default()
                .push(image.to_string());
        }
    }
    // Smart collections' rules (a catalog without the table has none).
    let mut contents: HashMap<i64, String> = HashMap::new();
    if let Ok(mut q) = conn.prepare(
        "SELECT collection, content FROM AgLibraryCollectionContent
         WHERE owningModule = 'ag.library.smart_collection'",
    ) {
        for row in q.query_map([], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?))
        })? {
            if let (collection, Some(content)) = row? {
                contents.insert(collection, content);
            }
        }
    }
    let mut q = conn.prepare(
        "SELECT id_local, name, creationId, parent FROM AgLibraryCollection ORDER BY id_local",
    )?;
    for row in q.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, Option<i64>>(3)?,
        ))
    })? {
        let (id, name, kind, parent) = row?;
        match kind.as_str() {
            SMART => match contents.get(&id).and_then(|c| smart_rules(c)) {
                Some(rules) => albums.push(Album {
                    key: id.to_string(),
                    name,
                    parent: parent.map(|p| p.to_string()),
                    is_group: false,
                    photos: Vec::new(),
                    smart: Some(rules),
                }),
                None => {
                    not_read.smart_albums += 1;
                    not_read.smart_album_names.push(name);
                }
            },
            COLLECTION | GROUP => {
                let photos = members.remove(&id).unwrap_or_default();
                // Lightroom's Quick Collection: only when used.
                if kind == COLLECTION
                    && name.eq_ignore_ascii_case("quick collection")
                    && photos.is_empty()
                {
                    continue;
                }
                albums.push(Album {
                    key: id.to_string(),
                    name,
                    parent: parent.map(|p| p.to_string()),
                    is_group: kind == GROUP,
                    photos,
                    smart: None,
                })
            }
            _ => {}
        }
    }
    drop(q);
    // Groups left empty (they held only smart collections not read) go.
    loop {
        let used: std::collections::HashSet<String> =
            albums.iter().filter_map(|a| a.parent.clone()).collect();
        let before = albums.len();
        albums.retain(|a| !a.is_group || used.contains(&a.key));
        if albums.len() == before {
            break;
        }
    }
    let keys: std::collections::HashSet<String> = albums.iter().map(|a| a.key.clone()).collect();
    for album in &mut albums {
        if album.parent.as_ref().is_some_and(|p| !keys.contains(p)) {
            album.parent = None;
        }
    }

    let stacks = folder_stacks(&conn)?;

    Ok(Library {
        name: catalog
            .file_stem()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        photos,
        albums,
        stacks,
        keywords: keyword_library,
        not_read,
    })
}

/// Lightroom's folder stacks (`AgLibraryFolderStack`, members in
/// `AgLibraryFolderStackImage` by `position`, 1 being the top; Lightroom
/// stacks a virtual copy with its photograph when it makes one). Stacks
/// within a collection (`AgLibraryCollectionStack`) are the collection's
/// own view and are not read. A catalog without the tables has none.
fn folder_stacks(conn: &rusqlite::Connection) -> Result<Vec<Stack>> {
    let Ok(mut q) = conn.prepare(
        "SELECT s.id_local, s.id_global, s.collapsed, m.image
         FROM AgLibraryFolderStack s JOIN AgLibraryFolderStackImage m ON m.stack = s.id_local
         ORDER BY s.id_local, CAST(m.position AS REAL), m.id_local",
    ) else {
        return Ok(Vec::new());
    };
    let mut stacks: Vec<(i64, Stack)> = Vec::new();
    for row in q.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, Option<i64>>(2)?,
            r.get::<_, i64>(3)?,
        ))
    })? {
        let (id, uid, collapsed, image) = row?;
        match stacks.last_mut() {
            Some((last, stack)) if *last == id => stack.photos.push(image.to_string()),
            _ => stacks.push((
                id,
                Stack {
                    key: uid
                        .filter(|u| !u.trim().is_empty())
                        .unwrap_or_else(|| id.to_string()),
                    collapsed: collapsed.unwrap_or(0) != 0,
                    photos: vec![image.to_string()],
                },
            )),
        }
    }
    Ok(stacks
        .into_iter()
        .map(|(_, s)| s)
        .filter(|s| s.photos.len() >= 2)
        .collect())
}

/// Lightroom's keyword list (`AgLibraryKeyword`, a tree under a root
/// without a name).
struct Keywords {
    /// Every keyword's path from the top ("Places|Italy|Rome") by id; the
    /// root is left out.
    paths: HashMap<i64, String>,
    /// Ids with "Include on Export" off.
    not_exported: Vec<i64>,
    /// Ids with "Export Containing Keywords" off.
    without_parents: Vec<i64>,
    /// Person keywords (`keywordType = 'person'`).
    persons: Vec<i64>,
    /// Ids whose own name holds `|`.
    with_separator: Vec<i64>,
    /// Every keyword's names from the top, as Lightroom holds them.
    levels: HashMap<i64, Vec<String>>,
    /// Synonyms (`AgLibraryKeywordSynonym`).
    synonyms: usize,
}

/// Reads the keyword list. Catalogs without the export flags or the synonym
/// table (older schemas) have none of them.
fn keyword_list(conn: &rusqlite::Connection) -> Result<Keywords> {
    type Row = (i64, Option<String>, Option<i64>, i64, i64, Option<String>);
    let full = "SELECT id_local, name, parent, includeOnExport, includeParents, keywordType
                FROM AgLibraryKeyword";
    let rows: Vec<Row> = match conn.prepare(full) {
        Ok(mut q) => q
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get::<_, Option<i64>>(3)?.unwrap_or(1),
                    r.get::<_, Option<i64>>(4)?.unwrap_or(1),
                    r.get(5)?,
                ))
            })?
            .collect::<rusqlite::Result<_>>()?,
        Err(_) => conn
            .prepare("SELECT id_local, name, parent FROM AgLibraryKeyword")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, 1, 1, None)))?
            .collect::<rusqlite::Result<_>>()?,
    };
    let nodes: HashMap<i64, (Option<String>, Option<i64>)> = rows
        .iter()
        .map(|(id, name, parent, ..)| (*id, (name.clone(), *parent)))
        .collect();
    let mut paths = HashMap::new();
    let mut levels = HashMap::new();
    for (&id, (name, _)) in &nodes {
        if name.as_deref().is_none_or(str::is_empty) {
            continue;
        }
        let mut parts = Vec::new();
        let mut current = Some(id);
        let mut guard = 0;
        while let Some(node) = current.and_then(|c| nodes.get(&c)) {
            if let Some(name) = node.0.as_deref().filter(|n| !n.is_empty()) {
                parts.push(name.to_string());
            }
            current = node.1;
            guard += 1;
            if guard > 64 {
                break;
            }
        }
        parts.reverse();
        paths.insert(id, parts.join("|"));
        levels.insert(id, parts);
    }
    let named = |pick: &dyn Fn(&Row) -> bool| -> Vec<i64> {
        rows.iter()
            .filter(|r| paths.contains_key(&r.0) && pick(r))
            .map(|r| r.0)
            .collect()
    };
    let synonyms = conn
        .query_row(
            "SELECT count(*) FROM AgLibraryKeywordSynonym s
             JOIN AgLibraryKeyword k ON k.id_local = s.keyword
             WHERE trim(coalesce(s.name, '')) <> ''",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0) as usize;
    Ok(Keywords {
        not_exported: named(&|r| r.3 == 0),
        without_parents: named(&|r| r.4 == 0),
        persons: named(&|r| r.5.as_deref() == Some("person")),
        with_separator: named(&|r| r.1.as_deref().is_some_and(|n| n.contains('|'))),
        paths,
        levels,
        synonyms,
    })
}

/// The keyword list for the import (KWL1): every path, parents and unused
/// terms included, and the keywords whose Lightroom attributes Rawtonic has
/// no counterpart for, by path (sorted).
fn keyword_library(list: &Keywords, not_read: &mut NotRead) -> Vec<String> {
    use rawtonic_metadata::keywords::normalize;
    let sorted = |ids: &[i64]| {
        let mut out: Vec<String> = ids
            .iter()
            .filter_map(|id| list.paths.get(id))
            .map(|p| normalize(p))
            .collect();
        out.sort();
        out.dedup();
        out
    };
    not_read.keyword_synonyms = list.synonyms;
    not_read.keywords_not_exported = sorted(&list.not_exported);
    not_read.keywords_exported_without_parents = sorted(&list.without_parents);
    not_read.person_keywords = sorted(&list.persons);
    // A name holding `|` (Rawtonic's level separator, also Lightroom's in
    // `lr:hierarchicalSubject`) arrives as levels.
    // Shown as Lightroom names them, so the literal name stays recognisable
    // beside a real `pipe ▸ term` hierarchy it now shares a path with.
    let mut literal: Vec<Vec<String>> = list
        .with_separator
        .iter()
        .filter_map(|id| list.levels.get(id).cloned())
        .collect();
    literal.sort();
    literal.dedup();
    not_read.keywords_with_level_separator = literal;
    let ids: Vec<i64> = list.paths.keys().copied().collect();
    sorted(&ids)
}

/// A smart collection's rules (`s = { { criteria = …, operation = …,
/// value = … }, …, combine = "intersect" }`; a nested group is a table of
/// the same form), or `None` when the text is not of that form.
fn smart_rules(content: &str) -> Option<SmartRules> {
    let Lua::Table(entries) = lightroom_settings::parse(content)? else {
        return None;
    };
    group(&entries)
}

fn group(entries: &[(Option<String>, Lua)]) -> Option<SmartRules> {
    let mut combine = "intersect".to_string();
    let mut rules = Vec::new();
    for (key, value) in entries {
        match (key.as_deref(), value) {
            (Some("combine"), Lua::Str(c)) => combine = c.clone(),
            (None, Lua::Table(fields)) => rules.push(rule(fields)?),
            _ => {}
        }
    }
    Some(SmartRules { combine, rules })
}

fn rule(fields: &[(Option<String>, Lua)]) -> Option<SmartRule> {
    let get = |name: &str| {
        fields
            .iter()
            .find(|(k, _)| k.as_deref() == Some(name))
            .map(|(_, v)| v)
    };
    let text = |name: &str| match get(name) {
        Some(Lua::Str(s)) => Some(s.clone()),
        _ => None,
    };
    let value = |name: &str| match get(name) {
        Some(Lua::Bool(b)) => SmartValue::Bool(*b),
        Some(Lua::Num(n)) => SmartValue::Number(*n),
        Some(Lua::Str(s)) => SmartValue::Text(s.clone()),
        _ => SmartValue::None,
    };
    match text("criteria") {
        Some(criteria) => Some(SmartRule::Condition {
            criteria,
            operation: text("operation")?,
            value: value("value"),
            value2: value("value2"),
            units: text("value_units"),
        }),
        None => group(fields).map(SmartRule::Group),
    }
}

/// The XMP packet of `Adobe_AdditionalMetadata.xmp`: text in older
/// catalogs, in Lightroom Classic 15 a blob of the packet's length (4 bytes,
/// big-endian) and its zlib stream. `None` when it is neither.
fn packet(value: rusqlite::types::ValueRef) -> Option<String> {
    use rusqlite::types::ValueRef;
    match value {
        ValueRef::Text(text) => Some(String::from_utf8_lossy(text).into_owned()),
        ValueRef::Blob(blob) => {
            let (length, stream) = blob.split_first_chunk::<4>()?;
            let length = u32::from_be_bytes(*length) as usize;
            // A packet is small; a damaged length must not reserve gigabytes.
            let mut out = Vec::with_capacity(length.min(1 << 20));
            std::io::Read::read_to_end(
                &mut std::io::Read::take(flate2::read::ZlibDecoder::new(stream), 64 << 20),
                &mut out,
            )
            .ok()?;
            String::from_utf8(out).ok()
        }
        _ => None,
    }
}
