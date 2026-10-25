//! Open With and Edit With: the selected photographs' originals, or renders
//! of them, in another program.
//!
//! Open With hands the original files to the program the user picks and
//! never writes or moves them. Edit With renders each photograph through the
//! normal export (`edit_with_start` queues it, pinned, with the chosen
//! recipe to a free name beside the original), then `edit_with_finish` registers each render
//! as a photograph of its own stacked with its original
//! (`Library::set_derived_from`), opens them all in the program in one
//! launch and watches them, so a save there refreshes the thumbnail.
//!
//! The programs offered for a file come from LaunchServices on macOS. Other
//! systems report `supported: false` and the menus show the entries disabled.
use crate::export_queue::ExportRequest;
use rawtonic_catalog::Library;
use rawtonic_export::sidecars::parse_virtual_path;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{Duration, SystemTime},
};
use tauri::{AppHandle, Emitter, Manager};

/// Programs remembered as recently used, newest first.
const RECENT_APPS: usize = 5;
/// Edit With renders watched for saves, the newest kept.
const WATCHED_FILES: usize = 500;
/// How often watched renders are checked for a save.
const WATCH_INTERVAL: Duration = Duration::from_millis(1000);
/// What an Edit With render's name adds to its original's.
const EDIT_SUFFIX: &str = "_edit";

/// A program that can open the files.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppChoice {
    /// The application bundle (`/Applications/Preview.app`).
    pub path: String,
    pub name: String,
    /// The system's default program for the first file.
    pub is_default: bool,
    pub is_recent: bool,
    /// A small PNG of the program's icon as a data URL.
    pub icon: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppChoices {
    /// Whether this system can list and open programs here.
    pub supported: bool,
    pub apps: Vec<AppChoice>,
}

/// What Open With / Edit With remember between launches.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct OpenWithPrefs {
    /// Application paths, newest first.
    pub recent_apps: Vec<String>,
    /// The Edit With recipe last used (an export preset id or a built-in one).
    pub edit_recipe: Option<String>,
}

/// One render: the photograph (a virtual copy keeps its `?vc=`) and the
/// file made from it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditTarget {
    pub source: String,
    pub target: String,
    /// The session or catalog that held the photograph when the render was
    /// planned: the render joins that one even if another opens meanwhile.
    #[serde(default)]
    pub library: Option<String>,
    /// What the render is registered with, read with its pinned export
    /// (`plan_edits`): the photograph's library metadata, with a virtual
    /// copy's own rating, label and keywords.
    #[serde(default)]
    pub metadata: Option<rawtonic_metadata::Metadata>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditWithResult {
    /// Renders registered in the library and stacked with their originals.
    pub linked: usize,
    /// Messages for renders that could not be registered or linked.
    pub problems: Vec<String>,
}

/// The default program first, then the recently used ones (newest first),
/// then the others by name; each program once.
pub fn order_apps(
    mut apps: Vec<AppChoice>,
    default: Option<&str>,
    recent: &[String],
) -> Vec<AppChoice> {
    let mut seen = HashSet::new();
    apps.retain(|app| seen.insert(app.path.clone()));
    for app in &mut apps {
        app.is_default = Some(app.path.as_str()) == default;
        app.is_recent = recent.contains(&app.path);
    }
    let rank = |app: &AppChoice| {
        if app.is_default {
            (0, 0)
        } else if let Some(i) = recent.iter().position(|r| *r == app.path) {
            (1, i)
        } else {
            (2, 0)
        }
    };
    apps.sort_by(|a, b| {
        rank(a)
            .cmp(&rank(b))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    apps
}

/// One program per name: copies of an application in several places (old
/// versions, disk images) show once, the one in an Applications folder.
pub fn one_per_name(apps: Vec<AppChoice>) -> Vec<AppChoice> {
    let installed = |app: &AppChoice| {
        app.path.starts_with("/Applications/") || app.path.starts_with("/System/Applications/")
    };
    let mut kept: Vec<AppChoice> = Vec::new();
    for app in apps {
        match kept.iter_mut().find(|k| k.name == app.name) {
            Some(k) if !installed(k) && installed(&app) => *k = app,
            Some(_) => {}
            None => kept.push(app),
        }
    }
    kept
}

/// `path` first among `recent`, at most `RECENT_APPS` kept.
pub fn remember_recent(recent: &mut Vec<String>, path: &str) {
    recent.retain(|r| r != path);
    recent.insert(0, path.to_string());
    recent.truncate(RECENT_APPS);
}

/// The original files of `paths` (virtual copies are their original's
/// file), each once, in order.
fn original_files(paths: &[String]) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    paths
        .iter()
        .map(|path| parse_virtual_path(path).0)
        .filter(|path| seen.insert(path.clone()))
        .collect()
}

/// A free name for a render of `original` beside it: `NAME_edit.ext`, then
/// `NAME_edit-2.ext`, … Names in `taken` (other renders of the same run)
/// count as used. A folder that cannot be listed or a library that cannot
/// say whether a name is registered frees no name: planning fails. The
/// render itself is written no-clobber, so a file that appears in between
/// makes it fail rather than be overwritten.
pub fn edit_target(
    original: &Path,
    extension: &str,
    taken: &HashSet<PathBuf>,
    registered: &dyn Fn(&Path) -> Result<bool, String>,
) -> Result<PathBuf, String> {
    let folder = original.parent().unwrap_or(Path::new(""));
    let stem = original
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    // Names differing only in case count as taken on every volume, so a
    // render never depends on whether the volume tells them apart.
    let lower = |path: &Path| path.file_name().map(|n| n.to_string_lossy().to_lowercase());
    let mut used: HashSet<String> = fs::read_dir(folder)
        .and_then(|entries| {
            entries
                .map(|entry| Ok(entry?.file_name().to_string_lossy().to_lowercase()))
                .collect()
        })
        .map_err(|e| format!("{} cannot be listed: {e}", folder.display()))?;
    used.extend(
        taken
            .iter()
            .filter(|t| t.parent() == Some(folder))
            .filter_map(|t| lower(t)),
    );
    for n in 1u32.. {
        let suffix = if n == 1 {
            String::new()
        } else {
            format!("-{n}")
        };
        let path = folder.join(format!("{stem}{EDIT_SUFFIX}{suffix}.{extension}"));
        let Some(name) = lower(&path) else {
            continue;
        };
        // A sidecar left by an earlier render of that name
        // (`NAME_edit.tiff.rrdata`, `NAME_edit.xmp`) or a library
        // photograph still registered there (its file gone) keeps it
        // taken: the new render must not become that photograph.
        let name_stem = name
            .rsplit_once('.')
            .map_or(name.as_str(), |(stem, _)| stem);
        let free = !used.contains(&name)
            && !used
                .iter()
                .any(|entry| entry.starts_with(&format!("{name}.")))
            && !used.contains(&format!("{name_stem}.xmp"));
        if free && !registered(&path)? {
            return Ok(path);
        }
    }
    Err(format!(
        "No free name for a render of {}.",
        original.display()
    ))
}

fn prefs_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_config_dir()
        .map_err(|e| e.to_string())?
        .join("open_with.json"))
}

fn prefs_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn read_prefs(app: &AppHandle) -> OpenWithPrefs {
    prefs_path(app)
        .ok()
        .and_then(|path| fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn update_prefs(app: &AppHandle, change: impl FnOnce(&mut OpenWithPrefs)) -> Result<(), String> {
    let _lock = prefs_lock().lock().map_err(|e| e.to_string())?;
    let mut prefs = read_prefs(app);
    change(&mut prefs);
    let path = prefs_path(app)?;
    if let Some(folder) = path.parent() {
        fs::create_dir_all(folder).map_err(|e| e.to_string())?;
    }
    let bytes = serde_json::to_vec_pretty(&prefs).map_err(|e| e.to_string())?;
    rawtonic_metadata::atomic_write(&path, &bytes).map_err(|e| e.to_string())
}

/// A file of `extension` to ask the system about before any exists: an
/// empty file in the app's cache (the type follows from the extension).
fn probe_file(app: &AppHandle, extension: &str) -> Result<PathBuf, String> {
    let extension: String = extension
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    if extension.is_empty() {
        return Err("No file type to open.".into());
    }
    let folder = app
        .path()
        .app_cache_dir()
        .map_err(|e| e.to_string())?
        .join("open-with");
    fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
    let path = folder.join(format!("probe.{extension}"));
    if !path.exists() {
        rawtonic_metadata::atomic_write(&path, b"").map_err(|e| e.to_string())?;
    }
    Ok(path)
}

#[cfg(target_os = "macos")]
mod platform {
    use super::AppChoice;
    use base64::Engine;
    use objc2::{AnyThread, rc::autoreleasepool};
    use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSWorkspace};
    use objc2_foundation::{
        NSBundle, NSDictionary, NSFileManager, NSPoint, NSRect, NSSize, NSString, NSURL,
    };
    use std::{collections::HashSet, path::Path, process::Command};

    pub const SUPPORTED: bool = true;

    fn url(path: &Path) -> Option<objc2::rc::Retained<NSURL>> {
        Some(NSURL::fileURLWithPath(&NSString::from_str(path.to_str()?)))
    }

    fn icon(app: &str) -> Option<String> {
        let workspace = NSWorkspace::sharedWorkspace();
        let image = workspace.iconForFile(&NSString::from_str(app));
        let mut rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(32.0, 32.0));
        // SAFETY: `rect` lives for the call; no context or hints.
        let cg = unsafe { image.CGImageForProposedRect_context_hints(&mut rect, None, None) }?;
        let rep = NSBitmapImageRep::initWithCGImage(NSBitmapImageRep::alloc(), &cg);
        // SAFETY: an empty properties dictionary.
        let png = unsafe {
            rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new())
        }?;
        Some(format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(png.to_vec())
        ))
    }

    /// Every copy of Rawtonic itself (other builds or versions) shares its
    /// bundle identifier.
    fn is_own(app: &NSURL) -> bool {
        let own = NSBundle::mainBundle().bundleIdentifier();
        let theirs = NSBundle::bundleWithURL(app).and_then(|bundle| bundle.bundleIdentifier());
        own.is_some() && own == theirs
    }

    fn choice(app: &NSURL) -> Option<AppChoice> {
        if is_own(app) {
            return None;
        }
        let path = app.path()?.to_string();
        let name = NSFileManager::defaultManager()
            .displayNameAtPath(&NSString::from_str(&path))
            .to_string();
        let name = name.strip_suffix(".app").unwrap_or(&name).to_string();
        Some(AppChoice {
            icon: icon(&path),
            path,
            name,
            is_default: false,
            is_recent: false,
        })
    }

    /// The programs that open every one of `files` and the default
    /// program of the first.
    pub fn apps_for(files: &[&Path]) -> (Vec<AppChoice>, Option<String>) {
        autoreleasepool(|_| {
            let workspace = NSWorkspace::sharedWorkspace();
            let mut common: Option<Vec<objc2::rc::Retained<NSURL>>> = None;
            for file in files {
                let Some(file_url) = url(file) else { continue };
                let apps = workspace.URLsForApplicationsToOpenURL(&file_url).to_vec();
                common = Some(match common {
                    None => apps,
                    Some(before) => {
                        let these: HashSet<String> = apps
                            .iter()
                            .filter_map(|a| a.path().map(|p| p.to_string()))
                            .collect();
                        before
                            .into_iter()
                            .filter(|a| a.path().is_some_and(|p| these.contains(&p.to_string())))
                            .collect()
                    }
                });
            }
            let default = files
                .first()
                .and_then(|file| url(file))
                .and_then(|file_url| workspace.URLForApplicationToOpenURL(&file_url))
                .and_then(|app| app.path())
                .map(|p| p.to_string());
            let apps = common
                .unwrap_or_default()
                .iter()
                .filter_map(|app| choice(app))
                .collect();
            (apps, default)
        })
    }

    /// Opens `files` in `app` in one launch.
    pub fn open(app: &Path, files: &[std::path::PathBuf]) -> Result<(), String> {
        let status = Command::new("/usr/bin/open")
            .arg("-a")
            .arg(app)
            .args(files)
            .status()
            .map_err(|e| e.to_string())?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("{} could not open the files.", app.display()))
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::AppChoice;
    use std::path::Path;

    pub const SUPPORTED: bool = false;

    pub fn apps_for(_files: &[&Path]) -> (Vec<AppChoice>, Option<String>) {
        (Vec::new(), None)
    }

    pub fn open(_app: &Path, _files: &[std::path::PathBuf]) -> Result<(), String> {
        Err("Open With is available on macOS only.".into())
    }
}

/// One file per file type of `files` (the programs offered must open all).
fn one_per_type(files: &[PathBuf]) -> Vec<&Path> {
    let mut seen = HashSet::new();
    files
        .iter()
        .filter(|file| {
            seen.insert(
                file.extension()
                    .map(|e| e.to_string_lossy().to_lowercase())
                    .unwrap_or_default(),
            )
        })
        .map(PathBuf::as_path)
        .collect()
}

fn choices(app: &AppHandle, files: Vec<PathBuf>) -> AppChoices {
    if !platform::SUPPORTED {
        return AppChoices {
            supported: false,
            apps: Vec::new(),
        };
    }
    let (apps, default) = platform::apps_for(&one_per_type(&files));
    let recent = read_prefs(app).recent_apps;
    // The default and the recent programs keep their exact copy; the
    // others show once per name.
    let (mut apps, others): (Vec<_>, Vec<_>) = apps.into_iter().partition(|choice| {
        default.as_deref() == Some(choice.path.as_str()) || recent.contains(&choice.path)
    });
    let others: Vec<_> = one_per_name(others)
        .into_iter()
        .filter(|other| !apps.iter().any(|kept| kept.name == other.name))
        .collect();
    apps.extend(others);
    AppChoices {
        supported: true,
        apps: order_apps(apps, default.as_deref(), &recent),
    }
}

/// The programs that open the originals of `paths`.
#[tauri::command]
pub async fn open_with_apps(paths: Vec<String>, app: AppHandle) -> Result<AppChoices, String> {
    tauri::async_runtime::spawn_blocking(move || choices(&app, original_files(&paths)))
        .await
        .map_err(|e| e.to_string())
}

/// The programs that open files of `extension` (Edit With's renders).
#[tauri::command]
pub async fn edit_with_apps(extension: String, app: AppHandle) -> Result<AppChoices, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let probe = probe_file(&app, &extension)?;
        Ok(choices(&app, vec![probe]))
    })
    .await
    .map_err(|e| e.to_string())?
}

fn checked_app(app: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(app);
    if path.is_absolute() && path.is_dir() && path.extension().is_some_and(|e| e == "app") {
        Ok(path)
    } else {
        Err(format!("{app} is not an application."))
    }
}

fn open_in(app: &AppHandle, program: &str, files: &[PathBuf]) -> Result<(), String> {
    let program = checked_app(program)?;
    if let Some(missing) = files.iter().find(|file| !file.is_file()) {
        return Err(format!("{} is not available.", missing.display()));
    }
    platform::open(&program, files)?;
    let program = program.to_string_lossy().into_owned();
    update_prefs(app, |prefs| {
        remember_recent(&mut prefs.recent_apps, &program)
    })
}

/// Opens the originals of `paths` in `program`; nothing is written or moved.
#[tauri::command]
pub async fn open_with_open(
    program: String,
    paths: Vec<String>,
    app: AppHandle,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let files = original_files(&paths);
        if files.is_empty() {
            return Err("No photographs to open.".to_string());
        }
        open_in(&app, &program, &files)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn open_with_prefs(app: AppHandle) -> OpenWithPrefs {
    read_prefs(&app)
}

#[tauri::command]
pub fn edit_with_set_recipe(recipe: String, app: AppHandle) -> Result<(), String> {
    update_prefs(&app, |prefs| prefs.edit_recipe = Some(recipe))
}

/// What `edit_with_start` queued: each render's file and its export job.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditWithStart {
    pub targets: Vec<EditTarget>,
    pub jobs: Vec<String>,
}

/// Queues the renders of `paths`, each with `recipe` (an export request
/// without paths or output) to the free name `plan_edits` gives it.
#[tauri::command]
pub async fn edit_with_start(
    paths: Vec<String>,
    recipe: ExportRequest,
    app: AppHandle,
) -> Result<EditWithStart, String> {
    let state = app.clone();
    let (targets, requests) = tauri::async_runtime::spawn_blocking(move || {
        let state = state.state::<crate::app_state::AppState>();
        plan_edits(
            paths,
            recipe,
            &|original| crate::session_bridge::library_dir_for(original),
            &|adjustments| {
                let mut adjustments = adjustments.clone();
                crate::adjustment_utils::hydrate_adjustments(&state, &mut adjustments);
                rawtonic_engine::image_loader::has_generated_areas(&adjustments)
            },
        )
    })
    .await
    .map_err(|e| e.to_string())??;
    let jobs = crate::export_queue::enqueue_pinned(&app, requests)?;
    Ok(EditWithStart { targets, jobs })
}

/// The error `plan_edits` starts with when a photograph has areas a
/// generative model made: its render would hand them to another program
/// whose saves need not keep the AI marking, so Edit With is not offered
/// for it yet (the frontend says so; Open With hands over the original).
pub const GENERATED_AREAS: &str = "generated-areas";

/// The export of each photograph of `paths` with `recipe` to its own file
/// beside its original, pinned (`ExportRequest::pin`): what is checked here
/// is what renders. Refused as a whole, before anything is written, if one
/// has generated areas (`generated`, on its pinned recipe) or no name can
/// be reserved for it.
fn plan_edits(
    paths: Vec<String>,
    recipe: ExportRequest,
    library_for: &dyn Fn(&Path) -> Option<PathBuf>,
    generated: &dyn Fn(&serde_json::Value) -> bool,
) -> Result<(Vec<EditTarget>, Vec<ExportRequest>), String> {
    let mut targets = plan_targets(paths, &recipe.output_format, library_for)?;
    let requests = targets
        .iter_mut()
        .map(|target| {
            let mut request = ExportRequest {
                paths: vec![target.source.clone()],
                output_folder_or_file: target.target.clone(),
                is_explicit_file_path: true,
                pinned: None,
                ..recipe.clone()
            };
            // Pinned under the library the name was planned with, never
            // the one open now (another may have been opened meanwhile).
            crate::session_bridge::with_culling_owner(target.library.as_deref(), || request.pin())?;
            // Registered with the metadata the render embeds, not with what
            // the library holds once it has rendered.
            let pinned = request.pinned.iter().flatten().next();
            if let (Some(dir), Some(metadata)) =
                (&target.library, pinned.and_then(|p| p.metadata.clone()))
            {
                let lib = Library::open(Path::new(dir))
                    .map_err(|e| format!("{dir} cannot be read: {e:#}"))?;
                target.metadata = Some(match copy_state(&lib, &target.source) {
                    Some(state) => copy_metadata(metadata, &state),
                    None => metadata,
                });
            }
            Ok(request)
        })
        .collect::<Result<Vec<_>, String>>()?;
    let names: Vec<String> = requests
        .iter()
        .filter(|request| {
            request
                .pinned
                .iter()
                .flatten()
                .any(|photo| generated(&photo.adjustments))
        })
        .map(|request| {
            let (file, _) = parse_virtual_path(&request.paths[0]);
            file.file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
        })
        .collect();
    if !names.is_empty() {
        return Err(format!("{GENERATED_AREAS}: {}", names.join(", ")));
    }
    Ok((targets, requests))
}

/// Free names beside the originals for the renders of `paths`, with the
/// library of each original from `library_for`.
fn plan_targets(
    paths: Vec<String>,
    extension: &str,
    library_for: &dyn Fn(&Path) -> Option<PathBuf>,
) -> Result<Vec<EditTarget>, String> {
    if extension.is_empty() || !extension.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(format!("{extension:?} is not a file extension."));
    }
    let mut taken = HashSet::new();
    let mut seen = HashSet::new();
    let mut libraries: HashMap<PathBuf, Library> = HashMap::new();
    paths
        .into_iter()
        .filter(|path| seen.insert(path.clone()))
        .map(|source| {
            let (original, _) = parse_virtual_path(&source);
            if !original.is_file() {
                return Err(format!("{} is not available.", original.display()));
            }
            let library = library_for(&original);
            let lib = match &library {
                Some(dir) => Some(match libraries.entry(dir.clone()) {
                    std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                    std::collections::hash_map::Entry::Vacant(entry) => entry.insert(
                        Library::open(dir)
                            .map_err(|e| format!("{} cannot be read: {e:#}", dir.display()))?,
                    ),
                }),
                None => None,
            };
            let registered = |path: &Path| match &lib {
                Some(lib) => lib
                    .registered_ignoring_case(path)
                    .map_err(|e| format!("{e:#}")),
                None => Ok(false),
            };
            let target = edit_target(&original, extension, &taken, &registered)?;
            taken.insert(target.clone());
            Ok(EditTarget {
                library: library.map(|dir| dir.to_string_lossy().into_owned()),
                source,
                target: target.to_string_lossy().into_owned(),
                metadata: None,
            })
        })
        .collect()
}

/// What a virtual copy shows of its own (rating, colour label, keywords)
/// from its editor state, over its photograph's `metadata`.
fn copy_metadata(
    mut metadata: rawtonic_metadata::Metadata,
    state: &serde_json::Value,
) -> rawtonic_metadata::Metadata {
    if let Some(rating) = state.get("rating").and_then(serde_json::Value::as_u64) {
        metadata.rating = rating.min(5) as u8;
    }
    if let Some(tags) = state.get("tags").and_then(serde_json::Value::as_array) {
        let tags: Vec<&str> = tags.iter().filter_map(serde_json::Value::as_str).collect();
        metadata.label = tags
            .iter()
            .find_map(|tag| tag.strip_prefix(crate::tagging::COLOR_TAG_PREFIX))
            .map(|colour| {
                let mut chars = colour.chars();
                chars.next().map_or_else(String::new, |first| {
                    first.to_uppercase().chain(chars).collect()
                })
            })
            .unwrap_or_default();
        metadata.keywords = tags
            .iter()
            .filter_map(|tag| tag.strip_prefix(crate::tagging::USER_TAG_PREFIX))
            .map(str::to_string)
            .collect();
    }
    metadata
}

/// The editor state of the virtual copy `source` names (None for a
/// photograph): the library's, else its sidecar's.
fn copy_state(lib: &Library, source: &str) -> Option<serde_json::Value> {
    let (file, copy) = parse_virtual_path(source);
    let copy_id = source.rsplit_once("?vc=").map(|(_, id)| id)?;
    let original = lib.asset_at(&file).ok().flatten();
    original
        .and_then(|asset| lib.copy_state(&asset.id, copy_id).ok().flatten())
        .or_else(|| {
            fs::read(&copy)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
        })
}

/// Registers `render` in `lib` with `metadata` (`EditTarget::metadata`)
/// and stacks it with the photograph of `source` (a virtual copy stacks
/// with its photograph).
pub fn link_render(
    lib: &Library,
    source: &str,
    render: &Path,
    metadata: Option<&rawtonic_metadata::Metadata>,
) -> anyhow::Result<()> {
    let (file, _) = parse_virtual_path(source);
    let original = match lib.asset_at(&file)? {
        Some(asset) => asset,
        None => lib.add_asset(&file)?,
    };
    anyhow::ensure!(
        !lib.registered_ignoring_case(render)?,
        "{} is already a photograph of the library",
        render.display()
    );
    let edit = lib.add_asset(render)?;
    lib.set_derived_from(&edit.id, &original.id)?;
    if let Some(metadata) = metadata {
        lib.set_metadata(&edit.id, metadata)?;
    }
    Ok(())
}

fn link_renders(items: &[EditTarget]) -> EditWithResult {
    let mut result = EditWithResult {
        linked: 0,
        problems: Vec::new(),
    };
    let mut by_library: HashMap<PathBuf, Vec<&EditTarget>> = HashMap::new();
    // Outside a session or catalog the render is a file in the folder.
    for item in items {
        if let Some(dir) = &item.library {
            by_library.entry(PathBuf::from(dir)).or_default().push(item);
        }
    }
    for (dir, items) in by_library {
        let linked = crate::session_bridge::with_library_blocking(&dir, |lib| {
            Ok(items
                .iter()
                .map(|item| {
                    link_render(
                        lib,
                        &item.source,
                        Path::new(&item.target),
                        item.metadata.as_ref(),
                    )
                    .map_err(|e| format!("{}: {e:#}", item.target))
                })
                .collect::<Vec<_>>())
        });
        match linked {
            Ok(outcomes) => {
                for outcome in outcomes {
                    match outcome {
                        Ok(()) => result.linked += 1,
                        Err(e) => result.problems.push(e),
                    }
                }
            }
            Err(e) => result.problems.push(e),
        }
    }
    result
}

/// After the renders of `items` were exported: registers and stacks them,
/// watches them for saves and opens them in `program` in one launch.
#[tauri::command]
pub async fn edit_with_finish(
    items: Vec<EditTarget>,
    program: String,
    app: AppHandle,
) -> Result<EditWithResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let renders: Vec<PathBuf> = items
            .iter()
            .map(|item| PathBuf::from(&item.target))
            .collect();
        if let Some(missing) = renders.iter().find(|file| !file.is_file()) {
            return Err(format!("{} was not rendered.", missing.display()));
        }
        let result = link_renders(&items);
        watch(&app, &items);
        open_in(&app, &program, &renders)?;
        Ok(result)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// What a watched file looked like when last checked.
type Stamp = Option<(SystemTime, u64)>;

fn stamp(path: &Path) -> Stamp {
    let meta = fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

#[derive(Default)]
struct Watched {
    /// Each render with its last seen stamp, in the order they were added.
    files: Vec<(PathBuf, Stamp)>,
    /// The library each render was registered in.
    libraries: HashMap<PathBuf, PathBuf>,
    /// Renders that changed and wait for one unchanged check (the program
    /// may still be writing).
    settling: HashMap<PathBuf, Stamp>,
}

impl Watched {
    fn add(&mut self, path: &Path, library: Option<&Path>) {
        self.files.retain(|(p, _)| p != path);
        self.files.push((path.to_path_buf(), stamp(path)));
        match library {
            Some(dir) => self.libraries.insert(path.to_path_buf(), dir.to_path_buf()),
            None => self.libraries.remove(path),
        };
        let excess = self.files.len().saturating_sub(WATCHED_FILES);
        for (gone, _) in self.files.drain(..excess) {
            self.libraries.remove(&gone);
        }
    }

    /// The renders whose save has settled since the last check, each with
    /// its library.
    fn saved_in(&mut self) -> Vec<(PathBuf, Option<PathBuf>)> {
        self.saved()
            .into_iter()
            .map(|path| {
                let library = self.libraries.get(&path).cloned();
                (path, library)
            })
            .collect()
    }

    /// The renders whose save has settled since the last check.
    fn saved(&mut self) -> Vec<PathBuf> {
        let mut saved = Vec::new();
        for (path, seen) in &mut self.files {
            let now = stamp(path);
            if let Some(before) = self.settling.get(path) {
                if *before == now {
                    self.settling.remove(path);
                    *seen = now;
                    if now.is_some() {
                        saved.push(path.clone());
                    }
                } else {
                    self.settling.insert(path.clone(), now);
                }
            } else if now != *seen {
                self.settling.insert(path.clone(), now);
            }
        }
        saved
    }
}

fn watched() -> &'static Mutex<Watched> {
    static WATCHED: OnceLock<Mutex<Watched>> = OnceLock::new();
    WATCHED.get_or_init(Default::default)
}

/// The library records a render's size, time and fingerprint; after a save
/// they are its new ones (Locate and Synchronize Folder recognise it by them).
fn refresh_identity(render: &Path, library: &Path) {
    let result = crate::session_bridge::with_library_blocking(library, |lib| {
        match lib.asset_at(render).map_err(|e| e.to_string())? {
            Some(asset) => lib.file_saved(&asset.id).map_err(|e| e.to_string()),
            None => Ok(()),
        }
    });
    if let Err(e) = result {
        log::warn!(
            "Edit With: {} not brought up to date: {e}",
            render.display()
        );
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct EditSaved {
    path: String,
}

/// Watches `renders` for saves from the other program: a settled change
/// drops the kept decodes, makes the thumbnail again and tells the frontend (`edit-with-saved`).
fn watch(app: &AppHandle, renders: &[EditTarget]) {
    if let Ok(mut watched) = watched().lock() {
        for render in renders {
            watched.add(
                Path::new(&render.target),
                render.library.as_deref().map(Path::new),
            );
        }
    }
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        let app = app.clone();
        std::thread::Builder::new()
            .name("edit-with-watch".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(WATCH_INTERVAL);
                    let saved = watched()
                        .lock()
                        .map(|mut w| w.saved_in())
                        .unwrap_or_default();
                    if !saved.is_empty() {
                        // Decodes and thumbnail bases are kept by path (and
                        // recipe) alone: drop the stale ones.
                        let state = app.state::<crate::app_state::AppState>();
                        if let Ok(mut bases) = state.thumbnail_geometry_cache.lock() {
                            bases.clear();
                        }
                        crate::cache_utils::clear_image_caches(state);
                    }
                    for (path, library) in &saved {
                        if let Some(library) = library {
                            refresh_identity(path, library);
                        }
                    }
                    for (path, _) in saved {
                        let path = path.to_string_lossy().into_owned();
                        crate::file_management::refresh_thumbnail(&app, &path);
                        let _ = app.emit("edit-with-saved", EditSaved { path });
                    }
                }
            })
            .ok();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choice(path: &str, name: &str) -> AppChoice {
        AppChoice {
            path: path.into(),
            name: name.into(),
            is_default: false,
            is_recent: false,
            icon: None,
        }
    }

    #[test]
    fn the_default_program_comes_first_then_recent_ones_then_by_name() {
        let apps = vec![
            choice("/A/Zed.app", "Zed"),
            choice("/A/Preview.app", "Preview"),
            choice("/A/editor.app", "editor"),
            choice("/A/Paint.app", "Paint"),
            choice("/A/Paint.app", "Paint"),
            choice("/A/Browser.app", "Browser"),
        ];
        let recent = vec![
            "/A/Paint.app".to_string(),
            "/A/Zed.app".to_string(),
            "/Gone.app".to_string(),
        ];
        let ordered = order_apps(apps, Some("/A/Preview.app"), &recent);
        let names: Vec<_> = ordered.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["Preview", "Paint", "Zed", "Browser", "editor"]);
        assert!(ordered[0].is_default && !ordered[0].is_recent);
        assert!(ordered[1].is_recent && ordered[2].is_recent && !ordered[3].is_recent);
    }

    #[test]
    fn copies_of_a_program_show_once_preferring_the_installed_one() {
        let apps = vec![
            choice("/Volumes/Disk/Paint.app", "Paint"),
            choice("/Users/me/old/Viewer.app", "Viewer"),
            choice("/Applications/Paint.app", "Paint"),
            choice("/Users/me/other/Viewer.app", "Viewer"),
        ];
        let paths: Vec<_> = one_per_name(apps).into_iter().map(|a| a.path).collect();
        assert_eq!(
            paths,
            ["/Applications/Paint.app", "/Users/me/old/Viewer.app"]
        );
    }

    #[test]
    fn recent_programs_keep_the_newest_five() {
        let mut recent = Vec::new();
        for app in ["a", "b", "c", "d", "e", "f", "c"] {
            remember_recent(&mut recent, app);
        }
        assert_eq!(recent, ["c", "f", "e", "d", "b"]);
    }

    #[test]
    fn renders_get_free_names_beside_their_originals() {
        let tmp = tempfile::tempdir().unwrap();
        let original = tmp.path().join("DSC_0001.NEF");
        fs::write(&original, b"raw").unwrap();
        let mut taken = HashSet::new();
        let first = edit_target(&original, "tif", &taken, &|_| Ok(false)).unwrap();
        assert_eq!(first, tmp.path().join("DSC_0001_edit.tif"));
        // Another render of the same run, then files already there.
        taken.insert(first.clone());
        assert_eq!(
            edit_target(&original, "tif", &taken, &|_| Ok(false)).unwrap(),
            tmp.path().join("DSC_0001_edit-2.tif")
        );
        fs::write(tmp.path().join("DSC_0001_edit-2.tif"), b"there").unwrap();
        assert_eq!(
            edit_target(&original, "tif", &taken, &|_| Ok(false)).unwrap(),
            tmp.path().join("DSC_0001_edit-3.tif")
        );
        // A name that differs only in case is taken too (case-insensitive volumes).
        fs::write(tmp.path().join("DSC_0001_EDIT-3.TIF"), b"there").unwrap();
        assert_eq!(
            edit_target(&original, "tif", &taken, &|_| Ok(false)).unwrap(),
            tmp.path().join("DSC_0001_edit-4.tif")
        );
        // A different format starts again.
        assert_eq!(
            edit_target(&original, "jpg", &HashSet::new(), &|_| Ok(false)).unwrap(),
            tmp.path().join("DSC_0001_edit.jpg")
        );
    }

    #[test]
    fn no_name_is_given_when_it_cannot_be_confirmed_free() {
        let tmp = tempfile::tempdir().unwrap();
        let original = tmp.path().join("A.NEF");
        fs::write(&original, b"raw").unwrap();
        // The library cannot say whether the name is registered.
        assert!(edit_target(&original, "tif", &HashSet::new(), &|_| Err("busy".into())).is_err());
        // The folder cannot be listed.
        let gone = tmp.path().join("gone/A.NEF");
        assert!(edit_target(&gone, "tif", &HashSet::new(), &|_| Ok(false)).is_err());
        // A library that cannot be opened.
        let error = plan_targets(
            vec![original.to_string_lossy().into_owned()],
            "tif",
            &|_| Some(tmp.path().join("not-a-library")),
        )
        .unwrap_err();
        assert!(error.contains("not-a-library"), "{error}");
        assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 1);
    }

    #[test]
    fn targets_name_each_photograph_once_and_keep_virtual_copies() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("A.NEF");
        let b = tmp.path().join("B.NEF");
        fs::write(&a, b"raw").unwrap();
        fs::write(&b, b"raw").unwrap();
        let a = a.to_string_lossy().into_owned();
        let b = b.to_string_lossy().into_owned();
        let copy = format!("{a}?vc=x1");
        let targets = plan_targets(
            vec![a.clone(), copy.clone(), b.clone(), a.clone()],
            "tif",
            &|_| None,
        )
        .unwrap();
        let names: Vec<_> = targets
            .iter()
            .map(|t| {
                (
                    t.source.clone(),
                    Path::new(&t.target)
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                )
            })
            .collect();
        assert_eq!(
            names,
            [
                (a.clone(), "A_edit.tif".to_string()),
                (copy, "A_edit-2.tif".to_string()),
                (b, "B_edit.tif".to_string()),
            ]
        );
        assert!(plan_targets(vec![a.clone()], "t/f", &|_| None).is_err());
        assert!(plan_targets(vec![format!("{a}.missing")], "tif", &|_| None).is_err());
        // Planning writes nothing.
        assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 2);
    }

    #[test]
    fn a_render_is_registered_stacked_and_takes_the_originals_metadata() {
        use rawtonic_catalog::Mode;
        let tmp = tempfile::tempdir().unwrap();
        let lib = Library::create(&tmp.path().join("S"), "S", Mode::Session).unwrap();
        let capture = tmp.path().join("S/Capture");
        let source = capture.join("A.NEF");
        fs::write(&source, b"raw").unwrap();
        let original = lib.add_asset(&source).unwrap();
        let mut meta = lib.metadata(&original.id).unwrap();
        meta.rating = 4;
        lib.set_metadata(&original.id, &meta).unwrap();
        let before = fs::read(&source).unwrap();
        let dir = tmp.path().join("S");

        // The library planned with the render, not whichever is open now.
        let (targets, requests) = plan_edits(
            vec![source.to_string_lossy().into_owned()],
            tiff_recipe(None),
            &|_| Some(dir.clone()),
            &|_| false,
        )
        .unwrap();
        let render = PathBuf::from(&targets[0].target);
        assert_eq!(render, capture.join("A_edit.tiff"));
        // The rating changes while the render waits in the queue: the file
        // embeds the pinned one, and the library registers the same.
        meta.rating = 1;
        lib.set_metadata(&original.id, &meta).unwrap();
        drop(lib);
        let pinned = requests[0].pinned.as_ref().unwrap()[0].metadata.as_ref();
        assert_eq!(pinned.map(|m| m.rating), Some(4));
        fs::write(&render, b"tiff").unwrap();
        let result = link_renders(&targets);
        assert_eq!((result.linked, result.problems.len()), (1, 0));
        let lib = Library::open(&dir).unwrap();

        let edit = lib.asset_at(&render).unwrap().unwrap();
        assert_eq!(lib.derived_from(&edit.id).unwrap().unwrap().id, original.id);
        assert_eq!(lib.metadata(&edit.id).unwrap().rating, 4);
        assert_eq!(fs::read(&source).unwrap(), before);
    }

    #[test]
    fn a_name_kept_by_a_registered_photograph_or_its_sidecars_is_not_reused() {
        use rawtonic_catalog::Mode;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("S");
        let lib = Library::create(&dir, "S", Mode::Session).unwrap();
        let capture = dir.join("Capture");
        let source = capture.join("A.NEF");
        fs::write(&source, b"raw").unwrap();
        // An earlier render, registered, then deleted outside the app.
        // Its name in another case: the same file on a case-blind volume.
        let earlier = capture.join("A_EDIT.TIFF");
        fs::write(&earlier, b"old render").unwrap();
        lib.add_asset(&earlier).unwrap();
        fs::remove_file(&earlier).unwrap();
        // Another one's sidecars are left.
        fs::write(capture.join("A_edit-2.tiff.rrdata"), b"{}").unwrap();
        fs::write(capture.join("A_edit-3.xmp"), b"<x/>").unwrap();
        drop(lib);

        let targets = plan_targets(vec![source.to_string_lossy().into_owned()], "tiff", &|_| {
            Some(dir.clone())
        })
        .unwrap();
        assert_eq!(
            targets[0].target,
            capture.join("A_edit-4.tiff").to_string_lossy()
        );
        assert_eq!(
            targets[0].library.as_deref(),
            Some(dir.to_string_lossy().as_ref())
        );
    }

    #[test]
    fn a_virtual_copys_render_takes_the_copys_own_metadata() {
        use rawtonic_catalog::Mode;
        let tmp = tempfile::tempdir().unwrap();
        let lib = Library::create(&tmp.path().join("S"), "S", Mode::Session).unwrap();
        let capture = tmp.path().join("S/Capture");
        let source = capture.join("A.NEF");
        fs::write(&source, b"raw").unwrap();
        let original = lib.add_asset(&source).unwrap();
        let mut meta = lib.metadata(&original.id).unwrap();
        meta.rating = 2;
        meta.title = "Harbour".into();
        lib.set_metadata(&original.id, &meta).unwrap();
        lib.save_copy_state(
            &original.id,
            "9b8208",
            &serde_json::json!({"rating": 5, "adjustments": {}, "tags": ["color:green", "user:print", "ai-tag"]}),
        )
        .unwrap();
        let dir = tmp.path().join("S");
        let (targets, _) = plan_edits(
            vec![format!("{}?vc=9b8208", source.display())],
            tiff_recipe(None),
            &|_| Some(dir.clone()),
            &|_| false,
        )
        .unwrap();
        // Read when planned: a change of the copy afterwards does not count.
        lib.save_copy_state(&original.id, "9b8208", &serde_json::json!({"rating": 1}))
            .unwrap();
        let render = PathBuf::from(&targets[0].target);
        fs::write(&render, b"tiff").unwrap();
        link_render(
            &lib,
            &targets[0].source,
            &render,
            targets[0].metadata.as_ref(),
        )
        .unwrap();

        let edit = lib.asset_at(&render).unwrap().unwrap();
        assert_eq!(lib.derived_from(&edit.id).unwrap().unwrap().id, original.id);
        let got = lib.metadata(&edit.id).unwrap();
        assert_eq!(
            (got.rating, got.label.as_str(), got.title.as_str()),
            (5, "Green", "Harbour")
        );
        assert_eq!(got.keywords, ["print"]);
        // A render never takes over a photograph registered at its name.
        assert!(link_render(&lib, &source.to_string_lossy(), &render, None).is_err());
    }

    fn tiff_recipe(current_edit: Option<(&Path, serde_json::Value)>) -> ExportRequest {
        serde_json::from_value(serde_json::json!({
            "paths": [], "outputFolderOrFile": "", "isExplicitFilePath": true,
            "baseOriginFolders": [], "outputFormat": "tiff",
            "exportSettings": {"jpegQuality": 90, "keepMetadata": true, "stripGps": false},
            "currentEditPath": current_edit.as_ref().map(|(path, _)| path.to_string_lossy()),
            "currentEditAdjustments": current_edit.map(|(_, adjustments)| adjustments),
        }))
        .unwrap()
    }

    #[test]
    fn photographs_with_generated_areas_are_not_edited_with_another_program() {
        let generated = |adjustments: &serde_json::Value| {
            rawtonic_engine::image_loader::has_generated_areas(adjustments)
        };
        let patches = serde_json::json!({"aiPatches": [{"visible": true, "patchData": {"color": "iVBORw0K", "mask": "iVBORw0K"}}]});
        let tmp = tempfile::tempdir().unwrap();
        let plain = tmp.path().join("A.jpg");
        let patched = tmp.path().join("B.jpg");
        fs::write(&plain, b"jpeg").unwrap();
        fs::write(&patched, b"jpeg").unwrap();
        fs::write(
            tmp.path().join("B.jpg.rrdata"),
            serde_json::to_vec(&serde_json::json!({
                "version": 1, "rating": 0, "adjustments": patches,
            }))
            .unwrap(),
        )
        .unwrap();
        let path = |p: &Path| p.to_string_lossy().into_owned();
        let error = plan_edits(
            vec![path(&plain), path(&patched)],
            tiff_recipe(None),
            &|_| None,
            &generated,
        )
        .unwrap_err();
        assert_eq!(error, format!("{GENERATED_AREAS}: B.jpg"));
        // The open photograph is checked with the editor's recipe, which
        // its render takes, not with the one saved last.
        let error = plan_edits(
            vec![path(&plain)],
            tiff_recipe(Some((&plain, patches.clone()))),
            &|_| None,
            &generated,
        )
        .unwrap_err();
        assert_eq!(error, format!("{GENERATED_AREAS}: A.jpg"));
        // Nothing planned or written.
        assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 3);

        let (targets, requests) =
            plan_edits(vec![path(&plain)], tiff_recipe(None), &|_| None, &generated).unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(requests[0].output_folder_or_file, targets[0].target);
        assert_eq!(requests[0].paths, [path(&plain)]);
        // Pinned: the render takes the recipe that was checked.
        assert!(requests[0].pinned.as_ref().is_some_and(|p| p.len() == 1));
    }

    #[test]
    fn renders_are_pinned_with_the_library_they_were_planned_with() {
        use rawtonic_catalog::Mode;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("Catalog");
        let lib = Library::create(&dir, "Catalog", Mode::Catalog).unwrap();
        let folder = tmp.path().join("Photos");
        fs::create_dir(&folder).unwrap();
        let source = folder.join("A.jpg");
        fs::write(&source, b"jpeg").unwrap();
        let asset = lib.add_asset(&source).unwrap();
        let mut meta = lib.metadata(&asset.id).unwrap();
        meta.rating = 4;
        lib.set_metadata(&asset.id, &meta).unwrap();
        drop(lib);

        // The catalog was open when the photographs were planned and is
        // no longer the active one when they are pinned: the pin still
        // reads it, not whatever library is open now.
        let (targets, requests) = plan_edits(
            vec![source.to_string_lossy().into_owned()],
            tiff_recipe(None),
            &|_| Some(dir.clone()),
            &|_| false,
        )
        .unwrap();
        assert_eq!(
            targets[0].library.as_deref(),
            Some(dir.to_string_lossy().as_ref())
        );
        let pinned = &requests[0].pinned.as_ref().unwrap()[0];
        assert_eq!(pinned.metadata.as_ref().map(|m| m.rating), Some(4));
    }

    #[test]
    fn a_save_is_reported_once_it_has_settled() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("A_edit.tif");
        fs::write(&file, b"one").unwrap();
        let mut watched = Watched::default();
        watched.add(&file, None);
        assert!(watched.saved().is_empty());
        fs::write(&file, b"two, longer").unwrap();
        // First seen changed, then unchanged: saved.
        assert!(watched.saved().is_empty());
        assert_eq!(watched.saved(), std::slice::from_ref(&file));
        assert!(watched.saved().is_empty());
        // A file removed (a save by replacement in progress) is not a save.
        fs::remove_file(&file).unwrap();
        assert!(watched.saved().is_empty());
        assert!(watched.saved().is_empty());
        fs::write(&file, b"three").unwrap();
        assert!(watched.saved().is_empty());
        assert_eq!(watched.saved(), std::slice::from_ref(&file));
        // A save reports the library the render was registered in.
        let library = tmp.path().join("Library");
        watched.add(&file, Some(&library));
        fs::write(&file, b"four, saved again").unwrap();
        assert!(watched.saved_in().is_empty());
        assert_eq!(watched.saved_in(), [(file, Some(library))]);
    }

    /// LaunchServices on this Mac (needs the system's Preview).
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn macos_lists_the_programs_for_a_jpeg() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("probe.jpg");
        fs::write(&file, b"").unwrap();
        let (apps, default) = platform::apps_for(&[file.as_path()]);
        assert!(
            apps.iter().any(|a| a.path.ends_with("/Preview.app")),
            "{apps:?}"
        );
        assert!(apps.iter().all(|a| {
            a.icon
                .as_deref()
                .is_some_and(|i| i.starts_with("data:image/png;base64,"))
        }));
        assert!(default.is_some());
        let names: Vec<_> = apps
            .iter()
            .map(|a| (&a.name, a.icon.as_ref().map(|i| i.len())))
            .collect();
        eprintln!("default {default:?}; {names:?}");
    }
}
