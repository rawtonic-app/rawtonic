//! Verified ingest: streaming reads, bounded memory and no-clobber publication.
pub mod batch;
pub mod hot_folder;
pub mod transfer;

use anyhow::{Context, Result, bail, ensure};
use rawtonic_catalog::Library;
use rawtonic_naming::{Context as NamingContext, render};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CollisionPolicy {
    Suffix,
    Skip,
    Error,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct CopyReceipt {
    pub destination: PathBuf,
    pub bytes: u64,
    pub sha256: String,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct ImportResult {
    pub source: PathBuf,
    pub destination: Option<PathBuf>,
    pub backup: Option<PathBuf>,
    pub asset_id: Option<String>,
    pub error: Option<String>,
    pub skipped: bool,
}

pub fn sha256(path: &Path) -> Result<String> {
    let mut f = fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        let n = f.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        h.update(&buffer[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Destination parent must already exist. Atomic persist_noclobber closes the
/// existence-check race; a concurrent writer can never have its file replaced.
pub fn verified_copy(
    source: &Path,
    destination: &Path,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64),
) -> Result<CopyReceipt> {
    ensure!(
        !destination.exists(),
        "Destination already exists: {}",
        destination.display()
    );
    let mut input = fs::File::open(source).context("Cannot read source")?;
    let before = input.metadata()?;
    ensure!(before.is_file(), "Source is not a file");
    let parent = destination.parent().context("Destination has no parent")?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    let mut buffer = vec![0; 1024 * 1024];
    let mut h = Sha256::new();
    let mut bytes = 0;
    loop {
        ensure!(!cancel.load(Ordering::Acquire), "Ingest cancelled");
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        temp.write_all(&buffer[..n])?;
        h.update(&buffer[..n]);
        bytes += n as u64;
        progress(bytes);
    }
    temp.as_file().sync_all()?;
    let hash: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    ensure!(
        sha256(temp.path())? == hash,
        "Destination SHA-256 verification failed"
    );
    let after = input.metadata()?;
    ensure!(
        after.len() == before.len()
            && bytes == before.len()
            && after.modified()? == before.modified()?,
        "Source changed during ingest"
    );
    // A second source pass catches mutation during copying even if timestamps were preserved.
    ensure!(
        sha256(source)? == hash,
        "Source changed during verification"
    );
    ensure!(!cancel.load(Ordering::Acquire), "Ingest cancelled");
    temp.persist_noclobber(destination).map_err(|e| e.error)?;
    #[cfg(unix)]
    fs::File::open(parent)?.sync_all()?;
    Ok(CopyReceipt {
        destination: destination.into(),
        bytes,
        sha256: hash,
    })
}

pub fn choose_destination(
    folder: &Path,
    name: &str,
    policy: CollisionPolicy,
) -> Result<Option<PathBuf>> {
    rawtonic_naming::validate_component(name)?;
    let target = folder.join(name);
    if !target.exists() {
        return Ok(Some(target));
    }
    match policy {
        CollisionPolicy::Skip => Ok(None),
        CollisionPolicy::Error => bail!("Destination exists: {}", target.display()),
        CollisionPolicy::Suffix => {
            for i in 1..=MAX_COLLISIONS {
                let candidate = folder.join(collision_name(name, i)?);
                if !candidate.exists() {
                    return Ok(Some(candidate));
                }
            }
            bail!("Too many filename collisions")
        }
    }
}

const MAX_COLLISIONS: u32 = 100_000;

/// `name` with the `i`-th collision suffix (`DSC_0001_2.ARW`).
fn collision_name(name: &str, i: u32) -> Result<String> {
    let p = Path::new(name);
    let base = p
        .file_stem()
        .and_then(|s| s.to_str())
        .context("Invalid stem")?;
    let name = match p.extension().and_then(|s| s.to_str()) {
        Some(ext) => format!("{base}_{i}.{ext}"),
        None => format!("{base}_{i}"),
    };
    rawtonic_naming::validate_component(&name)?;
    Ok(name)
}

/// A file with the bytes of `source` at one of the names
/// `choose_destination(folder, name, Suffix)` goes through before a free
/// one (`name`, `name_1`, …), that `wanted` accepts. Symlinks never count.
fn identical_file(
    folder: &Path,
    name: &str,
    source: &Path,
    wanted: impl Fn(&Path) -> Result<bool>,
) -> Result<Option<PathBuf>> {
    let len = fs::metadata(source)?.len();
    let mut source_hash = None;
    for i in 0..=MAX_COLLISIONS {
        let candidate = if i == 0 {
            folder.join(name)
        } else {
            folder.join(collision_name(name, i)?)
        };
        if !candidate.exists() {
            break;
        }
        let Ok(meta) = fs::symlink_metadata(&candidate) else {
            continue;
        };
        if !meta.is_file() || meta.len() != len || !wanted(&candidate)? {
            continue;
        }
        let hash = match &source_hash {
            Some(hash) => hash,
            None => source_hash.insert(sha256(source)?),
        };
        if sha256(&candidate)? == *hash {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

pub fn import_one(
    library: &Library,
    source: &Path,
    template: &str,
    sequence: u64,
    backup: Option<&Path>,
    cancel: &AtomicBool,
) -> Result<ImportResult> {
    import_one_with_start(library, source, template, sequence, 1, backup, cancel)
}

/// Import using an explicit counter and its initial value for `{seq_start}`.
pub fn import_one_with_start(
    library: &Library,
    source: &Path,
    template: &str,
    sequence: u64,
    sequence_start: u64,
    backup: Option<&Path>,
    cancel: &AtomicBool,
) -> Result<ImportResult> {
    let destination = destination_folder(library, None, &[])?;
    import_one_into(
        library,
        source,
        &destination,
        template,
        sequence,
        sequence_start,
        backup,
        cancel,
    )
}

/// Folders of a session or catalog that hold no photographs of their own:
/// its Trash and the app's mask bitmaps and previews.
const RESERVED_FOLDERS: &[&str] = &[
    "Trash",
    "masks",
    "resources",
    "profiles",
    "previews",
    "smart-previews",
];

/// A folder an import may copy into, as `destination_folder` checked it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Destination {
    path: PathBuf,
    sort_by_date: bool,
}

impl Destination {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn hidden(component: std::path::Component<'_>) -> bool {
    component.as_os_str().to_string_lossy().starts_with('.')
}

/// The folder an import copies into: the library's capture folder, or
/// `folder`, a folder the user picked in the folder list (a drop onto it,
/// "Import into this folder…"). Allowed are folders inside the library
/// directory (not the directory itself, its Trash, the app's own folders or
/// hidden ones) and, for a catalog, folders its folder list shows outside
/// it: at or below one of its folders (`Library::folders`) or one of `roots`
/// (the library's top-level folders of the app's folder list), never in a
/// hidden folder. Symlinks are resolved before the check. Copies into such
/// a folder are verified and never replace a file there (`verified_copy`).
pub fn destination_folder(
    library: &Library,
    folder: Option<&Path>,
    roots: &[PathBuf],
) -> Result<Destination> {
    let sort_by_date = folder.is_none() && library.manifest.mode == rawtonic_catalog::Mode::Catalog;
    let folder = match folder {
        None => {
            let capture = library.directory.join(&library.manifest.capture);
            fs::create_dir_all(&capture)?;
            capture
        }
        Some(folder) => {
            ensure!(
                folder.is_dir(),
                "The destination folder does not exist: {}",
                folder.display()
            );
            folder.to_path_buf()
        }
    };
    let canonical = folder.canonicalize()?;
    if let Ok(relative) = canonical.strip_prefix(&library.directory) {
        let first = relative
            .components()
            .next()
            .map(|c| c.as_os_str().to_string_lossy());
        ensure!(
            first.is_some_and(|first| !RESERVED_FOLDERS.contains(&first.as_ref()))
                && !relative.components().any(hidden),
            "Photographs cannot be copied into {}",
            folder.display()
        );
        return Ok(Destination {
            path: folder,
            sort_by_date,
        });
    }
    ensure!(
        library.manifest.mode == rawtonic_catalog::Mode::Catalog,
        "Photographs can only be copied into a folder of this session"
    );
    let shown = library
        .folders()?
        .into_iter()
        .map(|record| record.path)
        .chain(roots.iter().cloned())
        .filter_map(|root| root.canonicalize().ok())
        .filter(|root| canonical.starts_with(root))
        .max_by_key(|root| root.components().count());
    let root = shown
        .with_context(|| format!("{} is not in this catalog's folder list", folder.display()))?;
    ensure!(
        !canonical
            .strip_prefix(&root)
            .map(|below| below.components().any(hidden))
            .unwrap_or(true)
            && !root
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with('.')),
        "Photographs cannot be copied into the hidden folder {}",
        folder.display()
    );
    Ok(Destination {
        path: folder,
        sort_by_date,
    })
}

/// `import_one_with_start` into `destination` (`destination_folder`).
#[allow(clippy::too_many_arguments)]
pub fn import_one_into(
    library: &Library,
    source: &Path,
    destination: &Destination,
    template: &str,
    sequence: u64,
    sequence_start: u64,
    backup: Option<&Path>,
    cancel: &AtomicBool,
) -> Result<ImportResult> {
    let mut context = NamingContext::new(
        source
            .file_name()
            .and_then(|s| s.to_str())
            .context("Invalid source filename")?,
        sequence,
    );
    context.sequence_start = sequence_start;
    context
        .values
        .insert("session".into(), library.manifest.name.clone());
    context
        .values
        .insert("job".into(), library.manifest.name.clone());
    // mtime is a documented fallback; capture EXIF extraction belongs in rawdecode.
    if let Ok(modified) = fs::metadata(source)?.modified() {
        context.captured = chrono_from_system(modified);
    }
    let name = render(template, &context)?;
    let mut folder = destination.path().to_path_buf();
    // Catalogs organize their managed originals by capture date. Explicit
    // folder drops retain the destination the user chose; old manifests keep
    // their existing capture folder. Projects retain the per-job layout.
    if destination.sort_by_date {
        let date = capture_date(source);
        folder = folder
            .join(date.format("%Y").to_string())
            .join(date.format("%Y-%m-%d").to_string());
        fs::create_dir_all(&folder)?;
    }
    ensure!(
        folder.is_dir(),
        "The destination folder is gone: {}",
        folder.display()
    );
    // Detect duplicates by sampled fingerprint then full SHA, never by sampled hash alone.
    let fp = rawtonic_catalog::fingerprint(source)?;
    for asset in library
        .assets(None)?
        .iter()
        .filter(|a| a.fingerprint == fp && a.path.is_file())
    {
        if sha256(&asset.path)? == sha256(source)? {
            return Ok(ImportResult {
                source: source.into(),
                destination: Some(asset.path.clone()),
                backup: None,
                asset_id: Some(asset.id.clone()),
                error: None,
                skipped: true,
            });
        }
    }
    // An earlier import of this file that copied and verified it but could
    // not record it (say the database was locked) left the copy in the
    // session under its name: record that copy instead of making another
    // (and keep the backup that import made).
    let earlier = identical_file(&folder, &name, source, |candidate| {
        Ok(library.asset_at(candidate)?.is_none())
    })?;
    let mut backup_result = None;
    if let Some(backup_folder) = backup {
        fs::create_dir_all(backup_folder)?;
        ensure!(
            backup_folder.canonicalize()? != folder.canonicalize()?,
            "Backup must be a different directory"
        );
        let kept = match &earlier {
            Some(_) => identical_file(backup_folder, &name, source, |_| Ok(true))?,
            None => None,
        };
        backup_result = Some(match kept {
            Some(kept) => kept,
            None => {
                let backup_path =
                    choose_destination(backup_folder, &name, CollisionPolicy::Suffix)?
                        .context("No backup destination")?;
                verified_copy(source, &backup_path, cancel, |_| {})?;
                backup_path
            }
        });
    }
    if let Some(earlier) = earlier {
        return Ok(register(library, source, earlier, backup_result, None));
    }
    let destination =
        choose_destination(&folder, &name, CollisionPolicy::Suffix)?.context("No destination")?;
    let receipt = match verified_copy(source, &destination, cancel, |_| {}) {
        Ok(receipt) => receipt,
        Err(error) => {
            return Ok(ImportResult {
                source: source.into(),
                destination: None,
                backup: backup_result,
                asset_id: None,
                error: Some(error.to_string()),
                skipped: false,
            });
        }
    };
    Ok(register(
        library,
        source,
        receipt.destination,
        backup_result,
        Some(&receipt.sha256),
    ))
}

/// Records `copy`, which an earlier `import_one` of `source` copied and
/// verified but could not record (its `destination` with an `error`), when
/// it is still a file with the bytes of `source` and no record. `None`
/// otherwise: then import `source` as usual. For callers that retry (the
/// hot folder) when a template's name changed meanwhile (`{seq}`).
pub fn record_earlier_copy(
    library: &Library,
    source: &Path,
    copy: &Path,
) -> Result<Option<ImportResult>> {
    let Ok(meta) = fs::symlink_metadata(copy) else {
        return Ok(None);
    };
    if !meta.is_file()
        || meta.len() != fs::metadata(source)?.len()
        || library.asset_at(copy)?.is_some()
        || sha256(copy)? != sha256(source)?
    {
        return Ok(None);
    }
    Ok(Some(register(
        library,
        source,
        copy.to_path_buf(),
        None,
        None,
    )))
}

/// Records the verified copy `destination` of `source` in the library,
/// with the content hash the copy was verified with (Synchronize Folder
/// proves moves with it).
fn register(
    library: &Library,
    source: &Path,
    destination: PathBuf,
    backup: Option<PathBuf>,
    sha256: Option<&str>,
) -> ImportResult {
    let (asset_id, error) = match library.add_asset(&destination) {
        Ok(asset) => {
            // Not recorded (the file changed meanwhile): the background
            // pass hashes it later; moves are only unverified until then.
            if let Some(hash) = sha256 {
                let _ = library.record_checksum(&asset.id, hash);
            }
            (Some(asset.id), None)
        }
        Err(error) => (
            None,
            Some(format!(
                "Verified file retained but library registration failed: {error}"
            )),
        ),
    };
    ImportResult {
        source: source.into(),
        destination: Some(destination),
        backup,
        asset_id,
        error,
        skipped: false,
    }
}

fn chrono_from_system(t: std::time::SystemTime) -> chrono::NaiveDateTime {
    chrono::DateTime::<chrono::Local>::from(t).naive_local()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn verifies_bytes_and_never_overwrites() {
        let d = tempfile::tempdir().unwrap();
        let source = d.path().join("source.arw");
        let dest = d.path().join("dest.arw");
        fs::write(&source, vec![42u8; 2_100_000]).unwrap();
        let cancel = AtomicBool::new(false);
        let r = verified_copy(&source, &dest, &cancel, |_| {}).unwrap();
        assert_eq!(r.bytes, 2_100_000);
        assert_eq!(sha256(&source).unwrap(), r.sha256);
        assert!(verified_copy(&source, &dest, &cancel, |_| {}).is_err());
        assert_eq!(fs::read(&source).unwrap(), fs::read(&dest).unwrap());
    }
    #[test]
    fn cancellation_never_publishes_partial_files() {
        let d = tempfile::tempdir().unwrap();
        let s = d.path().join("s");
        let target = d.path().join("t");
        fs::write(&s, vec![1; 2_000_000]).unwrap();
        let cancel = AtomicBool::new(false);
        assert!(
            verified_copy(&s, &target, &cancel, |_| cancel
                .store(true, Ordering::Release))
            .is_err()
        );
        assert!(!target.exists());
        assert_eq!(fs::read_dir(d.path()).unwrap().count(), 1);
    }
    #[test]
    fn concurrent_destination_is_preserved() {
        let d = tempfile::tempdir().unwrap();
        let s = d.path().join("s");
        let target = d.path().join("t");
        fs::write(&s, b"original").unwrap();
        assert!(
            verified_copy(&s, &target, &AtomicBool::new(false), |_| fs::write(
                &target,
                b"another writer"
            )
            .unwrap())
            .is_err()
        );
        assert_eq!(fs::read(&target).unwrap(), b"another writer");
    }
    #[test]
    fn dual_copy_skip_and_collision() {
        let d = tempfile::tempdir().unwrap();
        let source = d.path().join("DSC.ARW");
        fs::write(&source, b"original data").unwrap();
        let lib = Library::create(
            &d.path().join("job"),
            "Portraits",
            rawtonic_catalog::Mode::Session,
        )
        .unwrap();
        let backup = d.path().join("backup");
        let r = import_one(
            &lib,
            &source,
            "{job}_{seq:4}.{ext|lower}",
            1,
            Some(&backup),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(r.backup.unwrap().is_file());
        assert!(r.destination.unwrap().is_file());
        assert!(
            import_one(&lib, &source, "{orig}", 2, None, &AtomicBool::new(false))
                .unwrap()
                .skipped
        );
        assert_eq!(lib.assets(None).unwrap().len(), 1);
        assert_eq!(fs::read(source).unwrap(), b"original data");
    }
    /// A copy that an earlier import verified but could not record (the
    /// library was locked) is recorded on the next try, not copied again;
    /// a different file under that name is never taken for it.
    #[test]
    fn a_copied_but_unrecorded_file_is_recorded_on_retry() {
        let d = tempfile::tempdir().unwrap();
        let lib = Library::create(
            &d.path().join("job"),
            "Portraits",
            rawtonic_catalog::Mode::Session,
        )
        .unwrap();
        let capture = lib.directory.join(&lib.manifest.capture);
        fs::create_dir_all(&capture).unwrap();
        let backup = d.path().join("backup");
        fs::create_dir_all(&backup).unwrap();
        let source = d.path().join("A.ARW");
        fs::write(&source, b"first shot").unwrap();
        // What the failed import left: the copy, its backup, no record.
        fs::write(capture.join("A.ARW"), b"first shot").unwrap();
        fs::write(backup.join("A.ARW"), b"first shot").unwrap();
        let r = import_one(
            &lib,
            &source,
            "{orig}",
            1,
            Some(&backup),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(r.error.is_none() && !r.skipped, "{r:?}");
        let destination = r.destination.unwrap();
        assert_eq!(destination, capture.join("A.ARW"));
        assert_eq!(r.backup.unwrap(), backup.join("A.ARW"));
        let asset = lib.asset_at(&destination).unwrap().unwrap();
        assert_eq!(Some(asset.id), r.asset_id);
        assert_eq!(fs::read_dir(&capture).unwrap().count(), 1, "no second copy");
        assert_eq!(fs::read_dir(&backup).unwrap().count(), 1);

        // A different, unrecorded file under the name stays as it is.
        let other = d.path().join("B.ARW");
        fs::write(&other, b"second shot").unwrap();
        fs::write(capture.join("B.ARW"), b"someone else's").unwrap();
        let r = import_one(&lib, &other, "{orig}", 2, None, &AtomicBool::new(false)).unwrap();
        assert_eq!(r.destination.unwrap(), capture.join("B_1.ARW"));
        assert_eq!(fs::read(capture.join("B.ARW")).unwrap(), b"someone else's");
        assert_eq!(fs::read(capture.join("B_1.ARW")).unwrap(), b"second shot");
        assert!(lib.asset_at(&capture.join("B.ARW")).unwrap().is_none());
    }
}

#[cfg(test)]
mod counter_start_tests {
    use super::*;

    #[test]
    fn explicit_counter_names_real_copies_and_exposes_start() {
        let dir = tempfile::tempdir().unwrap();
        let library = Library::create(
            &dir.path().join("session"),
            "Counter",
            rawtonic_catalog::Mode::Session,
        )
        .unwrap();
        for offset in 0..2u64 {
            let source = dir.path().join(format!("source{offset}.jpg"));
            fs::write(&source, format!("different source {offset}")).unwrap();
            let result = import_one_with_start(
                &library,
                &source,
                "{seq_start}_{seq:4}.{ext}",
                42 + offset,
                42,
                None,
                &AtomicBool::new(false),
            )
            .unwrap();
            assert!(result.error.is_none());
            let destination = result.destination.unwrap();
            assert_eq!(
                destination.file_name().unwrap().to_str().unwrap(),
                format!("42_{:04}.jpg", 42 + offset)
            );
            assert_eq!(fs::read(&source).unwrap(), fs::read(destination).unwrap());
        }
    }
}

/// EXIF capture date, falling back to the source's modification date.
fn capture_date(source: &Path) -> chrono::NaiveDate {
    let exif = fs::File::open(source).ok().and_then(|file| {
        exif::Reader::new()
            .read_from_container(&mut std::io::BufReader::new(file))
            .ok()
    });
    if let Some(exif) = exif {
        for tag in [
            exif::Tag::DateTimeOriginal,
            exif::Tag::DateTimeDigitized,
            exif::Tag::DateTime,
        ] {
            if let Some(field) = exif.get_field(tag, exif::In::PRIMARY)
                && let exif::Value::Ascii(values) = &field.value
                && let Some(bytes) = values.first()
                && let Ok(text) = std::str::from_utf8(bytes)
                && let Some(date) = text.get(..10)
                && let Ok(date) = chrono::NaiveDate::parse_from_str(date, "%Y:%m:%d")
            {
                return date;
            }
        }
    }
    fs::metadata(source)
        .and_then(|m| m.modified())
        .map(chrono_from_system)
        .unwrap_or_else(|_| chrono::Utc::now().naive_utc())
        .date()
}
