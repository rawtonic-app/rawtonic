//! Transitional adapter between the inherited renderer and the portable DB.
use rawtonic_catalog::{Library, Mode, Summary, session_for};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    fs, io,
    path::{Path, PathBuf},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};
use tauri::{Emitter, Manager};

static WRITE_LOCK: Mutex<()> = Mutex::new(());
static CANCEL_IMPORT: AtomicBool = AtomicBool::new(false);
static IMPORT_RUNNING: AtomicBool = AtomicBool::new(false);
static ACTIVE_LIBRARY: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();

fn active() -> &'static Mutex<Option<PathBuf>> {
    ACTIVE_LIBRARY.get_or_init(|| Mutex::new(None))
}
fn io_error(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}
/// The photograph an editor sidecar belongs to. Its file may be missing (an
/// offline photograph the library still holds); a virtual copy's sidecar
/// names no registered photograph and stays a plain file.
fn source_for_sidecar(sidecar: &Path) -> Option<PathBuf> {
    let name = sidecar.file_name()?.to_str()?.strip_suffix(".rrdata")?;
    let source = sidecar.with_file_name(name);
    (source.is_file() || crate::formats::is_supported_image_file(name)).then_some(source)
}

/// The photograph and copy id a virtual copy's sidecar
/// (`<photo>.<copy>.rrdata`) belongs to; None for other sidecars.
pub(crate) fn copy_for_sidecar(sidecar: &Path) -> Option<(PathBuf, String)> {
    let name = sidecar.file_name()?.to_str()?.strip_suffix(".rrdata")?;
    let (source, copy) = name.rsplit_once('.')?;
    (rawtonic_catalog::valid_copy_id(copy) && crate::formats::is_supported_image_file(source))
        .then(|| (sidecar.with_file_name(source), copy.to_string()))
}

/// `source` in the form the library registers paths: canonical, also when
/// the file (or its whole drive) is gone.
fn registered_form(source: &Path) -> PathBuf {
    if let Ok(path) = source.canonicalize() {
        return path;
    }
    match (
        source.parent().and_then(|p| p.canonicalize().ok()),
        source.file_name(),
    ) {
        (Some(parent), Some(name)) => parent.join(name),
        _ => source.to_path_buf(),
    }
}
fn library_for(source: &Path) -> Option<PathBuf> {
    session_for(source).or_else(|| {
        active()
            .lock()
            .ok()?
            .clone()
            .filter(|dir| dir.join("catalog.json").is_file())
    })
}

/// The session or open catalog that manages `path`, if any.
pub(crate) fn library_dir_for(path: &Path) -> Option<PathBuf> {
    library_for(path)
}

pub fn is_managed_source(source: &Path) -> bool {
    library_for(source).is_some()
}

/// Catalog metadata of a managed source (for exports), `None` elsewhere.
pub fn catalog_metadata(source: &Path) -> Option<rawtonic_metadata::Metadata> {
    let lib = Library::open(&library_for(source)?).ok()?;
    let asset = lib.asset_at(source).ok()??;
    lib.metadata(&asset.id).ok()
}

/// Whether a virtual copy's sidecar (`<photo>.<copy>.rrdata`) is taken: its
/// file is there, or the library keeps that copy (an offline photograph's).
pub(crate) fn copy_exists(sidecar: &Path) -> bool {
    if sidecar.exists() {
        return true;
    }
    let Some((source, copy)) = copy_for_sidecar(sidecar) else {
        return false;
    };
    let Some(lib) = library_for(&source).and_then(|dir| Library::open(&dir).ok()) else {
        return false;
    };
    lib.asset_at(&source)
        .ok()
        .flatten()
        .is_some_and(|asset| matches!(lib.copy_state(&asset.id, &copy), Ok(Some(_))))
}

pub fn read_editor_sidecar(sidecar: &Path) -> Option<Value> {
    if let Some((source, copy)) = copy_for_sidecar(sidecar) {
        // A virtual copy: its sidecar file decides while the photograph is
        // there; offline, the copy the library keeps.
        if source.is_file() {
            return None;
        }
        let lib = Library::open(&library_for(&source)?).ok()?;
        let asset = lib.asset_at(&source).ok()??;
        return lib.copy_state(&asset.id, &copy).ok().flatten();
    }
    let source = source_for_sidecar(sidecar)?;
    let dir = library_for(&source)?;
    let lib = Library::open(&dir).ok()?;
    let asset = lib.asset_at(&source).ok()??;
    lib.editor_state(&asset.id).ok().flatten()
}

/// Resolve each owning library once per directory and stream primary states
/// through one query per library. The caller keeps only its listing fields;
/// full edit payloads are released before visiting the next photo.
pub(crate) fn visit_editor_sidecars(
    sources: &[PathBuf],
    mut visit: impl FnMut(&Path, &Path, Option<Value>),
) {
    let mut directories = HashMap::<PathBuf, Option<PathBuf>>::new();
    let mut groups = HashMap::<PathBuf, HashMap<PathBuf, Vec<PathBuf>>>::new();
    for source in sources {
        let Some(parent) = source.parent() else {
            continue;
        };
        let library = directories
            .entry(parent.to_owned())
            .or_insert_with(|| library_for(source));
        let Some(library) = library else { continue };
        // Offline photographs keep their edits from the library.
        let canonical = registered_form(source);
        groups
            .entry(library.clone())
            .or_default()
            .entry(canonical)
            .or_default()
            .push(source.clone());
    }
    for (directory, aliases) in groups {
        let Ok(lib) = Library::open(&directory) else {
            continue;
        };
        let mut pending: HashSet<_> = aliases.keys().cloned().collect();
        let paths = pending.clone();
        let mut publish = |canonical: &Path, state: Option<Value>| {
            for source in &aliases[canonical] {
                let Some(name) = source.file_name() else {
                    continue;
                };
                let sidecar = source.with_file_name(format!("{}.rrdata", name.to_string_lossy()));
                visit(source, &sidecar, state.clone());
            }
        };
        let result = lib.visit_editor_states(&paths, |path, state| {
            pending.remove(path);
            publish(path, state);
        });
        if result.is_ok() {
            // Unregistered paths have no database state. They still read their
            // real sidecars, without reopening the same database per photo.
            for path in pending {
                publish(&path, None);
            }
        }
    }
}

pub fn write_editor_sidecar(path: impl AsRef<Path>, bytes: impl AsRef<[u8]>) -> io::Result<()> {
    let _lock = WRITE_LOCK.lock().map_err(io_error)?;
    let path = path.as_ref();
    let bytes = bytes.as_ref();
    if let Some((source, copy)) = copy_for_sidecar(path)
        && let Some(dir) = library_for(&source)
    {
        // A virtual copy of a library photograph: the library keeps it too,
        // so it stays listed and editable while the photograph is offline;
        // its sidecar is written only beside a file that is there.
        let lib = Library::open(&dir).map_err(io_error)?;
        let state: Value = serde_json::from_slice(bytes).map_err(io_error)?;
        let online = source.is_file();
        let asset = if online {
            lib.add_asset(&source).ok()
        } else {
            lib.asset_at(&source).map_err(io_error)?
        };
        if let Some(asset) = &asset {
            lib.save_copy_state(&asset.id, &copy, &state)
                .map_err(io_error)?;
            if !online {
                return Ok(());
            }
        }
        return rawtonic_metadata::atomic_write(path, bytes).map_err(io_error);
    }
    if let Some(source) = source_for_sidecar(path)
        && let Some(dir) = library_for(&source)
        && !source.is_file()
    {
        // An offline photograph (file missing, drive disconnected) is edited
        // on its preview: the library keeps the edit; the derived sidecar and
        // XMP follow when it is relinked. Never write beside a missing file.
        let lib = Library::open(&dir).map_err(io_error)?;
        if let Some(asset) = lib.asset_at(&source).map_err(io_error)? {
            let state: Value = serde_json::from_slice(bytes).map_err(io_error)?;
            return lib.save_editor_state(&asset.id, &state).map_err(io_error);
        }
        // A virtual copy's sidecar or an unregistered file: a plain file.
        rawtonic_metadata::atomic_write(path, bytes).map_err(io_error)
    } else if let Some(source) = source_for_sidecar(path)
        && let Some(dir) = library_for(&source)
    {
        let lib = Library::open(&dir).map_err(io_error)?;
        let state: Value = serde_json::from_slice(bytes).map_err(io_error)?;
        let asset = lib.add_asset(&source).map_err(io_error)?;
        lib.save_editor_state(&asset.id, &state).map_err(io_error)?;
        // SQLite committed first. A failed derived write can be retried without losing edits.
        rawtonic_metadata::atomic_write(path, bytes).map_err(io_error)?;
        if let Err(e) = lib.sync_xmp(&asset.id) {
            log::warn!(
                "Edit saved in database; XMP pending for {}: {e}",
                source.display()
            );
        }
        Ok(())
    } else {
        rawtonic_metadata::atomic_write(path, bytes).map_err(io_error)
    }
}

/// The folder an output is written into; "." for a bare file name (a relative
/// `--output` of the headless export), whose parent is the empty path.
pub use rawtonic_export::output::{output_folder, write_new_output};

/// The photographs in a session folder (its working folders, not caches,
/// Trash or outputs), in walk order. Hidden files and folders are skipped,
/// as in ingest: macOS writes `._name` companions on exFAT and network
/// drives, which carry a photo's extension but are not photographs.
fn session_photos(lib: &Library) -> anyhow::Result<Vec<PathBuf>> {
    let mut photos = Vec::new();
    for entry in walkdir::WalkDir::new(&lib.directory)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            e.depth() == 0
                || !e.file_name().to_string_lossy().starts_with('.')
                    && !matches!(
                        e.file_name().to_str(),
                        Some(
                            "masks"
                                | "resources"
                                | "profiles"
                                | "Trash"
                                | "Output"
                                | "previews"
                                | "smart-previews"
                        )
                    )
        })
    {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let ext = entry
            .path()
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if ![
            "arw", "dng", "cr3", "cr2", "nef", "raf", "rw2", "orf", "pef", "jpg", "jpeg", "png",
            "tif", "tiff", "heic", "heif", "webp", "avif", "jxl",
        ]
        .contains(&ext.as_str())
        {
            continue;
        }
        photos.push(entry.into_path());
    }
    Ok(photos)
}

/// Brings a session up to its folder: photographs renamed or moved inside
/// it are followed with their edits, new ones are added with what their
/// sidecars hold (all in one transaction, one import batch), and
/// registered ones never edited adopt sidecars that appeared later (each in
/// one transaction). A photograph whose sidecar cannot be read is left out
/// and logged, so it is never registered without its edits.
fn index_folder(lib: &Library) -> anyhow::Result<()> {
    let known = lib.index_entries()?;
    let mut new = Vec::new();
    for photo in session_photos(lib)? {
        if let Some(asset) = known.get(&photo) {
            if !asset.needs_metadata_adoption {
                continue;
            }
            // Neutral files without sidecars need no more per-asset DB
            // queries; `adopt_contents` checks again inside its transaction,
            // so an edit or explicit clear made since the index snapshot stays.
            match rawtonic_catalog::Contents::read_sidecars(&photo) {
                Ok(contents) => {
                    lib.adopt_contents(&asset.id, &contents)?;
                }
                Err(e) => log::warn!("Sidecar not adopted: {e:#}"),
            }
        } else if let Some(id) = lib.vanished_as(&photo)?
            && let Ok(report) = lib.relink(&id, &photo, true)
        {
            // The same photograph, renamed or moved here in Finder (a
            // renamed folder): followed with its edits rather than
            // registered again as an unedited duplicate.
            after_relink(lib, &report);
        } else {
            new.push(photo);
        }
    }
    if new.is_empty() {
        return Ok(());
    }
    // Renamed (or moved and renamed) while the session was closed: a file
    // with the content hash on record for a missing photograph is that
    // photograph (Synchronize Folder's verified move), followed with its
    // edits instead of registered again unedited beside the missing one.
    // Files that may be a missing photograph without proof (probable or
    // contested matches) are not registered: Synchronize Folder settles
    // them (they are listed meanwhile, as every file of the folder).
    match follow_verified_moves(lib, &new) {
        Ok(taken) => new.retain(|photo| !taken.contains(photo)),
        Err(e) => log::warn!("Renamed photographs not followed: {e:#}"),
    }
    if new.is_empty() {
        return Ok(());
    }
    let _batch = lib.import_batch(rawtonic_catalog::ImportKind::Add);
    // A relink above may already have taken some of them.
    let report = lib.add_files(&new)?;
    for (path, error) in &report.failed {
        log::warn!("{} not added to the session: {error}", path.display());
    }
    Ok(())
}

/// Applies the verified moves among `new` (Synchronize Folder's, for these
/// files only); returns the places taken by them and the places of `new`
/// left for the user (probable and contested moves). Lists no folder and
/// hashes only files with the size and sampled fingerprint of a photograph
/// of the library, so opening a library never reads the whole of it.
fn follow_verified_moves(lib: &Library, new: &[PathBuf]) -> anyhow::Result<HashSet<PathBuf>> {
    use rawtonic_catalog::folder_sync::{SyncAction, SyncOptions};
    let never = std::sync::atomic::AtomicBool::new(false);
    let plan = lib.scan_files_sync(
        new,
        &SyncOptions {
            accept: &|path| crate::formats::is_supported_image_file(path),
            cancel: &never,
            progress: &|_| {},
        },
    )?;
    let mut taken: HashSet<PathBuf> = plan
        .probable_moves
        .iter()
        .map(|change| change.to.clone())
        .chain(plan.contested.iter().map(|file| file.path.clone()))
        .collect();
    let actions: Vec<SyncAction> = plan
        .verified_moves
        .iter()
        .filter(|change| new.contains(&change.to))
        .map(SyncAction::from)
        .collect();
    if !actions.is_empty() {
        let applied = lib.apply_folder_sync(&actions)?;
        after_relink(lib, &applied.relinked());
        taken.extend(applied.moved.into_iter().map(|(_, to)| to));
    }
    Ok(taken)
}

fn remember(app: &tauri::AppHandle, dir: &Path) -> Result<(), String> {
    *active().lock().map_err(|e| e.to_string())? = Some(dir.into());
    let cache = app.path().app_config_dir().map_err(|e| e.to_string())?;
    fs::create_dir_all(&cache).map_err(|e| e.to_string())?;
    rawtonic_metadata::atomic_write(
        &cache.join("last-library.json"),
        serde_json::to_vec(&dir).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}
#[tauri::command]
pub async fn session_create(
    directory: String,
    name: String,
    catalog: bool,
    app: tauri::AppHandle,
) -> Result<Summary, String> {
    if IMPORT_RUNNING.load(Ordering::Acquire) {
        return Err("Wait for ingest to finish before creating a library".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let lib = Library::create(
            Path::new(&directory),
            &name,
            if catalog {
                Mode::Catalog
            } else {
                Mode::Session
            },
        )
        .map_err(|e| e.to_string())?;
        remember(&app, &lib.directory)?;
        lib.summary().map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
pub async fn session_open(directory: String, app: tauri::AppHandle) -> Result<Summary, String> {
    if IMPORT_RUNNING.load(Ordering::Acquire) {
        return Err("Wait for ingest to finish before opening another library".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        #[cfg(feature = "performance-profiling")]
        let _open_stage = crate::performance::PerformanceStage::new("session_open");
        // A session lives with its photographs (Capture One): on a drive that
        // is not connected it cannot open; say so rather than a file error.
        if !Path::new(&directory).exists() {
            return Err(format!(
                "{directory} is not available. Its drive may be disconnected: connect it and open the library again."
            ));
        }
        let lib = {
            #[cfg(feature = "performance-profiling")]
            let _stage = crate::performance::PerformanceStage::new("session_db_open");
            Library::open(Path::new(&directory)).map_err(|e| e.to_string())?
        };
        {
            #[cfg(feature = "performance-profiling")]
            let _stage = crate::performance::PerformanceStage::new("session_index");
            index_folder(&lib).map_err(|e| e.to_string())?;
        }
        {
            #[cfg(feature = "performance-profiling")]
            let _stage = crate::performance::PerformanceStage::new("session_verify");
            check_files(&lib).map_err(|e| e.to_string())?;
        }
        {
            #[cfg(feature = "performance-profiling")]
            let _stage = crate::performance::PerformanceStage::new("session_remember");
            remember(&app, &lib.directory)?;
        }
        crate::folder_sync::start_checksum_pass(lib.directory.clone());
        #[cfg(feature = "performance-profiling")]
        let _stage = crate::performance::PerformanceStage::new("session_summary");
        lib.summary().map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
pub async fn session_last(app: tauri::AppHandle) -> Result<Option<Summary>, String> {
    let path = app
        .path()
        .app_config_dir()
        .map_err(|e| e.to_string())?
        .join("last-library.json");
    if !path.exists() {
        return Ok(None);
    }
    let dir: PathBuf = serde_json::from_slice(&fs::read(path).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    session_open(dir.to_string_lossy().into_owned(), app)
        .await
        .map(Some)
}
/// The remembered session folder, without opening it (for messages while it opens).
#[tauri::command]
pub fn session_last_directory(app: tauri::AppHandle) -> Result<Option<PathBuf>, String> {
    let path = app
        .path()
        .app_config_dir()
        .map_err(|e| e.to_string())?
        .join("last-library.json");
    if !path.exists() {
        return Ok(None);
    }
    serde_json::from_slice(&fs::read(path).map_err(|e| e.to_string())?)
        .map(Some)
        .map_err(|e| e.to_string())
}
#[tauri::command]
pub async fn session_summary(directory: String) -> Result<Summary, String> {
    tauri::async_runtime::spawn_blocking(move || {
        Library::open(Path::new(&directory))
            .and_then(|l| l.summary())
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}
/// One photograph found for the import dialog (`session_ingest_scan`).
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestCandidate {
    pub path: PathBuf,
    pub size: u64,
    /// The library already holds a file with this content (same size and
    /// content fingerprint); the import would skip it as a duplicate.
    pub duplicate: bool,
    /// On a memory card or on a drive (decides copy or add for a catalog).
    pub medium: rawtonic_ingest::batch::Medium,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestScan {
    pub files: Vec<IngestCandidate>,
    /// More photographs were there than the dialog lists (`SCAN_LIMIT`).
    pub truncated: bool,
}

/// The most photographs one import dialog lists (a drop of a whole drive
/// stops here rather than walking it for minutes).
const SCAN_LIMIT: usize = 20_000;

/// The photographs in what was chosen or dropped for an import: files as
/// they are, folders opened up recursively, only the formats the library
/// shows (`formats::is_supported_image_file`, as the file dialog's filter),
/// each with its size and whether the library at `directory` already holds
/// it. Reads only.
#[tauri::command]
pub async fn session_ingest_scan(
    directory: Option<String>,
    paths: Vec<String>,
) -> Result<IngestScan, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
        let (files, truncated) = rawtonic_ingest::batch::expand_sources(
            &paths,
            |path| crate::formats::is_supported_image_file(path),
            SCAN_LIMIT,
        );
        let duplicates = match directory {
            Some(directory) => Library::open(Path::new(&directory))
                .and_then(|lib| rawtonic_ingest::batch::known_in_library(&lib, &files))
                .unwrap_or_else(|e| {
                    log::warn!("Import: duplicates not checked: {e}");
                    vec![false; files.len()]
                }),
            None => vec![false; files.len()],
        };
        let files = files
            .into_iter()
            .zip(duplicates)
            .map(|(path, duplicate)| IngestCandidate {
                size: fs::metadata(&path).map(|m| m.len()).unwrap_or(0),
                medium: rawtonic_ingest::batch::source_medium(&path),
                path,
                duplicate,
            })
            .collect();
        Ok(IngestScan { files, truncated })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// A small JPEG preview of a photograph to import (the import dialog's
/// grid), from its embedded camera preview; empty when there is none quickly.
#[tauri::command]
pub async fn session_ingest_thumbnail(path: String) -> Result<tauri::ipc::Response, String> {
    tauri::async_runtime::spawn_blocking(move || {
        tauri::ipc::Response::new(
            crate::file_management::source_preview_jpeg(Path::new(&path), 320).unwrap_or_default(),
        )
    })
    .await
    .map_err(|e| e.to_string())
}

/// The import dialog's large view of a photograph to import: its largest
/// embedded camera JPEG (or the file itself for a JPEG), else its largest
/// preview scaled to `max_size`; empty when there is none. Read-only.
#[tauri::command]
pub async fn session_ingest_preview(
    path: String,
    max_size: Option<u32>,
) -> Result<tauri::ipc::Response, String> {
    let max_size = max_size.unwrap_or(2560).clamp(640, 8192);
    tauri::async_runtime::spawn_blocking(move || {
        tauri::ipc::Response::new(
            crate::file_management::source_large_preview_jpeg(Path::new(&path), max_size)
                .unwrap_or_default(),
        )
    })
    .await
    .map_err(|e| e.to_string())
}

/// ISO, shutter speed, aperture and focal length of a photograph to import,
/// for the import dialog's large view. Read-only.
#[tauri::command]
pub async fn session_ingest_capture_info(
    path: String,
) -> Result<crate::file_management::SourceCaptureInfo, String> {
    tauri::async_runtime::spawn_blocking(move || {
        crate::file_management::source_capture_info(Path::new(&path))
    })
    .await
    .map_err(|e| e.to_string())
}

/// The name the ingest would give `source` with `template` (counter
/// `sequence`, first `sequence_start`) in a library named `job`, filled from
/// its EXIF and card sidecar, and where its capture date came from: the
/// import dialog's preview. Reads the file's header and sidecar only.
#[tauri::command]
pub async fn session_naming_preview(
    source: String,
    template: String,
    job: String,
    sequence: Option<u64>,
    sequence_start: Option<u64>,
) -> Result<rawtonic_ingest::NamePreview, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let start = sequence_start.unwrap_or(1);
        rawtonic_ingest::preview_name(
            &job,
            Path::new(&source),
            &template,
            sequence.unwrap_or(start),
            start,
        )
        .map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// What `session-ingest-imported` reports for each photograph of an ingest,
/// in order, as soon as it is in the library.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestedFile {
    pub index: usize,
    pub total: usize,
    pub source: PathBuf,
    /// The copy in the library (for a skipped duplicate: the photograph the
    /// library already holds).
    pub destination: Option<PathBuf>,
    pub asset_id: Option<String>,
    pub skipped: bool,
    pub error: Option<String>,
}

impl IngestedFile {
    fn new(done: &rawtonic_ingest::batch::FileDone<'_>) -> Self {
        Self {
            index: done.index,
            total: done.total,
            source: done.result.source.clone(),
            destination: done.result.destination.clone(),
            asset_id: done.result.asset_id.clone(),
            skipped: done.result.skipped,
            error: done.result.error.clone(),
        }
    }
}

/// The RAW test an ingest pairs RAW+JPEG with (`batch::pair_sources`);
/// none when the user imports such JPEGs as photographs of their own.
fn pair_raw(separate_jpeg: Option<bool>) -> Option<fn(&Path) -> bool> {
    (!separate_jpeg.unwrap_or(false))
        .then_some(crate::formats::is_raw_file::<&Path> as fn(&Path) -> bool)
}

/// How an ingest brings photographs in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IngestMode {
    /// Verified copies into the library (sessions always).
    #[default]
    Copy,
    /// A catalog registers them where they are (never from a card).
    Add,
}

/// Copies `sources` into the library (its capture folder, or `folder` of
/// it), verified, one after the other; with `mode` `add` a catalog
/// registers them where they are instead (`ingest::batch::add_batch`). Each photograph is announced with
/// `session-ingest-imported` as soon as it is in the library, so the grid
/// shows it while the rest copy; `session-ingest-progress` carries the
/// count and speed.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn session_ingest(
    directory: String,
    sources: Vec<String>,
    template: String,
    backup: Option<String>,
    sequence_start: Option<u32>,
    folder: Option<String>,
    mode: Option<IngestMode>,
    separate_jpeg: Option<bool>,
    app: tauri::AppHandle,
) -> Result<Vec<rawtonic_ingest::ImportResult>, String> {
    if IMPORT_RUNNING.swap(true, Ordering::AcqRel) {
        return Err("An ingest is already running".into());
    }
    // The cancel flag is cleared when a run ends, not when it starts: a
    // Cancel pressed while this call was on its way still stops it.
    let library = PathBuf::from(&directory);
    let result = tauri::async_runtime::spawn_blocking(move || {
        let lib = Library::open(Path::new(&directory)).map_err(|e| e.to_string())?;
        let sources: Vec<PathBuf> = sources.into_iter().map(PathBuf::from).collect();
        // The folder list's top-level folders: below them a catalog's
        // photographs may be copied too (`ingest::destination_folder`).
        let roots: Vec<PathBuf> = crate::app_settings::load_settings(app.clone())
            .map(|settings| settings.root_folders.iter().map(PathBuf::from).collect())
            .unwrap_or_default();
        let options = rawtonic_ingest::batch::BatchOptions {
            template: &template,
            sequence_start: u64::from(sequence_start.unwrap_or(1)),
            backup: backup.as_deref().map(Path::new),
            folder: folder.as_deref().map(Path::new),
            roots: &roots,
            pair_raw: pair_raw(separate_jpeg),
        };
        let started = std::time::Instant::now();
        let report = |done: rawtonic_ingest::batch::FileDone<'_>| {
            let _ = app.emit("session-ingest-imported", IngestedFile::new(&done));
            let _ = app.emit(
                "session-ingest-progress",
                serde_json::json!({
                    "current": done.index + 1,
                    "total": done.total,
                    "bytes": done.bytes,
                    "mbps": done.bytes as f64
                        / 1_000_000.0
                        / started.elapsed().as_secs_f64().max(0.001),
                }),
            );
        };
        match mode.unwrap_or_default() {
            IngestMode::Copy => rawtonic_ingest::batch::import_batch(
                &lib,
                &sources,
                &options,
                &CANCEL_IMPORT,
                report,
            ),
            IngestMode::Add => rawtonic_ingest::batch::add_batch(
                &lib,
                &sources,
                pair_raw(separate_jpeg),
                rawtonic_ingest::batch::source_medium,
                &CANCEL_IMPORT,
                report,
            ),
        }
        .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string());
    CANCEL_IMPORT.store(false, Ordering::Release);
    IMPORT_RUNNING.store(false, Ordering::Release);
    // Photographs added where they are get their content hashes later.
    if matches!(mode, Some(IngestMode::Add)) {
        crate::folder_sync::start_checksum_pass(library);
    }
    result?
}
#[tauri::command]
pub fn session_cancel_ingest() {
    CANCEL_IMPORT.store(true, Ordering::Release);
}

#[tauri::command]
pub async fn session_flags(
    directory: String,
) -> Result<std::collections::HashMap<PathBuf, i8>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        Library::open(Path::new(&directory))
            .and_then(|lib| lib.flags())
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn session_set_flag(
    directory: String,
    paths: Vec<String>,
    flag: i8,
) -> Result<std::collections::HashMap<PathBuf, i8>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _lock = WRITE_LOCK.lock().map_err(|e| e.to_string())?;
        let lib = Library::open(Path::new(&directory)).map_err(|e| e.to_string())?;
        // Resolve and validate the whole selection before changing any metadata.
        let mut assets = std::collections::HashMap::new();
        for path in paths {
            let source = crate::file_management::parse_virtual_path(&path).0;
            let asset = lib
                .asset_at(&source)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| {
                    "This photograph is not registered in the open library".to_string()
                })?;
            assets.insert(asset.id, asset.path);
        }
        lib.set_flags(&assets.keys().cloned().collect::<Vec<_>>(), flag)
            .map_err(|e| e.to_string())?;
        for id in assets.keys() {
            if let Err(e) = lib.sync_xmp(id) {
                log::warn!("Flag saved; XMP pending: {e}");
            }
        }
        Ok(assets.into_values().map(|path| (path, flag)).collect())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Descriptive metadata of one photograph (IPTC core as Capture One's and
/// Lightroom's Metadata tools show it), stored in SQLite and the XMP sidecar.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextMetadata {
    pub title: String,
    pub description: String,
    pub creator: String,
    pub copyright: String,
    /// IPTC Core fields by key (`rawtonic_metadata::IPTC_FIELDS`), flat.
    #[serde(flatten)]
    pub iptc: std::collections::BTreeMap<String, String>,
}

/// Fields to change; absent fields keep each photograph's own value.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextMetadataUpdate {
    pub title: Option<String>,
    pub description: Option<String>,
    pub creator: Option<String>,
    pub copyright: Option<String>,
    /// IPTC fields to set (an empty value clears it in the session).
    #[serde(flatten)]
    pub iptc: std::collections::BTreeMap<String, String>,
}

/// The asset id of every path; the whole selection must be registered.
fn registered_assets(lib: &Library, paths: &[String]) -> Result<Vec<(String, String)>, String> {
    paths
        .iter()
        .map(|path| {
            let source = crate::file_management::parse_virtual_path(path).0;
            let asset = lib
                .asset_at(&source)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| {
                    "This photograph is not registered in the open library".to_string()
                })?;
            Ok((path.clone(), asset.id))
        })
        .collect()
}

impl From<rawtonic_metadata::Metadata> for TextMetadata {
    fn from(meta: rawtonic_metadata::Metadata) -> Self {
        Self {
            title: meta.title,
            description: meta.description,
            creator: meta.creator,
            copyright: meta.copyright,
            iptc: meta.iptc,
        }
    }
}

/// Title, description, creator and copyright per registered path (tests read
/// back what `write_text_metadata` stored).
#[cfg(test)]
fn read_text_metadata(
    lib: &Library,
    paths: Vec<String>,
) -> Result<HashMap<String, TextMetadata>, String> {
    let mut result = HashMap::new();
    for path in paths {
        let source = crate::file_management::parse_virtual_path(&path).0;
        let Some(asset) = lib.asset_at(&source).map_err(|e| e.to_string())? else {
            continue;
        };
        let meta = lib.metadata(&asset.id).map_err(|e| e.to_string())?;
        result.insert(path, meta.into());
    }
    Ok(result)
}

fn write_text_metadata(
    lib: &Library,
    paths: Vec<String>,
    update: &TextMetadataUpdate,
) -> Result<HashMap<String, TextMetadata>, String> {
    let assets = registered_assets(lib, &paths)?;
    let mut changed = HashMap::new();
    for (_, id) in &assets {
        if changed.contains_key(id) {
            continue;
        }
        let mut meta = lib.metadata(id).map_err(|e| e.to_string())?;
        for (field, value) in [
            (&mut meta.title, &update.title),
            (&mut meta.description, &update.description),
            (&mut meta.creator, &update.creator),
            (&mut meta.copyright, &update.copyright),
        ] {
            if let Some(value) = value {
                *field = value.trim().to_string();
            }
        }
        for (key, value) in &update.iptc {
            if rawtonic_metadata::iptc_field(key).is_none() {
                return Err(format!("Unknown metadata field {key}"));
            }
            meta.set_iptc(key, value);
        }
        meta.validate().map_err(|e| e.to_string())?;
        changed.insert(id.clone(), meta);
    }
    for (id, meta) in &changed {
        lib.set_metadata(id, meta).map_err(|e| e.to_string())?;
    }
    for id in changed.keys() {
        if let Err(e) = lib.sync_xmp(id) {
            log::warn!("Metadata saved; XMP pending: {e}");
        }
    }
    Ok(assets
        .into_iter()
        .map(|(path, id)| (path, changed[&id].clone().into()))
        .collect())
}

/// Title, description, creator and copyright of every photograph in the
/// session, by path; `None` where all four are empty.
#[tauri::command]
pub async fn session_all_text_metadata(
    directory: String,
) -> Result<HashMap<PathBuf, Option<TextMetadata>>, String> {
    with_library(directory, false, |lib| {
        Ok(lib
            .all_metadata()
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|(path, meta)| {
                let empty = meta.title.is_empty()
                    && meta.description.is_empty()
                    && meta.creator.is_empty()
                    && meta.copyright.is_empty()
                    && meta.iptc.is_empty();
                (path, (!empty).then(|| meta.into()))
            })
            .collect())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Apply the given fields to every photograph of a selection: the whole
/// selection is resolved and validated before SQLite changes, then the XMP
/// sidecars follow (a failed sidecar stays pending, as with flags).
#[tauri::command]
pub async fn session_set_text_metadata(
    directory: String,
    paths: Vec<String>,
    update: TextMetadataUpdate,
) -> Result<HashMap<String, TextMetadata>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _lock = WRITE_LOCK.lock().map_err(|e| e.to_string())?;
        let lib = Library::open(Path::new(&directory)).map_err(|e| e.to_string())?;
        write_text_metadata(&lib, paths, &update)
    })
    .await
    .map_err(|e| e.to_string())?
}

pub(crate) fn with_library<T: Send + 'static>(
    directory: String,
    write: bool,
    work: impl FnOnce(&Library) -> Result<T, String> + Send + 'static,
) -> tauri::async_runtime::JoinHandle<Result<T, String>> {
    tauri::async_runtime::spawn_blocking(move || {
        let _lock = if write {
            Some(WRITE_LOCK.lock().map_err(|e| e.to_string())?)
        } else {
            None
        };
        let lib = Library::open(Path::new(&directory)).map_err(|e| e.to_string())?;
        work(&lib)
    })
}

fn asset_ids(lib: &Library, paths: &[String]) -> Result<Vec<String>, String> {
    Ok(registered_assets(lib, paths)?
        .into_iter()
        .map(|(_, id)| id)
        .collect())
}

/// The session's albums with their photo counts.
#[tauri::command]
pub async fn session_albums(
    directory: String,
) -> Result<Vec<rawtonic_catalog::Collection>, String> {
    with_library(directory, false, |lib| {
        lib.collections().map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// A new album holding `paths` (Capture One's "New Album from selection"),
/// or with `kind` "group" a group for albums; `parent` is a group or none.
#[tauri::command]
pub async fn session_create_album(
    directory: String,
    name: String,
    paths: Vec<String>,
    kind: Option<String>,
    parent: Option<String>,
) -> Result<rawtonic_catalog::Collection, String> {
    with_library(directory, true, move |lib| {
        let kind = kind.as_deref().unwrap_or(rawtonic_catalog::ALBUM);
        let ids = if kind == rawtonic_catalog::ALBUM {
            asset_ids(lib, &paths)?
        } else {
            Vec::new()
        };
        let mut album = lib
            .create_collection_of_kind(&name, parent.as_deref(), kind)
            .map_err(|e| e.to_string())?;
        if !ids.is_empty() {
            lib.add_to_collection(&album.id, &ids)
                .map_err(|e| e.to_string())?;
        }
        album.count = ids.iter().collect::<HashSet<_>>().len() as u32;
        Ok(album)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Moves an album or group into a group (or the top level) at `index`.
#[tauri::command]
pub async fn session_move_album(
    directory: String,
    id: String,
    parent: Option<String>,
    index: usize,
) -> Result<(), String> {
    with_library(directory, true, move |lib| {
        lib.move_collection(&id, parent.as_deref(), index)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn session_rename_album(
    directory: String,
    id: String,
    name: String,
) -> Result<(), String> {
    with_library(directory, true, move |lib| {
        lib.rename_collection(&id, &name).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Removes the album; its photographs stay in the session.
#[tauri::command]
pub async fn session_delete_album(directory: String, id: String) -> Result<(), String> {
    with_library(directory, true, move |lib| {
        lib.delete_collection(&id).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn session_album_add(
    directory: String,
    id: String,
    paths: Vec<String>,
) -> Result<(), String> {
    with_library(directory, true, move |lib| {
        let ids = asset_ids(lib, &paths)?;
        lib.add_to_collection(&id, &ids).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn session_album_remove(
    directory: String,
    id: String,
    paths: Vec<String>,
) -> Result<(), String> {
    with_library(directory, true, move |lib| {
        let ids = asset_ids(lib, &paths)?;
        lib.remove_from_collection(&id, &ids)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Absolute paths of the album's photographs, in path order.
#[tauri::command]
pub async fn session_album_paths(directory: String, id: String) -> Result<Vec<PathBuf>, String> {
    with_library(directory, false, move |lib| {
        Ok(lib
            .collection_assets(&id)
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|asset| asset.path)
            .collect())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// An offline photograph as the Library shows it: where it was, and the
/// disconnected drive it is on (None: the file itself is gone).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct OfflineEntry {
    pub path: PathBuf,
    pub volume: Option<String>,
    /// Modification time (seconds) when it was registered.
    pub mtime: u64,
    /// Its folder is gone too (renamed, moved, or its drive disconnected):
    /// the Folders tool lists it as a missing folder.
    pub folder_missing: bool,
    /// Its virtual copies the library keeps (copy ids): listed with it,
    /// offline too.
    #[serde(default)]
    pub copies: Vec<String>,
}

impl From<rawtonic_catalog::OfflineAsset> for OfflineEntry {
    fn from(asset: rawtonic_catalog::OfflineAsset) -> Self {
        Self {
            volume: asset.volume,
            mtime: u64::try_from(asset.mtime).unwrap_or(0),
            folder_missing: asset.path.parent().is_none_or(|folder| !folder.is_dir()),
            path: asset.path,
            copies: Vec::new(),
        }
    }
}

/// The offline entry of `asset` with the virtual copies the library keeps.
fn offline_entry(lib: &Library, asset: rawtonic_catalog::OfflineAsset) -> OfflineEntry {
    let copies = lib.copies(&asset.id).unwrap_or_default();
    OfflineEntry {
        copies,
        ..OfflineEntry::from(asset)
    }
}

/// After a file operation touched a virtual copy's sidecar: while its
/// photograph is there, the library keeps the copy when the sidecar is
/// beside it and forgets it when it is not (deleted; put back by undo it is
/// kept again). Offline or moved photographs are left as they are.
pub(crate) fn reconcile_copy(sidecar: &Path) {
    let Some((source, copy)) = copy_for_sidecar(sidecar) else {
        return;
    };
    if !source.is_file() {
        return;
    }
    let Some(dir) = library_for(&source) else {
        return;
    };
    let Ok(_lock) = WRITE_LOCK.lock() else {
        return;
    };
    let Ok(lib) = Library::open(&dir) else {
        return;
    };
    let Ok(Some(asset)) = lib.asset_at(&source) else {
        return;
    };
    let result = match fs::read(sidecar)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
    {
        Some(state) => lib.save_copy_state(&asset.id, &copy, &state),
        None => lib.remove_copy(&asset.id, &copy).map(|_| ()),
    };
    if let Err(e) = result {
        log::warn!("Virtual copy {} not updated: {e}", sidecar.display());
    }
}

/// Records the virtual copies a folder listing found (`(photograph, copy
/// id, sidecar)`) that the library does not keep yet (made before it kept
/// them, or copied in from elsewhere), so they stay listed when the
/// photograph goes offline. Only registered photographs; reads the sidecars.
pub(crate) fn adopt_virtual_copies(found: &[(PathBuf, String, PathBuf)]) {
    let Some((first, _, _)) = found.first() else {
        return;
    };
    let Some(dir) = library_for(first) else {
        return;
    };
    let Ok(_lock) = WRITE_LOCK.lock() else {
        return;
    };
    let Ok(lib) = Library::open(&dir) else {
        return;
    };
    let mut known: HashMap<PathBuf, Option<(String, Vec<String>)>> = HashMap::new();
    for (source, copy, sidecar) in found {
        let entry = known.entry(source.clone()).or_insert_with(|| {
            let asset = lib.asset_at(source).ok()??;
            let copies = lib.copies(&asset.id).ok()?;
            Some((asset.id, copies))
        });
        let Some((id, copies)) = entry else { continue };
        if copies.contains(copy) {
            continue;
        }
        let Some(state) = fs::read(sidecar)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        else {
            continue;
        };
        if let Err(e) = lib.save_copy_state(id, copy, &state) {
            log::warn!("Virtual copy {} not kept: {e}", sidecar.display());
        } else {
            copies.push(copy.clone());
        }
    }
}

/// The photographs a library holds in `folder` (and below it with
/// `recursive`) whose file is not there, so the folder keeps listing them
/// (Lightroom's missing-photo badge; Capture One's offline images).
pub(crate) fn offline_in_folder(folder: &Path, recursive: bool) -> Vec<OfflineEntry> {
    let Some(dir) = library_for(&folder.join("_")) else {
        return Vec::new();
    };
    let Ok(lib) = Library::open(&dir) else {
        return Vec::new();
    };
    lib.offline_in(folder, recursive)
        .map(|assets| {
            assets
                .into_iter()
                .map(|asset| offline_entry(&lib, asset))
                .collect()
        })
        .unwrap_or_default()
}

/// The companion files (a RAW's JPEG, `Library::companion`) a library
/// records in `folder` or below it, under `folder` as it was given (not
/// resolved), so a listing of it leaves them out: they are part of their
/// RAW's photograph.
pub(crate) fn companions_in_folder(folder: &Path) -> HashSet<PathBuf> {
    let Some(dir) = library_for(&folder.join("_")) else {
        return HashSet::new();
    };
    let (Ok(lib), Ok(canonical)) = (Library::open(&dir), folder.canonicalize()) else {
        return HashSet::new();
    };
    lib.companions()
        .map(|companions| {
            companions
                .into_keys()
                .filter_map(|path| {
                    path.strip_prefix(&canonical)
                        .ok()
                        .map(|below| folder.join(below))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The companion file of the photograph at `photo` (it moves and goes to
/// the Trash with it), if its library records one and it is there.
pub(crate) fn companion_of(photo: &Path) -> Option<PathBuf> {
    let lib = Library::open(&library_for(photo)?).ok()?;
    let asset = lib.asset_at(photo).ok()??;
    let companion = lib.companion(&asset).ok()??;
    // Under the photograph's path as given, as the caller's other files.
    let companion = photo.with_file_name(companion.file_name()?);
    companion.is_file().then_some(companion)
}

/// Which of `paths` (whose files are gone) a library holds, with the
/// disconnected drive each is on; unregistered paths are left out.
pub(crate) fn offline_status(paths: &[PathBuf]) -> HashMap<PathBuf, OfflineEntry> {
    let mut by_library = HashMap::<PathBuf, Vec<&PathBuf>>::new();
    for path in paths {
        if let Some(dir) = library_for(path) {
            by_library.entry(dir).or_default().push(path);
        }
    }
    let mut status = HashMap::new();
    for (dir, paths) in by_library {
        let Ok(lib) = Library::open(&dir) else {
            continue;
        };
        for path in paths {
            if let Ok(Some(asset)) = lib.asset_at(path)
                && let Ok(offline) = lib.offline_asset(&asset.id)
            {
                // Keyed by the path asked for (an album's stored form).
                let entry = OfflineEntry {
                    path: path.clone(),
                    ..offline_entry(&lib, offline)
                };
                status.insert(path.clone(), entry);
            }
        }
    }
    status
}

/// The registered modification time (seconds) of a managed photograph,
/// for its cached thumbnail while its file is offline.
pub(crate) fn registered_mtime(source: &Path) -> Option<u64> {
    let lib = Library::open(&library_for(source)?).ok()?;
    let asset = lib.asset_at(source).ok()??;
    u64::try_from(asset.mtime).ok()
}

/// Every offline photograph of the library (files are checked first), for
/// the Missing Photographs view; nothing is removed.
#[tauri::command]
pub async fn session_offline(directory: String) -> Result<Vec<OfflineEntry>, String> {
    with_library(directory, true, |lib| {
        check_files(lib).map_err(|e| e.to_string())?;
        Ok(lib
            .offline_assets()
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|asset| offline_entry(lib, asset))
            .collect())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The event the webview gets when a drive was connected, ejected or
/// mounted elsewhere: the open library checks its offline photographs and
/// its folders again (a drive mounted elsewhere is followed by that check).
pub const VOLUMES_CHANGED: &str = "volumes-changed";

/// Watches the mounts for the whole run (system notifications where the
/// platform has them, else a poll of the mount list) and tells the webview.
pub fn start_mount_watch(app: tauri::AppHandle) {
    let kind = rawtonic_catalog::volume::watch_mounts(move || {
        let seen = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64() * 1000.0);
        log::info!("Mounted drives changed (seen at {seen:.0} ms since the epoch)");
        let _ = app.emit(
            VOLUMES_CHANGED,
            serde_json::json!({ "seen_epoch_ms": seen }),
        );
    });
    log::info!("Watching mounted drives by {kind:?}");
}

/// The catalog's folders grouped by drive (`catalog`), with the library's
/// top-level folders `roots` placed on theirs, for the Folders panel. Folders
/// on a disconnected drive stay listed; nothing is removed. For a session
/// (`catalog` false) the list is empty: a session's folders are its own.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CatalogFoldersView {
    pub catalog: bool,
    pub volumes: Vec<rawtonic_catalog::folders::VolumeFolders>,
}

#[tauri::command]
pub async fn catalog_folders(
    directory: String,
    roots: Vec<String>,
) -> Result<CatalogFoldersView, String> {
    with_library(directory, false, move |lib| {
        if lib.manifest.mode != Mode::Catalog {
            return Ok(CatalogFoldersView {
                catalog: false,
                volumes: Vec::new(),
            });
        }
        let records = lib.folders().map_err(|e| e.to_string())?;
        // As the Folders panel has them: it finds its trees by them.
        let roots: Vec<PathBuf> = roots.iter().map(PathBuf::from).collect();
        Ok(CatalogFoldersView {
            catalog: true,
            volumes: rawtonic_catalog::folders::by_volume(&records, &roots, &AppProbe),
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The system probe, with free space from `sysinfo` where the catalog crate
/// has none (Windows).
struct AppProbe;

impl rawtonic_catalog::folders::Probe for AppProbe {
    fn mounted(&self) -> Vec<rawtonic_catalog::Volume> {
        rawtonic_catalog::folders::SystemProbe.mounted()
    }
    fn volume_of(&self, path: &Path) -> Option<rawtonic_catalog::Volume> {
        rawtonic_catalog::folders::SystemProbe.volume_of(path)
    }
    fn is_dir(&self, path: &Path) -> bool {
        path.is_dir()
    }
    fn space(&self, mount: &Path) -> Option<(u64, u64)> {
        rawtonic_catalog::folders::SystemProbe
            .space(mount)
            .or_else(|| {
                let disks = sysinfo::Disks::new_with_refreshed_list();
                disks
                    .list()
                    .iter()
                    .filter(|disk| mount.starts_with(disk.mount_point()))
                    .max_by_key(|disk| disk.mount_point().as_os_str().len())
                    .map(|disk| (disk.available_space(), disk.total_space()))
            })
    }
}

/// After a relink: previews move to the photograph's new place (cache files
/// only, never replacing one), and the derived editor sidecar and XMP are
/// written there from the library, as an edit writes them.
pub(crate) fn after_relink(lib: &Library, report: &rawtonic_catalog::RelinkReport) {
    for (from, to) in &report.relinked {
        crate::preview_proxy::carry_over(from, to);
        if let Ok(Some(asset)) = lib.asset_at(to) {
            rederive(lib, &asset.id);
        }
    }
}

/// A photograph is back (relinked, found again, its drive reconnected): the
/// derived editor sidecar beside it is brought up to the library's state
/// (edits made while it was offline; written only when it differs) and its
/// XMP follows, as after an edit. The original is not touched.
pub(crate) fn rederive(lib: &Library, id: &str) {
    let Ok(asset) = lib.asset(id) else { return };
    let Ok(Some(state)) = lib.editor_state(id) else {
        return;
    };
    let Some(name) = asset.path.file_name() else {
        return;
    };
    let sidecar = asset
        .path
        .with_file_name(format!("{}.rrdata", name.to_string_lossy()));
    let current: Option<Value> = fs::read(&sidecar)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    if current.as_ref() != Some(&state)
        && let Ok(bytes) = serde_json::to_vec_pretty(&state)
        && let Err(e) = rawtonic_metadata::atomic_write(&sidecar, bytes)
    {
        log::warn!("Editor sidecar pending for {}: {e}", asset.path.display());
    }
    if let Err(e) = lib.sync_xmp(id) {
        log::warn!("XMP pending for {}: {e}", asset.path.display());
    }
}

/// `verify_paths` for an opened library, bringing the derived files of
/// photographs that came back up to date.
fn check_files(lib: &Library) -> anyhow::Result<usize> {
    let check = lib.check_files()?;
    // A drive followed to another mount point keeps its previews.
    for (from, to) in &check.followed {
        crate::preview_proxy::carry_over(from, to);
    }
    for id in &check.found {
        rederive(lib, id);
    }
    Ok(check.missing)
}

/// Relinks the offline photograph at `path` to the file the user located
/// (same size and content, any name); with `nearby` the other offline
/// photographs of its folder are found beside it (Lightroom's "find nearby
/// missing photos"). Originals are only read.
#[tauri::command]
pub async fn session_relink(
    directory: String,
    path: String,
    to: String,
    nearby: bool,
) -> Result<rawtonic_catalog::RelinkReport, String> {
    with_library(directory, true, move |lib| {
        let source = crate::file_management::parse_virtual_path(&path).0;
        let asset = lib
            .asset_at(&source)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "This photograph is not registered in the open library".to_string())?;
        let report = lib
            .relink(&asset.id, Path::new(&to), nearby)
            .map_err(|e| e.to_string())?;
        after_relink(lib, &report);
        Ok(report)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Relinks the offline photographs registered in or below the folder
/// `from` to the same places in or below `to` (a moved, renamed or copied
/// folder or drive). Originals are only read.
#[tauri::command]
pub async fn session_relink_folder(
    directory: String,
    from: String,
    to: String,
) -> Result<rawtonic_catalog::RelinkReport, String> {
    with_library(directory, true, move |lib| {
        let report = lib
            .relink_folder(Path::new(&from), Path::new(&to))
            .map_err(|e| e.to_string())?;
        after_relink(lib, &report);
        Ok(report)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Encode generated images into a temporary file without ever replacing a source.
pub fn save_new_image(image: &image::DynamicImage, path: &Path) -> anyhow::Result<()> {
    let parent = output_folder(path).ok_or_else(|| anyhow::anyhow!("No output folder"))?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    image.write_to(temp.as_file_mut(), image::ImageFormat::from_path(path)?)?;
    temp.as_file().sync_all()?;
    temp.persist_noclobber(path).map_err(|e| e.error)?;
    #[cfg(unix)]
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_renamed_in_finder_is_followed_with_its_edits_not_duplicated() {
        let temp = tempfile::tempdir().unwrap();
        let lib = Library::create(temp.path(), "Renamed", Mode::Session).unwrap();
        let day = lib.directory.join("Capture/Day 1");
        fs::create_dir_all(&day).unwrap();
        for (name, rating) in [("A.ARW", 3), ("B.ARW", 4)] {
            fs::write(day.join(name), format!("fixture {name}")).unwrap();
            write_editor_sidecar(
                day.join(format!("{name}.rrdata")),
                format!(r#"{{"rating":{rating},"tags":[],"adjustments":{{"exposure":1}}}}"#),
            )
            .unwrap();
        }
        index_folder(&lib).unwrap();
        let a = lib.asset_at(&day.join("A.ARW")).unwrap().unwrap().id;
        let renamed = lib.directory.join("Capture/Day one");
        fs::rename(&day, &renamed).unwrap();

        index_folder(&lib).unwrap();
        assert_eq!(check_files(&lib).unwrap(), 0);
        assert_eq!(lib.summary().unwrap().assets, 2, "no duplicates");
        let moved = lib.asset_at(&renamed.join("A.ARW")).unwrap().unwrap();
        assert_eq!(moved.id, a);
        assert_eq!(moved.rating, 3);
        assert_eq!(
            lib.asset_at(&renamed.join("B.ARW"))
                .unwrap()
                .unwrap()
                .rating,
            4
        );
    }

    /// macOS's `._` companions (exFAT, network drives) and other hidden
    /// files are not photographs of the session.
    #[test]
    fn hidden_files_are_not_registered_as_photographs() {
        let temp = tempfile::tempdir().unwrap();
        let lib = Library::create(temp.path(), "Hidden", Mode::Session).unwrap();
        let day = lib.directory.join("Capture");
        fs::create_dir_all(day.join(".Spotlight-V100")).unwrap();
        fs::write(day.join("A.ARW"), "fixture A").unwrap();
        fs::write(day.join("._A.ARW"), "AppleDouble").unwrap();
        fs::write(day.join(".Spotlight-V100/B.JPG"), "fixture B").unwrap();

        index_folder(&lib).unwrap();
        assert_eq!(session_photos(&lib).unwrap(), vec![day.join("A.ARW")]);
        assert_eq!(lib.summary().unwrap().assets, 1);
    }

    /// A photograph renamed while the session was closed is followed with
    /// its edits when the session opens (its content hash is on record), not
    /// registered again unedited beside the missing original. Two files
    /// that both may be a missing photograph wait for Synchronize Folder; a
    /// copy of a photograph that is there is a photograph of the folder.
    #[test]
    fn a_photograph_renamed_while_closed_is_followed_by_its_content_hash() {
        let temp = tempfile::tempdir().unwrap();
        let lib = Library::create(temp.path(), "Renamed", Mode::Session).unwrap();
        let capture = lib.directory.join("Capture");
        for (name, seed) in [("A.ARW", 1u8), ("B.ARW", 2), ("C.ARW", 3)] {
            let mut bytes = vec![seed; 300_000];
            bytes[150_000] = seed + 1;
            fs::write(capture.join(name), bytes).unwrap();
        }
        index_folder(&lib).unwrap();
        let id = |name: &str| lib.asset_at(&capture.join(name)).unwrap().unwrap().id;
        let (a, b, c) = (id("A.ARW"), id("B.ARW"), id("C.ARW"));
        for id in [&a, &b] {
            let mut meta = lib.metadata(id).unwrap();
            meta.rating = 4;
            lib.set_metadata(id, &meta).unwrap();
        }
        lib.fill_all_checksums(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        // While closed: A renamed and moved; B renamed with a copy beside
        // it (either may be B); C copied.
        fs::create_dir(capture.join("Day 2")).unwrap();
        fs::rename(capture.join("A.ARW"), capture.join("Day 2/A final.ARW")).unwrap();
        fs::rename(capture.join("B.ARW"), capture.join("B renamed.ARW")).unwrap();
        fs::copy(capture.join("B renamed.ARW"), capture.join("B copy.ARW")).unwrap();
        fs::copy(capture.join("C.ARW"), capture.join("C copy.ARW")).unwrap();

        index_folder(&lib).unwrap();
        let moved = lib
            .asset_at(&capture.join("Day 2/A final.ARW"))
            .unwrap()
            .unwrap();
        assert_eq!((moved.id.as_str(), moved.rating), (a.as_str(), 4));
        for name in ["B renamed.ARW", "B copy.ARW"] {
            assert!(
                lib.asset_at(&capture.join(name)).unwrap().is_none(),
                "{name}"
            );
        }
        assert_eq!(check_files(&lib).unwrap(), 1, "B waits for the user");
        assert_eq!(lib.asset(&b).unwrap().rating, 4);
        assert_ne!(id("C copy.ARW"), c);
        assert_eq!(lib.summary().unwrap().assets, 4);
        // Synchronize Folder offers B's choice.
        let plan = lib
            .scan_folder_sync(
                None,
                &rawtonic_catalog::folder_sync::SyncOptions {
                    accept: &|p| crate::formats::is_supported_image_file(p),
                    cancel: &std::sync::atomic::AtomicBool::new(false),
                    progress: &|_| {},
                },
            )
            .unwrap();
        assert_eq!(plan.contested.len(), 2, "{plan:#?}");
        assert!(plan.contested.iter().all(|f| f.moves[0].asset_id == b));
    }

    /// A crash or full disk while a session indexes new photographs leaves
    /// none of them half added (without the edits of their sidecars); the
    /// next open adds them complete.
    #[test]
    fn indexing_new_photographs_is_all_or_nothing_and_a_retry_completes_them() {
        let temp = tempfile::tempdir().unwrap();
        let lib = Library::create(temp.path(), "Atomic", Mode::Session).unwrap();
        let capture = lib.directory.join("Capture");
        fs::write(capture.join("A.ARW"), b"fixture A").unwrap();
        fs::write(
            capture.join("A.ARW.rrdata"),
            br#"{"rating":4,"tags":["user:pier"],"adjustments":{"exposure":1}}"#,
        )
        .unwrap();
        fs::write(capture.join("B.ARW"), b"fixture B").unwrap();
        fs::write(capture.join("B.xmp"), r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Rating="2"/></rdf:RDF></x:xmpmeta>"#).unwrap();
        fs::write(capture.join("C.ARW"), b"fixture C").unwrap();
        lib.inject_write_failure("recipes").unwrap();
        assert!(index_folder(&lib).is_err());
        assert_eq!(lib.summary().unwrap().assets, 0);
        lib.clear_write_failures().unwrap();
        index_folder(&lib).unwrap();
        let a = lib.asset_at(&capture.join("A.ARW")).unwrap().unwrap();
        assert_eq!(a.rating, 4);
        assert_eq!(lib.metadata(&a.id).unwrap().keywords, ["pier"]);
        assert_eq!(
            lib.editor_state(&a.id).unwrap().unwrap()["adjustments"]["exposure"],
            1
        );
        assert_eq!(
            lib.asset_at(&capture.join("B.ARW"))
                .unwrap()
                .unwrap()
                .rating,
            2
        );
        assert_eq!(lib.summary().unwrap().assets, 3);
        let batch = lib.previous_import().unwrap().unwrap();
        assert_eq!(
            (batch.kind, batch.count),
            (rawtonic_catalog::ImportKind::Add, 3)
        );
        // Reopening adds nothing and starts no new batch.
        index_folder(&lib).unwrap();
        assert_eq!(lib.previous_import().unwrap().unwrap().id, batch.id);
        assert_eq!(fs::read(capture.join("A.ARW")).unwrap(), b"fixture A");
    }

    #[test]
    fn virtual_copies_stay_listed_and_editable_while_their_photograph_is_offline() {
        let temp = tempfile::tempdir().unwrap();
        let lib = Library::create(temp.path(), "Copies", Mode::Session).unwrap();
        let day = lib.directory.join("Capture");
        let source = day.join("A.ARW");
        fs::write(&source, b"copies fixture").unwrap();
        write_editor_sidecar(
            day.join("A.ARW.rrdata"),
            br#"{"version":1,"rating":3,"tags":[],"adjustments":{}}"#,
        )
        .unwrap();
        index_folder(&lib).unwrap();
        // A copy made before the library kept copies: only its sidecar.
        let old = day.join("A.ARW.0a0a0a.rrdata");
        fs::write(
            &old,
            br#"{"version":1,"rating":2,"tags":[],"adjustments":{}}"#,
        )
        .unwrap();
        // A copy made now is kept by the library as it is written.
        let new = day.join("A.ARW.b1b2b3.rrdata");
        write_editor_sidecar(
            &new,
            br#"{"version":1,"rating":4,"tags":[],"adjustments":{}}"#,
        )
        .unwrap();
        assert!(new.is_file());
        assert_eq!(
            copy_for_sidecar(&new),
            Some((source.clone(), "b1b2b3".to_string()))
        );
        assert_eq!(copy_for_sidecar(&day.join("A.ARW.rrdata")), None);
        // A folder listing finds the old one and the library keeps it too.
        adopt_virtual_copies(&[
            (source.clone(), "0a0a0a".into(), old.clone()),
            (source.clone(), "b1b2b3".into(), new.clone()),
        ]);
        let id = lib.asset_at(&source).unwrap().unwrap().id;
        assert_eq!(lib.copies(&id).unwrap(), ["0a0a0a", "b1b2b3"]);

        // The drive goes: photograph, sidecars and XMP with it.
        let away = temp.path().join("away");
        fs::create_dir_all(&away).unwrap();
        for entry in fs::read_dir(&day).unwrap().flatten() {
            fs::rename(entry.path(), away.join(entry.file_name())).unwrap();
        }
        lib.verify_paths().unwrap();
        let listed = offline_in_folder(&day, false);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].copies, ["0a0a0a", "b1b2b3"]);
        let files =
            crate::file_management::offline_image_files(listed, &crate::AppSettings::default());
        let shown: Vec<_> = files
            .iter()
            .map(|f| (f.path.clone(), f.is_virtual_copy, f.is_offline, f.rating))
            .collect();
        let path = source.to_string_lossy().into_owned();
        assert_eq!(
            shown,
            [
                (path.clone(), false, true, 3),
                (format!("{path}?vc=0a0a0a"), true, true, 2),
                (format!("{path}?vc=b1b2b3"), true, true, 4),
            ]
        );
        assert_eq!(
            offline_status(std::slice::from_ref(&source))[&source]
                .copies
                .len(),
            2
        );

        // Edited offline: the library keeps the edit, no file is written.
        write_editor_sidecar(
            &new,
            br#"{"version":1,"rating":1,"tags":[],"adjustments":{}}"#,
        )
        .unwrap();
        assert!(!new.exists());
        assert_eq!(read_editor_sidecar(&new).unwrap()["rating"], 1);

        // Back; a copy deleted from disk afterwards is forgotten.
        for entry in fs::read_dir(&away).unwrap().flatten() {
            fs::rename(entry.path(), day.join(entry.file_name())).unwrap();
        }
        check_files(&lib).unwrap();
        assert_eq!(
            read_editor_sidecar(&new),
            None,
            "online, the sidecar decides"
        );
        fs::remove_file(&old).unwrap();
        reconcile_copy(&old);
        assert_eq!(lib.copies(&id).unwrap(), ["b1b2b3"]);
        assert_eq!(fs::read(&source).unwrap(), b"copies fixture");
    }

    #[test]
    fn a_photograph_coming_back_gets_the_edits_made_while_it_was_offline() {
        let temp = tempfile::tempdir().unwrap();
        let lib = Library::create(temp.path(), "Back", Mode::Session).unwrap();
        let source = lib.directory.join("Capture/A.ARW");
        fs::write(&source, b"back fixture").unwrap();
        let sidecar = lib.directory.join("Capture/A.ARW.rrdata");
        write_editor_sidecar(&sidecar, br#"{"rating":2,"tags":[],"adjustments":{}}"#).unwrap();
        index_folder(&lib).unwrap();
        // The drive goes away (here: the file and its sidecars are moved off).
        let away = temp.path().join("away");
        fs::create_dir_all(&away).unwrap();
        fs::rename(&source, away.join("A.ARW")).unwrap();
        fs::rename(&sidecar, away.join("A.ARW.rrdata")).unwrap();
        fs::rename(source.with_extension("xmp"), away.join("A.xmp")).unwrap();
        assert_eq!(check_files(&lib).unwrap(), 1);
        // A renamed folder: its photograph lists under a missing folder.
        fs::rename(lib.directory.join("Capture"), lib.directory.join("Renamed")).unwrap();
        assert!(offline_in_folder(&lib.directory.join("Capture"), false)[0].folder_missing);
        fs::rename(lib.directory.join("Renamed"), lib.directory.join("Capture")).unwrap();
        write_editor_sidecar(&sidecar, br#"{"rating":5,"tags":[],"adjustments":{}}"#).unwrap();
        // It comes back with its old sidecar.
        fs::rename(away.join("A.ARW"), &source).unwrap();
        fs::rename(away.join("A.ARW.rrdata"), &sidecar).unwrap();
        fs::rename(away.join("A.xmp"), source.with_extension("xmp")).unwrap();
        assert_eq!(check_files(&lib).unwrap(), 0);
        let derived: Value = serde_json::from_slice(&fs::read(&sidecar).unwrap()).unwrap();
        assert_eq!(derived["rating"], 5);
        assert_eq!(fs::read(&source).unwrap(), b"back fixture");
        // Nothing came back the second time: nothing is rewritten.
        let before = fs::metadata(&sidecar).unwrap().modified().unwrap();
        assert_eq!(check_files(&lib).unwrap(), 0);
        assert_eq!(fs::metadata(&sidecar).unwrap().modified().unwrap(), before);
    }

    #[test]
    fn offline_photographs_keep_their_edits_listing_and_relink_without_stray_files() {
        let temp = tempfile::tempdir().unwrap();
        let lib = Library::create(temp.path(), "Offline", Mode::Session).unwrap();
        let day = lib.directory.join("Capture/Day 1");
        fs::create_dir_all(&day).unwrap();
        let source = day.join("A.ARW");
        fs::write(&source, b"offline fixture").unwrap();
        let sidecar = day.join("A.ARW.rrdata");
        write_editor_sidecar(
            &sidecar,
            br#"{"rating":3,"tags":[],"adjustments":{"exposure":1}}"#,
        )
        .unwrap();
        index_folder(&lib).unwrap();

        // The file leaves the session in Finder, with its sidecars.
        let away = temp.path().join("away");
        fs::create_dir_all(&away).unwrap();
        fs::rename(&source, away.join("A.ARW")).unwrap();
        fs::rename(&sidecar, away.join("A.ARW.rrdata")).unwrap();
        fs::rename(day.join("A.xmp"), away.join("A.xmp")).unwrap();
        lib.verify_paths().unwrap();

        // Still read from the library and listed in its folder and albums.
        let state = read_editor_sidecar(&sidecar).unwrap();
        assert_eq!(state["rating"], 3);
        let listed = offline_in_folder(&day, false);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].path, source);
        assert!(!listed[0].folder_missing, "Day 1 is still there");
        assert_eq!(offline_in_folder(&lib.directory, true).len(), 1);
        assert!(offline_status(std::slice::from_ref(&source)).contains_key(&source));
        assert!(offline_status(&[day.join("Unknown.ARW")]).is_empty());
        assert!(registered_mtime(&source).is_some_and(|t| t > 0));
        let mut visited = Vec::new();
        visit_editor_sidecars(std::slice::from_ref(&source), |path, _, state| {
            visited.push((path.to_path_buf(), state.map(|s| s["rating"].clone())));
        });
        assert_eq!(visited, [(source.clone(), Some(serde_json::json!(3)))]);

        // Edited on its preview: the library keeps it, nothing is written
        // beside the missing file.
        write_editor_sidecar(
            &sidecar,
            br#"{"rating":5,"tags":[],"adjustments":{"exposure":2}}"#,
        )
        .unwrap();
        assert!(!sidecar.exists());
        assert!(!source.with_extension("xmp").exists());
        assert_eq!(read_editor_sidecar(&sidecar).unwrap()["rating"], 5);

        // Back into the session under another folder: relinked, the derived
        // sidecar follows; the original is untouched.
        let back = lib.directory.join("Selects/A.ARW");
        fs::rename(away.join("A.ARW"), &back).unwrap();
        let asset = lib.asset_at(&source).unwrap().unwrap();
        let report = lib.relink(&asset.id, &back, false).unwrap();
        after_relink(&lib, &report);
        assert_eq!(fs::read(&back).unwrap(), b"offline fixture");
        let derived: Value =
            serde_json::from_slice(&fs::read(lib.directory.join("Selects/A.ARW.rrdata")).unwrap())
                .unwrap();
        assert_eq!(derived["adjustments"]["exposure"], 2);
        assert!(offline_in_folder(&lib.directory, true).is_empty());
    }

    #[test]
    fn reindex_adopts_late_sidecars_discovers_new_sources_and_keeps_saved_clears() {
        let temp = tempfile::tempdir().unwrap();
        let lib = Library::create(temp.path(), "Reindex", Mode::Session).unwrap();
        let source = lib.directory.join("Capture/existing.arw");
        fs::write(&source, b"existing fixture").unwrap();
        index_folder(&lib).unwrap();
        let existing = lib.asset_at(&source).unwrap().unwrap();
        let sidecar = source.with_file_name("existing.arw.rrdata");
        fs::write(
            &sidecar,
            br#"{"rating":4,"tags":["late"],"adjustments":{"exposure":1}}"#,
        )
        .unwrap();
        index_folder(&lib).unwrap();
        assert_eq!(lib.asset_at(&source).unwrap().unwrap().id, existing.id);
        assert_eq!(lib.metadata(&existing.id).unwrap().rating, 4);

        let cleared = serde_json::json!({"rating":0,"tags":[],"adjustments":{}});
        lib.save_editor_state(&existing.id, &cleared).unwrap();
        let saved = lib.editor_state(&existing.id).unwrap();
        let version = lib.primary_version(&existing.id).unwrap();
        let recipe = lib.recipe(&version).unwrap();
        fs::write(
            &sidecar,
            br#"{"rating":5,"tags":["stale"],"adjustments":{"exposure":2}}"#,
        )
        .unwrap();
        let new_source = lib.directory.join("Capture/new.arw");
        fs::write(&new_source, b"new fixture").unwrap();
        index_folder(&lib).unwrap();
        assert!(lib.asset_at(&new_source).unwrap().is_some());
        assert_eq!(lib.summary().unwrap().assets, 2);
        assert_eq!(lib.editor_state(&existing.id).unwrap(), saved);
        assert_eq!(lib.recipe(&version).unwrap(), recipe);
        assert_eq!(fs::read(source).unwrap(), b"existing fixture");
        assert_eq!(fs::read(new_source).unwrap(), b"new fixture");
    }

    #[test]
    fn iptc_fields_apply_to_a_selection_reach_the_xmp_and_unknown_ones_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::create(&dir.path().join("Job"), "Job", Mode::Session).unwrap();
        let photo = lib.directory.join("Capture/A.ARW");
        fs::write(&photo, b"a").unwrap();
        lib.add_asset(&photo).unwrap();
        let path = photo.to_string_lossy().into_owned();
        let mut update = TextMetadataUpdate::default();
        update.iptc.insert("city".into(), " Ascona ".into());
        update
            .iptc
            .insert("creatorEmail".into(), "n@example.org".into());
        let written = write_text_metadata(&lib, vec![path.clone()], &update).unwrap();
        assert_eq!(
            written[&path].iptc.get("city").map(String::as_str),
            Some("Ascona")
        );
        let xmp = fs::read_to_string(photo.with_extension("xmp")).unwrap();
        assert!(
            xmp.contains("<photoshop:City>Ascona</photoshop:City>"),
            "{xmp}"
        );
        assert!(xmp.contains("<Iptc4xmpCore:CiEmailWork>n@example.org</Iptc4xmpCore:CiEmailWork>"));
        // The frontend reads them flat beside the title.
        let json = serde_json::to_value(&written[&path]).unwrap();
        assert_eq!(json["city"], "Ascona");
        let mut bad = TextMetadataUpdate::default();
        bad.iptc.insert("nonsense".into(), "x".into());
        assert!(write_text_metadata(&lib, vec![path], &bad).is_err());
    }

    #[test]
    fn text_metadata_applies_to_a_selection_and_reaches_xmp() {
        let temp = tempfile::tempdir().unwrap();
        let lib = Library::create(temp.path(), "Text", Mode::Session).unwrap();
        let mut paths = Vec::new();
        for name in ["a", "b"] {
            let source = lib.directory.join(format!("Capture/{name}.arw"));
            fs::write(&source, b"original").unwrap();
            lib.add_asset(&source).unwrap();
            paths.push(source.to_string_lossy().into_owned());
        }
        let first = TextMetadataUpdate {
            title: Some(" Harbour ".into()),
            creator: Some("N. F.".into()),
            ..Default::default()
        };
        write_text_metadata(&lib, paths.clone(), &first).unwrap();
        // A second update keeps fields it does not name; a virtual copy
        // resolves to its original.
        let copy = format!("{}?vc=abc123", paths[1]);
        let second = TextMetadataUpdate {
            copyright: Some("© 2026".into()),
            ..Default::default()
        };
        let written = write_text_metadata(&lib, vec![copy.clone()], &second).unwrap();
        assert_eq!(written[&copy].title, "Harbour");
        assert_eq!(written[&copy].copyright, "© 2026");
        let read = read_text_metadata(&lib, paths.clone()).unwrap();
        assert_eq!(read[&paths[0]].copyright, "");
        assert_eq!(read[&paths[1]].creator, "N. F.");
        let xmp = fs::read_to_string(Path::new(&paths[1]).with_extension("xmp")).unwrap();
        let (meta, _) = rawtonic_metadata::read_xmp(&xmp).unwrap();
        assert_eq!(
            (meta.title.as_str(), meta.copyright.as_str()),
            ("Harbour", "© 2026")
        );
        assert_eq!(fs::read(&paths[1]).unwrap(), b"original");
        // Nothing changes when one path of the selection is unknown.
        let unknown = lib.directory.join("Capture/unknown.arw");
        let error = write_text_metadata(
            &lib,
            vec![paths[0].clone(), unknown.to_string_lossy().into_owned()],
            &TextMetadataUpdate {
                title: Some("Changed".into()),
                ..Default::default()
            },
        );
        assert!(error.is_err());
        assert_eq!(
            read_text_metadata(&lib, paths.clone()).unwrap()[&paths[0]].title,
            "Harbour"
        );
    }

    #[test]
    fn exports_carry_session_creator_copyright_and_description() {
        let temp = tempfile::tempdir().unwrap();
        let lib = Library::create(temp.path(), "Export", Mode::Session).unwrap();
        let source = lib.directory.join("Capture/photo.jpg");
        let encode = || {
            let mut bytes = Vec::new();
            image::DynamicImage::new_rgb8(8, 8)
                .write_to(&mut io::Cursor::new(&mut bytes), image::ImageFormat::Jpeg)
                .unwrap();
            bytes
        };
        fs::write(&source, encode()).unwrap();
        let asset = lib.add_asset(&source).unwrap();
        let path = source.to_string_lossy().into_owned();
        // The metadata sources the app's export passes (EXIF cache, catalog).
        let app_export_metadata = |path: &str| {
            let source = Path::new(path);
            crate::exif_processing::export_metadata_with(
                path,
                "jpeg",
                true,
                false,
                8,
                8,
                false,
                &|| crate::exif_processing::read_rrexif_sidecar(source),
                &|| catalog_metadata(source),
            )
        };
        let exported = |path: &str| {
            let mut bytes = encode();
            let metadata = app_export_metadata(path).unwrap();
            crate::exif_processing::embed_export_metadata(&mut bytes, "jpeg", &metadata).unwrap();
            let exif = exif::Reader::new()
                .read_from_container(&mut io::Cursor::new(&bytes))
                .unwrap();
            [
                exif::Tag::Artist,
                exif::Tag::Copyright,
                exif::Tag::ImageDescription,
            ]
            .map(|tag| {
                exif.get_field(tag, exif::In::PRIMARY)
                    .map(|f| f.display_value().to_string().trim_matches('"').to_string())
            })
        };
        assert_eq!(exported(&path), [None, None, None]);
        write_text_metadata(
            &lib,
            vec![path.clone()],
            &TextMetadataUpdate {
                title: Some("Pier".into()),
                description: Some("Evening at the pier".into()),
                creator: Some("N. F.".into()),
                copyright: Some("© 2026 N. F.".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let [artist, copyright, description] = exported(&path);
        assert_eq!(artist.as_deref(), Some("N. F."));
        assert!(copyright.unwrap().contains("2026 N. F."));
        assert_eq!(description.as_deref(), Some("Evening at the pier"));
        assert_eq!(lib.metadata(&asset.id).unwrap().title, "Pier");
        // The same fields, with the title, travel in the embedded XMP packet.
        let xmp = app_export_metadata(&path).unwrap().xmp.unwrap();
        let (read, _) = rawtonic_metadata::read_xmp(&xmp).unwrap();
        assert_eq!(
            (read.title.as_str(), read.creator.as_str()),
            ("Pier", "N. F.")
        );
        assert!(xmp.contains(crate::exif_processing::SOFTWARE));
    }

    #[test]
    fn albums_from_a_selection_resolve_virtual_copies_and_list_their_photographs() {
        use tauri::async_runtime::block_on;
        let temp = tempfile::tempdir().unwrap();
        let lib = Library::create(temp.path(), "Albums", Mode::Session).unwrap();
        let directory = lib.directory.to_string_lossy().into_owned();
        let mut paths = Vec::new();
        for name in ["b", "a"] {
            let source = lib.directory.join(format!("Capture/{name}.arw"));
            fs::write(&source, b"original").unwrap();
            lib.add_asset(&source).unwrap();
            paths.push(source.to_string_lossy().into_owned());
        }
        drop(lib);
        let selection = vec![
            paths[0].clone(),
            format!("{}?vc=abc123", paths[0]),
            paths[1].clone(),
        ];
        let album = block_on(session_create_album(
            directory.clone(),
            "Picks".into(),
            selection,
            None,
            None,
        ))
        .unwrap();
        // A group holds albums: the selection is not added to it.
        let group = block_on(session_create_album(
            directory.clone(),
            "Client".into(),
            paths.clone(),
            Some("group".into()),
            None,
        ))
        .unwrap();
        assert_eq!((group.kind.as_str(), group.count), ("group", 0));
        block_on(session_move_album(
            directory.clone(),
            album.id.clone(),
            Some(group.id.clone()),
            0,
        ))
        .unwrap();
        let moved = block_on(session_albums(directory.clone())).unwrap();
        assert_eq!(
            moved.iter().find(|a| a.id == album.id).unwrap().parent,
            Some(group.id.clone())
        );
        block_on(session_delete_album(directory.clone(), group.id.clone())).unwrap();
        let album = block_on(session_create_album(
            directory.clone(),
            "Picks".into(),
            vec![paths[0].clone(), paths[1].clone()],
            None,
            None,
        ))
        .unwrap();
        assert_eq!(album.count, 2);
        let listed = block_on(session_album_paths(directory.clone(), album.id.clone())).unwrap();
        assert_eq!(
            listed
                .iter()
                .map(|p| p.file_name().unwrap().to_owned())
                .collect::<Vec<_>>(),
            ["a.arw", "b.arw"]
        );
        // An unregistered photograph rejects the whole request.
        let outside = temp.path().join("Capture/outside.arw");
        fs::write(&outside, b"x").unwrap();
        let mixed = vec![paths[0].clone(), outside.to_string_lossy().into_owned()];
        assert!(
            block_on(session_album_remove(
                directory.clone(),
                album.id.clone(),
                mixed
            ))
            .is_err()
        );
        block_on(session_album_remove(
            directory.clone(),
            album.id.clone(),
            vec![paths[1].clone()],
        ))
        .unwrap();
        block_on(session_rename_album(
            directory.clone(),
            album.id.clone(),
            "Final".into(),
        ))
        .unwrap();
        let albums = block_on(session_albums(directory.clone())).unwrap();
        assert_eq!((albums[0].name.as_str(), albums[0].count), ("Final", 1));
        block_on(session_delete_album(directory.clone(), album.id)).unwrap();
        assert!(block_on(session_albums(directory)).unwrap().is_empty());
        assert_eq!(fs::read(&paths[0]).unwrap(), b"original");
    }

    #[test]
    fn outputs_with_a_bare_file_name_are_written_into_the_current_folder() {
        assert_eq!(output_folder(Path::new("out.jpg")), Some(Path::new(".")));
        assert_eq!(output_folder(Path::new("a/out.jpg")), Some(Path::new("a")));
        assert_eq!(output_folder(Path::new("/")), None);
    }

    #[test]
    fn initial_xmp_label_survives_editor_adoption() {
        let temp = tempfile::tempdir().unwrap();
        let lib = Library::create(temp.path(), "Labels", Mode::Session).unwrap();
        let source = lib.directory.join("Capture/chart.arw");
        fs::write(&source, b"original").unwrap();
        let asset = lib.add_asset(&source).unwrap();
        fs::write(source.with_extension("xmp"), r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Label="Red" xmp:Rating="3"/></rdf:RDF></x:xmpmeta>"#).unwrap();
        index_folder(&lib).unwrap();
        let metadata = lib.metadata(&asset.id).unwrap();
        assert_eq!(metadata.label, "Red");
        assert_eq!(metadata.rating, 3);
        assert_eq!(
            lib.editor_state(&asset.id).unwrap().unwrap()["tags"],
            serde_json::json!(["color:red"])
        );
        assert_eq!(fs::read(source).unwrap(), b"original");
    }

    #[tokio::test]
    async fn reopening_unedited_flagged_assets_does_not_adopt_conflicting_xmp() {
        let temp = tempfile::tempdir().unwrap();
        let lib = Library::create(temp.path(), "Pending flags", Mode::Session).unwrap();
        let source = lib.directory.join("Capture/chart.arw");
        fs::write(&source, b"original").unwrap();
        let asset = lib.add_asset(&source).unwrap();
        assert!(!lib.metadata_initialized(&asset.id).unwrap());
        let path = source.to_string_lossy().into_owned();
        let directory = lib.directory.to_string_lossy().into_owned();
        session_set_flag(directory.clone(), vec![path.clone()], 1)
            .await
            .unwrap();
        let xmp_path = source.with_extension("xmp");
        // A valid external edit retains the old flag while changing the digest.
        let external = format!(
            "{}<!-- external edit -->",
            fs::read_to_string(&xmp_path).unwrap()
        );
        fs::write(&xmp_path, &external).unwrap();
        session_set_flag(directory, vec![path], 0).await.unwrap();
        assert!(lib.metadata_initialized(&asset.id).unwrap());
        assert!(lib.editor_state(&asset.id).unwrap().is_none());
        assert_eq!(lib.summary().unwrap().xmp_pending, 1);
        index_folder(&lib).unwrap();
        assert_eq!(lib.metadata(&asset.id).unwrap().flag, 0);
        assert_eq!(lib.summary().unwrap().xmp_pending, 1);
        assert_eq!(fs::read_to_string(xmp_path).unwrap(), external);
        assert_eq!(fs::read(source).unwrap(), b"original");
    }

    #[tokio::test]
    async fn flag_batch_rejects_unregistered_sources_and_deduplicates_virtual_copies() {
        let temp = tempfile::tempdir().unwrap();
        let lib = Library::create(temp.path(), "Culling", Mode::Session).unwrap();
        let source = lib.directory.join("Capture/chart.arw");
        let unregistered = lib.directory.join("Capture/unregistered.arw");
        fs::write(&source, b"original").unwrap();
        fs::write(&unregistered, b"unregistered original").unwrap();
        let asset = lib.add_asset(&source).unwrap();
        let directory = lib.directory.to_string_lossy().into_owned();
        let path = source.to_string_lossy().into_owned();
        assert!(
            session_set_flag(
                directory.clone(),
                vec![path.clone(), unregistered.to_string_lossy().into_owned()],
                1
            )
            .await
            .is_err()
        );
        assert_eq!(lib.metadata(&asset.id).unwrap().flag, 0);
        assert!(lib.asset_at(&unregistered).unwrap().is_none());
        let changed = session_set_flag(
            directory.clone(),
            vec![path.clone(), format!("{path}?vc=copy")],
            1,
        )
        .await
        .unwrap();
        assert_eq!(changed.len(), 1);
        assert_eq!(changed.get(&source), Some(&1));
        assert_eq!(session_flags(directory).await.unwrap(), changed);
        assert_eq!(fs::read(source).unwrap(), b"original");
        assert_eq!(fs::read(unregistered).unwrap(), b"unregistered original");
    }

    #[test]
    fn managed_edits_have_one_xmp_writer() {
        let temp = tempfile::tempdir().unwrap();
        let lib = Library::create(temp.path(), "XMP bridge", Mode::Session).unwrap();
        let source = temp.path().join("Capture/chart.jpg");
        fs::write(&source, b"synthetic source").unwrap();
        let sidecar = source.with_file_name("chart.jpg.rrdata");
        let state = serde_json::json!({"rating":4,"tags":["test"],"adjustments":{"exposure":0.5}});
        write_editor_sidecar(&sidecar, serde_json::to_vec(&state).unwrap()).unwrap();
        let xmp = source.with_extension("xmp");
        let saved = fs::read(&xmp).unwrap();
        let metadata = crate::exif_processing::load_sidecar(&sidecar);
        crate::file_management::sync_metadata_to_xmp(&source, &metadata, true);
        assert_eq!(saved, fs::read(&xmp).unwrap());
        assert_eq!(lib.summary().unwrap().xmp_pending, 0);
        assert_eq!(read_editor_sidecar(&sidecar).unwrap()["rating"], 4);
        assert_eq!(fs::read(&source).unwrap(), b"synthetic source");
    }

    #[test]
    fn rendered_output_never_replaces_an_existing_file() {
        let temp = tempfile::tempdir().unwrap();
        let output = temp.path().join("original.png");
        fs::write(&output, b"existing original").unwrap();
        let img = image::DynamicImage::new_rgb8(2, 2);
        assert!(save_new_image(&img, &output).is_err());
        assert!(write_new_output(&output, b"replacement").is_err());
        assert_eq!(fs::read(&output).unwrap(), b"existing original");
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
    }
}

/// A full snapshot for transfer/backup, or a verified restoration into a new
/// folder. Shares the ingest gate so originals cannot be imported concurrently.
#[tauri::command]
pub async fn library_copy(
    source: String,
    destination: String,
    purpose: String,
    app: tauri::AppHandle,
) -> Result<rawtonic_ingest::transfer::Receipt, String> {
    if crate::hot_folder::hot_folder_status().is_some() {
        return Err("Stop the watched folder before copying a library".into());
    }
    if IMPORT_RUNNING.swap(true, Ordering::AcqRel) {
        return Err("Wait for the current import or library copy to finish".into());
    }
    // Cleared when the copy ends (as for an ingest), so an early Cancel counts.
    tauri::async_runtime::spawn_blocking(move || {
        struct Release;
        impl Drop for Release {
            fn drop(&mut self) {
                CANCEL_IMPORT.store(false, Ordering::Release);
                IMPORT_RUNNING.store(false, Ordering::Release);
            }
        }
        let _release = Release;
        rawtonic_ingest::transfer::copy_library(
            Path::new(&source),
            Path::new(&destination),
            &purpose,
            &CANCEL_IMPORT,
            |progress| {
                let _ = app.emit("library-copy-progress", progress);
            },
        )
        .map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| e.to_string())?
}
