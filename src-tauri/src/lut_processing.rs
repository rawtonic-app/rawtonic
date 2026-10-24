use image::{DynamicImage, Rgb, Rgb32FImage};
use serde::Serialize;
use std::fs::{create_dir_all, read_dir};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tauri::{AppHandle, Manager, State};

use crate::app_state::AppState;

pub use rawtonic_engine::lut::Lut;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LutEntry {
    pub name: String,
    pub path: String,
    pub is_built_in: bool,
    /// The pack an imported LUT belongs to: the folder (and subfolders)
    /// it was imported with, `" / "`-joined; `None` for a single file.
    pub group: Option<String>,
}

/// What an import added and which files it left out, and why.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LutImport {
    pub luts: Vec<LutEntry>,
    pub imported: usize,
    pub skipped: Vec<SkippedLut>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct SkippedLut {
    /// The file's name, with its path inside an imported folder.
    pub file: String,
    /// The reason in English (logs; shown beside the translated issue
    /// when that has no more to say).
    pub reason: String,
    /// What the frontend translates: `kind` (an engine
    /// [`rawtonic_engine::lut::LutIssue`], or `tooDeep`, `folderUnreadable`,
    /// `unreadable`, `copyFailed`) and its values.
    pub issue: serde_json::Value,
}

impl SkippedLut {
    fn new(file: String, reason: String, issue: serde_json::Value) -> Self {
        Self {
            file,
            reason,
            issue,
        }
    }

    /// A folder (or subfolder) that could not be read.
    fn folder(file: String, error: std::io::Error) -> Self {
        Self::new(
            file,
            error.to_string(),
            serde_json::json!({"kind": "folderUnreadable"}),
        )
    }
}

#[derive(Serialize)]
pub struct LutParseResult {
    pub size: u32,
}

fn strip_verbatim(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(&s).to_string())
}

fn film_luts_dir(app_handle: &AppHandle) -> Option<PathBuf> {
    app_handle
        .path()
        .resolve("resources/film_luts", tauri::path::BaseDirectory::Resource)
        .ok()
        .map(|p| strip_verbatim(&p))
}

pub fn get_luts_dir(app_data_dir: &Path) -> anyhow::Result<PathBuf> {
    let luts_dir = app_data_dir.join("luts");
    if !luts_dir.exists() {
        create_dir_all(&luts_dir)?;
    }
    Ok(luts_dir)
}

const EXTENSIONS: [&str; 6] = ["cube", "3dl", "png", "jpg", "jpeg", "tiff"];
/// Folder levels a pack may nest (a pack's own subfolders).
const MAX_DEPTH: usize = 4;

fn lut_extension(path: &Path) -> Option<String> {
    let extension = path.extension()?.to_str()?.to_lowercase();
    EXTENSIONS
        .contains(&extension.as_str())
        .then_some(extension)
}

/// Hidden files and folders (`.DS_Store`, AppleDouble `._x.cube` copies on
/// exFAT) are never LUTs.
fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_none_or(|n| n.starts_with('.'))
}

/// The LUTs in `dir` and its subfolders (packs), by group and name.
pub fn list_luts_in_dir(dir: &Path, is_built_in: bool) -> anyhow::Result<Vec<LutEntry>> {
    let mut entries: Vec<LutEntry> = Vec::new();
    if dir.exists() {
        collect_luts(dir, &[], is_built_in, &mut entries)?;
    }
    entries.sort_by_key(|a| {
        (
            a.group.as_ref().map(|g| g.to_lowercase()),
            a.name.to_lowercase(),
        )
    });
    Ok(entries)
}

fn collect_luts(
    dir: &Path,
    group: &[String],
    is_built_in: bool,
    entries: &mut Vec<LutEntry>,
) -> anyhow::Result<()> {
    for entry in read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if is_hidden(&path) {
            continue;
        }
        // Links are listed as files, never followed into folders.
        if entry.file_type()?.is_dir() {
            if group.len() < MAX_DEPTH
                && let Some(name) = path.file_name().and_then(|n| n.to_str())
            {
                let mut inner = group.to_vec();
                inner.push(name.to_string());
                // A pack that cannot be read is left out, not the library.
                if let Err(error) = collect_luts(&path, &inner, is_built_in, entries) {
                    log::warn!("Cannot list LUT pack '{}': {error}", path.display());
                }
            }
            continue;
        }
        if lut_extension(&path).is_some() {
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("LUT")
                .to_string();
            entries.push(LutEntry {
                name,
                path: strip_verbatim(&path).to_string_lossy().into_owned(),
                is_built_in,
                group: (!group.is_empty()).then(|| group.join(" / ")),
            });
        }
    }
    Ok(())
}

/// A folder name usable as a pack folder: no separators, no leading dot,
/// no control characters.
fn folder_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.').trim().to_string();
    if cleaned.is_empty() {
        "LUTs".to_string()
    } else {
        cleaned
    }
}

/// One file to import: where it is, its name in the report, and the pack
/// folder (relative to the LUT folder) it goes to.
struct ImportItem {
    source: PathBuf,
    label: String,
    folder: PathBuf,
    /// A file found while walking a folder: an image that is no HALD LUT
    /// (a pack's preview pictures) is passed over without a report.
    found_in_folder: bool,
}

/// The files `source_paths` name: a folder becomes a pack named after it
/// (its subfolders nested packs), several files a pack named after their
/// folder, a single file stays ungrouped.
fn import_items(source_paths: &[String]) -> (Vec<ImportItem>, Vec<SkippedLut>) {
    let mut items = Vec::new();
    let mut skipped = Vec::new();
    let files = source_paths
        .iter()
        .filter(|p| !Path::new(p).is_dir())
        .count();
    for source in source_paths {
        let source = Path::new(source);
        let name = source
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| source.to_string_lossy().into_owned());
        if source.is_dir() {
            let pack = PathBuf::from(folder_name(&name));
            if let Err(error) = walk_folder(source, &pack, &name, 1, &mut items, &mut skipped) {
                skipped.push(SkippedLut::folder(name, error));
            }
            continue;
        }
        let folder = match source.parent().and_then(|p| p.file_name()) {
            Some(parent) if files > 1 => PathBuf::from(folder_name(&parent.to_string_lossy())),
            _ => PathBuf::new(),
        };
        items.push(ImportItem {
            source: source.to_path_buf(),
            label: name,
            folder,
            found_in_folder: false,
        });
    }
    (items, skipped)
}

fn walk_folder(
    dir: &Path,
    folder: &Path,
    label: &str,
    depth: usize,
    items: &mut Vec<ImportItem>,
    skipped: &mut Vec<SkippedLut>,
) -> std::io::Result<()> {
    let mut entries: Vec<_> = read_dir(dir)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        if is_hidden(&path) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let kind = entry.file_type()?;
        if kind.is_dir() {
            // `depth` counts the pack folders down to `folder`, as the
            // listing counts groups.
            if depth < MAX_DEPTH {
                // A subfolder that cannot be read is reported on its own;
                // its siblings are still imported.
                let inner = format!("{label}/{name}");
                if let Err(error) = walk_folder(
                    &path,
                    &folder.join(folder_name(&name)),
                    &inner,
                    depth + 1,
                    items,
                    skipped,
                ) {
                    skipped.push(SkippedLut::folder(inner, error));
                }
            } else {
                skipped.push(SkippedLut::new(
                    format!("{label}/{name}"),
                    format!("Folders nested deeper than {MAX_DEPTH} levels are not imported"),
                    serde_json::json!({"kind": "tooDeep", "max": MAX_DEPTH}),
                ));
            }
        } else if lut_extension(&path).is_some() {
            items.push(ImportItem {
                source: path,
                label: format!("{label}/{name}"),
                folder: folder.to_path_buf(),
                found_in_folder: true,
            });
        }
    }
    Ok(())
}

/// Imports LUT files and folders into `dir`: each file is read once,
/// parsed from those bytes and written as a new file (temporary file,
/// published without replacing anything; a taken name gets a number).
/// Files that do not read as LUTs are reported, not copied.
pub fn import_luts_to_dir(dir: &Path, source_paths: &[String]) -> anyhow::Result<LutImport> {
    let (items, mut skipped) = import_items(source_paths);
    let mut imported = 0;
    for item in items {
        match import_one(dir, &item) {
            Ok(()) => imported += 1,
            Err(ImportError::NotALut(_)) if item.found_in_folder && !is_lut_text(&item.source) => {}
            Err(failure) => {
                let issue = failure.issue();
                let (ImportError::NotALut(error)
                | ImportError::Unreadable(error)
                | ImportError::Failed(error)) = failure;
                log::warn!("Skipping LUT '{}': {:#}", item.source.display(), error);
                skipped.push(SkippedLut::new(item.label, format!("{error:#}"), issue));
            }
        }
    }
    Ok(LutImport {
        luts: list_luts_in_dir(dir, false)?,
        imported,
        skipped,
    })
}

fn is_lut_text(path: &Path) -> bool {
    matches!(lut_extension(path).as_deref(), Some("cube" | "3dl"))
}

/// Why a file was not imported: it does not read as a LUT, it cannot be
/// read, or writing its copy failed.
enum ImportError {
    NotALut(anyhow::Error),
    Unreadable(anyhow::Error),
    Failed(anyhow::Error),
}

impl ImportError {
    /// The report's issue: the parser's own, else that the file could not
    /// be read (as a LUT) or its copy not written.
    fn issue(&self) -> serde_json::Value {
        match self {
            Self::NotALut(error) => error
                .downcast_ref::<rawtonic_engine::lut::LutIssue>()
                .and_then(|issue| serde_json::to_value(issue).ok())
                .unwrap_or_else(|| serde_json::json!({"kind": "unreadable"})),
            Self::Unreadable(_) => serde_json::json!({"kind": "unreadable"}),
            Self::Failed(_) => serde_json::json!({"kind": "copyFailed"}),
        }
    }
}

fn import_one(dir: &Path, item: &ImportItem) -> Result<(), ImportError> {
    let extension = lut_extension(&item.source).unwrap_or_default();
    let bytes = std::fs::read(&item.source).map_err(|e| ImportError::Unreadable(e.into()))?;
    rawtonic_engine::lut::parse_lut_bytes_for_import(&extension, bytes.clone())
        .map_err(ImportError::NotALut)?;
    write_copy(dir, item, &extension, &bytes).map_err(ImportError::Failed)
}

fn write_copy(dir: &Path, item: &ImportItem, extension: &str, bytes: &[u8]) -> anyhow::Result<()> {
    let folder = dir.join(&item.folder);
    create_dir_all(&folder)?;
    let stem = item
        .source
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("LUT");
    let mut temp = tempfile::NamedTempFile::new_in(&folder)?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    for suffix in 0..1000 {
        let name = if suffix == 0 {
            format!("{stem}.{extension}")
        } else {
            format!("{stem} ({suffix}).{extension}")
        };
        let path = folder.join(name);
        if path.symlink_metadata().is_ok() {
            continue;
        }
        match rawtonic_metadata::no_clobber::persist(temp, &path) {
            Ok(_) => {
                #[cfg(unix)]
                std::fs::File::open(&folder)?.sync_all()?;
                return Ok(());
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                // Taken meanwhile; `persist` has used up the temporary file.
                temp = tempfile::NamedTempFile::new_in(&folder)?;
                temp.write_all(bytes)?;
                temp.as_file().sync_all()?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    anyhow::bail!("no free name for '{stem}.{extension}'")
}

pub fn parse_lut_file(path_str: &str) -> anyhow::Result<Lut> {
    rawtonic_engine::lut::parse_lut_file(path_str)
}

pub fn generate_identity_lut_image(size: u32) -> DynamicImage {
    let width = size;
    let height = size * size;
    let mut img = Rgb32FImage::new(width, height);

    for z in 0..size {
        for y in 0..size {
            for x in 0..size {
                let r = x as f32 / (size - 1) as f32;
                let g = y as f32 / (size - 1) as f32;
                let b = z as f32 / (size - 1) as f32;

                img.put_pixel(x, z * size + y, Rgb([r, g, b]));
            }
        }
    }

    DynamicImage::ImageRgb32F(img)
}

pub fn convert_image_to_cube_lut(image: &DynamicImage, size: u32) -> Result<Vec<u8>, String> {
    let f32_image = image.to_rgb32f();
    let mut out = String::new();

    out.push_str(&format!("LUT_3D_SIZE {}\n", size));
    out.push_str("DOMAIN_MIN 0.0 0.0 0.0\n");
    out.push_str("DOMAIN_MAX 1.0 1.0 1.0\n");

    for z in 0..size {
        for y in 0..size {
            for x in 0..size {
                let pixel = f32_image.get_pixel(x, z * size + y);
                out.push_str(&format!(
                    "{:.6} {:.6} {:.6}\n",
                    pixel[0].clamp(0.0, 1.0),
                    pixel[1].clamp(0.0, 1.0),
                    pixel[2].clamp(0.0, 1.0)
                ));
            }
        }
    }

    Ok(out.into_bytes())
}

/// The file's modification time and length, which a cached LUT must match.
type FileStamp = Option<(std::time::SystemTime, u64)>;

fn stamp(path: &str) -> FileStamp {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

/// Parsed LUTs by path, each with the file stamp it was read at: a file
/// replaced since (a LUT removed and imported again under its name, a
/// linked LUT edited) is read again, never served from the cache.
#[derive(Default)]
pub struct LutCache(std::collections::HashMap<String, (FileStamp, Arc<Lut>)>);

impl LutCache {
    pub fn get_or_load(&mut self, path: &str) -> Result<Arc<Lut>, String> {
        let now = stamp(path);
        if let Some((read, lut)) = self.0.get(path)
            && now.is_some()
            && *read == now
        {
            return Ok(lut.clone());
        }
        let lut = parse_lut_file(path).map_err(|e| e.to_string())?;
        Ok(self.insert(path.to_string(), now, lut))
    }

    fn insert(&mut self, path: String, read: FileStamp, lut: Lut) -> Arc<Lut> {
        let lut = Arc::new(lut);
        self.0.insert(path, (read, lut.clone()));
        lut
    }
}

pub fn get_or_load_lut(state: &AppState, path: &str) -> Result<Arc<Lut>, String> {
    state.lut_cache.lock().unwrap().get_or_load(path)
}

#[tauri::command]
pub fn list_luts(app_handle: AppHandle) -> Result<Vec<LutEntry>, String> {
    let mut all_luts = Vec::new();

    if let Some(resource_path) = film_luts_dir(&app_handle)
        && let Ok(built_in) = list_luts_in_dir(&resource_path, true)
    {
        all_luts.extend(built_in);
    }

    let data_dir = app_handle
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?;
    let luts_dir = get_luts_dir(&data_dir).map_err(|e| e.to_string())?;

    if let Ok(user_luts) = list_luts_in_dir(&luts_dir, false) {
        all_luts.extend(user_luts);
    }

    Ok(all_luts)
}

/// Async, with the walking, parsing and copying in a blocking worker: a
/// large pack on slow storage must not hold up the window.
#[tauri::command]
pub async fn import_luts(
    app_handle: AppHandle,
    source_paths: Vec<String>,
) -> Result<LutImport, String> {
    let data_dir = app_handle
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?;
    let luts_dir = get_luts_dir(&data_dir).map_err(|e| e.to_string())?;
    let report = tokio::task::spawn_blocking(move || import_luts_to_dir(&luts_dir, &source_paths))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;

    Ok(LutImport {
        luts: list_luts(app_handle)?,
        ..report
    })
}

#[tauri::command]
pub fn remove_lut(app_handle: AppHandle, path: String) -> Result<Vec<LutEntry>, String> {
    let data_dir = app_handle
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?;
    let luts_dir = strip_verbatim(&get_luts_dir(&data_dir).map_err(|e| e.to_string())?);
    let target_path = strip_verbatim(Path::new(&path));

    if let Some(resource_path) = film_luts_dir(&app_handle)
        && target_path.starts_with(&resource_path)
    {
        return Err("Cannot delete built-in film emulations".to_string());
    }

    let target_path = user_lut_file(&luts_dir, &target_path)?;
    std::fs::remove_file(&target_path).map_err(|e| e.to_string())?;
    remove_empty_packs(&luts_dir, &target_path);

    list_luts(app_handle)
}

/// Removes the pack folders a removal left empty, from the removed file's
/// folder up to (not including) the LUT folder; a folder with anything
/// left in it stays (`remove_dir` refuses it).
fn remove_empty_packs(luts_dir: &Path, removed: &Path) {
    let Ok(luts_dir) = std::fs::canonicalize(luts_dir) else {
        return;
    };
    let mut folder = removed.parent();
    while let Some(dir) = folder {
        if dir == luts_dir || !dir.starts_with(&luts_dir) || std::fs::remove_dir(dir).is_err() {
            break;
        }
        folder = dir.parent();
    }
}

/// The directory entry `path` names when it lies inside the user LUT folder
/// `luts_dir` (its folder resolved, `..` and links included) and is a file or
/// a link; any other path is refused, so a removal never reaches another file.
/// A link itself is removed, never the file it points to.
fn user_lut_file(luts_dir: &Path, path: &Path) -> Result<PathBuf, String> {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err("LUT file not found".to_string());
    };
    let entry = std::fs::canonicalize(parent)
        .map_err(|_| "LUT file not found".to_string())?
        .join(name);
    let kind = std::fs::symlink_metadata(&entry)
        .map_err(|_| "LUT file not found".to_string())?
        .file_type();
    let dir = std::fs::canonicalize(luts_dir).map_err(|e| e.to_string())?;
    if !entry.starts_with(&dir) || kind.is_dir() {
        return Err(
            "Access denied: Cannot remove files outside the user LUT directory".to_string(),
        );
    }
    Ok(entry)
}

#[tauri::command]
pub fn load_and_parse_lut(path: String, state: State<AppState>) -> Result<LutParseResult, String> {
    let read = stamp(&path);
    let lut = parse_lut_file(&path).map_err(|e| e.to_string())?;
    let lut_size = lut.size;

    state.lut_cache.lock().unwrap().insert(path, read, lut);

    Ok(LutParseResult { size: lut_size })
}

#[cfg(test)]
mod tests {
    use super::{LutCache, SkippedLut, import_luts_to_dir, remove_empty_packs, user_lut_file};

    fn cube(value: f32) -> String {
        let mut text = "LUT_3D_SIZE 2\n".to_string();
        for _ in 0..8 {
            text.push_str(&format!("{value} {value} {value}\n"));
        }
        text
    }

    #[test]
    fn a_lut_file_replaced_under_its_name_is_read_again() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("Warm.cube");
        std::fs::write(&path, cube(0.25)).unwrap();
        let path = path.to_str().unwrap();
        let mut cache = LutCache::default();
        let first = cache.get_or_load(path).unwrap();
        assert_eq!(first.data[0], 0.25);
        assert!(std::sync::Arc::ptr_eq(
            &first,
            &cache.get_or_load(path).unwrap()
        ));
        // Removed and imported again (another file, other values).
        std::fs::remove_file(path).unwrap();
        std::fs::write(path, cube(0.5) + "\n").unwrap();
        let second = cache.get_or_load(path).unwrap();
        assert_eq!(second.data[0], 0.5);
        // Gone: an error, not the old LUT.
        std::fs::remove_file(path).unwrap();
        assert!(cache.get_or_load(path).is_err());
    }

    #[test]
    fn only_files_inside_the_user_lut_folder_may_be_removed() {
        let temp = tempfile::tempdir().unwrap();
        let luts = temp.path().join("luts");
        std::fs::create_dir(&luts).unwrap();
        std::fs::write(luts.join("Warm.cube"), "x").unwrap();
        std::fs::write(temp.path().join("photo.ARW"), "x").unwrap();

        assert!(user_lut_file(&luts, &luts.join("Warm.cube")).is_ok());
        // Lexically below the folder, really beside it.
        let escape = luts.join("..").join("photo.ARW");
        assert!(escape.starts_with(&luts));
        assert!(user_lut_file(&luts, &escape).is_err());
        assert!(user_lut_file(&luts, &luts).is_err());
        assert!(user_lut_file(&luts, &luts.join("Gone.cube")).is_err());
        assert!(user_lut_file(&luts, &luts.join("..")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn removing_a_linked_lut_removes_the_link_not_its_target() {
        let temp = tempfile::tempdir().unwrap();
        let luts = temp.path().join("luts");
        std::fs::create_dir(&luts).unwrap();
        std::fs::write(luts.join("Warm.cube"), "x").unwrap();
        std::os::unix::fs::symlink(luts.join("Warm.cube"), luts.join("Alias.cube")).unwrap();
        let entry = user_lut_file(&luts, &luts.join("Alias.cube")).unwrap();
        assert_eq!(entry.file_name().unwrap(), "Alias.cube");
        std::fs::remove_file(&entry).unwrap();
        assert!(luts.join("Warm.cube").exists());
    }

    fn strings(paths: &[&std::path::Path]) -> Vec<String> {
        paths
            .iter()
            .map(|p| p.to_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn a_folder_becomes_a_pack_and_bad_files_are_reported_one_by_one() {
        let temp = tempfile::tempdir().unwrap();
        let luts = temp.path().join("luts");
        std::fs::create_dir(&luts).unwrap();
        let pack = temp.path().join("Summer Pack");
        std::fs::create_dir_all(pack.join("Extras")).unwrap();
        std::fs::write(pack.join("Bright.cube"), cube(0.75)).unwrap();
        std::fs::write(pack.join("Soft.CUBE"), cube(0.5)).unwrap();
        std::fs::write(pack.join("Extras").join("Night.cube"), cube(0.1)).unwrap();
        std::fs::write(pack.join("Broken.cube"), "LUT_3D_SIZE 2\n0 0 0\n").unwrap();
        std::fs::write(pack.join("preview.jpg"), "not an image").unwrap();
        std::fs::write(pack.join("._Bright.cube"), "AppleDouble").unwrap();
        std::fs::write(pack.join("README.txt"), "text").unwrap();

        let report = import_luts_to_dir(&luts, &strings(&[&pack])).unwrap();
        assert_eq!(report.imported, 3);
        assert_eq!(report.skipped.len(), 1);
        assert_eq!(report.skipped[0].file, "Summer Pack/Broken.cube");
        assert!(
            report.skipped[0].reason.contains("mismatch"),
            "{:?}",
            report.skipped
        );
        assert_eq!(
            report.skipped[0].issue,
            serde_json::json!({"kind": "incomplete", "expected": 24, "found": 3})
        );
        let listed: Vec<_> = report
            .luts
            .iter()
            .map(|e| (e.group.clone(), e.name.clone()))
            .collect();
        assert_eq!(
            listed,
            [
                (Some("Summer Pack".into()), "Bright".into()),
                (Some("Summer Pack".into()), "Soft".into()),
                (Some("Summer Pack / Extras".into()), "Night".into()),
            ]
        );
        // The originals are untouched, the copies equal them.
        assert_eq!(
            std::fs::read_to_string(pack.join("Bright.cube")).unwrap(),
            cube(0.75)
        );
        assert_eq!(
            std::fs::read_to_string(luts.join("Summer Pack").join("Bright.cube")).unwrap(),
            cube(0.75)
        );
    }

    #[test]
    fn several_files_make_a_pack_one_file_stays_single_and_names_are_never_replaced() {
        let temp = tempfile::tempdir().unwrap();
        let luts = temp.path().join("luts");
        std::fs::create_dir(&luts).unwrap();
        let folder = temp.path().join("Downloads");
        std::fs::create_dir(&folder).unwrap();
        let (a, b) = (folder.join("A.cube"), folder.join("B.cube"));
        std::fs::write(&a, cube(0.2)).unwrap();
        std::fs::write(&b, cube(0.4)).unwrap();

        let report = import_luts_to_dir(&luts, &strings(&[&a, &b])).unwrap();
        assert_eq!(
            (report.imported, report.skipped),
            (2, Vec::<SkippedLut>::new())
        );
        assert!(
            report
                .luts
                .iter()
                .all(|e| e.group.as_deref() == Some("Downloads"))
        );

        let report = import_luts_to_dir(&luts, &strings(&[&a])).unwrap();
        let single: Vec<_> = report.luts.iter().filter(|e| e.group.is_none()).collect();
        assert_eq!(single.len(), 1);
        // Imported again: a second copy beside the first, which stays.
        import_luts_to_dir(&luts, &strings(&[&a])).unwrap();
        assert_eq!(
            std::fs::read_to_string(luts.join("A (1).cube")).unwrap(),
            cube(0.2)
        );
        std::fs::write(&a, cube(0.9)).unwrap();
        import_luts_to_dir(&luts, &strings(&[&a, &b])).unwrap();
        assert_eq!(
            std::fs::read_to_string(luts.join("Downloads").join("A.cube")).unwrap(),
            cube(0.2)
        );
        assert_eq!(
            std::fs::read_to_string(luts.join("Downloads").join("A (1).cube")).unwrap(),
            cube(0.9)
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_subfolder_is_reported_and_its_siblings_imported() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let luts = temp.path().join("luts");
        std::fs::create_dir(&luts).unwrap();
        let pack = temp.path().join("Pack");
        std::fs::create_dir_all(pack.join("A")).unwrap();
        std::fs::write(pack.join("A").join("Hidden.cube"), cube(0.1)).unwrap();
        std::fs::write(pack.join("Z.cube"), cube(0.2)).unwrap();
        std::fs::set_permissions(pack.join("A"), std::fs::Permissions::from_mode(0o000)).unwrap();
        let report = import_luts_to_dir(&luts, &strings(&[&pack]));
        std::fs::set_permissions(pack.join("A"), std::fs::Permissions::from_mode(0o755)).unwrap();
        let report = report.unwrap();
        assert_eq!(report.imported, 1);
        assert_eq!(report.luts[0].name, "Z");
        let files: Vec<_> = report.skipped.iter().map(|s| s.file.as_str()).collect();
        assert_eq!(files, ["Pack/A"]);
    }

    #[test]
    fn names_with_two_dots_import_and_load() {
        let temp = tempfile::tempdir().unwrap();
        let luts = temp.path().join("luts");
        std::fs::create_dir(&luts).unwrap();
        let pack = temp.path().join("Pack..v2");
        std::fs::create_dir(&pack).unwrap();
        std::fs::write(pack.join("Tone..v2.cube"), cube(0.4)).unwrap();
        let report = import_luts_to_dir(&luts, &strings(&[&pack])).unwrap();
        assert_eq!(report.imported, 1);
        let lut = super::parse_lut_file(&report.luts[0].path).unwrap();
        assert_eq!(lut.data[0], 0.4);
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_pack_leaves_the_others_listed() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let luts = temp.path().join("luts");
        std::fs::create_dir_all(luts.join("Locked")).unwrap();
        std::fs::create_dir_all(luts.join("Open")).unwrap();
        std::fs::write(luts.join("Locked").join("A.cube"), cube(0.1)).unwrap();
        std::fs::write(luts.join("Open").join("B.cube"), cube(0.2)).unwrap();
        std::fs::write(luts.join("C.cube"), cube(0.3)).unwrap();
        std::fs::set_permissions(luts.join("Locked"), std::fs::Permissions::from_mode(0o000))
            .unwrap();
        let listed = super::list_luts_in_dir(&luts, false);
        std::fs::set_permissions(luts.join("Locked"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        let names: Vec<_> = listed.unwrap().into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["C", "B"]);
    }

    #[test]
    fn removing_a_packs_last_lut_removes_its_empty_folders() {
        let temp = tempfile::tempdir().unwrap();
        let luts = temp.path().join("luts");
        let inner = luts.join("Pack").join("Extras");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(luts.join("Pack").join("Keep.cube"), "x").unwrap();
        std::fs::write(inner.join("Night.cube"), "x").unwrap();
        let entry = user_lut_file(&luts, &inner.join("Night.cube")).unwrap();
        std::fs::remove_file(&entry).unwrap();
        remove_empty_packs(&luts, &entry);
        assert!(!inner.exists());
        assert!(luts.join("Pack").join("Keep.cube").exists());
        assert!(luts.exists());
    }

    #[test]
    fn packs_nest_as_deep_as_the_listing_shows_and_deeper_folders_are_reported() {
        let temp = tempfile::tempdir().unwrap();
        let luts = temp.path().join("luts");
        std::fs::create_dir(&luts).unwrap();
        let pack = temp.path().join("Pack");
        let deepest = pack.join("a").join("b").join("c");
        std::fs::create_dir_all(deepest.join("d")).unwrap();
        std::fs::write(deepest.join("Shown.cube"), cube(0.3)).unwrap();
        std::fs::write(deepest.join("d").join("Deep.cube"), cube(0.3)).unwrap();

        let report = import_luts_to_dir(&luts, &strings(&[&pack])).unwrap();
        assert_eq!(report.imported, 1);
        assert_eq!(report.luts.len(), 1);
        assert_eq!(report.luts[0].group.as_deref(), Some("Pack / a / b / c"));
        assert_eq!(report.skipped.len(), 1);
        assert_eq!(report.skipped[0].file, "Pack/a/b/c/d");
        assert_eq!(
            report.skipped[0].issue,
            serde_json::json!({"kind": "tooDeep", "max": 4})
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_lut_that_reads_but_cannot_be_written_is_reported() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let luts = temp.path().join("luts");
        std::fs::create_dir_all(luts.join("Pack")).unwrap();
        let pack = temp.path().join("Pack");
        std::fs::create_dir(&pack).unwrap();
        // A HALD image (8 × 8 = 4³ entries) and a picture that is none.
        image::RgbImage::new(8, 8)
            .save(pack.join("Hald.png"))
            .unwrap();
        image::RgbImage::new(8, 5)
            .save(pack.join("preview.png"))
            .unwrap();
        std::fs::set_permissions(luts.join("Pack"), std::fs::Permissions::from_mode(0o555))
            .unwrap();
        let report = import_luts_to_dir(&luts, &strings(&[&pack]));
        std::fs::set_permissions(luts.join("Pack"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        let report = report.unwrap();
        assert_eq!(report.imported, 0);
        let files: Vec<_> = report.skipped.iter().map(|s| s.file.as_str()).collect();
        assert_eq!(files, ["Pack/Hald.png"]);
        assert_eq!(
            report.skipped[0].issue,
            serde_json::json!({"kind": "copyFailed"})
        );
    }
}
