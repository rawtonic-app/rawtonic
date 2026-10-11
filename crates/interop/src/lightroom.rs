//! Lightroom Classic catalogs (`.lrcat`, schema of Lightroom Classic 6–14).
//!
//! Photos: `Adobe_images` → `AgLibraryFile` → `AgLibraryFolder` →
//! `AgLibraryRootFolder` (path = root + folder + base name + extension).
//! Rating, pick and colour label from `Adobe_images`; caption and copyright
//! from `AgLibraryIPTC`; keywords from `AgLibraryKeyword(Image)` with their
//! hierarchy; title and creator from the XMP packet Lightroom keeps for
//! every photo in `Adobe_AdditionalMetadata.xmp`, which also holds the
//! develop settings (`crs:`), used when `Adobe_imageDevelopSettings` marks
//! the photo as adjusted. Collections and collection sets become albums
//! and album groups; smart collections are counted, not translated.
use crate::{Album, Develop, Library, NotRead, Photo, label_from_name, open_copy};
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

    let keywords = keyword_paths(&conn)?;
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

    let mut photos = Vec::new();
    let mut q = conn.prepare(
        "SELECT i.id_local, r.absolutePath, fo.pathFromRoot, f.baseName, f.extension,
                i.rating, i.colorLabels, i.pick, i.masterImage,
                x.xmp, ip.caption, ip.copyright,
                coalesce(d.hasDevelopAdjustmentsEx, d.hasDevelopAdjustments, 0)
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
            r.get::<_, Option<String>>(9)?,
            r.get::<_, Option<String>>(10)?,
            r.get::<_, Option<String>>(11)?,
            r.get::<_, Option<f64>>(12)?.unwrap_or(0.0) != 0.0,
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
        };
        // The XMP lists every setting, defaults included; only photos
        // Lightroom marks as adjusted carry them.
        let develop = match xmp {
            Some(xmp) if adjusted && xmp.contains("crs:") => Develop::LightroomXmp(xmp),
            _ => Develop::None,
        };
        photos.push(Photo {
            key: id.to_string(),
            path,
            metadata,
            develop,
            copy_of: master.filter(|m| *m != id).map(|m| m.to_string()),
        });
    }
    drop(q);

    let mut albums = Vec::new();
    let mut not_read = NotRead::default();
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
            SMART => not_read.smart_albums += 1,
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
                })
            }
            _ => {}
        }
    }
    drop(q);
    // Groups that only held smart collections are left out.
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

    Ok(Library {
        name: catalog
            .file_stem()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        photos,
        albums,
        not_read,
    })
}

/// Every keyword's path from the top ("Places|Italy|Rome"); Lightroom's
/// root keyword has no name and is left out.
fn keyword_paths(conn: &rusqlite::Connection) -> Result<HashMap<i64, String>> {
    let mut q = conn.prepare("SELECT id_local, name, parent FROM AgLibraryKeyword")?;
    let nodes: HashMap<i64, (Option<String>, Option<i64>)> = q
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                (r.get::<_, Option<String>>(1)?, r.get::<_, Option<i64>>(2)?),
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut paths = HashMap::new();
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
    }
    Ok(paths)
}
