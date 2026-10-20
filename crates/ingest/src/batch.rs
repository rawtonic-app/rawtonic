//! Ingest of a list of photographs, as Lightroom's and Capture One's import:
//! the sources the user chose or dropped (folders opened up), which of them
//! the library already holds, and the copy itself one file after the other,
//! each reported as soon as it is in the library so the app shows it while
//! the rest still copy.
use crate::{ImportOptions, ImportResult, destination_folder, import_pair};
use anyhow::{Context, Result};
use rawtonic_catalog::Library;
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

fn hidden(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name.to_string_lossy().starts_with('.'))
}

struct Expansion<'a> {
    accept: &'a dyn Fn(&Path) -> bool,
    found: Vec<PathBuf>,
    seen: HashSet<PathBuf>,
    limit: usize,
}

impl Expansion<'_> {
    /// Takes `file`; `false` once the limit is reached.
    fn take(&mut self, file: &Path) -> bool {
        if !(self.accept)(file) || self.seen.contains(file) {
            return true;
        }
        if self.found.len() >= self.limit {
            return false;
        }
        self.seen.insert(file.to_path_buf());
        self.found.push(file.to_path_buf());
        true
    }

    fn walk(&mut self, folder: &Path) -> bool {
        let Ok(entries) = fs::read_dir(folder) else {
            return true;
        };
        let mut entries: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        entries.sort();
        for entry in entries {
            if hidden(&entry) {
                continue;
            }
            // Symlinks are not followed into folders (no loops, no surprises).
            let Ok(meta) = fs::symlink_metadata(&entry) else {
                continue;
            };
            let more = if meta.is_dir() {
                self.walk(&entry)
            } else if entry.is_file() {
                self.take(&entry)
            } else {
                true
            };
            if !more {
                return false;
            }
        }
        true
    }
}

/// The files to import from what was chosen or dropped: files as they are,
/// folders with every file inside them (recursively, in name order; hidden
/// files and folders and symlinked folders left out), each only if `accept`
/// takes it, without repeats and at most `limit`. The `bool` tells whether
/// files were left out for the limit.
pub fn expand_sources(
    paths: &[PathBuf],
    accept: impl Fn(&Path) -> bool,
    limit: usize,
) -> (Vec<PathBuf>, bool) {
    let mut expansion = Expansion {
        accept: &accept,
        found: Vec::new(),
        seen: HashSet::new(),
        limit,
    };
    for path in paths {
        let more = if path.is_dir() {
            expansion.walk(path)
        } else if path.is_file() && !hidden(path) {
            expansion.take(path)
        } else {
            true
        };
        if !more {
            return (expansion.found, true);
        }
    }
    (expansion.found, false)
}

/// For each source, whether the library already holds a file with its
/// content (same size and content fingerprint). Cheap: only sources whose
/// size some photograph of the library has are read (two 64 KB samples).
/// A hint for the import dialog; the import itself compares SHA-256.
pub fn known_in_library(library: &Library, sources: &[PathBuf]) -> Result<Vec<bool>> {
    let keys = library.content_keys()?;
    let sizes: HashSet<u64> = keys.iter().map(|(size, _)| *size).collect();
    Ok(sources
        .iter()
        .map(|source| {
            let Ok(meta) = fs::metadata(source) else {
                return false;
            };
            sizes.contains(&meta.len())
                && rawtonic_catalog::fingerprint(source)
                    .is_ok_and(|fp| keys.contains(&(meta.len(), fp)))
        })
        .collect())
}

/// Formats a camera writes beside a RAW of the same name (RAW+JPEG,
/// RAW+HEIF): such a file comes in as the RAW's companion.
const COMPANION_EXTENSIONS: &[&str] = &["jpg", "jpeg", "heic", "heif", "hif"];

/// A photograph to import and the file that comes in with it as its
/// companion (`pair_sources`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceGroup {
    pub photo: PathBuf,
    pub companion: Option<PathBuf>,
}

/// `sources` as photographs to import: a RAW (`is_raw`, the app's RAW
/// formats) and a JPEG (or HEIF) of the same name in the same folder, both
/// chosen, are one photograph, the RAW with the JPEG as its companion
/// (Lightroom's default: one photograph, both files kept together; ADR
/// 0013). Names compare ignoring ASCII case. Only exactly one RAW and one
/// such image pair up: a name shared by more files (a RAW and a DNG, a
/// JPEG and a HEIF) leaves all of them photographs of their own, as do
/// files of the same name in different folders (cameras number each
/// folder on their own). Each photograph takes the place of the first of
/// its files, so `{seq}` follows the order of the files. Without `is_raw`
/// every file is a photograph of its own.
pub fn pair_sources(sources: &[PathBuf], is_raw: Option<fn(&Path) -> bool>) -> Vec<SourceGroup> {
    let Some(raw) = is_raw else {
        return sources
            .iter()
            .map(|photo| SourceGroup {
                photo: photo.clone(),
                companion: None,
            })
            .collect();
    };
    let companion_type = |path: &Path| {
        path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
            COMPANION_EXTENSIONS
                .iter()
                .any(|c| c.eq_ignore_ascii_case(e))
        })
    };
    let mut by_name: HashMap<(&Path, String), Vec<usize>> = HashMap::new();
    for (index, source) in sources.iter().enumerate() {
        if let (Some(parent), Some(stem)) =
            (source.parent(), source.file_stem().and_then(|s| s.to_str()))
        {
            by_name
                .entry((parent, stem.to_ascii_lowercase()))
                .or_default()
                .push(index);
        }
    }
    // The pairs by the place of their first file.
    let mut pairs: HashMap<usize, (usize, usize)> = HashMap::new();
    let mut paired = HashSet::new();
    for members in by_name.values() {
        let &[a, b] = members.as_slice() else {
            continue;
        };
        let pair = if raw(&sources[a]) && companion_type(&sources[b]) {
            (a, b)
        } else if raw(&sources[b]) && companion_type(&sources[a]) {
            (b, a)
        } else {
            continue;
        };
        pairs.insert(a.min(b), pair);
        paired.extend([a, b]);
    }
    (0..sources.len())
        .filter_map(|index| match pairs.get(&index) {
            Some(&(photo, companion)) => Some(SourceGroup {
                photo: sources[photo].clone(),
                companion: Some(sources[companion].clone()),
            }),
            None if paired.contains(&index) => None,
            None => Some(SourceGroup {
                photo: sources[index].clone(),
                companion: None,
            }),
        })
        .collect()
}

/// How a batch names and places its copies.
#[derive(Debug, Clone, Copy)]
pub struct BatchOptions<'a> {
    pub template: &'a str,
    /// The first `{seq}` (and `{seq_start}`).
    pub sequence_start: u64,
    /// A second, verified copy of each file here.
    pub backup: Option<&'a Path>,
    /// The folder to copy into (`None`: the capture folder); see
    /// `destination_folder`.
    pub folder: Option<&'a Path>,
    /// The library's top-level folders as the app's folder list shows them
    /// (for a catalog, folders below them are destinations too).
    pub roots: &'a [PathBuf],
    /// Which files are RAWs, to import RAW+JPEG pairs as one photograph
    /// (`pair_sources`); `None`: JPEGs next to RAWs of the same name come
    /// in as photographs of their own.
    pub pair_raw: Option<fn(&Path) -> bool>,
}

/// One file of a batch finished: in the library (or skipped as a duplicate,
/// or failed, see `result`).
#[derive(Debug)]
pub struct FileDone<'a> {
    /// Its place in the batch (0-based); files finish in this order.
    pub index: usize,
    pub total: usize,
    pub result: &'a ImportResult,
    /// Bytes copied by the batch so far.
    pub bytes: u64,
}

/// Imports `sources` one after the other (in their order, which `{seq}`
/// numbers: a RAW+JPEG pair takes one number, `pair_sources`), calling
/// `on_file` as soon as each is done, so it can be shown while the rest
/// copy; a pair's JPEG is reported right after its RAW, with the RAW's
/// asset. A cancel stops before the next file; the file it interrupted is
/// neither published nor reported (`verified_copy` never leaves a partial
/// copy). Fails at once only when the destination folder is not one an
/// import may copy into.
pub fn import_batch(
    library: &Library,
    sources: &[PathBuf],
    options: &BatchOptions<'_>,
    cancel: &AtomicBool,
    mut on_file: impl FnMut(FileDone<'_>),
) -> Result<Vec<ImportResult>> {
    let destination = destination_folder(library, options.folder, options.roots)?;
    let _batch = library.import_batch(rawtonic_catalog::ImportKind::Ingest);
    let groups = pair_sources(sources, options.pair_raw);
    let total = sources.len();
    let mut results = Vec::with_capacity(total);
    let mut bytes = 0u64;
    for (number, group) in groups.iter().enumerate() {
        if cancel.load(Ordering::Acquire) {
            break;
        }
        let import = ImportOptions {
            destination: &destination,
            template: options.template,
            sequence: options.sequence_start + number as u64,
            sequence_start: options.sequence_start,
            backup: options.backup,
        };
        let failed = |source: &Path, error: anyhow::Error| ImportResult {
            source: source.to_path_buf(),
            error: Some(error.to_string()),
            ..Default::default()
        };
        let (photo, companion) = match import_pair(
            library,
            &group.photo,
            group.companion.as_deref(),
            &import,
            None,
            cancel,
        ) {
            Ok(pair) => (pair.photo, pair.companion),
            Err(error) => {
                let message = error.to_string();
                (
                    failed(&group.photo, error),
                    group
                        .companion
                        .as_deref()
                        .map(|c| failed(c, anyhow::anyhow!(message))),
                )
            }
        };
        let mut stop = false;
        for result in std::iter::once(photo).chain(companion) {
            if cancel.load(Ordering::Acquire) && result.error.is_some() {
                stop = true;
                break;
            }
            if !result.skipped && result.error.is_none() {
                bytes += fs::metadata(&result.source).map(|m| m.len()).unwrap_or(0);
            }
            on_file(FileDone {
                index: results.len(),
                total,
                result: &result,
                bytes,
            });
            results.push(result);
        }
        if stop {
            break;
        }
    }
    Ok(results)
}

/// Where a source photograph is, which decides how an import into a
/// catalog starts (copied off a card, added where it is from a drive).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Medium {
    /// A memory card or camera: removable media, or a camera's `DCIM` folder.
    Card,
    /// A hard drive (internal, external or network).
    Disk,
}

/// `path`'s medium from what the system says about it (`media`): a card
/// when its file system is on removable media or the path runs through a
/// camera's `DCIM` folder (DCF, every camera's card layout); where the
/// system does not say, a Linux removable-media mount point (`/media/…`,
/// `/run/media/…`) counts as a card too. External drives are not
/// removable media: their photographs are on a drive.
pub fn classify_medium(path: &Path, media: &rawtonic_catalog::volume::MediaInfo) -> Medium {
    let dcim = path
        .components()
        .any(|c| c.as_os_str().eq_ignore_ascii_case("DCIM"));
    let removable_mount = media.removable.is_none()
        && media
            .mount_point
            .as_deref()
            .is_some_and(|mount| mount.starts_with("/media") || mount.starts_with("/run/media"));
    if dcim || media.removable == Some(true) || removable_mount {
        Medium::Card
    } else {
        Medium::Disk
    }
}

/// `classify_medium` with the system's answer for `path`.
pub fn source_medium(path: &Path) -> Medium {
    classify_medium(path, &rawtonic_catalog::volume::media_info(path))
}

/// Adds `sources` to a catalog where they are (Lightroom's "Add"), one
/// after the other, each reported through `on_file` as `import_batch`
/// does. Nothing is written into or beside the files: each is registered
/// with what its sidecars hold (`Library::add_files`: editor sidecar, else
/// XMP), in one transaction. All form one import batch. A file already registered, or whose content the catalog holds
/// already (fingerprint, then SHA-256), is skipped (`destination` is the
/// registered one). Refused for a session (it keeps its photographs inside
/// it) and for photographs on a card (they must be copied off it first,
/// `classify`). A cancel stops before the next file.
pub fn add_batch(
    library: &Library,
    sources: &[PathBuf],
    pair_raw: Option<fn(&Path) -> bool>,
    classify: impl Fn(&Path) -> Medium,
    cancel: &AtomicBool,
    mut on_file: impl FnMut(FileDone<'_>),
) -> Result<Vec<ImportResult>> {
    anyhow::ensure!(
        library.manifest.mode == rawtonic_catalog::Mode::Catalog,
        "A session keeps its photographs inside it: copy them into the session"
    );
    if let Some(card) = sources.iter().find(|s| classify(s) == Medium::Card) {
        anyhow::bail!(
            "{} is on a memory card: copy photographs off the card instead of adding them where they are",
            card.display()
        );
    }
    let mut known: HashMap<(u64, String), Vec<PathBuf>> = HashMap::new();
    for asset in library.assets(None)? {
        known
            .entry((asset.size, asset.fingerprint))
            .or_default()
            .push(asset.path);
    }
    let groups = pair_sources(sources, pair_raw);
    let _batch = library.import_batch(rawtonic_catalog::ImportKind::Add);
    let total = sources.len();
    let mut results = Vec::with_capacity(total);
    let failed = |source: &Path, error: anyhow::Error| ImportResult {
        source: source.to_path_buf(),
        error: Some(error.to_string()),
        ..Default::default()
    };
    for group in &groups {
        if cancel.load(Ordering::Acquire) {
            break;
        }
        let mut photo =
            add_one(library, &group.photo, &mut known).unwrap_or_else(|e| failed(&group.photo, e));
        let companion = group.companion.as_deref().map(|companion| {
            add_companion(library, &mut photo, companion, &mut known)
                .unwrap_or_else(|e| failed(companion, e))
        });
        for result in std::iter::once(photo).chain(companion) {
            on_file(FileDone {
                index: results.len(),
                total,
                result: &result,
                bytes: 0,
            });
            results.push(result);
        }
    }
    Ok(results)
}

/// Adds the companion of `photo` (`add_one`'s result) where it is: recorded
/// with `photo`'s asset when that is registered at the RAW's own place and
/// has none yet (nothing is written beside the files), else added as a
/// photograph of its own.
fn add_companion(
    library: &Library,
    photo: &mut ImportResult,
    companion: &Path,
    known: &mut HashMap<(u64, String), Vec<PathBuf>>,
) -> Result<ImportResult> {
    let in_place = photo.error.is_none()
        && photo
            .destination
            .as_deref()
            .and_then(|d| d.canonicalize().ok())
            == photo.source.canonicalize().ok();
    let owner = match &photo.asset_id {
        Some(id) if in_place => Some(library.asset(id)?),
        _ => None,
    };
    // Only the file beside the RAW's own (a symlinked RAW resolves to
    // another folder), not registered and no content the library holds.
    let resolved = companion.canonicalize()?;
    let beside = owner
        .as_ref()
        .is_some_and(|owner| resolved.parent() == owner.path.parent());
    let free = beside
        && library.asset_at(companion)?.is_none()
        && library.companion_owner(companion)?.is_none()
        && crate::library_copy(library, companion)?.is_none();
    match owner {
        Some(owner) if free && library.companion(&owner)?.is_none() => {
            let name = resolved
                .file_name()
                .and_then(|n| n.to_str())
                .context("Invalid file name")?;
            library.set_companion(&owner.id, Some(name))?;
            photo.companion = Some(companion.to_path_buf());
            Ok(ImportResult {
                source: companion.into(),
                destination: Some(companion.into()),
                asset_id: Some(owner.id),
                companion_of: Some(owner.path),
                ..Default::default()
            })
        }
        _ => add_one(library, companion, known),
    }
}

fn add_one(
    library: &Library,
    source: &Path,
    known: &mut HashMap<(u64, String), Vec<PathBuf>>,
) -> Result<ImportResult> {
    let skipped = |asset: rawtonic_catalog::Asset| ImportResult {
        source: source.into(),
        destination: Some(asset.path),
        backup: None,
        asset_id: Some(asset.id),
        error: None,
        skipped: true,
        ..Default::default()
    };
    anyhow::ensure!(source.is_file(), "Not a file: {}", source.display());
    if let Some(asset) = library.asset_at(source)? {
        return Ok(skipped(asset));
    }
    // A registered photograph's companion is part of it.
    if let Some(asset) = library.companion_owner(source)? {
        let mut result = skipped(asset);
        result.companion_of = result.destination.take();
        result.destination = Some(source.into());
        return Ok(result);
    }
    let key = (
        fs::metadata(source)?.len(),
        rawtonic_catalog::fingerprint(source)?,
    );
    if let Some(paths) = known.get(&key) {
        let hash = crate::sha256(source)?;
        for path in paths {
            if path.is_file()
                && crate::sha256(path)? == hash
                && let Some(asset) = library.asset_at(path)?
            {
                return Ok(skipped(asset));
            }
        }
    }
    // The content of a photograph's companion: part of that photograph.
    if let Some((owner, path)) = crate::held_companion(library, source)? {
        let mut result = skipped(owner);
        result.companion_of = result.destination.replace(path);
        return Ok(result);
    }
    // Registered with what its sidecars hold (editor sidecar, else XMP
    // rating, label and keywords) in one transaction; they are read, never
    // written. An unreadable sidecar keeps the file out.
    let mut report = library.add_files(&[source.to_path_buf()])?;
    if let Some((_, error)) = report.failed.pop() {
        anyhow::bail!(error);
    }
    let asset = report
        .added
        .pop()
        .or_else(|| report.known.pop())
        .context("Nothing was registered")?;
    known.entry(key).or_default().push(asset.path.clone());
    Ok(ImportResult {
        source: source.into(),
        destination: Some(asset.path),
        backup: None,
        asset_id: Some(asset.id),
        error: None,
        skipped: false,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rawtonic_catalog::Mode;

    fn library(dir: &Path) -> Library {
        Library::create(&dir.join("job"), "Job", Mode::Session).unwrap()
    }
    fn photos(dir: &Path, count: usize) -> Vec<PathBuf> {
        let card = dir.join("card");
        fs::create_dir_all(&card).unwrap();
        (0..count)
            .map(|i| {
                let path = card.join(format!("DSC_{i:04}.ARW"));
                fs::write(&path, vec![i as u8 + 1; 300_000 + i]).unwrap();
                path
            })
            .collect()
    }
    fn options(template: &str) -> BatchOptions<'_> {
        BatchOptions {
            template,
            sequence_start: 1,
            backup: None,
            folder: None,
            roots: &[],
            pair_raw: Some(raw),
        }
    }
    /// The RAW formats of these tests (the app passes its own).
    fn raw(path: &Path) -> bool {
        path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
            ["arw", "cr3", "dng", "nef", "raf"]
                .iter()
                .any(|r| r.eq_ignore_ascii_case(e))
        })
    }
    fn names(folder: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(folder)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    /// Each file is reported once it is in the library, in batch order:
    /// when its event arrives its copy exists and is registered.
    #[test]
    fn reports_each_file_in_order_as_soon_as_it_is_in_the_library() {
        let d = tempfile::tempdir().unwrap();
        let lib = library(d.path());
        let sources = photos(d.path(), 4);
        let mut events = Vec::new();
        let results = import_batch(
            &lib,
            &sources,
            &options("{orig}"),
            &AtomicBool::new(false),
            |done| {
                let destination = done.result.destination.clone().unwrap();
                assert!(destination.is_file(), "copy published before its event");
                assert!(lib.asset_at(&destination).unwrap().is_some(), "registered");
                events.push((
                    done.index,
                    done.total,
                    done.result.source.clone(),
                    done.bytes,
                ));
            },
        )
        .unwrap();
        assert_eq!(results.len(), 4);
        assert_eq!(
            events.iter().map(|e| e.0).collect::<Vec<_>>(),
            vec![0, 1, 2, 3]
        );
        assert!(events.iter().all(|e| e.1 == 4));
        assert_eq!(
            events.iter().map(|e| e.2.clone()).collect::<Vec<_>>(),
            sources
        );
        assert!(events.windows(2).all(|w| w[1].3 > w[0].3), "bytes grow");
    }

    /// A cancel during the batch stops before the next file: the files done
    /// stay (reported and registered), nothing after is copied, no partial
    /// file is left and the originals are untouched.
    #[test]
    fn cancel_stops_before_the_next_file_and_leaves_no_partial_copy() {
        let d = tempfile::tempdir().unwrap();
        let lib = library(d.path());
        let sources = photos(d.path(), 5);
        let before: Vec<Vec<u8>> = sources.iter().map(|s| fs::read(s).unwrap()).collect();
        let cancel = AtomicBool::new(false);
        let mut reported = Vec::new();
        let results = import_batch(&lib, &sources, &options("{orig}"), &cancel, |done| {
            reported.push(done.index);
            if done.index == 1 {
                cancel.store(true, Ordering::Release);
            }
        })
        .unwrap();
        assert_eq!(reported, vec![0, 1]);
        assert_eq!(results.len(), 2);
        let capture = lib.directory.join(&lib.manifest.capture);
        assert_eq!(names(&capture), vec!["DSC_0000.ARW", "DSC_0001.ARW"]);
        assert_eq!(lib.assets(None).unwrap().len(), 2);
        for (source, bytes) in sources.iter().zip(before) {
            assert_eq!(fs::read(source).unwrap(), bytes);
        }
    }

    /// A cancel that interrupts a copy: that file is neither published nor
    /// reported as failed.
    #[test]
    fn the_copy_a_cancel_interrupts_is_not_reported() {
        let d = tempfile::tempdir().unwrap();
        let lib = library(d.path());
        let sources = photos(d.path(), 2);
        let cancel = AtomicBool::new(false);
        let mut reported = 0;
        // Cancelled while the first file is in progress (its source is
        // read for its duplicate check before the copy starts).
        let results = std::thread::scope(|scope| {
            scope.spawn(|| cancel.store(true, Ordering::Release));
            import_batch(&lib, &sources, &options("{orig}"), &cancel, |_| {
                reported += 1
            })
            .unwrap()
        });
        assert!(results.len() <= 1);
        assert_eq!(reported, results.len());
        assert!(results.iter().all(|r| r.error.is_none()));
        let capture = lib.directory.join(&lib.manifest.capture);
        assert_eq!(names(&capture).len(), results.len(), "no partial copies");
    }

    /// Copies go into a folder of the library when one is given (a drop onto
    /// a folder), never into one outside it, the Trash, a hidden folder or
    /// the library folder itself.
    #[test]
    fn copies_into_a_library_folder_only() {
        let d = tempfile::tempdir().unwrap();
        let lib = library(d.path());
        let sources = photos(d.path(), 1);
        let selects = lib.directory.join("Selects");
        fs::create_dir_all(&selects).unwrap();
        let mut opts = options("{orig}");
        opts.folder = Some(&selects);
        let results = import_batch(&lib, &sources, &opts, &AtomicBool::new(false), |_| {}).unwrap();
        assert_eq!(
            results[0].destination.as_deref(),
            Some(selects.join("DSC_0000.ARW").as_path())
        );
        let outside = d.path().join("elsewhere");
        fs::create_dir_all(&outside).unwrap();
        let trash = lib.directory.join("Trash");
        fs::create_dir_all(&trash).unwrap();
        let cache = lib.directory.join(".cache");
        fs::create_dir_all(&cache).unwrap();
        for folder in [&outside, &trash, &cache, &lib.directory.clone()] {
            opts.folder = Some(folder);
            assert!(import_batch(&lib, &sources, &opts, &AtomicBool::new(false), |_| {}).is_err());
            assert!(!names(folder).contains(&"DSC_0000.ARW".to_string()));
        }
    }

    /// A catalog's folder list also shows folders outside its directory (on
    /// another drive): those, and folders below them, are destinations too
    /// (as Lightroom's "Import into this folder…"); a folder the list does
    /// not show, a hidden one or the app's own folders are not. An existing
    /// file there is never replaced.
    #[test]
    fn a_catalog_copies_into_the_folders_its_list_shows() {
        let d = tempfile::tempdir().unwrap();
        let lib = Library::create(&d.path().join("Main.catalog"), "Main", Mode::Catalog).unwrap();
        // A folder the catalog holds photographs of, elsewhere.
        let shoot = d.path().join("Drive/Shoots/2026");
        fs::create_dir_all(shoot.join("Day 2")).unwrap();
        fs::create_dir_all(shoot.join(".thumbs")).unwrap();
        fs::write(shoot.join("KEEP.ARW"), b"registered").unwrap();
        lib.add_asset(&shoot.join("KEEP.ARW")).unwrap();
        // A top-level folder of the app's folder list, without photographs yet.
        let root = d.path().join("Drive/Clients");
        fs::create_dir_all(root.join("Acme")).unwrap();
        let unlisted = d.path().join("Drive/Private");
        fs::create_dir_all(&unlisted).unwrap();
        let sources = photos(d.path(), 4);
        // A file of that name already there is kept as it is.
        fs::write(shoot.join("Day 2/DSC_0001.ARW"), b"someone else's").unwrap();

        let roots = [root.clone()];
        let mut opts = options("{orig}");
        opts.roots = &roots;
        for (i, (folder, expected)) in [
            (shoot.clone(), shoot.join("DSC_0000.ARW")),
            (shoot.join("Day 2"), shoot.join("Day 2/DSC_0001_1.ARW")),
            (root.join("Acme"), root.join("Acme/DSC_0002.ARW")),
        ]
        .into_iter()
        .enumerate()
        {
            let opts = BatchOptions {
                folder: Some(&folder),
                ..opts
            };
            let results = import_batch(
                &lib,
                &sources[i..=i],
                &opts,
                &AtomicBool::new(false),
                |_| {},
            )
            .unwrap();
            let result = &results[0];
            assert!(result.error.is_none() && !result.skipped, "{result:?}");
            assert_eq!(result.destination.as_deref(), Some(expected.as_path()));
            assert_eq!(fs::read(&expected).unwrap(), fs::read(&sources[i]).unwrap());
            assert!(lib.asset_at(&expected).unwrap().is_some(), "registered");
        }
        assert_eq!(
            fs::read(shoot.join("Day 2/DSC_0001.ARW")).unwrap(),
            b"someone else's"
        );
        let copy = sources[3].clone();
        for folder in [
            unlisted.clone(),
            shoot.join(".thumbs"),
            lib.directory.join("Trash"),
            lib.directory.join(".cache"),
        ] {
            fs::create_dir_all(&folder).unwrap();
            let opts = BatchOptions {
                folder: Some(&folder),
                ..opts
            };
            assert!(
                import_batch(
                    &lib,
                    std::slice::from_ref(&copy),
                    &opts,
                    &AtomicBool::new(false),
                    |_| {}
                )
                .is_err(),
                "{}",
                folder.display()
            );
            assert!(!names(&folder).contains(&"DSC_0003.ARW".to_string()));
        }
        // A session copies into its own folders only.
        let session = library(&d.path().join("s"));
        opts.folder = Some(&root);
        assert!(
            import_batch(
                &session,
                &sources[1..],
                &opts,
                &AtomicBool::new(false),
                |_| {}
            )
            .is_err()
        );
    }

    fn tree(dir: &Path) -> Vec<(PathBuf, Vec<u8>, std::time::SystemTime)> {
        let mut found = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(folder) = stack.pop() {
            for entry in fs::read_dir(&folder).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    let meta = fs::metadata(&path).unwrap();
                    found.push((
                        path.clone(),
                        fs::read(&path).unwrap(),
                        meta.modified().unwrap(),
                    ));
                }
            }
        }
        found.sort_by(|a, b| a.0.cmp(&b.0));
        found
    }

    /// Adding where they are registers the photographs and writes nothing
    /// into or beside them; a file already there or a copy of one is skipped.
    #[test]
    fn adds_photographs_where_they_are_without_writing_a_file() {
        let d = tempfile::tempdir().unwrap();
        let lib = Library::create(&d.path().join("Main.catalog"), "Main", Mode::Catalog).unwrap();
        let drive = d.path().join("Archive/2026");
        fs::create_dir_all(&drive).unwrap();
        let a = drive.join("A.NEF");
        let b = drive.join("B.NEF");
        let copy_of_a = drive.join("A copy.NEF");
        fs::write(&a, vec![1u8; 200_000]).unwrap();
        fs::write(&b, vec![2u8; 210_000]).unwrap();
        fs::write(&copy_of_a, vec![1u8; 200_000]).unwrap();
        fs::write(
            drive.join("B.xmp"),
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Rating="4"/></rdf:RDF></x:xmpmeta>"#,
        )
        .unwrap();
        let before = tree(d.path().join("Archive").as_path());
        let sources = vec![a.clone(), b.clone(), copy_of_a.clone(), a.clone()];
        let mut events = Vec::new();
        let results = add_batch(
            &lib,
            &sources,
            Some(raw),
            |_| Medium::Disk,
            &AtomicBool::new(false),
            |done| events.push((done.index, done.result.skipped)),
        )
        .unwrap();
        assert_eq!(events, vec![(0, false), (1, false), (2, true), (3, true)]);
        assert!(results.iter().all(|r| r.error.is_none()));
        assert_eq!(
            results[0].destination.as_deref(),
            Some(a.canonicalize().unwrap().as_path())
        );
        assert_eq!(lib.assets(None).unwrap().len(), 2);
        let b_asset = lib.asset_at(&b).unwrap().unwrap();
        assert_eq!(b_asset.rating, 4, "XMP sidecar read");
        // Not a byte, name or timestamp changed, nothing added beside them.
        assert_eq!(tree(d.path().join("Archive").as_path()), before);
        // The two added photographs are Previous Import, an "add" batch.
        let batch = lib.previous_import().unwrap().unwrap();
        assert_eq!(
            (batch.kind, batch.count),
            (rawtonic_catalog::ImportKind::Add, 2)
        );
        // A photograph whose sidecar cannot be read fails alone, unregistered.
        let c = drive.join("C.NEF");
        fs::write(&c, vec![3u8; 220_000]).unwrap();
        fs::write(drive.join("C.xmp"), "<x:xmpmeta").unwrap();
        let results = add_batch(
            &lib,
            std::slice::from_ref(&c),
            Some(raw),
            |_| Medium::Disk,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        assert!(results[0].error.as_deref().unwrap().contains("C.xmp"));
        assert!(lib.asset_at(&c).unwrap().is_none());
        assert_eq!(lib.previous_import().unwrap().unwrap().id, batch.id);
    }

    /// A verified copy batch is one import batch: Previous Import.
    #[test]
    fn an_ingest_is_one_import_batch() {
        let d = tempfile::tempdir().unwrap();
        let lib = library(d.path());
        let sources = photos(d.path(), 3);
        import_batch(
            &lib,
            &sources,
            &options("{orig}"),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        let batch = lib.previous_import().unwrap().unwrap();
        assert_eq!(
            (batch.kind, batch.count),
            (rawtonic_catalog::ImportKind::Ingest, 3)
        );
        assert_eq!(lib.previous_import_assets().unwrap().len(), 3);
        // The verified copies' hashes are on record for Synchronize Folder.
        assert_eq!(lib.checksum_backlog().unwrap(), 0);
        // Again: all duplicates, nothing added, no new batch.
        import_batch(
            &lib,
            &sources,
            &options("{orig}"),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        assert_eq!(lib.previous_import().unwrap().unwrap().id, batch.id);
    }

    #[test]
    fn adding_in_place_is_refused_for_sessions_and_cards() {
        let d = tempfile::tempdir().unwrap();
        let session = library(d.path());
        let sources = photos(d.path(), 1);
        let no_files = |_: FileDone<'_>| {};
        assert!(
            add_batch(
                &session,
                &sources,
                Some(raw),
                |_| Medium::Disk,
                &AtomicBool::new(false),
                no_files
            )
            .is_err()
        );
        let catalog = Library::create(&d.path().join("C.catalog"), "C", Mode::Catalog).unwrap();
        assert!(
            add_batch(
                &catalog,
                &sources,
                Some(raw),
                |_| Medium::Card,
                &AtomicBool::new(false),
                no_files
            )
            .is_err()
        );
        assert!(catalog.assets(None).unwrap().is_empty());
        assert!(session.assets(None).unwrap().is_empty());
    }

    #[test]
    fn classifies_cards_and_drives() {
        use rawtonic_catalog::volume::MediaInfo;
        let info = |removable: Option<bool>, mount: Option<&str>| MediaInfo {
            removable,
            mount_point: mount.map(PathBuf::from),
        };
        let card = Path::new("/Volumes/EOS_DIGITAL/DCIM/100CANON/IMG_0001.CR3");
        let drive = Path::new("/Volumes/Archive/2026/Wedding/IMG_0001.CR3");
        // A camera's DCIM folder is a card, wherever it is mounted.
        assert_eq!(
            classify_medium(card, &info(Some(false), Some("/Volumes/EOS_DIGITAL"))),
            Medium::Card
        );
        assert_eq!(
            classify_medium(Path::new("/Volumes/X/dcim/A.ARW"), &info(None, None)),
            Medium::Card
        );
        // Removable media (an SD card in a reader, a USB stick).
        assert_eq!(
            classify_medium(drive, &info(Some(true), Some("/Volumes/Archive"))),
            Medium::Card
        );
        // An external or internal drive.
        assert_eq!(
            classify_medium(drive, &info(Some(false), Some("/Volumes/Archive"))),
            Medium::Disk
        );
        assert_eq!(
            classify_medium(
                Path::new("/home/me/Pictures/A.NEF"),
                &info(Some(false), Some("/"))
            ),
            Medium::Disk
        );
        // Linux: the system does not say; a removable-media mount point is a card.
        assert_eq!(
            classify_medium(
                Path::new("/run/media/me/SD/A.NEF"),
                &info(None, Some("/run/media/me/SD"))
            ),
            Medium::Card
        );
        assert_eq!(
            classify_medium(
                Path::new("/media/me/CARD/A.NEF"),
                &info(None, Some("/media/me/CARD"))
            ),
            Medium::Card
        );
        // …but sysfs saying "fixed" wins over the mount point.
        assert_eq!(
            classify_medium(
                Path::new("/media/me/Archive/A.NEF"),
                &info(Some(false), Some("/media/me/Archive"))
            ),
            Medium::Disk
        );
        // Windows: a removable drive letter.
        assert_eq!(
            classify_medium(
                Path::new("E:\\Photos\\A.NEF"),
                &info(Some(true), Some("E:\\"))
            ),
            Medium::Card
        );
        assert_eq!(
            classify_medium(
                Path::new("D:\\Photos\\A.NEF"),
                &info(Some(false), Some("D:\\"))
            ),
            Medium::Disk
        );
        // A folder of this machine is a drive.
        let d = tempfile::tempdir().unwrap();
        assert_eq!(source_medium(d.path()), Medium::Disk);
    }

    #[test]
    fn expands_folders_recursively_with_the_filter() {
        let d = tempfile::tempdir().unwrap();
        let card = d.path().join("DCIM");
        fs::create_dir_all(card.join("100CANON")).unwrap();
        fs::create_dir_all(card.join(".hidden")).unwrap();
        fs::write(card.join("100CANON/B.CR3"), b"b").unwrap();
        fs::write(card.join("100CANON/A.CR3"), b"a").unwrap();
        fs::write(card.join("100CANON/notes.txt"), b"x").unwrap();
        fs::write(card.join("100CANON/._A.CR3"), b"x").unwrap();
        fs::write(card.join(".hidden/C.CR3"), b"c").unwrap();
        let single = d.path().join("Z.CR3");
        fs::write(&single, b"z").unwrap();
        let accept = |p: &Path| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("cr3"));
        let (found, truncated) =
            expand_sources(&[single.clone(), card.clone(), single.clone()], accept, 100);
        assert!(!truncated);
        assert_eq!(
            found,
            vec![
                single.clone(),
                card.join("100CANON/A.CR3"),
                card.join("100CANON/B.CR3")
            ]
        );
        let (found, truncated) = expand_sources(&[card], accept, 1);
        assert!(truncated);
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn marks_files_the_library_already_holds() {
        let d = tempfile::tempdir().unwrap();
        let lib = library(d.path());
        let sources = photos(d.path(), 2);
        import_batch(
            &lib,
            &sources[..1],
            &options("{orig}"),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        assert_eq!(known_in_library(&lib, &sources).unwrap(), vec![true, false]);
    }

    fn write(path: &Path, bytes: &[u8]) -> PathBuf {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
        path.to_path_buf()
    }

    /// Every file below `folder` with its bytes (the card, to prove it
    /// unchanged).
    fn snapshot(folder: &Path) -> Vec<(PathBuf, Vec<u8>, std::time::SystemTime)> {
        let mut files = Vec::new();
        let mut stack = vec![folder.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    let meta = fs::metadata(&path).unwrap();
                    files.push((
                        path.clone(),
                        fs::read(&path).unwrap(),
                        meta.modified().unwrap(),
                    ));
                }
            }
        }
        files.sort();
        files
    }

    #[test]
    fn a_raw_and_a_jpeg_of_one_name_in_one_folder_pair_up() {
        let p = |s: &str| PathBuf::from(s);
        let group = |photo: &str, companion: Option<&str>| SourceGroup {
            photo: p(photo),
            companion: companion.map(p),
        };
        let sources = [
            p("/c/100/A.RAF"),
            p("/c/100/a.jpg"),
            p("/c/100/B.CR3"),
            p("/c/100/B.DNG"),
            p("/c/100/B.JPG"),
            p("/c/100/C.JPG"),
            p("/c/100/C.HEIF"),
            p("/c/100/D.ARW"),
            p("/c/101/D.JPG"),
            p("/c/100/E.NEF"),
            p("/c/100/E.HIF"),
            p("/c/100/F.TIF"),
            p("/c/100/F.JPG"),
            p("/c/100/G.GIF"),
            p("/c/100/G.JPG"),
        ];
        assert_eq!(
            pair_sources(&sources, Some(raw)),
            vec![
                // Letter case aside; the RAW leads wherever it is listed.
                group("/c/100/A.RAF", Some("/c/100/a.jpg")),
                // Three files of one name: none pairs.
                group("/c/100/B.CR3", None),
                group("/c/100/B.DNG", None),
                group("/c/100/B.JPG", None),
                // No RAW.
                group("/c/100/C.JPG", None),
                group("/c/100/C.HEIF", None),
                // Another folder: another photograph (cameras number
                // each folder on their own).
                group("/c/100/D.ARW", None),
                group("/c/101/D.JPG", None),
                group("/c/100/E.NEF", Some("/c/100/E.HIF")),
                group("/c/100/F.TIF", None),
                group("/c/100/F.JPG", None),
                // Only the app's RAW formats are RAWs.
                group("/c/100/G.GIF", None),
                group("/c/100/G.JPG", None),
            ]
        );
        // The JPEG listed first: the pair takes its place.
        assert_eq!(
            pair_sources(&[p("/c/Z.ARW"), p("/c/A.JPG"), p("/c/A.RAF")], Some(raw)),
            vec![group("/c/Z.ARW", None), group("/c/A.RAF", Some("/c/A.JPG"))]
        );
    }

    /// A RAW+JPEG pair is one photograph: the JPEG is copied beside the
    /// RAW under its new name and recorded with its asset; the folder's
    /// files are not registered again; the card is untouched.
    #[test]
    fn a_pair_is_imported_as_one_photograph_with_its_companion() {
        let d = tempfile::tempdir().unwrap();
        let lib = library(d.path());
        let card = d.path().join("card/DCIM/100CAM");
        let sources = vec![
            write(&card.join("DSC_0001.ARW"), b"raw one"),
            write(&card.join("DSC_0001.JPG"), b"jpeg one"),
            write(&card.join("DSC_0002.ARW"), b"raw two"),
        ];
        write(&card.join("DSC_0001.xmp"), br#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Rating="4"/></rdf:RDF></x:xmpmeta>"#);
        let before = snapshot(&d.path().join("card"));
        let mut reported = Vec::new();
        let results = import_batch(
            &lib,
            &sources,
            &options("Shoot_{seq:3}"),
            &AtomicBool::new(false),
            |done| reported.push((done.index, done.total, done.result.source.clone())),
        )
        .unwrap();
        assert_eq!(
            reported,
            vec![
                (0, 3, sources[0].clone()),
                (1, 3, sources[1].clone()),
                (2, 3, sources[2].clone()),
            ]
        );
        let capture = lib.directory.join("Capture");
        assert_eq!(
            names(&capture),
            [
                "Shoot_001.ARW",
                "Shoot_001.JPG",
                "Shoot_001.xmp",
                "Shoot_002.ARW"
            ]
        );
        let raw = &results[0];
        assert_eq!(
            raw.companion.as_deref(),
            Some(capture.join("Shoot_001.JPG").as_path())
        );
        let jpeg = &results[1];
        assert_eq!(jpeg.asset_id, raw.asset_id);
        assert_eq!(jpeg.companion_of, raw.destination);
        assert!(!jpeg.skipped && jpeg.error.is_none());
        assert_eq!(
            fs::read(capture.join("Shoot_001.JPG")).unwrap(),
            b"jpeg one"
        );
        let asset = lib.asset(raw.asset_id.as_ref().unwrap()).unwrap();
        assert_eq!(asset.rating, 4, "the RAW owns the stem's sidecar");
        assert_eq!(
            lib.companion(&asset).unwrap(),
            Some(capture.join("Shoot_001.JPG"))
        );
        assert_eq!(lib.summary().unwrap().assets, 2);
        // The session's folder index finds nothing new.
        let all: Vec<PathBuf> = names(&capture)
            .iter()
            .filter(|n| !n.ends_with(".xmp"))
            .map(|n| capture.join(n))
            .collect();
        assert!(lib.add_files(&all).unwrap().added.is_empty());
        assert_eq!(snapshot(&d.path().join("card")), before, "card untouched");

        // Imported again: all skipped, nothing copied.
        let again = import_batch(
            &lib,
            &sources,
            &options("Shoot_{seq:3}"),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        assert!(again.iter().all(|r| r.skipped), "{again:?}");
        assert_eq!(again[1].destination, raw.companion);
        assert_eq!(names(&capture).len(), 4);
        assert_eq!(lib.summary().unwrap().assets, 2);
    }

    /// The pair keeps one name: a name taken for either file moves both on
    /// to the next free one, and nothing there is replaced.
    #[test]
    fn a_pair_takes_a_name_free_for_both_files() {
        let d = tempfile::tempdir().unwrap();
        let lib = library(d.path());
        let capture = lib.directory.join("Capture");
        write(&capture.join("IMG.JPG"), b"someone else's");
        let card = d.path().join("card");
        let sources = vec![
            write(&card.join("IMG.CR3"), b"raw"),
            write(&card.join("IMG.JPG"), b"jpeg"),
        ];
        let results = import_batch(
            &lib,
            &sources,
            &options("{orig}"),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        assert_eq!(results[0].destination, Some(capture.join("IMG_1.CR3")));
        assert_eq!(results[1].destination, Some(capture.join("IMG_1.JPG")));
        assert_eq!(
            fs::read(capture.join("IMG.JPG")).unwrap(),
            b"someone else's"
        );
    }

    /// With separate JPEGs both files are photographs of their own.
    #[test]
    fn jpegs_can_come_in_as_photographs_of_their_own() {
        let d = tempfile::tempdir().unwrap();
        let lib = library(d.path());
        let card = d.path().join("card");
        let sources = vec![
            write(&card.join("A.NEF"), b"raw"),
            write(&card.join("A.JPG"), b"jpeg"),
        ];
        let mut opts = options("{orig}");
        opts.pair_raw = None;
        let results = import_batch(&lib, &sources, &opts, &AtomicBool::new(false), |_| {}).unwrap();
        assert!(
            results
                .iter()
                .all(|r| r.companion.is_none() && r.companion_of.is_none())
        );
        assert_ne!(results[0].asset_id, results[1].asset_id);
        assert_eq!(lib.summary().unwrap().assets, 2);
    }

    /// A RAW imported alone before: its JPEG comes in as a photograph of
    /// its own, not left out; a JPEG the library holds already stays that
    /// photograph and the RAW comes in alone.
    #[test]
    fn a_pair_half_the_library_holds_already() {
        let d = tempfile::tempdir().unwrap();
        let lib = library(d.path());
        let card = d.path().join("card");
        let raw = write(&card.join("A.NEF"), b"raw");
        let jpeg = write(&card.join("A.JPG"), b"jpeg");
        let opts = options("{orig}");
        let never = AtomicBool::new(false);
        import_batch(&lib, std::slice::from_ref(&raw), &opts, &never, |_| {}).unwrap();
        let results =
            import_batch(&lib, &[raw.clone(), jpeg.clone()], &opts, &never, |_| {}).unwrap();
        assert!(results[0].skipped);
        assert!(
            !results[1].skipped && results[1].error.is_none(),
            "{:?}",
            results[1]
        );
        assert_ne!(results[1].asset_id, results[0].asset_id);
        assert_eq!(lib.summary().unwrap().assets, 2);

        let other = library(&d.path().join("other"));
        let b_raw = write(&card.join("B.NEF"), b"raw b");
        let b_jpeg = write(&card.join("B.JPG"), b"jpeg b");
        import_batch(&other, std::slice::from_ref(&b_jpeg), &opts, &never, |_| {}).unwrap();
        let results = import_batch(&other, &[b_raw, b_jpeg], &opts, &never, |_| {}).unwrap();
        assert!(
            !results[0].skipped && results[0].companion.is_none(),
            "{:?}",
            results[0]
        );
        assert!(results[1].skipped && results[1].companion_of.is_none());
        assert_eq!(other.summary().unwrap().assets, 2);
        assert_eq!(names(&other.directory.join("Capture")), ["B.JPG", "B.NEF"]);
    }

    /// Killed between copy and commit (the copies are published, nothing is
    /// recorded): an import again records those copies, the pair as one
    /// photograph, and makes no second copy.
    #[test]
    fn an_import_killed_between_copy_and_commit_is_finished_without_copies() {
        for copied in [&["A.ARW", "A.JPG"][..], &["A.ARW"][..]] {
            let d = tempfile::tempdir().unwrap();
            let lib = library(d.path());
            let card = d.path().join("card");
            let sources = vec![
                write(&card.join("A.ARW"), b"raw"),
                write(&card.join("A.JPG"), b"jpeg"),
            ];
            let capture = lib.directory.join("Capture");
            for name in copied {
                fs::copy(card.join(name), capture.join(name)).unwrap();
            }
            let results = import_batch(
                &lib,
                &sources,
                &options("{orig}"),
                &AtomicBool::new(false),
                |_| {},
            )
            .unwrap();
            assert!(results.iter().all(|r| r.error.is_none()), "{results:?}");
            assert_eq!(names(&capture), ["A.ARW", "A.JPG"], "{copied:?}");
            assert_eq!(lib.summary().unwrap().assets, 1);
            let asset = lib.asset_at(&capture.join("A.ARW")).unwrap().unwrap();
            assert_eq!(lib.companion(&asset).unwrap(), Some(capture.join("A.JPG")));
        }
    }

    /// Adding where they are pairs too: the JPEG is recorded with the RAW,
    /// nothing is written beside them.
    #[test]
    fn adding_a_pair_where_it_is_records_the_companion() {
        let d = tempfile::tempdir().unwrap();
        let lib = Library::create(&d.path().join("Main"), "Main", Mode::Catalog).unwrap();
        let shoot = d.path().join("Drive/Shoot");
        let sources = vec![
            write(&shoot.join("A.ARW"), b"raw"),
            write(&shoot.join("A.JPG"), b"jpeg"),
        ];
        let before = snapshot(&shoot);
        let results = add_batch(
            &lib,
            &sources,
            Some(raw),
            |_| Medium::Disk,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        assert_eq!(results.len(), 2);
        let asset = lib.asset_at(&sources[0]).unwrap().unwrap();
        assert_eq!(
            lib.companion(&asset).unwrap(),
            Some(sources[1].canonicalize().unwrap())
        );
        assert_eq!(results[1].companion_of, Some(asset.path.clone()));
        assert_eq!(lib.summary().unwrap().assets, 1);
        // Adding the JPEG on its own again: it is the RAW's.
        let again = add_batch(
            &lib,
            &sources[1..],
            Some(raw),
            |_| Medium::Disk,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        assert!(again[0].skipped, "{again:?}");
        assert_eq!(lib.summary().unwrap().assets, 1);
        assert_eq!(snapshot(&shoot), before);
    }

    /// Property-style: random cards (pairs, singles, the same names in two
    /// folders, files whose content repeats), imported with random cancels
    /// and runs again until done, always end with each content in the
    /// library exactly once (as a photograph or a companion), every copy
    /// accounted for, and the card unchanged.
    #[test]
    fn interrupted_and_repeated_imports_never_duplicate_or_lose_a_file() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        for round in 0..100 {
            let d = tempfile::tempdir().unwrap();
            let lib = library(d.path());
            let card = d.path().join("card/DCIM");
            let mut sources = Vec::new();
            for i in 0..(2 + next(6)) {
                let folder = card.join(format!("10{}CAM", next(2)));
                let stem = format!("DSC_{:04}", next(4) + i * 10);
                // Contents repeat now and then (the same shot twice).
                let (a, b) = (next(3), next(3));
                let content = |kind: &str| {
                    let variant = if kind == "raw" { a } else { b };
                    format!("{kind} {variant}").into_bytes()
                };
                match next(4) {
                    0 => sources.push(write(&folder.join(format!("{stem}.ARW")), &content("raw"))),
                    1 => sources.push(write(&folder.join(format!("{stem}.JPG")), &content("jpeg"))),
                    _ => {
                        sources.push(write(&folder.join(format!("{stem}.ARW")), &content("raw")));
                        sources.push(write(&folder.join(format!("{stem}.jpg")), &content("jpeg")));
                    }
                }
            }
            sources.sort();
            sources.dedup();
            let before = snapshot(&d.path().join("card"));
            let template = ["{orig}", "R{seq:2}", "{orig_base}_{seq}"][next(3) as usize];
            for run in 0..4 {
                let cancel = AtomicBool::new(false);
                let stop_after = if run < 3 {
                    next(sources.len() as u64 + 1)
                } else {
                    u64::MAX
                };
                let mut seen = 0;
                import_batch(&lib, &sources, &options(template), &cancel, |_| {
                    seen += 1;
                    if seen >= stop_after {
                        cancel.store(true, Ordering::Release);
                    }
                })
                .unwrap();
            }
            // Every distinct content of the card is in the library once.
            let capture = lib.directory.join("Capture");
            let companions = lib.companions().unwrap();
            let assets = lib.assets(None).unwrap();
            let mut held: Vec<Vec<u8>> = assets
                .iter()
                .map(|a| fs::read(&a.path).unwrap())
                .chain(companions.keys().map(|c| fs::read(c).unwrap()))
                .collect();
            let copies = names(&capture);
            assert_eq!(
                held.len(),
                copies.len(),
                "round {round}: every copy accounted for: {copies:?}"
            );
            held.sort();
            let before_len = held.len();
            held.dedup();
            assert_eq!(held.len(), before_len, "round {round}: a content twice");
            let mut wanted: Vec<Vec<u8>> = sources.iter().map(|s| fs::read(s).unwrap()).collect();
            wanted.sort();
            wanted.dedup();
            assert_eq!(
                held,
                wanted,
                "round {round}: lost or extra content; card {:?}, library {copies:?}",
                sources
                    .iter()
                    .map(|s| (s.clone(), String::from_utf8(fs::read(s).unwrap()).unwrap()))
                    .collect::<Vec<_>>()
            );
            assert_eq!(
                snapshot(&d.path().join("card")),
                before,
                "round {round}: card changed"
            );
        }
    }

    /// The wire names the app reads (`SessionWorkspace.tsx` `ImportResult`).
    #[test]
    fn import_results_keep_their_field_names_on_the_wire() {
        let result = ImportResult {
            companion: Some("/l/A.JPG".into()),
            companion_of: Some("/l/A.ARW".into()),
            ..Default::default()
        };
        let value = serde_json::to_value(&result).unwrap();
        assert_eq!(value["companion"], "/l/A.JPG");
        assert_eq!(value["companion_of"], "/l/A.ARW");
    }

    /// A pair's JPEG that is gone (or unreadable) when its turn comes: the
    /// RAW comes in alone, the JPEG's result says why.
    #[test]
    fn a_vanished_companion_leaves_its_raw_imported() {
        let d = tempfile::tempdir().unwrap();
        let lib = library(d.path());
        let card = d.path().join("card");
        let raw = write(&card.join("A.ARW"), b"raw");
        let gone = card.join("A.JPG");
        let results = import_batch(
            &lib,
            &[raw, gone.clone()],
            &options("{orig}"),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        assert!(
            results[0].error.is_none() && results[0].asset_id.is_some(),
            "{results:?}"
        );
        assert_eq!(results[1].source, gone);
        assert!(results[1].error.is_some());
        assert_eq!(names(&lib.directory.join("Capture")), ["A.ARW"]);
    }

    /// Adding where they are: a RAW reached through a symlink to another
    /// folder never takes that folder's JPEG of its name as its companion;
    /// a JPEG whose content the catalog holds (as a photograph or as a
    /// companion) is not recorded again.
    #[cfg(unix)]
    #[test]
    fn adding_pairs_checks_the_folder_and_the_content() {
        let d = tempfile::tempdir().unwrap();
        let lib = Library::create(&d.path().join("Main"), "Main", Mode::Catalog).unwrap();
        let archive = d.path().join("Drive/Archive");
        write(&archive.join("A.ARW"), b"raw a");
        write(&archive.join("A.JPG"), b"someone else's jpeg");
        let links = d.path().join("Drive/Links");
        fs::create_dir_all(&links).unwrap();
        std::os::unix::fs::symlink(archive.join("A.ARW"), links.join("A.ARW")).unwrap();
        let jpeg = write(&links.join("A.JPG"), b"jpeg a");
        let never = AtomicBool::new(false);
        let results = add_batch(
            &lib,
            &[links.join("A.ARW"), jpeg.clone()],
            Some(raw),
            |_| Medium::Disk,
            &never,
            |_| {},
        )
        .unwrap();
        let asset = lib.asset_at(&archive.join("A.ARW")).unwrap().unwrap();
        assert_eq!(lib.companion(&asset).unwrap(), None, "{results:?}");
        assert!(results[1].companion_of.is_none());

        // B's JPEG has the content of a photograph the catalog holds.
        let shoot = d.path().join("Drive/Shoot");
        let b_raw = write(&shoot.join("B.ARW"), b"raw b");
        let b_jpeg = write(&shoot.join("B.JPG"), b"jpeg a");
        let results = add_batch(
            &lib,
            &[b_raw.clone(), b_jpeg],
            Some(raw),
            |_| Medium::Disk,
            &never,
            |_| {},
        )
        .unwrap();
        assert!(results[1].skipped, "{results:?}");
        let b = lib.asset_at(&b_raw).unwrap().unwrap();
        assert_eq!(lib.companion(&b).unwrap(), None);

        // C's pair is recorded; D.JPG, C's JPEG's content elsewhere, is
        // part of C then, not another photograph.
        let c_raw = write(&shoot.join("C.ARW"), b"raw c");
        let c_jpeg = write(&shoot.join("C.JPG"), b"jpeg c");
        add_batch(
            &lib,
            &[c_raw.clone(), c_jpeg],
            Some(raw),
            |_| Medium::Disk,
            &never,
            |_| {},
        )
        .unwrap();
        let d_jpeg = write(&d.path().join("Drive/Other/D.JPG"), b"jpeg c");
        let count = lib.summary().unwrap().assets;
        let results =
            add_batch(&lib, &[d_jpeg], Some(raw), |_| Medium::Disk, &never, |_| {}).unwrap();
        assert!(
            results[0].skipped && results[0].companion_of.is_some(),
            "{results:?}"
        );
        assert_eq!(lib.summary().unwrap().assets, count);
    }
}
