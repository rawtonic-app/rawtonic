//! Export as Catalog (G39): some photographs of a session or catalog as a
//! new, independent catalog — to take a job to another computer and bring
//! the work back with Import from Catalog.
//!
//! The catalog is built in a hidden folder beside the destination and only
//! renamed into place when complete, so a cancel or a failure leaves no
//! half catalog. The source library is never written: its database is read
//! through a snapshot (`Library::subset_from`), its originals only read.
//!
//! With `include_originals` each original is copied (verified, no clobber)
//! into the catalog's `Photos/<its folder's name>/` with its XMP and
//! companion JPEG, and stored relative to the catalog, so the folder can
//! move. Without, the catalog refers to the originals where they are; on
//! another computer they show as offline (on their previews) until their
//! drive is connected. Photographs inside a library's folder (the source's
//! own, a session's) belong to that library and are always copied. Edit resources
//! (LUTs, profiles, mask images) are always copied into the catalog's
//! `resources/`.
use crate::transfer::{ResourceCopier, check, copy_file, flush_staged, walk};
use anyhow::{Context, Result, ensure};
use rawtonic_catalog::{Library, portable::ResourceUse};
use serde::Serialize;
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

#[derive(Debug, Clone, Default)]
pub struct Options {
    /// The new catalog's name (shown in the app).
    pub name: String,
    pub include_originals: bool,
}

/// A photograph of the new catalog: where its original is in the source
/// library and where the new catalog has it (inside the staged catalog
/// folder when copied, else the same file).
#[derive(Debug, Clone)]
pub struct Exported {
    pub id: String,
    pub from: PathBuf,
    pub to: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
pub struct Progress {
    pub phase: &'static str,
    pub done: usize,
    pub total: usize,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Receipt {
    pub photographs: usize,
    pub originals_copied: usize,
    /// Photographs whose original was not there (by reference only: they
    /// are in the catalog, marked missing).
    pub offline: usize,
    /// What the `stage` step reported (the app: previews copied).
    pub previews: usize,
    pub missing_resources: usize,
    pub missing_current_resources: usize,
}

/// Checks the destination: a new folder (or an empty existing one) whose
/// name is valid, outside the source library. Returns (parent, folder).
pub fn check_destination(source: &Path, destination: &Path) -> Result<(PathBuf, PathBuf)> {
    let source = source.canonicalize()?;
    let parent = destination
        .parent()
        .context("Choose a destination folder")?
        .canonicalize()
        .context("The destination folder is not there")?;
    let name = destination.file_name().context("Choose a folder name")?;
    rawtonic_naming::validate_component(
        name.to_str().context("Folder name must be valid Unicode")?,
    )?;
    let destination = parent.join(name);
    if destination.exists() {
        ensure!(
            destination.is_dir()
                && !fs::symlink_metadata(&destination)?.file_type().is_symlink()
                && fs::read_dir(&destination)?.next().is_none(),
            "{} already exists and is not an empty folder; choose a new name",
            destination.display()
        );
    }
    ensure!(
        !parent.starts_with(&source) && !source.starts_with(&destination),
        "Choose a destination outside the library you export from"
    );
    ensure!(
        rawtonic_catalog::session_for(&destination.join("_")).is_none(),
        "Choose a destination outside any Rawtonic session or catalog"
    );
    Ok((parent, destination))
}

/// Exports the photographs `ids` of the library at `source` as a new
/// catalog at `destination`. `stage` runs on the complete staged catalog
/// before it is published (the app copies previews there) and returns a
/// count for the receipt.
pub fn export_catalog(
    source: &Path,
    ids: &[String],
    destination: &Path,
    options: &Options,
    cancel: &AtomicBool,
    mut progress: impl FnMut(Progress),
    stage: impl FnOnce(&Path, &[Exported], &AtomicBool) -> Result<usize>,
) -> Result<Receipt> {
    let (parent, destination) = check_destination(source, destination)?;
    let source = source.canonicalize()?;
    let unique: HashSet<&String> = ids.iter().collect();
    ensure!(!unique.is_empty(), "Choose at least one photograph");
    check(cancel)?;
    let staged = tempfile::Builder::new()
        .prefix(".rawtonic-export-")
        .tempdir_in(&parent)?;
    let stage_root = staged.path().canonicalize()?;
    let catalog = Library::subset_from(&source, &stage_root, &options.name, ids)?;
    catalog.check_integrity()?;
    let assets = catalog.assets(None)?;
    // Where each original is: a photograph inside the source library is at
    // its place there (the snapshot sees it relative to the stage).
    let originals: Vec<PathBuf> = assets
        .iter()
        .map(|asset| match asset.path.strip_prefix(&stage_root) {
            Ok(relative) => source.join(relative),
            Err(_) => asset.path.clone(),
        })
        .collect();
    let mut receipt = Receipt {
        photographs: assets.len(),
        ..Receipt::default()
    };
    let mut exported = Vec::with_capacity(assets.len());
    // A photograph inside a library's folder belongs to that library (the
    // source's own copies, a session's photographs, also those a catalog
    // added where they are): a catalog referring to it would edit it in
    // that library's name. It is always copied; only photographs outside
    // any library are referred to where they are.
    let copied: Vec<bool> = originals
        .iter()
        .map(|path| options.include_originals || rawtonic_catalog::session_for(path).is_some())
        .collect();
    let missing: Vec<String> = originals
        .iter()
        .zip(&copied)
        .filter(|(path, copy)| **copy && !path.is_file())
        .map(|(path, _)| path.display().to_string())
        .collect();
    ensure!(
        missing.is_empty(),
        "Originals not available ({}); connect their drive or export without originals:\n{}",
        missing.len(),
        missing.join("\n")
    );
    let to_copy: Vec<PathBuf> = originals
        .iter()
        .zip(&copied)
        .filter(|(_, copy)| **copy)
        .map(|(path, _)| path.clone())
        .collect();
    let folders = folder_names(&to_copy);
    let mut places = Vec::new();
    let mut referenced = Vec::new();
    for ((asset, from), copy) in assets.iter().zip(&originals).zip(&copied) {
        check(cancel)?;
        if !copy {
            if !from.is_file() {
                receipt.offline += 1;
            }
            referenced.push((asset.id.clone(), from.clone()));
            exported.push(Exported {
                id: asset.id.clone(),
                from: from.clone(),
                to: from.clone(),
            });
            continue;
        }
        let folder = &folders[from.parent().context("Original has no folder")?];
        let relative = PathBuf::from("Photos")
            .join(folder)
            .join(from.file_name().context("Original has no filename")?);
        rawtonic_catalog::portable_relative(&relative)?;
        let to = stage_root.join(&relative);
        ensure!(
            !to.exists(),
            "Two photographs would share {}",
            relative.display()
        );
        copy_file(from, &to, cancel)?;
        ensure!(
            fs::metadata(&to)?.len() == asset.size
                && rawtonic_catalog::fingerprint(&to)? == asset.fingerprint,
            "Original has changed since it was registered: {}",
            from.display()
        );
        // Its XMP and its companion JPEG come along beside it.
        let mut sidecars = Vec::new();
        let xmp = from.with_extension("xmp");
        if xmp.try_exists()? {
            sidecars.push(xmp);
        }
        if let Some(record) = catalog.companion_record(asset)?
            && let Some(name) = record.path.file_name()
        {
            let companion = from.with_file_name(name);
            if record.matches(&companion) {
                sidecars.push(companion);
            }
        }
        for file in sidecars {
            ensure!(
                fs::symlink_metadata(&file)?.file_type().is_file(),
                "Sidecar is not a regular file: {}",
                file.display()
            );
            let beside = to.with_file_name(file.file_name().context("Sidecar has no name")?);
            if !beside.exists() {
                copy_file(&file, &beside, cancel)?;
            }
        }
        places.push((asset.id.clone(), relative));
        exported.push(Exported {
            id: asset.id.clone(),
            from: from.clone(),
            to,
        });
        receipt.originals_copied += 1;
        progress(Progress {
            phase: "originals",
            done: receipt.originals_copied,
            total: to_copy.len(),
        });
    }
    copy_resources(&catalog, &stage_root, &source, cancel, &mut receipt)?;
    if !referenced.is_empty() {
        catalog.reference_assets(&referenced)?;
    }
    if !places.is_empty() {
        catalog.internalize_assets(&places)?;
        catalog.write_portable_sidecars()?;
    }
    for folder in [
        &catalog.manifest.capture,
        &catalog.manifest.output,
        "masks",
        "Trash",
    ] {
        fs::create_dir_all(stage_root.join(folder))?;
    }
    check(cancel)?;
    receipt.previews = stage(&stage_root, &exported, cancel)?;
    progress(Progress {
        phase: "finishing",
        done: assets.len(),
        total: assets.len(),
    });
    catalog.check_integrity()?;
    catalog.compact()?;
    catalog.checkpoint()?;
    drop(catalog);
    let mut all = Vec::new();
    walk(&stage_root, &mut all, false)?;
    for path in all {
        check(cancel)?;
        flush_staged(path)?;
    }
    check(cancel)?;
    if destination.exists() {
        // Checked empty above; `remove_dir` removes nothing else.
        fs::remove_dir(&destination)?;
    }
    rawtonic_fileops::rename_no_clobber(&stage_root, &destination)?;
    // Published: the temporary folder's name is gone, nothing to clean up.
    let _ = staged.keep();
    Ok(receipt)
}

/// Copies the edit resources the catalog's recipes name into it.
fn copy_resources(
    catalog: &Library,
    stage_root: &Path,
    source: &Path,
    cancel: &AtomicBool,
    receipt: &mut Receipt,
) -> Result<()> {
    let mut copier = ResourceCopier::new(stage_root, source, cancel);
    catalog.rewrite_resources(|value, used: ResourceUse<'_>| copier.rewrite(value, used))?;
    receipt.missing_resources = copier.missing.len();
    receipt.missing_current_resources = copier.missing_current();
    Ok(())
}

/// A folder name under `Photos/` for each folder originals come from: its
/// own name, numbered when two different folders share one ("Day 1",
/// "Day 1 (2)").
fn folder_names(originals: &[PathBuf]) -> BTreeMap<PathBuf, String> {
    let mut names = BTreeMap::new();
    let mut taken = HashSet::new();
    let parents: std::collections::BTreeSet<&Path> =
        originals.iter().filter_map(|path| path.parent()).collect();
    for parent in parents {
        let base = parent
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "Photos".into());
        let mut name = base.clone();
        let mut n = 2;
        while !taken.insert(name.to_lowercase()) {
            name = format!("{base} ({n})");
            n += 1;
        }
        names.insert(parent.to_path_buf(), name);
    }
    names
}
