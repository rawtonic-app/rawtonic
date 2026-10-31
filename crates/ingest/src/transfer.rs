//! Full, verified library copies for transfer, backup and restore. A staging
//! directory is published only after originals, recipes and dependencies pass.
//! A missing original stops the copy; an edit resource (LUT, profile, mask
//! image) that is already missing does not, wherever it is referenced: it is
//! counted in the receipt and its reference kept (`Receipt::missing_resources`).
use anyhow::{Context, Result, ensure};
use rawtonic_catalog::{Library, portable::ResourceUse};
use rawtonic_metadata::resources;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

const RECEIPT: &str = "rawtonic-copy.json";
#[derive(Debug, Serialize, Deserialize)]
pub struct Receipt {
    pub version: u32,
    pub created_at: String,
    pub purpose: String,
    pub photographs: usize,
    /// Relative filenames and full SHA-256 digests. No machine-specific paths.
    pub files: BTreeMap<String, String>,
    /// Edit resource files (LUTs, camera profiles, mask images) that edits
    /// refer to but that were already missing when the copy was made. They
    /// cannot be in the copy; their references are kept unchanged and the
    /// copy is complete for everything else. A missing original still stops
    /// the copy.
    #[serde(default)]
    pub missing_resources: usize,
    /// How many of `missing_resources` what photographs show now uses (a
    /// current recipe, a version's state, a mask); the rest only undo
    /// history, presets, styles or export recipes refer to.
    #[serde(default)]
    pub missing_current_resources: usize,
}
#[derive(Debug, Clone, Serialize)]
pub struct Progress {
    pub phase: &'static str,
    pub files: usize,
    pub path: String,
}

pub(crate) fn check(cancel: &AtomicBool) -> Result<()> {
    ensure!(!cancel.load(Ordering::Acquire), "Copy cancelled");
    Ok(())
}
pub(crate) fn digest(path: &Path, cancel: &AtomicBool) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        check(cancel)?;
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(hash.finalize().iter().map(|b| format!("{b:02x}")).collect())
}
pub(crate) fn walk(root: &Path, out: &mut Vec<PathBuf>, skip_cache: bool) -> Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if skip_cache && matches!(name.as_ref(), ".cache" | "previews" | "smart-previews") {
            continue;
        }
        let kind = entry.file_type()?;
        ensure!(
            !kind.is_symlink(),
            "Cannot make a self-contained copy of symbolic link: {}",
            path.display()
        );
        if kind.is_dir() {
            walk(&path, out, skip_cache)?;
        } else {
            ensure!(kind.is_file(), "Not a regular file: {}", path.display());
            out.push(path);
        }
    }
    Ok(())
}
fn excluded(relative: &Path) -> bool {
    relative.components().count() == 1
        && matches!(
            relative.to_str(),
            Some(
                "session.json"
                    | "catalog.json"
                    | "session.sqlite"
                    | "catalog.sqlite"
                    | "session.sqlite-wal"
                    | "session.sqlite-shm"
                    | "catalog.sqlite-wal"
                    | "catalog.sqlite-shm"
                    | RECEIPT
            )
        )
}
pub(crate) fn copy_file(source: &Path, to: &Path, cancel: &AtomicBool) -> Result<()> {
    check(cancel)?;
    fs::create_dir_all(to.parent().context("Missing destination parent")?)?;
    let metadata = fs::metadata(source)?;
    let modified = metadata.modified()?;
    crate::verified_copy(source, to, cancel, |_| {})
        .with_context(|| format!("Cannot copy {}", source.display()))?;
    fs::File::options()
        .write(true)
        .open(to)?
        .set_times(fs::FileTimes::new().set_modified(modified))?;
    // Verified copying creates a new file rather than using CopyFileW;
    // carry the original's read-only attribute to our staged copy explicitly.
    #[cfg(windows)]
    fs::set_permissions(to, metadata.permissions())?;
    Ok(())
}

/// Verify before opening/migrating the backup, so SQLite cannot change a file
/// before it has been checked. Refuse incomplete and modified snapshots.
pub fn verify(
    folder: &Path,
    cancel: &AtomicBool,
    mut progress: impl FnMut(Progress),
) -> Result<Receipt> {
    let receipt: Receipt = serde_json::from_slice(
        &fs::read(folder.join(RECEIPT)).context("Not a verified Rawtonic backup")?,
    )?;
    ensure!(
        receipt.version == 1 && !receipt.files.is_empty(),
        "Unsupported or empty backup manifest"
    );
    let mut actual = Vec::new();
    walk(folder, &mut actual, false)?;
    for path in actual {
        let relative = path
            .strip_prefix(folder)?
            .to_string_lossy()
            .replace('\\', "/");
        ensure!(
            relative == RECEIPT
                || path.file_name().is_some_and(|n| n == ".DS_Store")
                || receipt.files.contains_key(&relative),
            "Backup contains an unverified file: {relative}"
        );
    }
    for (index, (relative, expected)) in receipt.files.iter().enumerate() {
        check(cancel)?;
        rawtonic_catalog::validate_relative(Path::new(relative))?;
        let path = folder.join(relative);
        ensure!(
            path.canonicalize()?.starts_with(folder.canonicalize()?),
            "Backup file escapes its folder"
        );
        ensure!(
            digest(&path, cancel)? == *expected,
            "Backup verification failed: {relative}"
        );
        progress(Progress {
            phase: "verify",
            files: index + 1,
            path: relative.clone(),
        });
    }
    for needed in if folder.join("catalog.json").exists() {
        ["catalog.json", "catalog.sqlite"]
    } else {
        ["session.json", "session.sqlite"]
    } {
        ensure!(
            receipt.files.contains_key(needed),
            "Backup is missing {needed}"
        );
    }
    Ok(receipt)
}

/// `destination` must be a new sibling/outside folder, not an existing library.
/// Older backups are independent snapshots and are never overwritten.
pub fn copy_library(
    source: &Path,
    destination: &Path,
    purpose: &str,
    cancel: &AtomicBool,
    mut progress: impl FnMut(Progress),
) -> Result<Receipt> {
    ensure!(
        matches!(purpose, "transfer" | "backup" | "restore"),
        "Unknown copy purpose"
    );
    let source = source.canonicalize()?;
    let parent = destination
        .parent()
        .context("Choose a destination parent")?
        .canonicalize()?;
    let name = destination.file_name().context("Choose a folder name")?;
    rawtonic_naming::validate_component(
        name.to_str().context("Folder name must be valid Unicode")?,
    )?;
    let destination = parent.join(name);
    ensure!(
        !destination.exists(),
        "Destination already exists: {}",
        destination.display()
    );
    ensure!(
        !parent.starts_with(&source) && !source.starts_with(&destination),
        "Choose a destination outside the source library"
    );
    let restore = if purpose == "restore" {
        Some(verify(&source, cancel, &mut progress)?)
    } else {
        None
    };
    check(cancel)?;
    let stage = tempfile::Builder::new()
        .prefix(".rawtonic-copy-")
        .tempdir_in(&parent)?;
    let stage_root = stage.path().canonicalize()?;
    let snapshot = if let Some(receipt) = &restore {
        // Never open the backup itself in SQLite: migration/checkpointing would
        // invalidate it. Copy only verified files, and check the copied bytes
        // again before opening the destination (the source could have changed).
        for (index, (relative, expected)) in receipt.files.iter().enumerate() {
            let to = stage_root.join(relative);
            copy_file(&source.join(relative), &to, cancel)?;
            ensure!(
                digest(&to, cancel)? == *expected,
                "Backup changed during restoration: {relative}"
            );
            progress(Progress {
                phase: "copy",
                files: index + 1,
                path: relative.clone(),
            });
        }
        Library::open(&stage_root)?
    } else {
        Library::snapshot_from(&source, &stage_root)?
    };
    snapshot.check_integrity()?;
    let assets = snapshot.assets(None)?;
    let mut places = Vec::new();
    let mut originals = Vec::new();
    let mut missing = Vec::new();
    for asset in &assets {
        let (from, relative) = match asset.path.strip_prefix(&stage_root) {
            Ok(relative) => (source.join(relative), relative.to_path_buf()),
            Err(_) => (
                asset.path.clone(),
                PathBuf::from("Photos/Imported")
                    .join(&asset.id)
                    .join(asset.path.file_name().context("Original has no filename")?),
            ),
        };
        // `relative` is native (a `strip_prefix` of the staged library, or a
        // `Path::join`): validate it as the perhaps non-portable path it is,
        // not as the stored string. Stored fields are still checked by
        // `validate_relative` where they are read.
        rawtonic_catalog::portable_relative(&relative)?;
        if !from.is_file() {
            missing.push(from.display().to_string());
        }
        originals.push((from, relative.clone()));
        places.push((asset.id.clone(), relative));
    }
    ensure!(
        missing.is_empty(),
        "Originals unavailable ({}):\n{}",
        missing.len(),
        missing.join("\n")
    );
    let mut files = Vec::new();
    if restore.is_none() {
        walk(&source, &mut files, true)?;
    }
    let mut copied = 0;
    for from in files {
        let relative = from.strip_prefix(&source)?;
        if excluded(relative) {
            continue;
        }
        copy_file(&from, &stage_root.join(relative), cancel)?;
        copied += 1;
        progress(Progress {
            phase: "copy",
            files: copied,
            path: relative.to_string_lossy().into_owned(),
        });
    }
    // Scan each external folder once, even for large catalogs. Sidecars are
    // grouped by their original filename; legacy virtual copies stay included.
    let mut external_sidecars = BTreeMap::<PathBuf, Vec<PathBuf>>::new();
    let mut scanned = std::collections::BTreeSet::new();
    for (from, _) in &originals {
        if from.starts_with(&source) {
            continue;
        }
        let parent = from.parent().context("Original has no parent")?;
        if !scanned.insert(parent.to_path_buf()) {
            continue;
        }
        for entry in fs::read_dir(parent)? {
            check(cancel)?;
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let original = if let Some(base) = name.strip_suffix(".rrdata") {
                base.rsplit_once('.')
                    .filter(|(_, id)| rawtonic_catalog::valid_copy_id(id))
                    .map(|(base, _)| base)
                    .unwrap_or(base)
            } else if let Some(base) = name.strip_suffix(".rrexif") {
                base
            } else {
                continue;
            };
            external_sidecars
                .entry(parent.join(original))
                .or_default()
                .push(entry.path());
        }
    }
    for ((from, relative), asset) in originals.iter().zip(&assets) {
        let to = stage_root.join(relative);
        if !from.starts_with(&source) || !to.exists() {
            copy_file(from, &to, cancel)?;
        }
        ensure!(
            fs::metadata(&to)?.len() == asset.size
                && rawtonic_catalog::fingerprint(&to)? == asset.fingerprint,
            "Original has changed since registration: {}",
            from.display()
        );
        // External originals bring their XMP, EXIF cache, all legacy virtual
        // copies and their companion JPEG (recorded by name beside them, so
        // it stays theirs beside the copy). Registered virtual copies are
        // emitted from the snapshot below.
        if !from.starts_with(&source) {
            let mut sidecars = external_sidecars.remove(from).unwrap_or_default();
            let xmp = rawtonic_metadata::sidecar_path(from);
            if xmp.try_exists()? {
                sidecars.push(xmp);
            }
            if let Some(companion) = snapshot.companion_verified(asset)? {
                sidecars.push(companion);
            }
            for file in sidecars {
                ensure!(
                    fs::symlink_metadata(&file)?.file_type().is_file(),
                    "Sidecar is not a regular file: {}",
                    file.display()
                );
                copy_file(&file, &to.with_file_name(file.file_name().unwrap()), cancel)?;
            }
        }
        copied += 1;
        progress(Progress {
            phase: "copy",
            files: copied,
            path: relative.to_string_lossy().into_owned(),
        });
    }
    let mut copier = ResourceCopier::new(&stage_root, &source, cancel);
    snapshot.rewrite_resources(|value, used: ResourceUse<'_>| copier.rewrite(value, used))?;
    let owners: BTreeMap<_, _> = originals
        .iter()
        .map(|(from, to)| (to.parent().unwrap(), from.as_path()))
        .collect();
    let mut sidecars = Vec::new();
    walk(&stage_root, &mut sidecars, true)?;
    for path in sidecars
        .iter()
        .filter(|p| p.extension().is_some_and(|e| e == "rrdata"))
    {
        let mut value: serde_json::Value = serde_json::from_slice(&fs::read(path)?)?;
        let relative = path.strip_prefix(&stage_root)?;
        let owner = relative
            .parent()
            .and_then(|parent| owners.get(parent))
            .copied();
        let mut error = None;
        // An editor sidecar is a photograph's (or legacy copy's) state now.
        let used = ResourceUse {
            owner,
            current: true,
        };
        resources::visit(
            &mut value,
            &mut |resource| match copier.rewrite_sidecar(resource, used) {
                Ok(next) => *resource = next,
                Err(e) => error = Some(e),
            },
        );
        if let Some(e) = error {
            return Err(e);
        }
        fs::write(path, serde_json::to_vec(&value)?)?;
    }
    snapshot.internalize_assets(&places)?;
    snapshot.write_portable_sidecars()?;
    for folder in [
        &snapshot.manifest.capture,
        &snapshot.manifest.output,
        "masks",
        "Trash",
    ] {
        fs::create_dir_all(stage_root.join(folder))?;
    }
    snapshot.check_integrity()?;
    snapshot.checkpoint()?;
    drop(snapshot);
    let mut all = Vec::new();
    walk(&stage_root, &mut all, false)?;
    let mut receipt = Receipt {
        version: 1,
        purpose: purpose.into(),
        created_at: chrono::Utc::now().to_rfc3339(),
        photographs: assets.len(),
        files: BTreeMap::new(),
        missing_resources: copier.missing.len(),
        missing_current_resources: copier.missing_current(),
    };
    for path in all {
        check(cancel)?;
        let relative = path
            .strip_prefix(&stage_root)?
            .to_string_lossy()
            .replace('\\', "/");
        receipt
            .files
            .insert(relative.clone(), digest(&path, cancel)?);
        progress(Progress {
            phase: "verify",
            files: receipt.files.len(),
            path: relative,
        });
    }
    fs::write(
        stage_root.join(RECEIPT),
        serde_json::to_vec_pretty(&receipt)?,
    )?;
    // Flush payloads and metadata before the completed directory appears.
    for relative in receipt
        .files
        .keys()
        .chain(std::iter::once(&RECEIPT.to_string()))
    {
        flush_staged(stage_root.join(relative))?;
    }
    check(cancel)?;
    rawtonic_fileops::rename_no_clobber(&stage_root, &destination)?;
    Ok(receipt)
}

/// Copies the edit resources (LUTs, camera profiles, mask images) a staged
/// library's recipes name into its `resources/<sha256>/` folder and rewrites
/// each reference to that copy. Resources already gone are counted
/// (`missing`, with whether what a photograph shows now uses them) and keep
/// their reference.
pub(crate) struct ResourceCopier<'a> {
    stage_root: &'a Path,
    /// The folder the library's database sees its own photographs in (the
    /// stage, unless a snapshot lies elsewhere): their files are `source`'s.
    view: &'a Path,
    source: &'a Path,
    cancel: &'a AtomicBool,
    copied: BTreeMap<PathBuf, String>,
    pub(crate) missing: BTreeMap<PathBuf, bool>,
}

impl<'a> ResourceCopier<'a> {
    pub(crate) fn new(stage_root: &'a Path, source: &'a Path, cancel: &'a AtomicBool) -> Self {
        Self {
            stage_root,
            view: stage_root,
            source,
            cancel,
            copied: BTreeMap::new(),
            missing: BTreeMap::new(),
        }
    }

    pub(crate) fn missing_current(&self) -> usize {
        self.missing.values().filter(|&&current| current).count()
    }

    /// A reference stored in the library's database: a relative one is the
    /// library's own (`resources::localize` made it relative to the library
    /// folder), wherever its photograph is.
    pub(crate) fn rewrite(&mut self, value: &str, used: ResourceUse<'_>) -> Result<String> {
        let root = self.source.to_path_buf();
        self.rewrite_from(value, used, root)
    }

    /// A reference in an editor sidecar beside a photograph: a relative one
    /// is the library's holding that photograph.
    pub(crate) fn rewrite_sidecar(&mut self, value: &str, used: ResourceUse<'_>) -> Result<String> {
        let (view, source) = (self.view, self.source);
        let owner = used.owner.map(|path| {
            path.strip_prefix(view)
                .map(|r| source.join(r))
                .unwrap_or_else(|_| path.to_path_buf())
        });
        let root = owner
            .as_deref()
            .and_then(rawtonic_catalog::session_for)
            .unwrap_or_else(|| source.to_path_buf());
        self.rewrite_from(value, used, root)
    }

    fn rewrite_from(
        &mut self,
        value: &str,
        used: ResourceUse<'_>,
        owner_root: PathBuf,
    ) -> Result<String> {
        let (stage_root, cancel) = (self.stage_root, self.cancel);
        check(cancel)?;
        if value.is_empty() {
            return Ok(value.into());
        }
        let path = if let Some(relative) = resources::relative(value) {
            owner_root.join(relative)
        } else if Path::new(value).is_absolute() {
            PathBuf::from(value)
        } else {
            rawtonic_catalog::validate_relative(Path::new(value))?;
            owner_root.join(value)
        };
        // Already gone (a LUT deleted after an old undo step or a preset
        // used it): there is nothing to copy, and stopping would leave the
        // user without any copy. The reference stays as it is (the file
        // may come back on this computer) and is counted.
        if !path
            .try_exists()
            .with_context(|| format!("Cannot read edit resource {}", path.display()))?
        {
            *self.missing.entry(path).or_default() |= used.current;
            return Ok(value.into());
        }
        let path = path
            .canonicalize()
            .with_context(|| format!("Cannot read edit resource {}", path.display()))?;
        if let Some(value) = self.copied.get(&path) {
            return Ok(value.clone());
        }
        let hash = digest(&path, cancel)?;
        let relative = PathBuf::from("resources")
            .join(&hash)
            .join(path.file_name().context("Resource has no filename")?);
        let to = stage_root.join(&relative);
        if !to.exists() {
            copy_file(&path, &to, cancel)?;
        }
        ensure!(
            digest(&to, cancel)? == hash,
            "Copied resource checksum mismatch: {}",
            to.display()
        );
        let value = format!(
            "{}{}",
            resources::PREFIX,
            relative.to_string_lossy().replace('\\', "/")
        );
        self.copied.insert(path, value.clone());
        Ok(value)
    }
}

/// Flushes a staged copy to its drive, opened for writing: Windows flushes
/// only a handle with write access (`FlushFileBuffers` answers access
/// denied to a read-only one, which `File::open` asks for). The staged
/// files are this function's own copies made a moment earlier (an original
/// is only ever read by `copy_file`), so a copy that came along read-only
/// is made writable for the flush, and always gets its permissions back,
/// whether the flush worked or not, before it can be published. Other
/// systems flush a read-only handle as it is.
#[cfg(windows)]
pub(crate) fn flush_staged(path: PathBuf) -> Result<()> {
    let kept = fs::metadata(&path)?.permissions();
    let mut writable = kept.clone();
    #[expect(
        clippy::permissions_set_readonly_false,
        reason = "Windows-only: clears our staged copy's read-only attribute, never Unix mode bits"
    )]
    writable.set_readonly(false);
    let widened = writable.readonly() != kept.readonly();
    if widened {
        // A copy of ours: its read-only attribute protects nothing here,
        // and it comes back below, also when the flush fails.
        fs::set_permissions(&path, writable)
            .with_context(|| format!("Cannot write the staged copy {}", path.display()))?;
    }
    let flushed = fs::File::options()
        .write(true)
        .open(&path)
        .and_then(|file| file.sync_all());
    if widened {
        fs::set_permissions(&path, kept)
            .with_context(|| format!("Cannot restore the permissions of {}", path.display()))?;
    }
    flushed.with_context(|| format!("Cannot flush {}", path.display()))
}

#[cfg(not(windows))]
pub(crate) fn flush_staged(path: PathBuf) -> Result<()> {
    fs::File::open(&path)
        .and_then(|file| file.sync_all())
        .with_context(|| format!("Cannot flush {}", path.display()))
}
