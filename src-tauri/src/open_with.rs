//! Open With and Edit With: the selected photographs' originals, or renders
//! of them, in another program.
//!
//! Open With hands the original files to the program the user picks and
//! never writes or moves them. Edit With renders each photograph through the
//! normal export (the frontend queues it with the chosen recipe to the files
//! `edit_with_targets` names), then `edit_with_finish` registers each render
//! as a photograph of its own stacked with its original
//! (`Library::set_derived_from`), opens them all in the program in one
//! launch and watches them, so a save there refreshes the thumbnail.
//!
//! The programs offered for a file come from LaunchServices on macOS. Other
//! systems report `supported: false` and the menus show the entries disabled.
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
/// count as used. The render itself is written no-clobber, so a file that
/// appears in between makes it fail rather than be overwritten.
pub fn edit_target(original: &Path, extension: &str, taken: &HashSet<PathBuf>) -> PathBuf {
    let folder = original.parent().unwrap_or(Path::new(""));
    let stem = original
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    // Names differing only in case count as taken on every volume, so a
    // render never depends on whether the volume tells them apart.
    let lower = |path: &Path| path.file_name().map(|n| n.to_string_lossy().to_lowercase());
    let mut used: HashSet<String> = fs::read_dir(folder)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().to_lowercase())
                .collect()
        })
        .unwrap_or_default();
    used.extend(
        taken
            .iter()
            .filter(|t| t.parent() == Some(folder))
            .filter_map(|t| lower(t)),
    );
    (1u32..)
        .map(|n| {
            let suffix = if n == 1 {
                String::new()
            } else {
                format!("-{n}")
            };
            folder.join(format!("{stem}{EDIT_SUFFIX}{suffix}.{extension}"))
        })
        .find(|path| lower(path).is_some_and(|name| !used.contains(&name)))
        .expect("a free name")
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

/// Free names beside the originals for the renders of `paths`.
#[tauri::command]
pub fn edit_with_targets(paths: Vec<String>, extension: String) -> Result<Vec<EditTarget>, String> {
    if extension.is_empty() || !extension.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(format!("{extension:?} is not a file extension."));
    }
    let mut taken = HashSet::new();
    let mut seen = HashSet::new();
    paths
        .into_iter()
        .filter(|path| seen.insert(path.clone()))
        .map(|source| {
            let (original, _) = parse_virtual_path(&source);
            if !original.is_file() {
                return Err(format!("{} is not available.", original.display()));
            }
            let target = edit_target(&original, &extension, &taken);
            taken.insert(target.clone());
            Ok(EditTarget {
                source,
                target: target.to_string_lossy().into_owned(),
            })
        })
        .collect()
}

/// Registers `render` in `lib` and stacks it with `source`'s photograph,
/// which gives it its rating, label and keywords (as the original had them).
pub fn link_render(lib: &Library, source: &Path, render: &Path) -> anyhow::Result<()> {
    let original = match lib.asset_at(source)? {
        Some(asset) => asset,
        None => lib.add_asset(source)?,
    };
    let edit = lib.add_asset(render)?;
    lib.set_derived_from(&edit.id, &original.id)?;
    lib.set_metadata(&edit.id, &lib.metadata(&original.id)?)?;
    Ok(())
}

fn link_renders(items: &[EditTarget]) -> EditWithResult {
    let mut result = EditWithResult {
        linked: 0,
        problems: Vec::new(),
    };
    let mut by_library: HashMap<PathBuf, Vec<&EditTarget>> = HashMap::new();
    for item in items {
        let (source, _) = parse_virtual_path(&item.source);
        // Outside a session or catalog the render is a file in the folder.
        if let Some(dir) = crate::session_bridge::library_dir_for(&source) {
            by_library.entry(dir).or_default().push(item);
        }
    }
    for (dir, items) in by_library {
        let linked = crate::session_bridge::with_library_blocking(&dir, |lib| {
            Ok(items
                .iter()
                .map(|item| {
                    let (source, _) = parse_virtual_path(&item.source);
                    link_render(lib, &source, Path::new(&item.target))
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
        watch(&app, &renders);
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
    /// Renders that changed and wait for one unchanged check (the program
    /// may still be writing).
    settling: HashMap<PathBuf, Stamp>,
}

impl Watched {
    fn add(&mut self, path: &Path) {
        self.files.retain(|(p, _)| p != path);
        self.files.push((path.to_path_buf(), stamp(path)));
        let excess = self.files.len().saturating_sub(WATCHED_FILES);
        self.files.drain(..excess);
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
fn refresh_identity(render: &Path) {
    let Some(dir) = crate::session_bridge::library_dir_for(render) else {
        return;
    };
    let result = crate::session_bridge::with_library_blocking(&dir, |lib| {
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
fn watch(app: &AppHandle, renders: &[PathBuf]) {
    if let Ok(mut watched) = watched().lock() {
        for render in renders {
            watched.add(render);
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
                    let saved = watched().lock().map(|mut w| w.saved()).unwrap_or_default();
                    if !saved.is_empty() {
                        // Decodes are kept by path alone: drop the stale one.
                        crate::cache_utils::clear_image_caches(
                            app.state::<crate::app_state::AppState>(),
                        );
                    }
                    for path in &saved {
                        refresh_identity(path);
                    }
                    for path in saved {
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
        let first = edit_target(&original, "tif", &taken);
        assert_eq!(first, tmp.path().join("DSC_0001_edit.tif"));
        // Another render of the same run, then files already there.
        taken.insert(first.clone());
        assert_eq!(
            edit_target(&original, "tif", &taken),
            tmp.path().join("DSC_0001_edit-2.tif")
        );
        fs::write(tmp.path().join("DSC_0001_edit-2.tif"), b"there").unwrap();
        assert_eq!(
            edit_target(&original, "tif", &taken),
            tmp.path().join("DSC_0001_edit-3.tif")
        );
        // A name that differs only in case is taken too (case-insensitive volumes).
        fs::write(tmp.path().join("DSC_0001_EDIT-3.TIF"), b"there").unwrap();
        assert_eq!(
            edit_target(&original, "tif", &taken),
            tmp.path().join("DSC_0001_edit-4.tif")
        );
        // A different format starts again.
        assert_eq!(
            edit_target(&original, "jpg", &HashSet::new()),
            tmp.path().join("DSC_0001_edit.jpg")
        );
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
        let targets = edit_with_targets(
            vec![a.clone(), copy.clone(), b.clone(), a.clone()],
            "tif".into(),
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
        assert!(edit_with_targets(vec![a.clone()], "t/f".into()).is_err());
        assert!(edit_with_targets(vec![format!("{a}.missing")], "tif".into()).is_err());
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
        let render = capture.join("A_edit.tif");
        fs::write(&render, b"tiff").unwrap();
        let before = fs::read(&source).unwrap();

        link_render(&lib, &source, &render).unwrap();

        let edit = lib.asset_at(&render).unwrap().unwrap();
        assert_eq!(lib.derived_from(&edit.id).unwrap().unwrap().id, original.id);
        assert_eq!(lib.metadata(&edit.id).unwrap().rating, 4);
        assert_eq!(fs::read(&source).unwrap(), before);
    }

    #[test]
    fn a_save_is_reported_once_it_has_settled() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("A_edit.tif");
        fs::write(&file, b"one").unwrap();
        let mut watched = Watched::default();
        watched.add(&file);
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
        assert_eq!(watched.saved(), [file]);
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
