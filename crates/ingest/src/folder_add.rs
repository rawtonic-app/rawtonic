//! A catalog's Folders › "Add Folder…" (Lightroom's Add for a folder): what
//! of a folder the catalog would register where it is. Planning only reads;
//! the registration is `batch::add_batch_with_metadata`, the same path as
//! the import dialog's "Add where they are" (pairs, sidecar XMP, duplicate
//! skip, one import batch, background SHA-256 pass), so there is one way a
//! photograph enters a catalog in place.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rawtonic_catalog::{Library, Mode};
use serde::Serialize;

use crate::batch::{COMPANION_EXTENSIONS, expand_sources_checked, pair_sources};

/// Files of the folder another library owns (they lie inside its folder: a
/// session keeps its own photographs), by that library.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnedFiles {
    pub library: PathBuf,
    /// The library's name as the user knows it.
    pub name: String,
    pub count: usize,
}

/// What adding a folder would do.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderPlan {
    /// The files to register, in name order (at most the limit).
    pub files: Vec<PathBuf>,
    /// New photographs among `files` (a RAW+JPEG pair counts once).
    pub photographs: usize,
    /// JPEGs (or HEIFs) that join a RAW the catalog holds already, beside
    /// them (the RAW is in `files` for the pairing, and skipped there).
    pub companions: usize,
    /// Files the catalog holds already, at their place or as a copy of the
    /// same content elsewhere (photographs or companions): skipped.
    pub registered: usize,
    /// Folders that could not be read (in whole or in part): their
    /// photographs were left out.
    pub unreadable: Vec<PathBuf>,
    /// Files refused because another library owns them.
    pub owned: Vec<OwnedFiles>,
    /// More files were there than `limit`: adding again takes the rest.
    pub truncated: bool,
}

/// Plans adding `folder` (with its subfolders when `subfolders`) to the
/// catalog `library` where its files are: every file `accept` takes (the
/// app's supported formats), hidden files and folders and symlinked folders
/// left out, without those the catalog holds already, those inside another
/// library's folder (refused, by owner) and those inside this library's own
/// folder (it lists them itself). Pairs count as `pair_raw` pairs them.
/// Reads only.
pub fn plan_folder_add(
    library: &Library,
    folder: &Path,
    subfolders: bool,
    accept: impl Fn(&Path) -> bool,
    pair_raw: Option<fn(&Path) -> bool>,
    limit: usize,
) -> Result<FolderPlan> {
    anyhow::ensure!(
        library.manifest.mode == Mode::Catalog,
        "A session keeps its photographs inside it: copy them into the session"
    );
    let folder = folder
        .canonicalize()
        .with_context(|| format!("{} is not there", folder.display()))?;
    anyhow::ensure!(folder.is_dir(), "{} is not a folder", folder.display());
    let own = library
        .directory
        .canonicalize()
        .unwrap_or_else(|_| library.directory.clone());
    let companions = library.companions_in(Some(&folder))?;
    let registered = RefCell::new(0usize);
    let owned = RefCell::new(BTreeMap::<PathBuf, (String, usize)>::new());
    // Which library's folder holds a folder, looked up once per folder.
    let owners = RefCell::new(HashMap::<PathBuf, Option<PathBuf>>::new());
    let failure = RefCell::new(None::<anyhow::Error>);
    // Content the catalog holds at another place: such a file would be
    // skipped as a duplicate (`add_batch` compares SHA-256), so it counts as
    // held and does not take a place in the limit (adding the folder again
    // reaches the files after it).
    let mut by_content: HashMap<(u64, String), Vec<PathBuf>> = HashMap::new();
    for asset in library.assets(None)? {
        by_content
            .entry((asset.size, asset.fingerprint))
            .or_default()
            .push(asset.path);
    }
    let sizes: std::collections::HashSet<u64> = by_content.keys().map(|(size, _)| *size).collect();
    let hashes = RefCell::new(HashMap::<PathBuf, Option<String>>::new());
    let held = |path: &Path| -> bool {
        let Ok(size) = fs::metadata(path).map(|m| m.len()) else {
            return false;
        };
        if !sizes.contains(&size) {
            return false;
        }
        let Some(paths) = rawtonic_catalog::fingerprint(path)
            .ok()
            .and_then(|fp| by_content.get(&(size, fp)))
        else {
            return false;
        };
        let Ok(hash) = crate::sha256(path) else {
            return false;
        };
        paths.iter().any(|known| {
            hashes
                .borrow_mut()
                .entry(known.clone())
                .or_insert_with(|| known.is_file().then(|| crate::sha256(known).ok()).flatten())
                .as_deref()
                == Some(hash.as_str())
        })
    };
    let take = |path: &Path| -> bool {
        if failure.borrow().is_some() || !accept(path) {
            return false;
        }
        let parent = path.parent().map(Path::to_path_buf).unwrap_or_default();
        let owner = owners
            .borrow_mut()
            .entry(parent)
            .or_insert_with(|| rawtonic_catalog::session_for(path))
            .clone();
        if let Some(owner) = owner {
            if owner != own && owner != library.directory {
                let name = rawtonic_catalog::library_name(&owner);
                owned.borrow_mut().entry(owner).or_insert((name, 0)).1 += 1;
            }
            return false;
        }
        let known = companions.contains_key(path)
            || match library.asset_at(path) {
                Ok(asset) => asset.is_some(),
                Err(e) => {
                    *failure.borrow_mut() = Some(e);
                    return false;
                }
            }
            || held(path)
            // The content of a photograph's companion: part of it.
            || crate::held_companion(library, path).is_ok_and(|held| held.is_some());
        if known {
            *registered.borrow_mut() += 1;
        }
        !known
    };
    let (mut files, truncated, unreadable) = if subfolders {
        expand_sources_checked(std::slice::from_ref(&folder), take, limit)
    } else {
        let mut files: Vec<PathBuf> = fs::read_dir(&folder)
            .with_context(|| format!("{} cannot be read", folder.display()))?
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_file())
            .collect();
        files.sort();
        expand_sources_checked(&files, take, limit)
    };
    if let Some(e) = failure.into_inner() {
        return Err(e);
    }
    anyhow::ensure!(
        !unreadable.contains(&folder),
        "{} cannot be read",
        folder.display()
    );
    // A JPEG beside a RAW the catalog holds: the RAW comes along so the two
    // pair up as `add_batch` pairs them (ADR 0013), not as a photograph of
    // its own.
    let context = registered_partners(library, &files, &accept, pair_raw)?;
    files.extend(context.iter().cloned());
    files.sort();
    let groups = pair_sources(&files, pair_raw);
    let companions = groups
        .iter()
        .filter(|g| context.contains(&g.photo) && g.companion.is_some())
        .count();
    let photographs = groups.len() - context.len();
    Ok(FolderPlan {
        photographs,
        companions,
        unreadable,
        files,
        registered: registered.into_inner(),
        owned: owned
            .into_inner()
            .into_iter()
            .map(|(library, (name, count))| OwnedFiles {
                library,
                name,
                count,
            })
            .collect(),
        truncated,
    })
}

/// The registered RAWs that `files`' JPEGs (or HEIFs) belong beside: the
/// only other photograph file (`accept`; sidecars such as `NAME.xmp` do not
/// count) of the JPEG's name (letter case aside) in its folder is a RAW the
/// catalog holds at that place without a companion.
fn registered_partners(
    library: &Library,
    files: &[PathBuf],
    accept: &dyn Fn(&Path) -> bool,
    pair_raw: Option<fn(&Path) -> bool>,
) -> Result<Vec<PathBuf>> {
    let Some(is_raw) = pair_raw else {
        return Ok(Vec::new());
    };
    let stem = |path: &Path| {
        path.file_stem()
            .map(|s| s.to_string_lossy().to_ascii_lowercase())
    };
    let mut listings: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
    let mut partners = Vec::new();
    for file in files {
        let companion = file.extension().and_then(|e| e.to_str()).is_some_and(|e| {
            COMPANION_EXTENSIONS
                .iter()
                .any(|c| c.eq_ignore_ascii_case(e))
        });
        let Some(folder) = file.parent().filter(|_| companion) else {
            continue;
        };
        let listing = listings.entry(folder.to_path_buf()).or_insert_with(|| {
            fs::read_dir(folder)
                .map(|entries| {
                    entries
                        .flatten()
                        .map(|e| e.path())
                        .filter(|p| p.is_file())
                        .collect()
                })
                .unwrap_or_default()
        });
        let same: Vec<&PathBuf> = listing
            .iter()
            .filter(|p| *p != file && stem(p) == stem(file) && accept(p))
            .collect();
        let [raw] = same.as_slice() else {
            continue;
        };
        if !is_raw(raw) || files.contains(raw) || partners.contains(*raw) {
            continue;
        }
        if let Some(asset) = library.asset_at(raw)?
            && library.companion_record(&asset)?.is_none()
        {
            partners.push((*raw).clone());
        }
    }
    Ok(partners)
}
