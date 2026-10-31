//! Verified ingest: streaming reads, bounded memory and no-clobber publication.
pub mod batch;
pub mod capture;
pub mod catalog_export;
pub mod catalog_import;
mod copy;
pub mod folder_add;
pub mod hot_folder;
pub mod metadata;
pub mod queue;
pub mod transfer;

use anyhow::{Context, Result, bail, ensure};
use copy::Barrier;
use rawtonic_catalog::{Contents, Library};
use rawtonic_naming::{Context as NamingContext, render};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
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
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ImportResult {
    pub source: PathBuf,
    pub destination: Option<PathBuf>,
    pub backup: Option<PathBuf>,
    pub asset_id: Option<String>,
    pub error: Option<String>,
    pub skipped: bool,
    /// The source's XMP sidecar (`capture::card_sidecar`), copied beside
    /// `destination` under its name and adopted by the library.
    #[serde(default)]
    pub sidecar: Option<PathBuf>,
    /// A problem that did not keep the photograph out: its sidecar could
    /// not be read or copied (the photograph is imported without it).
    #[serde(default)]
    pub warning: Option<String>,
    /// RAW+JPEG (`batch::pair_sources`): on the RAW's result, its companion
    /// JPEG copied beside `destination` and recorded with its asset
    /// (`Library::companion`); on the JPEG's own result, the RAW's copy it
    /// belongs to.
    #[serde(default)]
    pub companion: Option<PathBuf>,
    #[serde(default)]
    pub companion_of: Option<PathBuf>,
    /// The copy's header as registration read it, for an import preset
    /// (`metadata::apply_to_copy`); never sent.
    #[serde(skip)]
    pub(crate) header: Option<rawtonic_naming::Capture>,
}

/// What a source brings to its import besides its bytes: its capture facts
/// (`capture::read_capture`) and its card sidecar with the metadata read
/// from it (`capture::card_sidecar`).
#[derive(Debug, Clone, Default)]
pub struct SourceFacts {
    pub capture: rawtonic_naming::Capture,
    pub sidecar: Option<PathBuf>,
    /// Why the sidecar is not used (unreadable, not XMP).
    pub sidecar_error: Option<String>,
}

/// Reads `source`'s capture facts and card sidecar. Reads only.
pub fn source_facts(source: &Path) -> SourceFacts {
    let mut facts = SourceFacts {
        capture: capture::read_capture(source),
        ..SourceFacts::default()
    };
    match capture::card_sidecar(source) {
        Ok(Some(sidecar)) => match read_sidecar(&sidecar) {
            Ok(metadata) => {
                capture::apply_sidecar(&mut facts.capture, &metadata);
                facts.sidecar = Some(sidecar);
            }
            Err(error) => {
                facts.sidecar_error = Some(format!(
                    "The sidecar {} was not copied: {error:#}",
                    sidecar.display()
                ))
            }
        },
        Ok(None) => {}
        Err(error) => facts.sidecar_error = Some(error),
    }
    facts
}

fn read_sidecar(path: &Path) -> Result<rawtonic_metadata::Metadata> {
    ensure!(
        fs::metadata(path)?.len() <= 16 * 1024 * 1024,
        "XMP exceeds 16 MiB limit"
    );
    let text = fs::read_to_string(path)?;
    Ok(rawtonic_metadata::read_xmp(&text)?.0)
}

/// The naming context of `source` in a library named `job`: its name,
/// counter and the facts of `source_facts`.
pub fn naming_context(
    job: &str,
    source: &Path,
    facts: &SourceFacts,
    sequence: u64,
    sequence_start: u64,
) -> Result<NamingContext> {
    let mut context = NamingContext::new(
        source
            .file_name()
            .and_then(|s| s.to_str())
            .context("Invalid source filename")?,
        sequence,
    );
    context.sequence_start = sequence_start;
    context.values.insert("session".into(), job.into());
    context.values.insert("job".into(), job.into());
    if let Some(folder) = source
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
    {
        context.values.insert("folder".into(), folder.into());
    }
    context.apply_capture(&facts.capture);
    Ok(context)
}

/// `template` filled for `source`, keeping the photograph's file type: a
/// name that does not end in the source's extension (say from
/// `{date}_{orig_base}`) gets it appended, as Capture One and Lightroom add
/// it themselves. Without it the copy would not be recognised as a
/// photograph (the library would list it nowhere). Templates may still place
/// `{ext}` (any case) themselves.
pub fn render_name(template: &str, context: &NamingContext, source: &Path) -> Result<String> {
    let name = render(template, context)?;
    let Some(ext) = source
        .extension()
        .and_then(|e| e.to_str())
        .filter(|e| !e.is_empty())
    else {
        return Ok(name);
    };
    let kept = Path::new(&name)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(ext));
    Ok(if kept { name } else { format!("{name}.{ext}") })
}

/// The name an import of `source` would give it with `template` (before a
/// collision suffix), and where its date came from: the import dialog's
/// preview. Reads `source`'s header and sidecar only.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NamePreview {
    pub name: String,
    pub date_source: rawtonic_naming::DateSource,
    pub sidecar: bool,
}

pub fn preview_name(
    job: &str,
    source: &Path,
    template: &str,
    sequence: u64,
    sequence_start: u64,
) -> Result<NamePreview> {
    let facts = source_facts(source);
    let context = naming_context(job, source, &facts, sequence, sequence_start)?;
    Ok(NamePreview {
        name: render_name(template, &context, source)?,
        date_source: facts.capture.date_source,
        sidecar: facts.sidecar.is_some(),
    })
}

/// Copies the card sidecar `sidecar` beside `photo` (a verified copy of its
/// photograph) as `<photo stem>.xmp` (`copy_beside`).
fn copy_sidecar(sidecar: &Path, photo: &Path, barrier: &mut Barrier) -> Result<PathBuf> {
    let target = photo.with_extension("xmp");
    copy_beside_all(
        sidecar,
        &[target],
        "sidecar",
        &AtomicBool::new(false),
        barrier,
        None,
    )
    .remove(0)
}

/// Copies `source` to `target` (beside a verified copy of its photograph:
/// its sidecar or companion JPEG), verified, never replacing a file and
/// durable when it returns (`copy_beside_all`).
fn copy_beside(source: &Path, target: &Path, what: &str, cancel: &AtomicBool) -> Result<PathBuf> {
    let mut barrier = Barrier::default();
    let copy = copy_beside_all(
        source,
        &[target.to_path_buf()],
        what,
        cancel,
        &mut barrier,
        None,
    )
    .remove(0)?;
    barrier.flush()?;
    Ok(copy)
}

/// Copies `source` to each of `targets` from one read, verified and never
/// replacing a file: a file already at a target with the same bytes is
/// taken as that copy (an earlier, interrupted import made it), a
/// different one is an error. One result per target; durable once
/// `barrier` is flushed.
fn copy_beside_all(
    source: &Path,
    targets: &[PathBuf],
    what: &str,
    cancel: &AtomicBool,
    barrier: &mut Barrier,
    ahead: Option<&copy::ReadAhead>,
) -> Vec<Result<PathBuf>> {
    let mut source_hash = None;
    let mut there = |target: &Path| -> Result<PathBuf> {
        let hash = match &source_hash {
            Some(hash) => hash,
            None => source_hash.insert(sha256(source)?),
        };
        ensure!(
            fs::symlink_metadata(target)?.is_file() && sha256(target)? == *hash,
            "A different {what} is already at {}; the card's {what} was not copied",
            target.display()
        );
        barrier.sync_existing(target)?;
        Ok(target.to_path_buf())
    };
    let mut results: Vec<Option<Result<PathBuf>>> = targets
        .iter()
        .map(|target| fs::symlink_metadata(target).is_ok().then(|| there(target)))
        .collect();
    let missing: Vec<usize> = (0..targets.len())
        .filter(|&i| results[i].is_none())
        .collect();
    if !missing.is_empty() {
        let paths: Vec<&Path> = missing.iter().map(|&i| targets[i].as_path()).collect();
        match copy::copy_verified(source, &paths, cancel, |_| {}, ahead) {
            Ok(mut copy) => {
                for (k, &i) in missing.iter().enumerate() {
                    results[i] = Some(copy.publish(k, barrier));
                }
            }
            // One read for all failed: each target on its own, so a
            // problem with one (a full backup drive) leaves the others.
            Err(_) if missing.len() > 1 => {
                for &i in &missing {
                    results[i] = Some(
                        copy::copy_verified(source, &[targets[i].as_path()], cancel, |_| {}, None)
                            .and_then(|mut copy| copy.publish(0, barrier)),
                    );
                }
            }
            Err(error) => results[missing[0]] = Some(Err(error)),
        }
    }
    results
        .into_iter()
        .map(|result| result.expect("every target handled"))
        .collect()
}

/// Where one copy of a photograph comes from.
enum Plan {
    /// A file with its bytes is there already (an earlier, cut-off import
    /// made it).
    There(PathBuf),
    /// To be copied here.
    Copy(PathBuf),
}

impl Plan {
    fn to_copy(&self) -> Option<&Path> {
        match self {
            Plan::Copy(path) => Some(path),
            Plan::There(_) => None,
        }
    }
}

/// `photo`'s companion name: its stem with `companion`'s extension.
fn companion_target(photo: &Path, companion: &Path) -> Result<PathBuf> {
    let extension = companion
        .extension()
        .context("A companion needs an extension")?;
    Ok(photo.with_extension(extension))
}

pub fn sha256(path: &Path) -> Result<String> {
    copy::sha256_of(&mut fs::File::open(path)?)
}

/// Copies `source` to `destination`, verified (`copy::copy_verified`),
/// never replacing a file, and durable when it returns. Destination parent
/// must already exist. The no-clobber persist closes the existence-check
/// race; a concurrent writer can never have its file replaced.
pub fn verified_copy(
    source: &Path,
    destination: &Path,
    cancel: &AtomicBool,
    progress: impl FnMut(u64),
) -> Result<CopyReceipt> {
    let mut barrier = Barrier::default();
    let receipt = verified_copy_with(source, destination, cancel, progress, &mut barrier)?;
    barrier.flush()?;
    Ok(receipt)
}

/// `verified_copy`, durable once `barrier` is flushed.
fn verified_copy_with(
    source: &Path,
    destination: &Path,
    cancel: &AtomicBool,
    progress: impl FnMut(u64),
    barrier: &mut Barrier,
) -> Result<CopyReceipt> {
    let mut copy = copy::copy_verified(source, &[destination], cancel, progress, None)?;
    let destination = copy.publish(0, barrier)?;
    Ok(CopyReceipt {
        destination,
        bytes: copy.bytes,
        sha256: copy.sha256,
    })
}

/// Creates `folder` and its missing parents like `fs::create_dir_all`, then
/// syncs every folder from `folder`'s parent up to `base` (each holds the
/// entry of the next): a power cut after an import committed cannot drop a
/// new year or date folder (with the photo in it) after the photo's staging
/// copy was removed and a camera that keeps no card copy let go of it.
/// Synced on every call, not only by the call that made a folder: a sync
/// that failed before, or a folder another import made a moment ago, is
/// made durable here too.
fn create_dir_all_durable(folder: &Path, base: &Path) -> std::io::Result<()> {
    create_dir_all_syncing(folder, base, |dir| {
        #[cfg(unix)]
        fs::File::open(dir)?.sync_all()?;
        #[cfg(not(unix))]
        let _ = dir;
        Ok(())
    })
}

/// `create_dir_all_durable`, durable once `barrier` is flushed.
fn create_dir_all_with(folder: &Path, base: &Path, barrier: &mut Barrier) -> std::io::Result<()> {
    create_dir_all_syncing(folder, base, |dir| barrier.sync_folder(dir))
}

/// `parent` as a directory to open: a relative path of one component
/// (`--backup backup`) has the empty parent, the current directory.
fn containing_dir(parent: &Path) -> &Path {
    if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    }
}

/// `create_dir_all_durable` with the directory sync given (`sync`: called
/// for each folder from `folder`'s parent up to `base`, innermost first).
fn create_dir_all_syncing(
    folder: &Path,
    base: &Path,
    mut sync: impl FnMut(&Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    fs::create_dir_all(folder)?;
    if !folder.starts_with(base) {
        return Err(std::io::Error::other(format!(
            "{} is not inside {}",
            folder.display(),
            base.display()
        )));
    }
    let mut at = folder;
    while at != base
        && let Some(parent) = at.parent()
    {
        sync(containing_dir(parent))?;
        at = parent;
    }
    Ok(())
}

pub fn choose_destination(
    folder: &Path,
    name: &str,
    policy: CollisionPolicy,
) -> Result<Option<PathBuf>> {
    choose_destination_with(folder, name, None, &|_| false, policy)
}

/// `choose_destination` for a photograph with a companion (`companion`'s
/// extension): a name free for both (`A.ARW` and `A.JPG`), so the pair
/// keeps one name. Names `reserved` takes (a recorded companion whose file
/// is gone) are not free either.
fn choose_destination_with(
    folder: &Path,
    name: &str,
    companion: Option<&Path>,
    reserved: &dyn Fn(&Path) -> bool,
    policy: CollisionPolicy,
) -> Result<Option<PathBuf>> {
    rawtonic_naming::validate_component(name)?;
    let taken = |path: &Path| path.exists() || reserved(path);
    let free = |candidate: &Path| -> Result<bool> {
        Ok(!taken(candidate)
            && match companion {
                Some(companion) => !taken(&companion_target(candidate, companion)?),
                None => true,
            })
    };
    let target = folder.join(name);
    if free(&target)? {
        return Ok(Some(target));
    }
    match policy {
        CollisionPolicy::Skip => Ok(None),
        CollisionPolicy::Error => bail!("Destination exists: {}", target.display()),
        CollisionPolicy::Suffix => {
            for i in 1..=MAX_COLLISIONS {
                let candidate = folder.join(collision_name(name, i)?);
                if free(&candidate)? {
                    return Ok(Some(candidate));
                }
            }
            bail!("Too many filename collisions")
        }
    }
}

const MAX_COLLISIONS: u32 = 100_000;

/// Whether `target` is a name a copy of the photograph named `name` gets
/// in a folder: `name` or one of its collision names (`collision_name`),
/// with its own extension or its companion's.
fn is_copy_name_of(name: &str, companion_extension: Option<&str>, target: &str) -> bool {
    let parts = |name: &str| {
        let path = Path::new(name);
        (
            path.file_stem().and_then(|s| s.to_str()).map(str::to_owned),
            path.extension().and_then(|e| e.to_str()).map(str::to_owned),
        )
    };
    let ((Some(stem), extension), (Some(target_stem), target_extension)) =
        (parts(name), parts(target))
    else {
        return false;
    };
    let same_extension = target_extension == extension
        || (target_extension.is_some() && target_extension.as_deref() == companion_extension);
    let same_stem = target_stem == stem
        || target_stem
            .strip_prefix(stem.as_str())
            .and_then(|rest| rest.strip_prefix('_'))
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    same_extension && same_stem
}

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
    companion: Option<&Path>,
    reserved: &dyn Fn(&Path) -> bool,
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
            // `choose_destination_with` skipped names reserved or taken
            // for a pair's companion.
            let skipped = reserved(&candidate)
                || match companion {
                    Some(companion) => {
                        let target = companion_target(&candidate, companion)?;
                        target.exists() || reserved(&target)
                    }
                    None => false,
                };
            if skipped {
                continue;
            }
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

/// `import_one` of a capture (the hot folder) whose new photograph is
/// stored with `contents` (its next-capture adjustments) in the same
/// transaction: it is never registered without them.
pub fn import_capture(
    library: &Library,
    source: &Path,
    template: &str,
    sequence: u64,
    contents: Option<&Contents>,
    cancel: &AtomicBool,
) -> Result<ImportResult> {
    let destination = destination_folder(library, None, &[])?;
    import_one_into_with(
        library,
        source,
        &destination,
        template,
        sequence,
        1,
        None,
        contents,
        cancel,
    )
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
            create_dir_all_durable(&capture, &library.directory)?;
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
    import_one_into_with(
        library,
        source,
        destination,
        template,
        sequence,
        sequence_start,
        backup,
        None,
        cancel,
    )
}

#[allow(clippy::too_many_arguments)]
fn import_one_into_with(
    library: &Library,
    source: &Path,
    destination: &Destination,
    template: &str,
    sequence: u64,
    sequence_start: u64,
    backup: Option<&Path>,
    contents: Option<&Contents>,
    cancel: &AtomicBool,
) -> Result<ImportResult> {
    let options = ImportOptions {
        destination,
        template,
        sequence,
        sequence_start,
        backup,
        earlier: None,
        before_copy: None,
        read_ahead: None,
    };
    Ok(import_pair(library, source, None, &options, contents, cancel)?.photo)
}

/// Called with the planned copies of a photograph and its companion right
/// before they are made (an ingest job records them, `queue`); an error
/// stops the import of that photograph.
pub(crate) type BeforeCopy<'a> = &'a dyn Fn(&Path, Option<&Path>) -> Result<()>;

/// How one photograph of an import is named and placed.
#[derive(Clone, Copy)]
pub(crate) struct ImportOptions<'a> {
    pub destination: &'a Destination,
    pub template: &'a str,
    pub sequence: u64,
    pub sequence_start: u64,
    pub backup: Option<&'a Path>,
    /// Where an earlier run planned the copy (`BeforeCopy`): a file with
    /// the source's bytes there that the library does not hold is that
    /// run's copy.
    pub earlier: Option<&'a Path>,
    pub before_copy: Option<BeforeCopy<'a>>,
    /// The batch's files read ahead.
    pub read_ahead: Option<&'a copy::ReadAhead>,
}

/// What importing a photograph with a companion did: the photograph's
/// result and, when it came with one, the companion's.
#[derive(Debug)]
pub(crate) struct PairResult {
    pub photo: ImportResult,
    pub companion: Option<ImportResult>,
}

/// A file of the library with the content of `source`.
pub(crate) enum Held {
    /// A photograph.
    Photo(rawtonic_catalog::Asset),
    /// A photograph's companion (the file at the path).
    Companion(rawtonic_catalog::Asset, PathBuf),
    /// A photograph's companion whose file is gone (the path where it
    /// belongs).
    LostCompanion(rawtonic_catalog::Asset, PathBuf),
}

/// The library's file with the content of `source` (same size and sampled
/// fingerprint, then the same SHA-256; companions by size, then SHA-256),
/// if any.
pub(crate) fn library_copy(library: &Library, source: &Path) -> Result<Option<Held>> {
    let fp = rawtonic_catalog::fingerprint(source)?;
    let len = fs::metadata(source)?.len();
    let mut source_hash = None;
    let mut same = |path: &Path| -> Result<bool> {
        let hash = match &source_hash {
            Some(hash) => hash,
            None => source_hash.insert(sha256(source)?),
        };
        Ok(sha256(path)? == *hash)
    };
    for asset in library
        .assets(None)?
        .into_iter()
        .filter(|a| a.fingerprint == fp && a.path.is_file())
    {
        if same(&asset.path)? {
            return Ok(Some(Held::Photo(asset)));
        }
    }
    if let Some((owner, path)) = held_companion_with(library, len, &mut same)? {
        return Ok(Some(Held::Companion(owner, path)));
    }
    for record in library.companion_records_of_size(len)? {
        if !record.path.exists() && record.matches(source) {
            return Ok(Some(Held::LostCompanion(
                library.asset(&record.owner)?,
                record.path,
            )));
        }
    }
    Ok(None)
}

/// The library's companion file (with its photograph) with the content of
/// `source` (same size, then the same SHA-256), if any.
pub(crate) fn held_companion(
    library: &Library,
    source: &Path,
) -> Result<Option<(rawtonic_catalog::Asset, PathBuf)>> {
    let len = fs::metadata(source)?.len();
    let mut source_hash = None;
    held_companion_with(library, len, &mut |path| {
        let hash = match &source_hash {
            Some(hash) => hash,
            None => source_hash.insert(sha256(source)?),
        };
        Ok(sha256(path)? == *hash)
    })
}

fn held_companion_with(
    library: &Library,
    len: u64,
    same: &mut dyn FnMut(&Path) -> Result<bool>,
) -> Result<Option<(rawtonic_catalog::Asset, PathBuf)>> {
    // Only companions of the source's size are candidates (from the
    // database: no other file is read).
    for record in library.companion_records_of_size(len)? {
        if record.sized() && same(&record.path)? && record.present() {
            return Ok(Some((library.asset(&record.owner)?, record.path)));
        }
    }
    Ok(None)
}

/// Puts `source`, the recorded companion of `owner` whose file is gone,
/// back where it belongs (`place`), verified and never replacing a file.
fn restore_companion(
    source: &Path,
    owner: &rawtonic_catalog::Asset,
    place: &Path,
    cancel: &AtomicBool,
) -> ImportResult {
    match copy_beside(source, place, "JPEG", cancel) {
        Ok(copy) => ImportResult {
            source: source.into(),
            destination: Some(copy),
            asset_id: Some(owner.id.clone()),
            companion_of: Some(owner.path.clone()),
            ..Default::default()
        },
        Err(error) => ImportResult {
            source: source.into(),
            error: Some(format!("{error:#}")),
            ..Default::default()
        },
    }
}

/// `import_pair` of `source` without a companion, a failure reported in
/// its result (a pair's JPEG brought in on its own must not fail its RAW).
fn import_alone(
    library: &Library,
    source: &Path,
    options: &ImportOptions<'_>,
    cancel: &AtomicBool,
) -> ImportResult {
    // A photograph of its own: not the job item's planned copy.
    let options = ImportOptions {
        earlier: None,
        before_copy: None,
        ..*options
    };
    import_pair(library, source, None, &options, None, cancel)
        .map(|pair| pair.photo)
        .unwrap_or_else(|error| ImportResult {
            source: source.into(),
            error: Some(format!("{error:#}")),
            ..Default::default()
        })
}

/// Imports `source` (named from it) and, for a RAW+JPEG pair, its
/// `companion`: copied verified beside the RAW's copy under the same name
/// and recorded with the RAW's asset in the same transaction. A companion
/// that cannot be copied leaves the RAW imported alone (its result says
/// why); one whose content the library holds already as a photograph of
/// its own stays that photograph (skipped).
pub(crate) fn import_pair(
    library: &Library,
    source: &Path,
    companion: Option<&Path>,
    options: &ImportOptions<'_>,
    contents: Option<&Contents>,
    cancel: &AtomicBool,
) -> Result<PairResult> {
    // One read of the source's header for its name, its date folder and
    // its card sidecar (the copy below reads the whole file).
    let facts = source_facts(source);
    let context = naming_context(
        &library.manifest.name,
        source,
        &facts,
        options.sequence,
        options.sequence_start,
    )?;
    let name = render_name(options.template, &context, source)?;
    // The folders and copies made for this photograph are synced as they
    // are made and the drive caches flushed once, before it is recorded
    // (`copy::Barrier`).
    let mut barrier = Barrier::default();
    let mut folder = options.destination.path().to_path_buf();
    // Catalogs organize their managed originals by capture date. Explicit
    // folder drops retain the destination the user chose; old manifests keep
    // their existing capture folder. Projects retain the per-job layout.
    if options.destination.sort_by_date {
        let date = context.captured.date();
        folder = folder
            .join(date.format("%Y").to_string())
            .join(date.format("%Y-%m-%d").to_string());
        create_dir_all_with(&folder, options.destination.path(), &mut barrier)?;
    }
    ensure!(
        folder.is_dir(),
        "The destination folder is gone: {}",
        folder.display()
    );
    let companion_result = |destination: Option<PathBuf>,
                            photo: Option<&PathBuf>,
                            asset_id: Option<String>,
                            skipped: bool,
                            error: Option<String>| {
        companion.map(|companion| ImportResult {
            source: companion.into(),
            destination,
            asset_id,
            skipped,
            error,
            companion_of: photo.cloned(),
            ..Default::default()
        })
    };
    // Detect duplicates by sampled fingerprint then full SHA, never by sampled hash alone.
    let held = match library_copy(library, source)? {
        Some(Held::Photo(asset)) => Some(asset),
        Some(Held::LostCompanion(owner, path)) => {
            return Ok(PairResult {
                photo: restore_companion(source, &owner, &path, cancel),
                companion: companion
                    .map(|companion| import_alone(library, companion, options, cancel)),
            });
        }
        Some(Held::Companion(owner, path)) => {
            return Ok(PairResult {
                photo: ImportResult {
                    source: source.into(),
                    destination: Some(path),
                    asset_id: Some(owner.id),
                    skipped: true,
                    companion_of: Some(owner.path),
                    ..Default::default()
                },
                companion: companion
                    .map(|companion| import_alone(library, companion, options, cancel)),
            });
        }
        None => None,
    };
    if let Some(asset) = held {
        let record = library.companion_record(&asset)?;
        let companion = companion.map(|companion| match &record {
            // Its recorded companion is gone and this is it: put back.
            Some(record) if !record.path.exists() && record.matches(companion) => {
                restore_companion(companion, &asset, &record.path, cancel)
            }
            // Checked on its own: the companion the library holds already
            // (skipped), else it comes in as a photograph of its own (the
            // RAW came in alone before, or with another JPEG) rather than
            // being left out.
            _ => import_alone(library, companion, options, cancel),
        });
        let kept = library.companion(&asset)?;
        return Ok(PairResult {
            photo: ImportResult {
                source: source.into(),
                destination: Some(asset.path.clone()),
                asset_id: Some(asset.id),
                skipped: true,
                companion: kept,
                ..Default::default()
            },
            companion,
        });
    }
    // A companion the library holds as a photograph of its own stays one.
    let (companion, known_companion) = match companion {
        Some(path) => match library_copy(library, path) {
            // Unreadable (gone meanwhile): the RAW comes in alone.
            Err(error) => (
                None,
                Some(ImportResult {
                    source: path.into(),
                    error: Some(format!("{error:#}")),
                    ..Default::default()
                }),
            ),
            Ok(Some(Held::Photo(asset))) => (
                None,
                Some(ImportResult {
                    source: path.into(),
                    destination: Some(asset.path),
                    asset_id: Some(asset.id),
                    skipped: true,
                    ..Default::default()
                }),
            ),
            Ok(Some(Held::LostCompanion(owner, place))) => {
                (None, Some(restore_companion(path, &owner, &place, cancel)))
            }
            Ok(Some(Held::Companion(owner, copy))) => (
                None,
                Some(ImportResult {
                    source: path.into(),
                    destination: Some(copy),
                    asset_id: Some(owner.id),
                    skipped: true,
                    companion_of: Some(owner.path),
                    ..Default::default()
                }),
            ),
            Ok(None) => (Some(path), None),
        },
        None => (None, None),
    };
    // Names of recorded companions whose file is gone are not free: the
    // companion may be put back there (a copy must never become another
    // photograph's companion).
    let records: HashSet<PathBuf> = library
        .companion_records()?
        .into_iter()
        .map(|record| record.path)
        .collect();
    let reserved = |path: &Path| {
        !records.is_empty()
            && path
                .parent()
                .and_then(|folder| folder.canonicalize().ok())
                .zip(path.file_name())
                .is_some_and(|(folder, name)| records.contains(&folder.join(name)))
    };
    // An earlier import of this file that copied and verified it but could
    // not record it (a crash, the database locked) left the copy in the
    // session, where it planned it (`ImportOptions::earlier`) or under its
    // name: record that copy instead of making another (and keep the
    // backup that import made).
    let unrecorded = |candidate: &Path| Ok(library.asset_at(candidate)?.is_none());
    // The temporary copies a cut-off run of this photograph left (G66), of
    // the photograph and its companion, under its name or a collision name
    // (the backup's may differ from the library's): this job alone writes
    // these names, one photograph at a time, so whatever their age they are
    // its own.
    if let Some(planned) = options.earlier {
        let companion_extension = companion
            .and_then(|path| path.extension())
            .and_then(|ext| ext.to_str());
        let planned_name = planned.file_name().and_then(|n| n.to_str());
        let ours = |target: &str| {
            [Some(name.as_str()), planned_name]
                .into_iter()
                .flatten()
                .any(|base| is_copy_name_of(base, companion_extension, target))
        };
        if let Some(library_folder) = planned.parent() {
            rawtonic_metadata::temp::remove_leftovers_matching(library_folder, ours);
        }
        if let Some(backup_folder) = options.backup {
            rawtonic_metadata::temp::remove_leftovers_matching(backup_folder, ours);
        }
    }
    let earlier = match options.earlier {
        Some(planned)
            if fs::symlink_metadata(planned).is_ok_and(|m| m.is_file())
                && unrecorded(planned)?
                && sha256(planned)? == sha256(source)? =>
        {
            Some(planned.to_path_buf())
        }
        _ => identical_file(&folder, &name, source, companion, &reserved, unrecorded)?,
    };
    // The backup: one an earlier, cut-off import made (also when it got no
    // further than the backup) is kept rather than made again.
    let backup_plan = match options.backup {
        None => None,
        Some(backup_folder) => {
            create_dir_all_with(
                backup_folder,
                backup_folder.parent().unwrap_or(backup_folder),
                &mut barrier,
            )?;
            ensure!(
                backup_folder.canonicalize()? != folder.canonicalize()?,
                "Backup must be a different directory"
            );
            let kept = identical_file(backup_folder, &name, source, companion, &|_| false, |_| {
                Ok(true)
            })?;
            Some(match kept {
                Some(kept) => Plan::There(kept),
                None => Plan::Copy(
                    choose_destination_with(
                        backup_folder,
                        &name,
                        companion,
                        &|_| false,
                        CollisionPolicy::Suffix,
                    )?
                    .context("No backup destination")?,
                ),
            })
        }
    };
    let photo_plan = match earlier {
        Some(earlier) => Plan::There(earlier),
        None => Plan::Copy(
            choose_destination_with(
                &folder,
                &name,
                companion,
                &reserved,
                CollisionPolicy::Suffix,
            )?
            .context("No destination")?,
        ),
    };
    // Recorded before anything is copied (an ingest job's record).
    let planned = match (&photo_plan, options.before_copy) {
        (Plan::Copy(destination), Some(before_copy)) => companion
            .map(|companion| companion_target(destination, companion))
            .transpose()
            .and_then(|target| before_copy(destination, target.as_deref())),
        _ => Ok(()),
    };
    // One read of the source for the backup and the library's copy.
    let targets: Vec<&Path> = backup_plan
        .iter()
        .chain([&photo_plan])
        .filter_map(Plan::to_copy)
        .collect();
    let copied = planned.and_then(|()| {
        (!targets.is_empty())
            .then(|| copy::copy_verified(source, &targets, cancel, |_| {}, options.read_ahead))
            .transpose()
    });
    let kept_backup = match &backup_plan {
        Some(Plan::There(kept)) => Some(kept.clone()),
        _ => None,
    };
    let failed =
        |error: &anyhow::Error, backup: Option<PathBuf>, known: Option<ImportResult>| PairResult {
            photo: ImportResult {
                source: source.into(),
                backup,
                error: Some(error.to_string()),
                ..Default::default()
            },
            companion: companion_result(None, None, None, false, Some(error.to_string())).or(known),
        };
    let mut copied = match copied {
        Ok(copied) => copied,
        Err(error) => return Ok(failed(&error, kept_backup, known_companion)),
    };
    let mut next = 0;
    let mut publish = |copied: &mut Option<copy::Verified>, barrier: &mut Barrier| {
        let copy = copied.as_mut().context("Nothing was copied")?;
        next += 1;
        copy.publish(next - 1, barrier)
    };
    // Copies an earlier run made are synced again: its flush may not have
    // happened (`copies_not_made_durable_are_recorded_by_the_next_run`).
    let mut reused = Ok(());
    let backup_result = match backup_plan {
        None => None,
        Some(Plan::There(kept)) => {
            reused = barrier.sync_existing(&kept);
            Some(kept)
        }
        Some(Plan::Copy(_)) => Some(publish(&mut copied, &mut barrier)?),
    };
    // The backup gets the card sidecar and the companion too; a failure
    // there only warns.
    let mut warnings: Vec<String> = facts.sidecar_error.iter().cloned().collect();
    if let (Some(card), Some(backup)) = (&facts.sidecar, &backup_result)
        && let Err(error) = copy_sidecar(card, backup, &mut barrier)
    {
        warnings.push(format!("Backup: {error:#}"));
    }
    let backup_companion = match (companion, &backup_result) {
        (Some(companion), Some(backup)) => match companion_target(backup, companion) {
            Ok(target) => Some(target),
            Err(error) => {
                warnings.push(format!("Backup: {error:#}"));
                None
            }
        },
        _ => None,
    };
    let (photo, hash) = match photo_plan {
        Plan::There(earlier) => {
            reused = reused.and(barrier.sync_existing(&earlier));
            (earlier, None)
        }
        Plan::Copy(_) => match publish(&mut copied, &mut barrier) {
            Ok(destination) => (destination, copied.as_ref().map(|copy| copy.sha256.clone())),
            Err(error) => {
                // The backup keeps its pair.
                if let (Some(companion), Some(target)) = (companion, &backup_companion)
                    && let Err(error) = copy_beside(companion, target, "JPEG", cancel)
                {
                    warnings.push(format!("Backup: {error:#}"));
                }
                let _ = barrier.flush();
                return Ok(failed(&error, backup_result, known_companion));
            }
        },
    };
    drop(copied);
    // The companion is copied (to the backup too, from one read) before
    // the photograph is recorded, so the record never names a file that is
    // not there.
    let copied_companion = companion.map(|companion| {
        let mut targets: Vec<PathBuf> = backup_companion.iter().cloned().collect();
        let copies = companion_target(&photo, companion).map(|target| {
            targets.push(target);
            copy_beside_all(
                companion,
                &targets,
                "JPEG",
                cancel,
                &mut barrier,
                options.read_ahead,
            )
        });
        // (`companion_target` fails for both or neither.)
        let mut copies = copies?;
        if let Some(Err(error)) = copies.first().filter(|_| copies.len() == 2) {
            warnings.push(format!("Backup: {error:#}"));
        }
        copies.pop().expect("one result per target")
    });
    let copied_sidecar = facts
        .sidecar
        .as_ref()
        .map(|card| copy_sidecar(card, &photo, &mut barrier));
    // Everything copied for this photograph reaches permanent storage
    // before it is recorded; a failure leaves the copies for a later run
    // to record (`earlier`).
    if let Err(error) = reused.and_then(|()| barrier.flush()) {
        let error = format!("The copies could not be made durable: {error}");
        return Ok(PairResult {
            photo: ImportResult {
                source: source.into(),
                destination: Some(photo.clone()),
                backup: backup_result,
                error: Some(error.clone()),
                ..Default::default()
            },
            companion: match copied_companion {
                None => known_companion,
                Some(copy) => companion_result(copy.ok(), Some(&photo), None, false, Some(error)),
            },
        });
    }
    let companion_name = match &copied_companion {
        Some(Ok(copy)) => copy.file_name().and_then(|n| n.to_str()),
        _ => None,
    };
    let mut result = register_with_sidecar(
        library,
        source,
        photo,
        backup_result,
        hash.as_deref(),
        contents,
        companion_name,
        copied_sidecar,
        warnings,
    );
    let companion = match copied_companion {
        None => known_companion,
        Some(Ok(copy)) if result.asset_id.is_some() => {
            result.companion = Some(copy.clone());
            companion_result(
                Some(copy),
                result.destination.as_ref(),
                result.asset_id.clone(),
                false,
                None,
            )
        }
        // Copied, but the photograph was not recorded: the copy stays
        // beside the photograph's, which an import again records with it.
        Some(Ok(copy)) => companion_result(
            Some(copy),
            result.destination.as_ref(),
            None,
            false,
            result.error.clone(),
        ),
        Some(Err(error)) => companion_result(
            None,
            result.destination.as_ref(),
            None,
            false,
            Some(format!("{error:#}")),
        ),
    };
    Ok(PairResult {
        photo: result,
        companion,
    })
}

/// `register` of `photo` with the card sidecar copied beside it
/// (`sidecar`, `copy_sidecar`), so the library adopts the sidecar's rating,
/// label, keywords and IPTC fields in the same transaction
/// (`Contents::read_sidecars`, as "Add" and `adopt_xmp` read them).
/// Without one, or when it could not be copied or read, the photograph is
/// registered as it is, with a warning.
#[allow(clippy::too_many_arguments)]
fn register_with_sidecar(
    library: &Library,
    source: &Path,
    photo: PathBuf,
    backup: Option<PathBuf>,
    sha256: Option<&str>,
    contents: Option<&Contents>,
    companion: Option<&str>,
    copied_sidecar: Option<Result<PathBuf>>,
    mut warnings: Vec<String>,
) -> ImportResult {
    let mut sidecar = None;
    let mut merged = contents.cloned();
    if let Some(copied) = copied_sidecar {
        let adopted = copied.and_then(|copied| Ok((copied, Contents::read_sidecars(&photo)?)));
        match adopted {
            Ok((copied, read)) => {
                sidecar = Some(copied);
                merged = Some(match contents {
                    None => read,
                    // A capture's next-capture adjustments stay; the
                    // sidecar's metadata takes precedence over theirs.
                    Some(given) => Contents {
                        editor_state: given.editor_state.clone().or(read.editor_state),
                        metadata: read.metadata.or(given.metadata.clone()),
                        xmp_digest: read.xmp_digest.or(given.xmp_digest.clone()),
                    },
                });
            }
            Err(error) => warnings.push(format!("{error:#}")),
        }
    }
    let mut result = register(
        library,
        source,
        photo,
        backup,
        sha256,
        merged.as_ref(),
        companion,
    );
    result.sidecar = sidecar;
    result.warning = (!warnings.is_empty()).then(|| warnings.join("; "));
    result
}

/// Records `copy`, which an earlier `import_one` of `source` copied and
/// verified but could not record (its `destination` with an `error`), when
/// it is still a file with the bytes of `source` and no record. `None`
/// otherwise: then import `source` as usual. For callers that retry (the
/// hot folder) when a template's name changed meanwhile (`{seq}`); the
/// photograph is stored with `contents` as by `import_capture`.
pub fn record_earlier_copy(
    library: &Library,
    source: &Path,
    copy: &Path,
    contents: Option<&Contents>,
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
    // On permanent storage before it is recorded (its own run's flush may
    // not have happened).
    let mut barrier = Barrier::default();
    barrier.sync_existing(copy)?;
    barrier.flush()?;
    Ok(Some(register(
        library,
        source,
        copy.to_path_buf(),
        None,
        None,
        contents,
        None,
    )))
}

/// Records the verified copy `destination` of `source` in the library,
/// with the content hash the copy was verified with (Synchronize Folder
/// proves moves with it) and, in the same transaction, `contents` and its
/// `companion` (a file beside it).
fn register(
    library: &Library,
    source: &Path,
    destination: PathBuf,
    backup: Option<PathBuf>,
    sha256: Option<&str>,
    contents: Option<&Contents>,
    companion: Option<&str>,
) -> ImportResult {
    // The copy's EXIF (its first megabyte; the copy was just verified) for
    // the library's search.
    let (facts, header) = capture::copy_header(&destination);
    let registered = library
        .plan_add(&destination, contents.cloned().unwrap_or_default())
        .map(|planned| match facts {
            Some(facts) => planned.with_facts(facts),
            None => planned,
        })
        .and_then(|planned| match companion {
            Some(name) => planned.with_companion(name),
            None => Ok(planned),
        })
        .and_then(|planned| library.insert_planned(vec![planned]))
        .and_then(|ids| {
            let (id, _) = ids.into_iter().next().context("Nothing was registered")?;
            library.asset(&id)
        });
    let (asset_id, error) = match registered {
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
        header,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;
    #[test]
    fn new_date_folders_are_synced_into_their_parents() {
        let d = tempfile::tempdir().unwrap();
        let library = d.path().join("Library");
        fs::create_dir(&library).unwrap();
        let day = library.join("2026").join("2026-10-20");
        let syncing = |folder: &Path, fail: bool| {
            let mut synced = Vec::new();
            let result = create_dir_all_syncing(folder, &library, |dir| {
                synced.push(dir.to_path_buf());
                if fail {
                    Err(std::io::Error::other("no sync"))
                } else {
                    Ok(())
                }
            });
            (result, synced)
        };
        // A failed sync fails the import, though the folders are there now.
        let (result, _) = syncing(&day, true);
        assert!(result.is_err() && day.is_dir());
        // The next import that day syncs the chain again: the year holds the
        // day, the library the year.
        let (result, synced) = syncing(&day, false);
        result.unwrap();
        assert_eq!(synced, [library.join("2026"), library.clone()]);
        // Every time, also when another import made the folders.
        assert_eq!(syncing(&day, false).1.len(), 2);
        assert!(syncing(&d.path().join("elsewhere"), false).0.is_err());
        // A backup folder given relative, of one component (`--backup
        // backup`): its parent is the current directory.
        let backup = Path::new("backup");
        assert_eq!(containing_dir(backup.parent().unwrap()), Path::new("."));
        assert_eq!(containing_dir(Path::new("/a")), Path::new("/a"));
        // `.` is a directory: one a sync opens where a directory can be
        // opened for that (not on Windows, whose folder sync does nothing).
        assert!(containing_dir(Path::new("")).is_dir());
        #[cfg(unix)]
        fs::File::open(containing_dir(Path::new(""))).unwrap();
        // The real one, on this filesystem.
        create_dir_all_durable(&library.join("2027").join("2027-01-01"), &library).unwrap();
    }

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
    /// A capture is registered with its contents (next-capture
    /// adjustments) in one transaction: a failed write keeps neither, the
    /// verified copy stays, and recording it on the retry stores both.
    #[test]
    fn a_capture_is_registered_with_its_contents_or_not_at_all() {
        for table in ["recipes", "metadata", "asset_imports"] {
            let d = tempfile::tempdir().unwrap();
            let lib = Library::create(
                &d.path().join("job"),
                "Portraits",
                rawtonic_catalog::Mode::Session,
            )
            .unwrap();
            let _batch = lib.import_batch(rawtonic_catalog::ImportKind::HotFolder);
            let source = d.path().join("A.ARW");
            fs::write(&source, b"capture").unwrap();
            let contents = Contents {
                editor_state: Some(
                    serde_json::json!({"version":1,"rating":0,"adjustments":{"contrast":12}}),
                ),
                ..Contents::default()
            };
            lib.inject_write_failure(table).unwrap();
            let failed = import_capture(
                &lib,
                &source,
                "{orig}",
                1,
                Some(&contents),
                &AtomicBool::new(false),
            )
            .unwrap();
            assert!(failed.asset_id.is_none(), "{table}: {failed:?}");
            assert!(failed.error.unwrap().contains("injected"), "{table}");
            let copy = failed.destination.unwrap();
            assert_eq!(fs::read(&copy).unwrap(), b"capture", "{table}");
            assert!(lib.assets(None).unwrap().is_empty(), "{table}");
            assert_eq!(lib.previous_import().unwrap(), None, "{table}");
            lib.clear_write_failures().unwrap();
            let r = record_earlier_copy(&lib, &source, &copy, Some(&contents))
                .unwrap()
                .unwrap();
            let id = r.asset_id.unwrap();
            assert_eq!(
                lib.editor_state(&id).unwrap().unwrap()["adjustments"]["contrast"],
                12,
                "{table}"
            );
            let batch = lib.previous_import().unwrap().unwrap();
            assert_eq!(
                (batch.kind, batch.count),
                (rawtonic_catalog::ImportKind::HotFolder, 1)
            );
        }
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
mod exif_and_sidecar_tests {
    use super::*;
    use crate::capture::tests::{jpeg_with_exif, tiff_exif};

    const CARD_XMP: &str = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:photoshop="http://ns.adobe.com/photoshop/1.0/" xmp:Rating="3" xmp:Label="Red" photoshop:City="Hamburg"><dc:subject><rdf:Bag><rdf:li>harbour</rdf:li></rdf:Bag></dc:subject><dc:title><rdf:Alt><rdf:li xml:lang="x-default">Ships</rdf:li></rdf:Alt></dc:title></rdf:Description></rdf:RDF></x:xmpmeta>"#;

    fn session(dir: &Path, mode: rawtonic_catalog::Mode) -> Library {
        Library::create(&dir.join("job"), "Harbour", mode).unwrap()
    }

    fn card(dir: &Path) -> PathBuf {
        let card = dir.join("card/DCIM/100TEST");
        fs::create_dir_all(&card).unwrap();
        card
    }

    /// Names and catalog date folders come from the EXIF capture date and
    /// camera, not the file's modification time.
    #[test]
    fn names_and_date_folders_come_from_exif() {
        let d = tempfile::tempdir().unwrap();
        let source = card(d.path()).join("DSC_0042.JPG");
        fs::write(&source, jpeg_with_exif(&tiff_exif("2025:12:24 18:30:15"))).unwrap();
        let lib = session(d.path(), rawtonic_catalog::Mode::Catalog);
        let r = import_one(
            &lib,
            &source,
            "{yyyy}{mm}{dd}-{hh}{MM}{ss}.{subsec}_{camera}_{orig_num}_{folder}_ISO{iso}_{shutter}.{ext|lower}",
            1,
            None,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(r.error.is_none() && r.warning.is_none(), "{r:?}");
        let destination = r.destination.unwrap();
        assert_eq!(
            destination.file_name().unwrap().to_str().unwrap(),
            "20251224-183015.042_Testmaker Model One_0042_100TEST_ISO800_1-250.jpg"
        );
        assert!(destination.parent().unwrap().ends_with("2025/2025-12-24"));
        let preview = preview_name("Harbour", &source, "{date}_{lens}", 1, 1).unwrap();
        assert_eq!(preview.name, "2025-12-24_Test Lens 35mm F2.8.JPG");
        assert_eq!(preview.date_source, rawtonic_naming::DateSource::Original);
        assert!(!preview.sidecar);
    }

    /// The copy is recorded with its capture facts (camera, lens,
    /// exposure, capture time) for the library's search, in the same
    /// transaction: the background pass has nothing left to read.
    #[test]
    fn ingested_photographs_are_searchable_by_their_capture_facts() {
        let d = tempfile::tempdir().unwrap();
        let source = card(d.path()).join("DSC_0042.JPG");
        fs::write(&source, jpeg_with_exif(&tiff_exif("2025:12:24 18:30:15"))).unwrap();
        let lib = session(d.path(), rawtonic_catalog::Mode::Session);
        let r = import_one(
            &lib,
            &source,
            "{job}_{seq:3}.{ext}",
            1,
            None,
            &AtomicBool::new(false),
        )
        .unwrap();
        let id = r.asset_id.unwrap();
        let (facts, read) = lib.facts(&id).unwrap().unwrap();
        assert!(read);
        assert_eq!(
            facts,
            rawtonic_catalog::CaptureFacts {
                captured: Some("2025-12-24 18:30:15".into()),
                make: Some("Testmaker".into()),
                model: Some("Model One".into()),
                lens: Some("Test Lens 35mm F2.8".into()),
                focal_length: Some(35.0),
                aperture: Some(2.8),
                iso: Some(800),
                exposure: Some(0.004),
                source_type: None,
                camera_rating: None,
                exposure_mode: Some(2),
                ..Default::default()
            }
        );
        assert_eq!(lib.facts_backlog().unwrap(), 0);
        let rule: rawtonic_catalog::Rule = serde_json::from_value(serde_json::json!(
            {"combine":"all","rules":[{"field":"lens","op":"contains","value":"35mm"},{"field":"iso","op":"ge","value":800}]}
        ))
        .unwrap();
        assert_eq!(lib.search_ids(&rule).unwrap(), [id]);
        // A file without EXIF has no facts and no invented capture time.
        let plain = d.path().join("card/plain.ARW");
        fs::write(&plain, b"raw bytes").unwrap();
        assert_eq!(
            capture::capture_facts(&plain),
            Some(rawtonic_catalog::CaptureFacts::default())
        );
        assert_eq!(capture::capture_facts(&d.path().join("gone.ARW")), None);
    }

    /// A card sidecar is copied beside the renamed copy (and its backup)
    /// under the new name, and the library adopts its metadata; the card's
    /// files stay as they were.
    #[test]
    fn card_sidecars_are_copied_under_the_new_name_and_adopted() {
        let d = tempfile::tempdir().unwrap();
        let card = card(d.path());
        let source = card.join("A0001.ARW");
        fs::write(&source, b"raw bytes").unwrap();
        let card_xmp = card.join("A0001.xmp");
        fs::write(&card_xmp, CARD_XMP).unwrap();
        let lib = session(d.path(), rawtonic_catalog::Mode::Session);
        let backup = d.path().join("backup");
        let r = import_one(
            &lib,
            &source,
            "{job}_{rating}{color|lower}_{title}_{seq:3}.{ext}",
            7,
            Some(&backup),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(r.error.is_none() && r.warning.is_none(), "{r:?}");
        let destination = r.destination.unwrap();
        assert_eq!(
            destination.file_name().unwrap().to_str().unwrap(),
            "Harbour_3red_Ships_007.ARW"
        );
        let copied = destination.with_file_name("Harbour_3red_Ships_007.xmp");
        assert_eq!(r.sidecar.as_deref(), Some(copied.as_path()));
        assert_eq!(fs::read_to_string(&copied).unwrap(), CARD_XMP);
        assert_eq!(
            fs::read_to_string(backup.join("Harbour_3red_Ships_007.xmp")).unwrap(),
            CARD_XMP
        );
        let asset = lib.asset(&r.asset_id.unwrap()).unwrap();
        assert_eq!((asset.rating, asset.label.as_str()), (3, "Red"));
        let metadata = lib.metadata(&asset.id).unwrap();
        assert_eq!(metadata.keywords, ["harbour"]);
        assert_eq!(metadata.title, "Ships");
        assert_eq!(
            metadata.iptc.get("city").map(String::as_str),
            Some("Hamburg")
        );
        assert_eq!(lib.summary().unwrap().xmp_pending, 0, "adopted as synced");
        assert_eq!(fs::read_to_string(&card_xmp).unwrap(), CARD_XMP);
        assert_eq!(fs::read(&source).unwrap(), b"raw bytes");
    }

    /// A different file where the sidecar would go is never replaced; a
    /// sidecar that is not XMP is not copied. Either way the photograph is
    /// imported, with a warning.
    #[test]
    fn sidecar_problems_warn_and_never_replace_a_file() {
        let d = tempfile::tempdir().unwrap();
        let card = card(d.path());
        let lib = session(d.path(), rawtonic_catalog::Mode::Session);
        let capture = lib.directory.join(&lib.manifest.capture);
        fs::create_dir_all(&capture).unwrap();
        fs::write(capture.join("B.xmp"), b"someone else's").unwrap();
        let b = card.join("B.ARW");
        fs::write(&b, b"b bytes").unwrap();
        fs::write(card.join("B.xmp"), CARD_XMP).unwrap();
        let r = import_one(&lib, &b, "{orig}", 1, None, &AtomicBool::new(false)).unwrap();
        assert!(r.asset_id.is_some() && r.error.is_none(), "{r:?}");
        assert!(r.warning.unwrap().contains("different sidecar"));
        assert_eq!(r.sidecar, None);
        assert_eq!(fs::read(capture.join("B.xmp")).unwrap(), b"someone else's");

        let c = card.join("C.ARW");
        fs::write(&c, b"c bytes").unwrap();
        fs::write(card.join("C.xmp"), b"not xml <").unwrap();
        let r = import_one(&lib, &c, "{orig}", 2, None, &AtomicBool::new(false)).unwrap();
        assert!(r.asset_id.is_some() && r.error.is_none(), "{r:?}");
        assert!(r.warning.unwrap().contains("was not copied"));
        assert!(!capture.join("C.xmp").exists());
    }

    /// A template without `{ext}` keeps the photograph's extension (native
    /// QA 2026-10-16: `{yyyy}{mm}{dd}_{orig_base}` made extension-less copies
    /// the library did not show); one that has it is left alone.
    #[test]
    fn names_keep_the_source_extension() {
        let d = tempfile::tempdir().unwrap();
        let card = card(d.path());
        let source = card.join("DSC_0042.JPG");
        fs::write(&source, jpeg_with_exif(&tiff_exif("2025:12:24 18:30:15"))).unwrap();
        fs::write(card.join("DSC_0042.xmp"), CARD_XMP).unwrap();
        for (template, expected) in [
            ("{yyyy}{mm}{dd}_{orig_base}", "20251224_DSC_0042.JPG"),
            ("{yyyy}_{orig_base}.{ext|lower}", "2025_DSC_0042.jpg"),
            ("{orig}", "DSC_0042.JPG"),
            ("{date}_{lens}", "2025-12-24_Test Lens 35mm F2.8.JPG"),
        ] {
            let preview = preview_name("Harbour", &source, template, 1, 1).unwrap();
            assert_eq!(preview.name, expected, "{template}");
        }
        let lib = session(d.path(), rawtonic_catalog::Mode::Session);
        let r = import_one(
            &lib,
            &source,
            "{yyyy}{mm}{dd}_{orig_base}",
            1,
            None,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(r.error.is_none() && r.warning.is_none(), "{r:?}");
        let destination = r.destination.unwrap();
        assert_eq!(destination.file_name().unwrap(), "20251224_DSC_0042.JPG");
        assert!(destination.with_extension("xmp").is_file());
        assert!(lib.asset_at(&destination).unwrap().is_some());
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
