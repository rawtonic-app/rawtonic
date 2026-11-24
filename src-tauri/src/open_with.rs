//! Open With and Edit With: the selected photographs' originals, or renders
//! of them, in another program.
//!
//! Open With hands the original files to the program the user picks and
//! never writes or moves them. Edit With renders each photograph through the
//! normal export queue (`edit_with_start` queues it with the chosen recipe
//! to a free name beside the original; like every queued export it renders
//! the snapshot of the photo's recipe and metadata taken at the click),
//! then `edit_with_finish` registers each render
//! as a photograph of its own stacked with its original
//! (`Library::set_derived_from`), opens them all in the program in one
//! launch and watches them, so a save there refreshes the thumbnail.
//!
//! The programs offered for a file come from LaunchServices on macOS and
//! from the shell's association handlers on Windows (`open_with/windows_shell.rs`),
//! plus, there, the program files the user picked with "Choose program…".
//! Other systems report `supported: false` and the menus show the entries
//! disabled.
use crate::app_settings::ExportPreset;
use crate::export_queue::ExportRequest;
use crate::ipc::CommandError;
use crate::state::develop::{DevelopSession, Scope};
use rawtonic_catalog::Library;
use rawtonic_export::sidecars::parse_virtual_path;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{Duration, SystemTime},
};
use tauri::{AppHandle, Emitter, Manager};

#[cfg(any(target_os = "linux", all(test, unix)))]
mod desktop;
#[cfg(any(windows, test))]
mod handlers;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows_shell;

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
    /// Among the programs the system recommends for the files (on Windows
    /// Explorer's "Open with" list); the others it offers (its "Choose
    /// another app" list) follow them in their own group.
    pub recommended: bool,
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
    /// Programs the user picked with "Choose program…" that the system does
    /// not list for the files (Windows), newest first: offered in every
    /// list while they exist.
    pub picked_apps: Vec<String>,
    /// The Edit With recipe last used (an export preset id or a built-in one):
    /// the default for every program without settings of its own.
    pub edit_recipe: Option<String>,
    /// Edit With settings of single programs (program id as the menu lists
    /// it → the recipe its renders are made with), as Lightroom's Edit In
    /// keeps them per editor: made by the first change of a program's
    /// settings from the default recipe, removed by "Use default".
    pub program_settings: BTreeMap<String, ExportPreset>,
}

/// One render: the photograph (a virtual copy keeps its `?vc=`) and the
/// file made from it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditTarget {
    pub source: String,
    pub target: String,
    /// The session or catalog its export's snapshot was read from (None:
    /// no library): the render joins that one, whichever is open later.
    #[serde(default)]
    pub library: Option<String>,
    /// What the render is registered with: the metadata its export's
    /// snapshot holds and the file embeds (`plan_edits`), with a virtual
    /// copy's own rating, label and keywords.
    #[serde(default)]
    pub metadata: Option<rawtonic_metadata::Metadata>,
    /// The render holds areas a generative model made (its export marks
    /// them, `PinnedPhoto::marks_generated`): the library marks it for good
    /// when it registers it (`Library::mark_generated_provenance`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub generated: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditWithResult {
    /// Renders registered in the library and stacked with their originals.
    pub linked: usize,
    /// Messages for renders that could not be registered or linked.
    pub problems: Vec<String>,
}

/// How `platform::open` started the program (Linux starts none, macOS
/// opens listed programs only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
pub enum Launched {
    /// A program the system lists for the files.
    Listed,
    /// A program file the user picked, started with the files.
    Picked,
}

/// The default program first, then the recently used ones (newest first),
/// then the recommended others by name, then the rest by name; each
/// program once.
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
        } else if app.recommended {
            (2, 0)
        } else {
            (3, 0)
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
    rawtonic_fs::atomic_write(&path, &bytes).map_err(|e| e.to_string())
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
        rawtonic_fs::atomic_write(&path, b"").map_err(|e| e.to_string())?;
    }
    Ok(path)
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{AppChoice, Launched};
    use base64::Engine;
    use objc2::{AnyThread, rc::autoreleasepool};
    use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSWorkspace};
    use objc2_foundation::{
        NSBundle, NSDictionary, NSFileManager, NSPoint, NSRect, NSSize, NSString, NSURL,
    };
    use std::{collections::HashSet, path::Path, process::Command};

    pub const SUPPORTED: bool = true;
    /// Copies of an application in several places show once.
    pub const ONE_PER_NAME: bool = true;

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
            recommended: true,
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

    /// Programs picked with "Other…" are opened as listed ones (`open -a`)
    /// and not offered separately.
    pub fn describe(_program: &str) -> Option<AppChoice> {
        None
    }

    /// An application bundle.
    pub fn check(app: &str) -> Result<(), String> {
        let path = Path::new(app);
        if path.is_absolute() && path.is_dir() && path.extension().is_some_and(|e| e == "app") {
            Ok(())
        } else {
            Err(format!("{app} is not an application."))
        }
    }

    /// Opens `files` in `app` in one launch.
    pub fn open(app: &str, files: &[std::path::PathBuf]) -> Result<Launched, String> {
        let status = Command::new("/usr/bin/open")
            .arg("-a")
            .arg(app)
            .args(files)
            .status()
            .map_err(|e| e.to_string())?;
        if status.success() {
            Ok(Launched::Listed)
        } else {
            Err(format!("{app} could not open the files."))
        }
    }
}

#[cfg(windows)]
use windows_shell as platform;

#[cfg(target_os = "linux")]
use linux as platform;

#[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
mod platform {
    use super::{AppChoice, Launched};
    use std::path::Path;

    pub const SUPPORTED: bool = false;
    pub const ONE_PER_NAME: bool = true;

    pub fn apps_for(_files: &[&Path]) -> (Vec<AppChoice>, Option<String>) {
        (Vec::new(), None)
    }

    pub fn describe(_program: &str) -> Option<AppChoice> {
        None
    }

    pub fn check(_program: &str) -> Result<(), String> {
        Err("Open With is not available on this system yet.".into())
    }

    pub fn open(_app: &str, _files: &[std::path::PathBuf]) -> Result<Launched, String> {
        Err("Open With is not available on this system yet.".into())
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

fn choices(app: &AppHandle, (mut apps, default): (Vec<AppChoice>, Option<String>)) -> AppChoices {
    if !platform::SUPPORTED {
        return AppChoices {
            supported: false,
            apps: Vec::new(),
        };
    }
    let prefs = read_prefs(app);
    let recent = prefs.recent_apps;
    add_picked(&mut apps, &prefs.picked_apps, &platform::describe);
    let apps = if platform::ONE_PER_NAME {
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
        apps
    } else {
        apps
    };
    AppChoices {
        supported: true,
        apps: order_apps(apps, default.as_deref(), &recent),
    }
}

/// Adds the programs of `picked` that `describe` knows (still there) and
/// `apps` does not list yet.
fn add_picked(
    apps: &mut Vec<AppChoice>,
    picked: &[String],
    describe: &dyn Fn(&str) -> Option<AppChoice>,
) {
    for program in picked {
        let listed = apps
            .iter()
            .any(|app| app.path.to_lowercase() == program.to_lowercase());
        if !listed && let Some(choice) = describe(program) {
            apps.push(choice);
        }
    }
}

/// Resolve the same UI name for picked programs and interrupted runs.
#[tauri::command]
pub async fn open_with_describe(program: String) -> Option<AppChoice> {
    tauri::async_runtime::spawn_blocking(move || {
        let files = [Path::new("probe.tif"), Path::new("probe.jpg")];
        for file in files {
            if let Some(choice) = platform::apps_for(&[file])
                .0
                .into_iter()
                .find(|choice| choice.path.eq_ignore_ascii_case(&program))
            {
                return Some(choice);
            }
        }
        platform::describe(&program)
    })
    .await
    .ok()
    .flatten()
}

/// "Browse for a program…" on Windows: the program file picker, owned by
/// the window it was asked from (`windows_shell::pick`).
#[tauri::command]
pub async fn open_with_pick(
    title: String,
    window: tauri::WebviewWindow,
) -> Result<Option<AppChoice>, String> {
    #[cfg(windows)]
    {
        let owner = window.hwnd().ok().map(|hwnd| hwnd.0 as isize);
        tauri::async_runtime::spawn_blocking(move || windows_shell::pick(&title, owner))
            .await
            .map_err(|e| e.to_string())?
    }
    #[cfg(not(windows))]
    {
        let _ = (title, window);
        Ok(None)
    }
}

/// The programs that open the originals of `paths`.
#[tauri::command]
pub async fn open_with_apps(paths: Vec<String>, app: AppHandle) -> Result<AppChoices, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let files = original_files(&paths);
        choices(&app, platform::apps_for(&one_per_type(&files)))
    })
    .await
    .map_err(|e| e.to_string())
}

/// The programs that open files of `extension` (Edit With's renders).
#[tauri::command]
pub async fn edit_with_apps(extension: String, app: AppHandle) -> Result<AppChoices, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let probe = probe_file(&app, &extension)?;
        #[cfg(target_os = "linux")]
        let found = platform::edit_apps_for(&probe);
        #[cfg(not(target_os = "linux"))]
        let found = platform::apps_for(&[&probe]);
        Ok(choices(&app, found))
    })
    .await
    .map_err(|e| e.to_string())?
}

fn open_in(
    app: &AppHandle,
    program: &str,
    files: &[PathBuf],
    edit_with: bool,
) -> Result<(), String> {
    platform::check(program)?;
    if let Some(missing) = files.iter().find(|file| !file.is_file()) {
        return Err(format!("{} is not available.", missing.display()));
    }
    #[cfg(target_os = "linux")]
    let launched = platform::open(program, files, edit_with)?;
    #[cfg(not(target_os = "linux"))]
    let launched = {
        let _ = edit_with;
        platform::open(program, files)?
    };
    update_prefs(app, |prefs| remember_launch(prefs, program, launched))
}

/// `program` is the newest recent one; a picked program file is offered
/// from now on.
fn remember_launch(prefs: &mut OpenWithPrefs, program: &str, launched: Launched) {
    remember_recent(&mut prefs.recent_apps, program);
    if launched == Launched::Picked {
        remember_recent(&mut prefs.picked_apps, program);
    }
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
        open_in(&app, &program, &files, false)
    })
    .await
    .map_err(|e| e.to_string())?
}

pub fn open_with_prefs_blocking(app: AppHandle) -> OpenWithPrefs {
    read_prefs(&app)
}

#[tauri::command]
pub async fn open_with_prefs(app: AppHandle) -> OpenWithPrefs {
    tauri::async_runtime::spawn_blocking(move || open_with_prefs_blocking(app))
        .await
        .expect("open_with_prefs task panicked")
}

pub fn edit_with_set_recipe_blocking(recipe: String, app: AppHandle) -> Result<(), String> {
    update_prefs(&app, |prefs| prefs.edit_recipe = Some(recipe))
}

#[tauri::command]
pub async fn edit_with_set_recipe(recipe: String, app: AppHandle) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || edit_with_set_recipe_blocking(recipe, app))
        .await
        .map_err(|e| e.to_string())?
}

/// `program`'s own Edit With settings become `settings`; None removes
/// them, so the program uses the default recipe again.
fn set_program_settings(
    prefs: &mut OpenWithPrefs,
    program: String,
    settings: Option<ExportPreset>,
) {
    match settings {
        Some(settings) => {
            prefs.program_settings.insert(program, settings);
        }
        None => {
            prefs.program_settings.remove(&program);
        }
    }
}

#[tauri::command]
pub async fn edit_with_set_program_settings(
    program: String,
    settings: Option<ExportPreset>,
    app: AppHandle,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        update_prefs(&app, |prefs| set_program_settings(prefs, program, settings))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// What `edit_with_start` queued: each render's file and its export job.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditWithStart {
    pub targets: Vec<EditTarget>,
    pub jobs: Vec<String>,
}

/// Queues the renders of `paths`, each with `recipe` (an export request
/// without paths or output, naming the library it is submitted from) to the
/// free name `plan_edits` gives it.
#[tauri::command]
pub async fn edit_with_start(
    paths: Vec<String>,
    mut recipe: ExportRequest,
    program: String,
    app: AppHandle,
) -> Result<EditWithStart, CommandError> {
    // Kept with each job: a quit ends them, and the next start offers this
    // Edit With again (`export_queue::take_interrupted_edits`).
    log::info!("Edit With in {program}: {} photo(s)", paths.len());
    recipe.edit_with = Some(crate::export_queue::EditWithRun {
        id: uuid::Uuid::new_v4().to_string(),
        program,
        photos: paths.clone(),
    });
    let state = app.clone();
    let (targets, jobs) = tauri::async_runtime::spawn_blocking(move || {
        plan_edits(
            paths,
            recipe,
            crate::export_queue::app_queue(&state.state::<crate::state::export::ExportState>()),
        )
    })
    .await
    .map_err(|e| e.to_string())?
    .inspect_err(|e| log::warn!("Edit With not started: {e:?}"))?;
    for (target, job) in targets.iter().zip(&jobs) {
        log::info!(
            "Edit With: {} renders to {} (job {job}, library {})",
            target.source,
            target.target,
            target
                .library
                .as_deref()
                .unwrap_or("none: folder browsing, not stacked")
        );
    }
    crate::export_queue::started(&app);
    Ok(EditWithStart { targets, jobs })
}

/// The Edit With runs the last quit or crash cut short, each handed out
/// once: the frontend offers "Try again" for them.
#[tauri::command]
pub fn edit_with_interrupted(
    exports: tauri::State<'_, crate::state::export::ExportState>,
) -> Vec<crate::export_queue::InterruptedEditWith> {
    crate::export_queue::take_interrupted_edits(&exports)
}

/// What a render's queued snapshot says about it (`plan_edits`).
#[derive(Clone)]
struct Snapshotted {
    generated: bool,
    owner: Option<String>,
    metadata: Option<rawtonic_metadata::Metadata>,
}

/// Queues the export of each photograph of `paths` with `recipe` to its own
/// file beside its original, as any queued export: each photo's recipe and
/// metadata are snapshotted from the library the request was submitted
/// from (`ExportRequest::library`), and the render takes only that. The
/// render is registered in the library of that snapshot, with the metadata
/// it embeds. A render of a photo with generated areas is marked as any
/// export of it (IPTC Digital Source Type in its XMP), and its target says
/// so (`EditTarget::generated`, also in the one kept for recovery), so its
/// library marks it for good when it registers it ([`link_render`]): it
/// stays marked when the other program saves the file without the marking.
/// Refused as a whole, before anything is queued or written, if no name
/// can be reserved for one.
fn plan_edits(
    paths: Vec<String>,
    mut recipe: ExportRequest,
    queue: &Mutex<Option<rawtonic_export::queue::ExportQueue>>,
) -> Result<(Vec<EditTarget>, Vec<String>), CommandError> {
    // An edit is a newly rendered file, even for saved recipes that preserve dates.
    recipe.export_settings.preserve_timestamps = false;
    let submitted = recipe.library.directory().map(Path::to_path_buf);
    let mut targets = plan_targets(paths, &recipe.output_format, &|original| {
        crate::session_bridge::culling_library_for(original, submitted.as_deref())
    })?;
    let requests: Vec<ExportRequest> = targets
        .iter()
        .map(|target| ExportRequest {
            paths: vec![target.source.clone()],
            output_folder_or_file: target.target.clone(),
            is_explicit_file_path: true,
            replace_confirmed: false,
            confirmed_file: None,
            panel_stamp: None,
            pinned: None,
            ..recipe.clone()
        })
        .collect();
    let (jobs, snapshots) = crate::export_queue::queue_inspected(
        queue,
        requests,
        &|photo| Snapshotted {
            generated: photo.marks_generated(&recipe.export_settings),
            owner: photo.owner.clone(),
            metadata: photo.metadata.clone(),
        },
        &|_| Ok(()),
    )?;
    for (target, photos) in targets.iter_mut().zip(snapshots) {
        if let Some(photo) = photos.into_iter().next() {
            target.library = photo.owner;
            target.metadata = recipe.embedded_library_metadata(photo.metadata);
            target.generated = photo.generated;
        }
    }
    Ok((targets, jobs))
}

/// Free names beside the originals for the renders of `paths`, each
/// reserved in the library `library_for` names for its original (the one
/// its snapshot will be read from).
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
                generated: false,
            })
        })
        .collect()
}

/// Registers `render` in `lib` with `metadata` (`EditTarget::metadata`)
/// and stacks it with the photograph of `source` (a virtual copy stacks
/// with its photograph). A render with `generated` areas is marked so for
/// good (`Library::mark_generated_provenance`), also on a repeated link.
pub fn link_render(
    lib: &Library,
    source: &str,
    render: &Path,
    metadata: Option<&rawtonic_metadata::Metadata>,
    generated: bool,
) -> anyhow::Result<()> {
    let (file, _) = parse_virtual_path(source);
    let original = match lib.asset_at(&file)? {
        Some(asset) => asset,
        None => lib.add_asset(&file)?,
    };
    let top = lib.derived_from(&original.id)?.unwrap_or(original);
    let existing = lib.asset_at(render)?;
    let edit = match existing {
        Some(edit) => {
            anyhow::ensure!(
                lib.derived_from(&edit.id)?
                    .is_some_and(|parent| parent.id == top.id),
                "{} is already a photograph of the library",
                render.display()
            );
            edit
        }
        None => {
            anyhow::ensure!(
                !lib.registered_ignoring_case(render)?,
                "{} is already registered",
                render.display()
            );
            let edit = lib.add_asset(render)?;
            lib.set_derived_from(&edit.id, &top.id)?;
            if let Some(metadata) = metadata {
                lib.set_metadata(&edit.id, metadata)?;
            }
            edit
        }
    };
    if generated {
        lib.mark_generated_provenance(&edit.id, rawtonic_metadata::DIGITAL_SOURCE_COMPOSITE_AI)?;
    }
    let member = rawtonic_catalog::stacks::Member::photo(&top.id);
    let added = rawtonic_catalog::stacks::Member::photo(&edit.id);
    if let Some(before) = lib.stack_of(&member)? {
        if !before.members.contains(&added) {
            let mut after = before.clone();
            after.members.push(added);
            lib.restore_stacks(
                &rawtonic_catalog::stacks::StackChange {
                    before: vec![before],
                    after: vec![after],
                },
                false,
            )?;
        }
    } else {
        lib.group_stack(&[member, added])?;
    }
    Ok(())
}

/// `current` (a render's library metadata) without what `registered`
/// (what an earlier build registered for it) holds and `embedded` (what its
/// file embeds) does not: only a field still at the registered value
/// changes, so the user's later edits of the render stay. An IPTC field
/// becomes an explicit empty value, which also clears it in the sidecar.
fn without_unembedded(
    mut current: rawtonic_metadata::Metadata,
    registered: &rawtonic_metadata::Metadata,
    embedded: &rawtonic_metadata::Metadata,
) -> rawtonic_metadata::Metadata {
    fn repair<T: PartialEq + Clone>(current: &mut T, registered: &T, embedded: &T) {
        if current == registered && registered != embedded {
            *current = embedded.clone();
        }
    }
    repair(&mut current.rating, &registered.rating, &embedded.rating);
    repair(&mut current.label, &registered.label, &embedded.label);
    repair(&mut current.flag, &registered.flag, &embedded.flag);
    // Keyword by keyword, so one the user added since stays.
    current.keywords.retain(|keyword| {
        !registered.keywords.contains(keyword) || embedded.keywords.contains(keyword)
    });
    repair(&mut current.title, &registered.title, &embedded.title);
    repair(
        &mut current.description,
        &registered.description,
        &embedded.description,
    );
    repair(&mut current.creator, &registered.creator, &embedded.creator);
    repair(
        &mut current.copyright,
        &registered.copyright,
        &embedded.copyright,
    );
    for (key, value) in &registered.iptc {
        let kept = embedded.iptc.get(key).cloned().unwrap_or_default();
        if current.iptc.get(key) == Some(value) && *value != kept {
            current.iptc.insert(key.clone(), kept);
        }
    }
    current
}

/// Repairs the render of `target` when an earlier build registered it
/// with `registered`, the photo's full metadata, although its file embeds
/// only `target.metadata` ([`without_unembedded`]). Nothing for a render the
/// library does not list.
pub(crate) fn repair_render_metadata(
    target: &EditTarget,
    registered: &rawtonic_metadata::Metadata,
) -> Result<(), String> {
    let Some(dir) = &target.library else {
        return Ok(());
    };
    let embedded = target.metadata.clone().unwrap_or_default();
    crate::session_bridge::with_library_blocking(Path::new(dir), |lib| {
        let failed = |e: rawtonic_catalog::CatalogError| format!("{e:#}");
        let Some(render) = lib.asset_at(Path::new(&target.target)).map_err(failed)? else {
            return Ok(());
        };
        let current = lib.metadata(&render.id).map_err(failed)?;
        let repaired = without_unembedded(current.clone(), registered, &embedded);
        if repaired != current {
            lib.set_metadata(&render.id, &repaired).map_err(failed)?;
        }
        Ok(())
    })
}

pub(crate) fn link_renders(items: &[EditTarget]) -> EditWithResult {
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
                        item.generated,
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
            log::warn!(
                "Edit With in {program}: {} was not rendered",
                missing.display()
            );
            return Err(format!("{} was not rendered.", missing.display()));
        }
        let result = link_renders(&items);
        log::info!(
            "Edit With in {program}: {} render(s), {} stacked in a library",
            items.len(),
            result.linked
        );
        for problem in &result.problems {
            log::warn!("Edit With: not added to the library: {problem}");
        }
        watch(&app, &items);
        open_in(&app, &program, &renders, true)
            .inspect_err(|e| log::warn!("Edit With: {program} did not open the renders: {e}"))?;
        if result.problems.is_empty() {
            crate::export_queue::handed_off(
                &app.state::<crate::state::export::ExportState>(),
                &items,
            );
        }
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
            Some(asset) => {
                let saved = lib.file_saved(&asset.id).map_err(|e| e.to_string());
                // Our own write: the open photograph's file is this one now.
                if crate::session_bridge::file_replaced_since_open(render).is_some() {
                    crate::session_bridge::note_file_opened(render);
                }
                saved
            }
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
                        crate::state::lock::lock_recover(
                            &app.state::<crate::state::thumbnails::ThumbnailState>()
                                .geometry_cache,
                        )
                        .clear();
                        app.state::<DevelopSession>().invalidate(Scope::Decoder);
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
    use rawtonic_fs::Canonical;

    fn choice(path: &str, name: &str) -> AppChoice {
        AppChoice {
            path: path.into(),
            name: name.into(),
            is_default: false,
            is_recent: false,
            recommended: true,
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

    /// The IPC contract for a packaged (Store) program such as Photos:
    /// the menu entry the backend sends, and the arguments the frontend
    /// sends back for it (`OpenWithMenus.test.tsx` asserts the same file),
    /// read into the commands' parameter types (Tauri reads each argument
    /// by its camelCase name).
    #[test]
    fn a_packaged_program_crosses_ipc_as_the_frontend_sends_it() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../src/components/studio/fixtures/openWithPackaged.json"
        ))
        .unwrap();
        // What the backend lists for the Photos handler.
        let photos = handlers::Handler {
            id: "Photos".into(),
            name: "Photos".into(),
            recommended: false,
        };
        let listed = handlers::choices(std::slice::from_ref(&photos), &|_| None);
        assert_eq!(
            serde_json::to_value(&listed[0]).unwrap(),
            fixture["choices"]["apps"][1]
        );
        let arg = |command: &str, name: &str| fixture[command][name].clone();
        // open_with_open(program, paths)
        let program: String = serde_json::from_value(arg("openWithOpen", "program")).unwrap();
        let paths: Vec<String> = serde_json::from_value(arg("openWithOpen", "paths")).unwrap();
        assert_eq!(program, photos.id);
        assert_eq!(
            original_files(&paths),
            [
                PathBuf::from(r"C:\Fotos\Größe A.NEF"),
                PathBuf::from(r"C:\Fotos\B.NEF")
            ]
        );
        // edit_with_finish(items, program)
        let items: Vec<EditTarget> =
            serde_json::from_value(arg("editWithFinish", "items")).unwrap();
        let program: String = serde_json::from_value(arg("editWithFinish", "program")).unwrap();
        assert_eq!(items[0].target, r"C:\Fotos\Größe A_edit.tiff");
        assert_eq!(items[0].library, None);
        // The id starts the packaged app through its registration.
        let progids = [handlers::ProgId {
            class: "AppX43hnxtbyyps62jhe9sqpdzxn1790zetc".into(),
            executable: None,
            friendly: Some("Photos".into()),
        }];
        assert_eq!(
            handlers::progid_for(&program, Some(&photos.name), &progids).as_deref(),
            Some("AppX43hnxtbyyps62jhe9sqpdzxn1790zetc")
        );
        // Remembered as a recent program by the same id.
        let mut prefs = OpenWithPrefs::default();
        remember_launch(&mut prefs, &program, Launched::Listed);
        assert_eq!(prefs.recent_apps, ["Photos"]);
        assert!(prefs.picked_apps.is_empty());
    }

    /// The IPC contract of a program's own Edit With settings: what the
    /// settings dialog sends (`OpenWithMenus.test.tsx` asserts the same
    /// file) reads into `edit_with_set_program_settings`' parameters, is
    /// stored in `open_with.json` and read back as the menu gets it, beside
    /// the default recipe; "Use default recipe" removes it again.
    #[test]
    fn program_settings_cross_ipc_as_the_dialog_sends_them() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../src/components/studio/fixtures/editWithProgramSettings.json"
        ))
        .unwrap();
        let arg = |command: &str, name: &str| fixture[command][name].clone();
        let gimp: String =
            serde_json::from_value(arg("editWithSetProgramSettings", "program")).unwrap();
        let settings: Option<ExportPreset> =
            serde_json::from_value(arg("editWithSetProgramSettings", "settings")).unwrap();
        let settings = settings.expect("the dialog sends settings");
        assert_eq!(settings.file_format, "tiff");
        assert_eq!(settings.tiff_bit_depth, Some(16));
        assert_eq!(settings.color_space.as_deref(), Some("proPhoto"));

        // A file written before per-program settings: the default recipe
        // stays, no program has settings of its own.
        let old = r#"{"recentApps":["/usr/share/applications/org.gimp.GIMP.desktop"],"pickedApps":[],"editRecipe":"builtin-tiff16-adobe"}"#;
        let mut prefs: OpenWithPrefs = serde_json::from_str(old).unwrap();
        assert_eq!(prefs.edit_recipe.as_deref(), Some("builtin-tiff16-adobe"));
        assert!(prefs.program_settings.is_empty());

        // The prefs the menu gets back (a second program, JPEG sRGB).
        let mut prefs_sent: OpenWithPrefs =
            serde_json::from_value(fixture["openWithPrefs"].clone()).unwrap();
        let gwenview = "/usr/share/applications/org.kde.gwenview.desktop";
        assert_eq!(prefs_sent.program_settings[gwenview].file_format, "jpeg");
        assert_eq!(
            prefs_sent.program_settings[gwenview].color_space.as_deref(),
            Some("srgb")
        );

        set_program_settings(&mut prefs, gimp.clone(), Some(settings.clone()));
        set_program_settings(
            &mut prefs,
            gwenview.into(),
            Some(prefs_sent.program_settings[gwenview].clone()),
        );
        // Stored and read back as written (`update_prefs`, `read_prefs`).
        let bytes = serde_json::to_vec_pretty(&prefs).unwrap();
        let read: OpenWithPrefs = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(read.edit_recipe.as_deref(), Some("builtin-tiff16-adobe"));
        assert_eq!(
            serde_json::to_value(&read.program_settings[&gimp]).unwrap(),
            serde_json::to_value(&settings).unwrap()
        );
        let sent: ExportPreset =
            serde_json::from_value(fixture["openWithPrefs"]["programSettings"][gwenview].clone())
                .unwrap();
        assert_eq!(
            serde_json::to_value(&read.program_settings[gwenview]).unwrap(),
            serde_json::to_value(&sent).unwrap()
        );
        // Every setting the dialog sends is kept, none dropped on the way.
        let kept = serde_json::to_value(&settings).unwrap();
        for (key, value) in arg("editWithSetProgramSettings", "settings")
            .as_object()
            .unwrap()
        {
            assert_eq!(
                kept[key].as_f64().or(kept[key].as_bool().map(f64::from)),
                value.as_f64().or(value.as_bool().map(f64::from)),
                "{key}"
            );
            if !value.is_number() {
                assert_eq!(&kept[key], value, "{key}");
            }
        }

        // "Use default recipe" for Gwenview.
        let program: String = serde_json::from_value(arg("useDefault", "program")).unwrap();
        let none: Option<ExportPreset> =
            serde_json::from_value(arg("useDefault", "settings")).unwrap();
        assert!(none.is_none());
        set_program_settings(&mut prefs_sent, program, none);
        assert!(prefs_sent.program_settings.is_empty());
        assert_eq!(prefs_sent.recent_apps, [gimp.as_str()]);
    }

    #[cfg(unix)]
    /// Linux: a program is its `.desktop` file; the list the desktop offers
    /// and the arguments the frontend sends read as the commands expect.
    #[test]
    fn a_desktop_program_crosses_ipc_as_the_frontend_sends_it() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../src/components/studio/fixtures/openWithLinux.json"
        ))
        .unwrap();
        let entry = |id: &str, path: &str, name: &str| {
            desktop::parse_entry(
                &format!("[Desktop Entry]\nType=Application\nName={name}\nExec=app %F\n"),
                Path::new(path),
                id,
                &[],
            )
            .unwrap()
        };
        let gimp = entry(
            "org.gimp.GIMP.desktop",
            "/usr/share/applications/org.gimp.GIMP.desktop",
            "GIMP",
        );
        let krita = entry(
            "org.kde.krita.desktop",
            "/var/lib/flatpak/exports/share/applications/org.kde.krita.desktop",
            "Krita",
        );
        let listed = order_apps(
            vec![desktop::choice(&krita, None), desktop::choice(&gimp, None)],
            Some(&gimp.path.to_string_lossy()),
            &[],
        );
        assert_eq!(
            serde_json::to_value(&listed).unwrap(),
            fixture["choices"]["apps"]
        );
        let program: String =
            serde_json::from_value(fixture["openWithOpen"]["program"].clone()).unwrap();
        let paths: Vec<String> =
            serde_json::from_value(fixture["openWithOpen"]["paths"].clone()).unwrap();
        assert_eq!(program, listed[1].path);
        assert_eq!(
            original_files(&paths),
            [
                PathBuf::from("/home/u/Fotos/Größe A.NEF"),
                PathBuf::from("/home/u/Fotos/B.NEF")
            ]
        );
        let items: Vec<EditTarget> =
            serde_json::from_value(fixture["editWithFinish"]["items"].clone()).unwrap();
        assert_eq!(items[0].target, "/home/u/Fotos/Größe A_edit.tiff");
        // A listed program is remembered as recent only; a picked one is
        // offered from now on.
        let mut prefs = OpenWithPrefs::default();
        remember_launch(&mut prefs, &program, Launched::Listed);
        assert_eq!(prefs.recent_apps, std::slice::from_ref(&program));
        assert!(prefs.picked_apps.is_empty());
        remember_launch(&mut prefs, "/opt/tool/run", Launched::Picked);
        assert_eq!(prefs.picked_apps, ["/opt/tool/run"]);
    }

    /// Windows: programs the shell offers without recommending them (the
    /// Photos app for TIFF on the Lenovo, task WEW4) come after the
    /// recommended ones, unless they are the default or recent.
    #[test]
    fn programs_not_recommended_come_after_the_recommended_ones() {
        let other = |path: &str, name: &str| AppChoice {
            recommended: false,
            ..choice(path, name)
        };
        let apps = vec![
            other("Photos", "Photos"),
            choice(r"C:\Paint\mspaint.exe", "Paint"),
            other(r"C:\Clip\Clipchamp.exe", "Clipchamp"),
            choice(r"C:\Snip\SnippingTool.exe", "Snipping Tool"),
            other("Gallery", "Gallery"),
        ];
        let recent = vec!["Gallery".to_string()];
        let names: Vec<_> = order_apps(apps, None, &recent)
            .into_iter()
            .map(|a| a.name)
            .collect();
        assert_eq!(
            names,
            ["Gallery", "Paint", "Snipping Tool", "Clipchamp", "Photos"]
        );
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
    fn a_picked_program_is_remembered_and_offered_while_it_exists() {
        let mut prefs = OpenWithPrefs::default();
        // A listed program is recent only; a picked program file is offered
        // from now on too.
        remember_launch(&mut prefs, "Paint", Launched::Listed);
        remember_launch(&mut prefs, r"C:\Tools\Editor.exe", Launched::Picked);
        assert_eq!(prefs.recent_apps, [r"C:\Tools\Editor.exe", "Paint"]);
        assert_eq!(prefs.picked_apps, [r"C:\Tools\Editor.exe"]);

        let mut apps = vec![choice(r"C:\tools\editor.exe", "Editor (listed)")];
        let picked = [
            r"C:\Tools\Editor.exe".to_string(),
            r"C:\Gone\Old.exe".to_string(),
            r"D:\Bild\Größe.exe".to_string(),
        ];
        let describe =
            |program: &str| (!program.contains("Gone")).then(|| choice(program, "described"));
        add_picked(&mut apps, &picked, &describe);
        let paths: Vec<_> = apps.iter().map(|a| a.path.as_str()).collect();
        // Listed already (another case), gone, added.
        assert_eq!(paths, [r"C:\tools\editor.exe", r"D:\Bild\Größe.exe"]);
        // Older preferences without the list read as before.
        let old: OpenWithPrefs =
            serde_json::from_str(r#"{"recentApps": ["/A/Preview.app"]}"#).unwrap();
        assert!(old.picked_apps.is_empty());
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

    /// The Edit With recipe (`editRequest` in the frontend), submitted from
    /// `library`.
    fn tiff_recipe(
        current_edit: Option<(&Path, serde_json::Value)>,
        library: Option<&Path>,
    ) -> ExportRequest {
        serde_json::from_value(serde_json::json!({
            "paths": [], "outputFolderOrFile": "", "isExplicitFilePath": true,
            "baseOriginFolders": [], "outputFormat": "tiff",
            "exportSettings": {"jpegQuality": 90, "keepMetadata": true, "stripGps": false},
            "currentEditPath": current_edit.as_ref().map(|(path, _)| path.to_string_lossy()),
            "currentEditAdjustments": current_edit.map(|(_, adjustments)| adjustments),
            "library": match library {
                Some(dir) => serde_json::json!({"library": dir}),
                None => serde_json::json!("noLibrary"),
            },
        }))
        .unwrap()
    }

    /// An export queue of its own, as the app's.
    struct Queue {
        _tmp: tempfile::TempDir,
        queue: Mutex<Option<rawtonic_export::queue::ExportQueue>>,
    }

    impl Queue {
        fn new() -> Self {
            let tmp = tempfile::tempdir().unwrap();
            let queue =
                rawtonic_export::queue::ExportQueue::open(&tmp.path().join("q.sqlite")).unwrap();
            Queue {
                _tmp: tmp,
                queue: Mutex::new(Some(queue)),
            }
        }

        fn jobs(&self) -> usize {
            self.queue
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .jobs()
                .unwrap()
                .len()
        }

        /// What the runner renders for the render of `job`.
        fn snapshot(&self, job: &str) -> crate::export_processing::PinnedPhoto {
            let guard = self.queue.lock().unwrap();
            let snapshot = guard.as_ref().unwrap().item_snapshot(job, 0).unwrap();
            serde_json::from_str(&snapshot.expect("kept with the job")).unwrap()
        }

        fn request(&self, job: &str) -> ExportRequest {
            let guard = self.queue.lock().unwrap();
            serde_json::from_value(guard.as_ref().unwrap().job(job).unwrap().request).unwrap()
        }
    }

    fn editor_recipe(exposure: f64, rating: u8) -> serde_json::Value {
        serde_json::json!({"version": 1, "rating": rating, "tags": ["color:red"], "adjustments": {"exposure": exposure}})
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
        let queue = Queue::new();

        let mut recipe = tiff_recipe(None, Some(&dir));
        recipe.export_settings.preserve_timestamps = true;
        let (targets, jobs) = plan_edits(
            vec![source.to_string_lossy().into_owned()],
            recipe,
            &queue.queue,
        )
        .unwrap();
        let render = PathBuf::from(&targets[0].target);
        assert_eq!(render, capture.join("A_edit.tiff"));
        // The job exports exactly that photo to that file.
        let request = queue.request(&jobs[0]);
        assert_eq!(request.paths, [source.to_string_lossy()]);
        assert_eq!(request.output_folder_or_file, targets[0].target);
        assert!(request.is_explicit_file_path);
        // The rating changes while the render waits in the queue: the file
        // embeds the snapshot's, and the library registers the same.
        meta.rating = 1;
        lib.set_metadata(&original.id, &meta).unwrap();
        drop(lib);
        let snapshot = queue.snapshot(&jobs[0]);
        assert_eq!(snapshot.metadata.as_ref().map(|m| m.rating), Some(4));
        fs::write(&render, b"tiff").unwrap();
        let result = link_renders(&targets);
        assert_eq!((result.linked, result.problems.len()), (1, 0));
        let lib = Library::open(&dir).unwrap();

        let edit = lib.asset_at(&render).unwrap().unwrap();
        assert_eq!(lib.derived_from(&edit.id).unwrap().unwrap().id, original.id);
        let stacks = lib.stacks().unwrap();
        assert_eq!(stacks.len(), 1);
        assert_eq!(stacks[0].members.len(), 2);
        assert_eq!(stacks[0].members[0].asset_id, original.id);
        assert_eq!(stacks[0].members[1].asset_id, edit.id);
        assert!(!request.export_settings.preserve_timestamps);
        assert_eq!(lib.metadata(&edit.id).unwrap().rating, 4);
        assert_eq!(fs::read(&source).unwrap(), before);
    }

    #[test]
    fn adding_a_render_preserves_an_expanded_stacks_top_and_order() {
        use rawtonic_catalog::{
            Mode,
            stacks::{Member, StackChange},
        };
        let tmp = tempfile::tempdir().unwrap();
        let lib = Library::create(&tmp.path().join("S"), "S", Mode::Session).unwrap();
        let a = tmp.path().join("S/Capture/A.NEF");
        let b = tmp.path().join("S/Capture/B.NEF");
        let render = tmp.path().join("S/Capture/A_edit.tif");
        for path in [&a, &b, &render] {
            fs::write(path, b"photo").unwrap();
        }
        let first = lib.add_asset(&a).unwrap();
        let top = lib.add_asset(&b).unwrap();
        lib.group_stack(&[Member::photo(&top.id), Member::photo(&first.id)])
            .unwrap();
        let before = lib.stacks().unwrap().remove(0);
        let mut expanded = before.clone();
        expanded.collapsed = false;
        lib.restore_stacks(
            &StackChange {
                before: vec![before],
                after: vec![expanded.clone()],
            },
            false,
        )
        .unwrap();
        link_render(&lib, &a.to_string_lossy(), &render, None, false).unwrap();
        // Recovery may repeat the link after the external launch failed.
        link_render(&lib, &a.to_string_lossy(), &render, None, false).unwrap();
        let stack = lib.stacks().unwrap().remove(0);
        assert_eq!(stack.id, expanded.id);
        assert!(!stack.collapsed);
        assert_eq!(&stack.members[..2], expanded.members.as_slice());
        assert_eq!(stack.members.len(), 3);
    }

    #[test]
    fn a_library_switch_after_the_click_never_changes_the_render() {
        use rawtonic_catalog::Mode;
        // One photograph in two catalogs, each with its own recipe and rating.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonical().unwrap();
        fs::create_dir(root.join("Photos")).unwrap();
        let source = root.join("Photos/A.jpg");
        fs::write(&source, b"jpeg").unwrap();
        let catalog = |name: &str, exposure: f64, rating: u8| {
            let lib = Library::create(&root.join(name), name, Mode::Catalog).unwrap();
            let asset = lib.add_asset(&source).unwrap();
            lib.save_editor_state(&asset.id, &editor_recipe(exposure, rating))
                .unwrap();
            (lib, asset.id)
        };
        let (a, a_id) = catalog("A", 1.0, 4);
        let (b, b_id) = catalog("B", 2.0, 1);
        let queue = Queue::new();

        // Edit With clicked in catalog A.
        let (targets, jobs) = plan_edits(
            vec![source.to_string_lossy().into_owned()],
            tiff_recipe(None, Some(&a.directory)),
            &queue.queue,
        )
        .unwrap();

        // The user switches to B and edits there, and A changes too, before
        // the render runs.
        b.save_editor_state(&b_id, &editor_recipe(3.0, 2)).unwrap();
        a.save_editor_state(&a_id, &editor_recipe(5.0, 5)).unwrap();

        // The render takes A's recipe and metadata as they were at the click.
        let snapshot = queue.snapshot(&jobs[0]);
        assert_eq!(snapshot.adjustments["exposure"], serde_json::json!(1.0));
        assert_eq!(snapshot.metadata.as_ref().map(|m| m.rating), Some(4));
        assert_eq!(
            snapshot.owner.as_deref(),
            Some(a.directory.to_string_lossy().as_ref())
        );
        // It joins A, with what it embeds, never B.
        assert_eq!(targets[0].library, snapshot.owner);
        let render = PathBuf::from(&targets[0].target);
        fs::write(&render, b"tiff").unwrap();
        let result = link_renders(&targets);
        assert_eq!((result.linked, result.problems.len()), (1, 0));
        let edit = a.asset_at(&render).unwrap().unwrap();
        assert_eq!(a.derived_from(&edit.id).unwrap().unwrap().id, a_id);
        assert_eq!(a.metadata(&edit.id).unwrap().rating, 4);
        assert!(b.asset_at(&render).unwrap().is_none());
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
        // The copy's own state, in its sidecar beside the photograph.
        let copy_state = capture.join("A.NEF.9b8208.rrdata");
        fs::write(
            &copy_state,
            serde_json::to_vec(&serde_json::json!({"version": 1, "rating": 5, "adjustments": {}, "tags": ["color:green", "user:print", "ai-tag"]}))
                .unwrap(),
        )
        .unwrap();
        let dir = tmp.path().join("S");
        let queue = Queue::new();
        let (targets, jobs) = plan_edits(
            vec![format!("{}?vc=9b8208", source.display())],
            tiff_recipe(None, Some(&dir)),
            &queue.queue,
        )
        .unwrap();
        // Read at the click: a change of the copy afterwards does not count.
        fs::write(
            &copy_state,
            serde_json::to_vec(&serde_json::json!({"version": 1, "rating": 1, "adjustments": {}}))
                .unwrap(),
        )
        .unwrap();
        // The file embeds the copy's own metadata, and it is registered so.
        assert_eq!(
            queue.snapshot(&jobs[0]).metadata.as_ref().map(|m| m.rating),
            Some(5)
        );
        let render = PathBuf::from(&targets[0].target);
        fs::write(&render, b"tiff").unwrap();
        link_render(
            &lib,
            &targets[0].source,
            &render,
            targets[0].metadata.as_ref(),
            targets[0].generated,
        )
        .unwrap();

        let edit = lib.asset_at(&render).unwrap().unwrap();
        assert_eq!(lib.derived_from(&edit.id).unwrap().unwrap().id, original.id);
        let got = lib.metadata(&edit.id).unwrap();
        assert_eq!(
            (got.rating, got.label.as_str(), got.title.as_str()),
            (5, "Green", "Harbour")
        );
        // Its keywords as the export embeds them (`copy_metadata`).
        assert_eq!(got.keywords, ["print", "ai-tag"]);
        // Recovery of the same derived render is idempotent and keeps its metadata.
        link_render(&lib, &source.to_string_lossy(), &render, None, false).unwrap();
        assert_eq!(lib.metadata(&edit.id).unwrap(), got);
        // A different photograph registered at the name is never taken over.
        let unrelated = capture.join("unrelated.tif");
        fs::write(&unrelated, b"photo").unwrap();
        lib.add_asset(&unrelated).unwrap();
        assert!(link_render(&lib, &source.to_string_lossy(), &unrelated, None, false).is_err());
    }

    /// A photograph of a new session with the creator's contact details,
    /// a place and a caption, its library and the session directory.
    fn photo_with_private_metadata(tmp: &Path) -> (Library, PathBuf, PathBuf) {
        use rawtonic_catalog::Mode;
        let dir = tmp.join("S");
        let lib = Library::create(&dir, "S", Mode::Session).unwrap();
        let source = dir.join("Capture/A.NEF");
        fs::write(&source, b"raw").unwrap();
        let original = lib.add_asset(&source).unwrap();
        let mut meta = lib.metadata(&original.id).unwrap();
        meta.rating = 3;
        meta.flag = 1;
        meta.keywords = vec!["Private client".into()];
        meta.title = "Harbour".into();
        meta.creator = "Ada".into();
        meta.copyright = "(c) Ada".into();
        for (key, value) in [
            ("creatorEmail", "ada@example.org"),
            ("creatorPhone", "+49 555 0100"),
            ("city", "Kiel"),
            ("usageTerms", "Editorial only"),
        ] {
            meta.iptc.insert(key.into(), value.into());
        }
        lib.set_metadata(&original.id, &meta).unwrap();
        (lib, source, dir)
    }

    /// Plans, "renders" and registers the Edit With render of `source` with
    /// `settings`, runs the XMP sync, and returns the render's registered
    /// metadata and every sidecar text beside it.
    fn register_render(
        lib: &Library,
        source: &Path,
        dir: &Path,
        settings: serde_json::Value,
    ) -> (rawtonic_metadata::Metadata, String) {
        let mut recipe = tiff_recipe(None, Some(dir));
        recipe.export_settings = serde_json::from_value(settings).unwrap();
        recipe.edit_with = Some(crate::export_queue::EditWithRun {
            id: "run".into(),
            program: "Editor".into(),
            photos: vec![source.to_string_lossy().into_owned()],
        });
        let queue = Queue::new();
        let (targets, jobs) = plan_edits(
            vec![source.to_string_lossy().into_owned()],
            recipe,
            &queue.queue,
        )
        .unwrap();
        // The target kept for recovery and Try again holds the same.
        let request = queue.request(&jobs[0]);
        assert_eq!(request.edit_target.as_ref(), Some(&targets[0]));
        let render = PathBuf::from(&targets[0].target);
        fs::write(&render, b"tiff").unwrap();
        link_render(
            lib,
            &targets[0].source,
            &render,
            targets[0].metadata.as_ref(),
            targets[0].generated,
        )
        .unwrap();
        let edit = lib.asset_at(&render).unwrap().unwrap();
        lib.sync_xmp(&edit.id).unwrap();
        let mut sidecars = String::new();
        for entry in fs::read_dir(render.parent().unwrap()).unwrap() {
            let path = entry.unwrap().path();
            if path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("xmp"))
            {
                sidecars.push_str(&fs::read_to_string(&path).unwrap());
            }
        }
        (lib.metadata(&edit.id).unwrap(), sidecars)
    }

    #[test]
    fn a_render_registers_only_the_metadata_its_file_embeds() {
        let tmp = tempfile::tempdir().unwrap();
        let (lib, source, dir) = photo_with_private_metadata(tmp.path());
        let original = std::fs::read(&source).unwrap();

        // Metadata limited to the rights, creator contact details off.
        let (got, sidecars) = register_render(
            &lib,
            &source,
            &dir,
            serde_json::json!({"jpegQuality": 90, "keepMetadata": false, "stripGps": true}),
        );
        assert_eq!(
            (got.creator.as_str(), got.copyright.as_str()),
            ("Ada", "(c) Ada")
        );
        assert_eq!(
            got.iptc.get("usageTerms").map(String::as_str),
            Some("Editorial only")
        );
        for key in ["creatorEmail", "creatorPhone", "city"] {
            assert!(!got.iptc.contains_key(key), "{key} registered");
        }
        assert_eq!((got.rating, got.title.as_str()), (0, ""));
        assert!(
            sidecars.contains("Ada"),
            "the sync wrote the render's sidecar"
        );
        for private in [
            "ada@example.org",
            "555 0100",
            "Kiel",
            "Harbour",
            "Private client",
        ] {
            assert!(!sidecars.contains(private), "{private} in the sidecar");
        }
        assert_eq!(std::fs::read(&source).unwrap(), original);
    }

    #[test]
    fn a_render_registers_contact_details_and_all_metadata_only_when_its_file_has_them() {
        let tmp = tempfile::tempdir().unwrap();
        let (lib, source, dir) = photo_with_private_metadata(tmp.path());
        let (got, sidecars) = register_render(
            &lib,
            &source,
            &dir,
            serde_json::json!({"jpegQuality": 90, "keepMetadata": false, "stripGps": true, "keepCreatorContact": true}),
        );
        assert_eq!(
            got.iptc.get("creatorEmail").map(String::as_str),
            Some("ada@example.org")
        );
        assert!(!got.iptc.contains_key("city"));
        assert!(sidecars.contains("ada@example.org") && !sidecars.contains("Kiel"));

        // All metadata without GPS: the file has the library's fields, so
        // the render is registered with them; GPS is never part of them.
        let tmp = tempfile::tempdir().unwrap();
        let (lib, source, dir) = photo_with_private_metadata(tmp.path());
        let (got, sidecars) = register_render(
            &lib,
            &source,
            &dir,
            serde_json::json!({"jpegQuality": 90, "keepMetadata": true, "stripGps": true}),
        );
        assert_eq!((got.rating, got.title.as_str()), (3, "Harbour"));
        assert_eq!(got.iptc.get("city").map(String::as_str), Some("Kiel"));
        assert!(!sidecars.contains("GPS"));
        // The pick flag stays with the photograph: the file has none.
        assert_eq!(got.flag, 0);
    }

    #[test]
    fn a_render_an_earlier_build_registered_with_everything_loses_what_its_file_lacks() {
        let tmp = tempfile::tempdir().unwrap();
        let (lib, source, dir) = photo_with_private_metadata(tmp.path());
        let original = lib.asset_at(&source).unwrap().unwrap();
        let full = lib.metadata(&original.id).unwrap();
        // The earlier build registered the stripped render with everything,
        // and its sidecar was synced so.
        let render = dir.join("Capture/A_edit.tiff");
        fs::write(&render, b"tiff").unwrap();
        link_render(&lib, &source.to_string_lossy(), &render, Some(&full), false).unwrap();
        let edit = lib.asset_at(&render).unwrap().unwrap();
        lib.sync_xmp(&edit.id).unwrap();
        // The user retitled the render and tagged it since.
        let mut retitled = lib.metadata(&edit.id).unwrap();
        retitled.title = "Harbour, retouched".into();
        retitled.keywords.push("retouched".into());
        lib.set_metadata(&edit.id, &retitled).unwrap();
        drop(lib);

        let target = EditTarget {
            source: source.to_string_lossy().into_owned(),
            target: render.to_string_lossy().into_owned(),
            library: Some(dir.to_string_lossy().into_owned()),
            metadata: rawtonic_export::exif_processing::embedded_library_metadata(
                "tiff",
                false,
                false,
                full.clone(),
            ),
            generated: false,
        };
        repair_render_metadata(&target, &full).unwrap();
        // Idempotent: a second recovery changes nothing more.
        repair_render_metadata(&target, &full).unwrap();

        let lib = Library::open(&dir).unwrap();
        let got = lib.metadata(&edit.id).unwrap();
        assert_eq!(got.title, "Harbour, retouched", "the user's edit stays");
        assert_eq!(
            got.keywords,
            ["retouched"],
            "only the inherited keyword goes"
        );
        assert_eq!((got.rating, got.flag), (0, 0));
        assert_eq!(
            (got.creator.as_str(), got.copyright.as_str()),
            ("Ada", "(c) Ada")
        );
        for key in ["creatorEmail", "creatorPhone", "city"] {
            assert!(
                got.iptc.get(key).is_none_or(|value| value.is_empty()),
                "{key} still registered"
            );
        }
        lib.sync_xmp(&edit.id).unwrap();
        let sidecar = fs::read_dir(dir.join("Capture"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("xmp"))
            })
            .map(|path| fs::read_to_string(path).unwrap())
            .collect::<String>();
        assert!(sidecar.contains("Harbour, retouched"));
        for private in ["ada@example.org", "555 0100", "Kiel", "Private client"] {
            assert!(!sidecar.contains(private), "{private} in the sidecar");
        }
        // A render the library does not list is left alone.
        let elsewhere = EditTarget {
            target: dir
                .join("Capture/B_edit.tiff")
                .to_string_lossy()
                .into_owned(),
            ..target
        };
        repair_render_metadata(&elsewhere, &full).unwrap();
    }

    #[test]
    fn a_render_with_generated_areas_keeps_its_marking_in_the_library() {
        use rawtonic_catalog::Mode;
        let patches = serde_json::json!({"aiPatches": [{"visible": true, "patchData": {"color": "iVBORw0K", "mask": "iVBORw0K"}}]});
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonical().unwrap();
        fs::create_dir(root.join("Photos")).unwrap();
        let plain = root.join("Photos/A.jpg");
        let patched = root.join("Photos/B.jpg");
        fs::write(&plain, b"jpeg").unwrap();
        fs::write(&patched, b"jpeg").unwrap();
        let lib = Library::create(&root.join("C"), "C", Mode::Catalog).unwrap();
        let a = lib.add_asset(&plain).unwrap();
        let b = lib.add_asset(&patched).unwrap();
        lib.save_editor_state(
            &b.id,
            &serde_json::json!({"version": 1, "rating": 0, "adjustments": patches}),
        )
        .unwrap();
        let path = |p: &Path| p.to_string_lossy().into_owned();
        let queue = Queue::new();
        let composite = "compositeWithTrainedAlgorithmicMedia";
        let mut marked = String::new();
        // Also when the recipe strips the metadata.
        for keep in [true, false] {
            let mut recipe = tiff_recipe(None, Some(&lib.directory));
            recipe.export_settings.keep_metadata = keep;
            recipe.edit_with = Some(crate::export_queue::EditWithRun {
                id: format!("click-{keep}"),
                program: "Editor".into(),
                photos: vec![path(&plain), path(&patched)],
            });
            let (targets, jobs) =
                plan_edits(vec![path(&plain), path(&patched)], recipe, &queue.queue).unwrap();
            assert_eq!((targets.len(), jobs.len()), (2, 2));
            assert_eq!(
                (targets[0].generated, targets[1].generated),
                (false, true),
                "keep {keep}"
            );
            // The target kept for recovery and Try again says the same.
            for (target, job) in targets.iter().zip(&jobs) {
                assert_eq!(queue.request(job).edit_target.as_ref(), Some(target));
            }

            // Another program saves over the renders without any XMP.
            for target in &targets {
                fs::write(&target.target, b"tiff").unwrap();
            }
            let result = link_renders(&targets);
            assert_eq!((result.linked, result.problems.len()), (2, 0));
            let lib = Library::open(&lib.directory).unwrap();
            let edit =
                |target: &EditTarget| lib.asset_at(Path::new(&target.target)).unwrap().unwrap().id;
            let source_type = |id: &str| lib.facts(id).unwrap().unwrap().0.source_type;
            marked = edit(&targets[1]);
            assert!(lib.has_generated_provenance(&marked).unwrap());
            assert_eq!(source_type(&marked).as_deref(), Some(composite));
            let unmarked = edit(&targets[0]);
            assert!(!lib.has_generated_provenance(&unmarked).unwrap());
            assert_eq!(source_type(&unmarked), None);
            assert_eq!(lib.derived_from(&marked).unwrap().unwrap().id, b.id);
            assert_eq!(lib.derived_from(&unmarked).unwrap().unwrap().id, a.id);
        }

        // Reading the saved file again (Use XMP, the facts pass), also when
        // it states another source type now, and replacing its metadata
        // leave the marking.
        lib.record_facts(&marked, &rawtonic_catalog::CaptureFacts::default())
            .unwrap();
        lib.record_facts(
            &marked,
            &rawtonic_catalog::CaptureFacts {
                source_type: Some("digitalCapture".into()),
                ..Default::default()
            },
        )
        .unwrap();
        lib.set_metadata(&marked, &rawtonic_metadata::Metadata::default())
            .unwrap();
        assert!(lib.has_generated_provenance(&marked).unwrap());
        assert_eq!(
            lib.facts(&marked)
                .unwrap()
                .unwrap()
                .0
                .source_type
                .as_deref(),
            Some(composite)
        );
        // Its own exports (here Edit With again) are marked, whatever its
        // recipe, and so is their render.
        let render = lib.asset(&marked).unwrap().path;
        let (targets, jobs) = plan_edits(
            vec![path(&render)],
            tiff_recipe(None, Some(&lib.directory)),
            &queue.queue,
        )
        .unwrap();
        assert!(queue.snapshot(&jobs[0]).generated_source);
        assert!(targets[0].generated);

        // The open photograph is checked with the editor's recipe, which
        // its render takes, not with the one saved last.
        let (targets, jobs) = plan_edits(
            vec![path(&plain)],
            tiff_recipe(Some((&plain, patches.clone())), Some(&lib.directory)),
            &queue.queue,
        )
        .unwrap();
        assert!(targets[0].generated);
        assert!(!queue.snapshot(&jobs[0]).generated_source);
        assert_eq!(queue.snapshot(&jobs[0]).adjustments, patches);
        // Every render was queued: none is refused for its generated areas.
        assert_eq!(queue.jobs(), 6);
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

    /// LaunchServices on this Mac (needs the system's Preview): the lists
    /// Edit With shows for its renders (JPEG, TIFF) and Open With for a RAW.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn macos_lists_the_programs_for_a_jpeg_a_tiff_and_a_raw() {
        let tmp = tempfile::tempdir().unwrap();
        for name in ["probe.jpg", "probe.tiff", "probe.NEF"] {
            let file = tmp.path().join(name);
            fs::write(&file, b"").unwrap();
            let (apps, default) = platform::apps_for(&[file.as_path()]);
            // LaunchServices lists only the programs that claim a RAW's own
            // type when one does (Rawtonic claims each), not Preview, which
            // claims all camera RAW images.
            let raw = name.ends_with(".NEF");
            assert!(
                raw || apps.iter().any(|a| a.path.ends_with("/Preview.app")),
                "{name}: {apps:?}"
            );
            assert!(apps.iter().all(|a| {
                a.icon
                    .as_deref()
                    .is_some_and(|i| i.starts_with("data:image/png;base64,"))
            }));
            assert!(default.is_some(), "{name}");
            let names: Vec<_> = apps.iter().map(|a| (&a.name, &a.path)).collect();
            eprintln!("{name}: default {default:?}; {names:?}");
        }
    }
}
