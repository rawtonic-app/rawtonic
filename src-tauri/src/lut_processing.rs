use image::{DynamicImage, Rgb, Rgb32FImage};
use serde::Serialize;
use std::fs::{copy, create_dir_all, read_dir};
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

pub fn list_luts_in_dir(dir: &Path, is_built_in: bool) -> anyhow::Result<Vec<LutEntry>> {
    let mut entries: Vec<LutEntry> = Vec::new();
    if !dir.exists() {
        return Ok(entries);
    }
    for entry in read_dir(dir)? {
        let path = entry?.path();
        let extension = path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_lowercase();

        let is_supported = matches!(
            extension.as_str(),
            "cube" | "3dl" | "png" | "jpg" | "jpeg" | "tiff"
        );

        if is_supported {
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("LUT")
                .to_string();

            entries.push(LutEntry {
                name,
                path: strip_verbatim(&path).to_string_lossy().into_owned(),
                is_built_in,
            });
        }
    }
    entries.sort_by_key(|a| a.name.to_lowercase());
    Ok(entries)
}

fn unique_lut_destination(dir: &Path, stem: &str, extension: &str) -> PathBuf {
    let mut candidate = dir.join(format!("{}.{}", stem, extension));
    let mut suffix = 1;
    while candidate.exists() && suffix < 1000 {
        candidate = dir.join(format!("{} ({}).{}", stem, suffix, extension));
        suffix += 1;
    }
    candidate
}

pub fn import_luts_to_dir(dir: &Path, source_paths: &[String]) -> anyhow::Result<Vec<LutEntry>> {
    for source in source_paths {
        if let Err(error) = parse_lut_file(source) {
            log::warn!("Skipping invalid LUT '{}': {}", source, error);
            continue;
        }

        let source_path = Path::new(source);
        let stem = source_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("LUT");
        let extension = source_path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("cube")
            .to_lowercase();
        let destination = unique_lut_destination(dir, stem, &extension);
        if let Err(error) = copy(source_path, &destination) {
            log::error!("Failed to copy LUT '{}': {}", source, error);
        }
    }
    list_luts_in_dir(dir, false)
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

pub fn get_or_load_lut(state: &State<AppState>, path: &str) -> Result<Arc<Lut>, String> {
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

#[tauri::command]
pub fn import_luts(
    app_handle: AppHandle,
    source_paths: Vec<String>,
) -> Result<Vec<LutEntry>, String> {
    let data_dir = app_handle
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?;
    let luts_dir = get_luts_dir(&data_dir).map_err(|e| e.to_string())?;
    import_luts_to_dir(&luts_dir, &source_paths).map_err(|e| e.to_string())?;

    list_luts(app_handle)
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

    list_luts(app_handle)
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
    use super::{LutCache, user_lut_file};

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
}
