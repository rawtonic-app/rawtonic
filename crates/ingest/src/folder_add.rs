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

use crate::batch::{expand_sources, pair_sources};

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
    /// Photographs among `files` (a RAW+JPEG pair counts once).
    pub photographs: usize,
    /// Files the catalog holds already (photographs or companions): skipped.
    pub registered: usize,
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
            };
        if known {
            *registered.borrow_mut() += 1;
        }
        !known
    };
    let (files, truncated) = if subfolders {
        expand_sources(std::slice::from_ref(&folder), take, limit)
    } else {
        let mut files: Vec<PathBuf> = fs::read_dir(&folder)
            .with_context(|| format!("{} cannot be read", folder.display()))?
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_file())
            .collect();
        files.sort();
        expand_sources(&files, take, limit)
    };
    if let Some(e) = failure.into_inner() {
        return Err(e);
    }
    let photographs = pair_sources(&files, pair_raw).len();
    Ok(FolderPlan {
        photographs,
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
