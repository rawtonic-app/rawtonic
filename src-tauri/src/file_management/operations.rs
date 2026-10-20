//! File operations: folders, copy, move, rename, delete, import, duplicates and virtual copies.
//! Part of `file_management`, split by concern (ADR 0009).
use super::add_to_album;
use super::generate_filename_from_template;
use super::parse_virtual_path;
use crate::exif_processing;
use crate::image_processing::ImageMetadata;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use tauri::AppHandle;
use uuid::Uuid;
use walkdir::WalkDir;

#[tauri::command]
pub fn get_supported_file_types() -> Result<serde_json::Value, String> {
    let raw_extensions: Vec<&str> = crate::formats::RAW_EXTENSIONS
        .iter()
        .map(|(ext, _)| *ext)
        .collect();
    let non_raw_extensions: Vec<&str> = crate::formats::NON_RAW_EXTENSIONS.to_vec();

    Ok(serde_json::json!({
        "raw": raw_extensions,
        "nonRaw": non_raw_extensions
    }))
}

#[tauri::command]
pub async fn create_folder(path: String) -> Result<(), String> {
    crate::file_management::blocking(move || create_folder_blocking(path)).await?
}

fn create_folder_blocking(path: String) -> Result<(), String> {
    let path_obj = Path::new(&path);
    if let (Some(parent), Some(new_folder_name_os)) = (path_obj.parent(), path_obj.file_name())
        && let Some(new_folder_name) = new_folder_name_os.to_str()
        && parent.exists()
    {
        for entry in fs::read_dir(parent).map_err(|e| e.to_string())? {
            if let Ok(entry) = entry
                && entry.file_name().to_string_lossy().to_lowercase()
                    == new_folder_name.to_lowercase()
            {
                return Err("A folder with that name already exists.".to_string());
            }
        }
    }
    fs::create_dir_all(&path).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn rename_folder(
    path: String,
    new_name: String,
    app_handle: AppHandle,
) -> Result<crate::file_operations::OperationSummary, String> {
    crate::file_management::blocking(move || rename_folder_blocking(path, new_name, app_handle))
        .await?
}

fn rename_folder_blocking(
    path: String,
    new_name: String,
    app_handle: AppHandle,
) -> Result<crate::file_operations::OperationSummary, String> {
    let p = Path::new(&path);
    if !p.is_dir() {
        return Err("Path is not a directory.".to_string());
    }
    if new_name.is_empty() || new_name.contains(['/', '\\']) || new_name == "." || new_name == ".."
    {
        return Err("That is not a valid folder name.".to_string());
    }
    let parent = p.parent().ok_or("Could not determine parent directory.")?;
    for entry in fs::read_dir(parent).map_err(|e| e.to_string())? {
        if let Ok(entry) = entry
            && entry.file_name().to_string_lossy().to_lowercase() == new_name.to_lowercase()
            && entry.path() != p
            && !entry
                .file_name()
                .eq_ignore_ascii_case(p.file_name().unwrap_or_default())
        {
            return Err("A folder with that name already exists.".to_string());
        }
    }
    let label = format!(
        "Rename folder {}",
        p.file_name().unwrap_or_default().to_string_lossy()
    );
    let op = crate::file_operations::run(
        &app_handle,
        "renameFolder",
        &label,
        vec![rawtonic_fileops::Step::rename(p, parent.join(&new_name))],
    )?;
    Ok((&op).into())
}

#[tauri::command]
pub async fn delete_folder(
    path: String,
    app_handle: AppHandle,
) -> Result<crate::file_operations::OperationSummary, String> {
    crate::file_management::blocking(move || delete_folder_blocking(path, app_handle)).await?
}

fn delete_folder_blocking(
    path: String,
    app_handle: AppHandle,
) -> Result<crate::file_operations::OperationSummary, String> {
    let folder = PathBuf::from(&path);
    if !folder.is_dir() {
        return Err("Path is not a directory.".to_string());
    }
    let label = format!(
        "Delete folder {}",
        folder.file_name().unwrap_or_default().to_string_lossy()
    );
    let steps = crate::file_operations::plan_trash(&[vec![folder]]);
    let op = crate::file_operations::run(&app_handle, "trash", &label, steps)?;
    Ok((&op).into())
}

#[tauri::command]
pub async fn duplicate_file(
    path: String,
    target_album_id: Option<String>,
    app_handle: AppHandle,
) -> Result<String, String> {
    crate::file_management::blocking(move || {
        duplicate_file_blocking(path, target_album_id, app_handle)
    })
    .await?
}

fn duplicate_file_blocking(
    path: String,
    target_album_id: Option<String>,
    app_handle: AppHandle,
) -> Result<String, String> {
    let (source_path, source_sidecar_path) = parse_virtual_path(&path);
    if !source_path.is_file() {
        return Err("Source path is not a file.".to_string());
    }

    let parent = source_path
        .parent()
        .ok_or("Could not get parent directory")?;
    let stem = source_path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or("Could not get file stem")?;
    let extension = source_path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("");

    let rrexif_of = |image: &Path| {
        let mut name = image.file_name().unwrap_or_default().to_os_string();
        name.push(".rrexif");
        image.with_file_name(name)
    };
    let source_rrexif = rrexif_of(&source_path);
    let has_sidecar = source_sidecar_path.exists();
    let has_rrexif = source_rrexif.exists();

    // The first name free for the photo and its sidecars; the photo is
    // written no-clobber, so a name taken meanwhile moves on to the next.
    let mut counter = 1;
    let (dest_path, dest_sidecar_path) = loop {
        let new_stem = if counter == 1 {
            format!("{}_copy", stem)
        } else {
            format!("{}_copy_{}", stem, counter - 1)
        };
        counter += 1;
        let dest_path = parent.join(format!("{}.{}", new_stem, extension));
        let (_, dest_sidecar_path) = parse_virtual_path(&dest_path.to_string_lossy());
        if taken(&dest_path)
            || (has_sidecar && taken(&dest_sidecar_path))
            || (has_rrexif && taken(&rrexif_of(&dest_path)))
        {
            continue;
        }
        match copy_to_new(&source_path, &dest_path) {
            Ok(()) => break (dest_path, dest_sidecar_path),
            Err(_) if taken(&dest_path) => continue,
            Err(e) => return Err(e),
        }
    };

    if has_sidecar {
        copy_to_new(&source_sidecar_path, &dest_sidecar_path)?;
    }
    if has_rrexif {
        let _ = copy_to_new(&source_rrexif, &rrexif_of(&dest_path));
    }

    let dest_path_str = dest_path.to_string_lossy().into_owned();

    if let Some(album_id) = target_album_id {
        let _ = add_to_album(album_id, vec![dest_path_str.clone()], app_handle);
    }

    Ok(dest_path_str)
}

pub(crate) fn find_all_associated_files(source_image_path: &Path) -> Result<Vec<PathBuf>, String> {
    let mut associated_files = vec![source_image_path.to_path_buf()];

    let mut rrexif_name = source_image_path
        .file_name()
        .unwrap_or_default()
        .to_os_string();
    rrexif_name.push(".rrexif");
    let rrexif_path = source_image_path.with_file_name(rrexif_name);

    if rrexif_path.exists() {
        associated_files.push(rrexif_path);
    }

    let parent_dir = source_image_path
        .parent()
        .ok_or("Could not determine parent directory")?;
    let source_filename = source_image_path
        .file_name()
        .ok_or("Could not get source filename")?
        .to_string_lossy();

    let primary_sidecar_name = format!("{}.rrdata", source_filename);
    let virtual_copy_prefix = format!("{}.", source_filename);

    if let Ok(entries) = fs::read_dir(parent_dir) {
        for entry in entries.filter_map(Result::ok) {
            let entry_path = entry.path();
            if !entry_path.is_file() {
                continue;
            }

            let entry_os_filename = entry.file_name();
            let entry_filename = entry_os_filename.to_string_lossy();

            if entry_filename == primary_sidecar_name
                || (entry_filename.starts_with(&virtual_copy_prefix)
                    && entry_filename.ends_with(".rrdata"))
            {
                associated_files.push(entry_path);
            }
        }
    }

    Ok(associated_files)
}

#[tauri::command]
pub async fn copy_files(
    source_paths: Vec<String>,
    destination_folder: String,
) -> Result<(), String> {
    crate::file_management::blocking(move || copy_files_blocking(source_paths, destination_folder))
        .await?
}

fn copy_files_blocking(
    source_paths: Vec<String>,
    destination_folder: String,
) -> Result<(), String> {
    let dest_path = Path::new(&destination_folder);
    if !dest_path.is_dir() {
        return Err(format!(
            "Destination is not a folder: {}",
            destination_folder
        ));
    }

    // In the order given, each photo once.
    let mut images: Vec<PathBuf> = Vec::new();
    for path in &source_paths {
        let image = parse_virtual_path(path).0;
        if !images.contains(&image) {
            images.push(image);
        }
    }
    for (source, dest) in plan_copies(&images, dest_path)? {
        copy_to_new(&source, &dest)?;
    }
    Ok(())
}

/// Whether anything (a file, folder or dangling link) is at `path`.
fn taken(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

/// Copies `from` to a new file `to`, verified and through a temporary file,
/// never replacing anything: fails when `to` exists, even if it appeared
/// during the copy.
fn copy_to_new(from: &Path, to: &Path) -> Result<(), String> {
    rawtonic_ingest::verified_copy(from, to, &AtomicBool::new(false), |_| {})
        .map(|_| ())
        .map_err(|e| format!("Copy failed for {}: {e:#}", from.display()))
}

/// Where each photo and its sidecars (editor data, virtual copies, EXIF
/// cache) are copied in `dest`, every target distinct and free. A photo
/// copied into its own folder, or whose name another photo of this copy
/// takes first, becomes `name_copy_N` with its sidecars; a name already in
/// `dest` stops the copy before anything is written.
fn plan_copies(images: &[PathBuf], dest: &Path) -> Result<Vec<(PathBuf, PathBuf)>, String> {
    let mut plan = Vec::new();
    let mut planned: HashSet<PathBuf> = HashSet::new();
    for image in images {
        let files = find_all_associated_files(image)?;
        let name = image
            .file_name()
            .ok_or("Could not get file name")?
            .to_string_lossy()
            .into_owned();
        let stem = image
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or("Could not get file stem")?;
        let extension = image.extension().and_then(|s| s.to_str()).unwrap_or("");
        let targets_for = |new_name: &str| -> Vec<PathBuf> {
            files
                .iter()
                .map(|file| {
                    let file_name = file.file_name().unwrap_or_default().to_string_lossy();
                    dest.join(file_name.replacen(&name, new_name, 1))
                })
                .collect()
        };
        let free = |targets: &[PathBuf]| targets.iter().all(|t| !taken(t) && !planned.contains(t));

        let same_folder = image.parent() == Some(dest);
        let mut targets = None;
        if !same_folder {
            let same_name = targets_for(&name);
            if let Some(existing) = same_name.iter().find(|t| taken(t)) {
                return Err(format!(
                    "Copy aborted: File already exists at destination: {}",
                    existing.display()
                ));
            }
            if free(&same_name) {
                targets = Some(same_name);
            }
        }
        let targets = match targets {
            Some(targets) => targets,
            None => (1..)
                .map(|counter| targets_for(&format!("{stem}_copy_{counter}.{extension}")))
                .find(|targets| free(targets))
                .expect("some numbered name is free"),
        };
        for (file, target) in files.into_iter().zip(targets) {
            planned.insert(target.clone());
            plan.push((file, target));
        }
    }
    Ok(plan)
}

#[tauri::command]
pub async fn move_files(
    source_paths: Vec<String>,
    destination_folder: String,
    app_handle: AppHandle,
) -> Result<crate::file_operations::OperationSummary, String> {
    crate::file_management::blocking(move || {
        move_files_blocking(source_paths, destination_folder, app_handle)
    })
    .await?
}

fn move_files_blocking(
    source_paths: Vec<String>,
    destination_folder: String,
    app_handle: AppHandle,
) -> Result<crate::file_operations::OperationSummary, String> {
    let dest_path = Path::new(&destination_folder);
    if !dest_path.is_dir() {
        return Err(format!(
            "Destination is not a folder: {}",
            destination_folder
        ));
    }

    let mut images: Vec<PathBuf> = Vec::new();
    for path in &source_paths {
        let image = parse_virtual_path(path).0;
        if !images.contains(&image) {
            images.push(image);
        }
    }
    for image in &images {
        if image.parent() == Some(dest_path) {
            return Err("Cannot move files into the same folder they are already in.".to_string());
        }
    }
    let steps = crate::file_operations::associated_files(&images)?
        .into_iter()
        .map(|file| {
            let target = dest_path.join(file.file_name().unwrap_or_default());
            rawtonic_fileops::Step::rename(file, target)
        })
        .collect();
    let folder = dest_path.file_name().unwrap_or_default().to_string_lossy();
    let label = if images.len() == 1 {
        format!("Move 1 photo to {folder}")
    } else {
        format!("Move {} photos to {folder}", images.len())
    };
    let op = crate::file_operations::run(&app_handle, "move", &label, steps)?;
    Ok((&op).into())
}

#[tauri::command]
pub async fn clear_all_sidecars(root_path: String) -> Result<usize, String> {
    crate::file_management::blocking(move || {
        clear_all_sidecars_blocking(root_path, &crate::file_operations::PlatformTrash)
    })
    .await?
}

/// Moves the editor sidecars and EXIF caches below `root_path` to the system
/// Trash (never deletes them): the edits can be put back from there. A file
/// the Trash refuses stays where it is and the error names it.
fn clear_all_sidecars_blocking(
    root_path: String,
    trash: &impl rawtonic_fileops::SystemTrash,
) -> Result<usize, String> {
    if !Path::new(&root_path).exists() {
        return Err(format!("Root path does not exist: {}", root_path));
    }

    let mut trashed = 0;
    let mut failed = Vec::new();
    let walker = WalkDir::new(root_path).into_iter();
    let mut libraries = HashMap::new();

    for entry in walker.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_file()
            && let Some(extension) = path.extension()
            && (extension == "rrdata" || extension == "rrexif")
        {
            if extension == "rrdata" && registered_sidecar(path, &mut libraries) {
                continue;
            }
            match trash.trash(path) {
                Ok(_) => trashed += 1,
                Err(e) => failed.push(format!("{}: {e:#}", path.display())),
            }
        }
    }

    if failed.is_empty() {
        Ok(trashed)
    } else {
        Err(format!(
            "{trashed} sidecar files moved to the Trash; {} could not be moved and were kept: {}",
            failed.len(),
            failed.join("; ")
        ))
    }
}

/// Whether a `.rrdata` sidecar belongs to a photo (or a virtual copy of one)
/// registered in a session or catalog. Its edits live in that database, which
/// "Clear All Sidecar Files" leaves alone, so the sidecar stays too; when the
/// database cannot be read, it is kept to be safe.
fn registered_sidecar(
    sidecar: &Path,
    libraries: &mut HashMap<PathBuf, Option<rawtonic_catalog::Library>>,
) -> bool {
    let source = match crate::session_bridge::copy_for_sidecar(sidecar) {
        Some((source, _)) => source,
        None => match sidecar.to_str().and_then(|s| s.strip_suffix(".rrdata")) {
            Some(source) => PathBuf::from(source),
            None => return false,
        },
    };
    let Some(dir) = crate::session_bridge::library_dir_for(&source) else {
        return false;
    };
    match libraries
        .entry(dir.clone())
        .or_insert_with(|| rawtonic_catalog::Library::open(&dir).ok())
    {
        Some(lib) => !matches!(lib.asset_at(&source), Ok(None)),
        None => true,
    }
}

#[tauri::command]
pub fn show_in_finder(path: String) -> Result<(), String> {
    let (source_path, _) = parse_virtual_path(&path);

    #[cfg(target_os = "windows")]
    {
        let source_path_str = source_path.to_string_lossy().to_string();
        Command::new("explorer")
            .args(["/select,", &source_path_str])
            .spawn()
            .map_err(|e| e.to_string())?;
    }

    #[cfg(target_os = "macos")]
    {
        let source_path_str = source_path.to_string_lossy().to_string();
        Command::new("open")
            .args(["-R", &source_path_str])
            .spawn()
            .map_err(|e| e.to_string())?;
    }

    #[cfg(target_os = "linux")]
    {
        if let Some(parent) = source_path.parent() {
            Command::new("xdg-open")
                .arg(parent)
                .spawn()
                .map_err(|e| e.to_string())?;
        } else {
            return Err("Could not get parent directory".into());
        }
    }

    #[cfg(target_os = "ios")]
    {
        return Err("Show in File Manager is not supported on iOS.".into());
    }

    Ok(())
}

#[tauri::command]
pub async fn delete_files_from_disk(
    paths: Vec<String>,
    app_handle: AppHandle,
) -> Result<crate::file_operations::OperationSummary, String> {
    crate::file_management::blocking(move || delete_files_from_disk_blocking(paths, app_handle))
        .await?
}

fn delete_files_from_disk_blocking(
    paths: Vec<String>,
    app_handle: AppHandle,
) -> Result<crate::file_operations::OperationSummary, String> {
    let op = crate::file_operations::trash_photos(&app_handle, &paths, false)?;
    Ok((&op).into())
}

#[tauri::command]
pub async fn delete_files_with_associated(
    paths: Vec<String>,
    app_handle: AppHandle,
) -> Result<crate::file_operations::OperationSummary, String> {
    crate::file_management::blocking(move || {
        delete_files_with_associated_blocking(paths, app_handle)
    })
    .await?
}

fn delete_files_with_associated_blocking(
    paths: Vec<String>,
    app_handle: AppHandle,
) -> Result<crate::file_operations::OperationSummary, String> {
    let op = crate::file_operations::trash_photos(&app_handle, &paths, true)?;
    Ok((&op).into())
}

#[tauri::command]
pub async fn rename_files(
    paths: Vec<String>,
    name_template: String,
    app_handle: AppHandle,
) -> Result<Vec<String>, String> {
    crate::file_management::blocking(move || {
        rename_files_blocking(paths, name_template, app_handle)
    })
    .await?
}

fn rename_files_blocking(
    paths: Vec<String>,
    name_template: String,
    app_handle: AppHandle,
) -> Result<Vec<String>, String> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let mut renames: Vec<(PathBuf, PathBuf)> = Vec::new();
    for (i, path_str) in paths.iter().enumerate() {
        let (original_path, _) = parse_virtual_path(path_str);
        if !original_path.exists() {
            return Err(format!("File not found: {}", path_str));
        }
        if renames.iter().any(|(old, _)| old == &original_path) {
            continue;
        }
        let parent = original_path
            .parent()
            .ok_or("Could not get parent directory")?;
        let extension = original_path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        let file_date = exif_processing::get_creation_date_from_path(&original_path);
        let new_stem = generate_filename_from_template(
            &name_template,
            &original_path,
            i + 1,
            paths.len(),
            &file_date,
        );
        if new_stem.is_empty() || new_stem.contains(['/', '\\']) {
            return Err(format!("\"{new_stem}\" is not a valid file name."));
        }
        let new_path = parent.join(format!("{}.{}", new_stem, extension));
        if new_path != original_path {
            renames.push((original_path, new_path));
        }
    }
    if renames.is_empty() {
        return Ok(paths);
    }
    let images: Vec<PathBuf> = renames.iter().map(|(old, _)| old.clone()).collect();
    let files = crate::file_operations::associated_files(&images)?;
    let mut steps = Vec::new();
    for (old, new) in &renames {
        let old_name = old
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let new_name = new
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        for file in &files {
            let name = file.file_name().unwrap_or_default().to_string_lossy();
            let target = if file == old {
                new.clone()
            } else if file.parent() == old.parent()
                && let Some(rest) = name.strip_prefix(&format!("{old_name}."))
            {
                // Editor sidecars, virtual copies and the EXIF cache.
                new.with_file_name(format!("{new_name}.{rest}"))
            } else if file == &old.with_extension("xmp") {
                new.with_extension("xmp")
            } else {
                continue;
            };
            steps.push(rawtonic_fileops::Step::rename(file, target));
        }
    }
    let label = if renames.len() == 1 {
        "Rename 1 photo".to_string()
    } else {
        format!("Rename {} photos", renames.len())
    };
    crate::file_operations::run(&app_handle, "rename", &label, steps)?;
    let renamed: HashMap<&PathBuf, &PathBuf> = renames.iter().map(|(a, b)| (a, b)).collect();
    Ok(paths
        .iter()
        .map(|p| {
            let (image, _) = parse_virtual_path(p);
            match (renamed.get(&image), p.rsplit_once("?vc=")) {
                (Some(new), Some((_, id))) => format!("{}?vc={id}", new.to_string_lossy()),
                (Some(new), None) => new.to_string_lossy().into_owned(),
                (None, _) => p.clone(),
            }
        })
        .collect())
}

#[tauri::command]
pub async fn create_virtual_copy(
    source_virtual_path: String,
    target_album_id: Option<String>,
    app_handle: AppHandle,
) -> Result<String, String> {
    crate::file_management::blocking(move || {
        create_virtual_copy_blocking(source_virtual_path, target_album_id, app_handle)
    })
    .await?
}

/// A new virtual copy's path (`<photo>?vc=<id>`) and sidecar, with an id
/// (from `new_id`) no copy of the photograph has yet: writing a taken one
/// would replace that copy's edit. `taken`: whether a copy's sidecar is in use.
fn new_copy_path(
    source: &Path,
    taken: impl Fn(&Path) -> bool,
    mut new_id: impl FnMut() -> String,
) -> Result<(String, PathBuf), String> {
    for _ in 0..16 {
        let path = format!("{}?vc={}", source.to_string_lossy(), new_id());
        let (_, sidecar) = parse_virtual_path(&path);
        if !taken(&sidecar) {
            return Ok((path, sidecar));
        }
    }
    Err("Could not find a free id for the virtual copy".to_string())
}

fn create_virtual_copy_blocking(
    source_virtual_path: String,
    target_album_id: Option<String>,
    app_handle: AppHandle,
) -> Result<String, String> {
    let (source_path, source_sidecar_path) = parse_virtual_path(&source_virtual_path);

    let (new_virtual_path, new_sidecar_path) =
        new_copy_path(&source_path, crate::session_bridge::copy_exists, || {
            Uuid::new_v4().to_string()[..6].to_string()
        })?;

    // The new copy starts from the photograph's state: its sidecar, or for an
    // offline photograph the library's. Written through the library, which
    // keeps the copy (offline it is kept there only).
    let source_state = fs::read(&source_sidecar_path).ok().or_else(|| {
        crate::session_bridge::read_editor_sidecar(&source_sidecar_path)
            .and_then(|state| serde_json::to_vec_pretty(&state).ok())
    });
    if let Some(bytes) = source_state {
        crate::session_bridge::write_editor_sidecar(&new_sidecar_path, bytes)
            .map_err(|e| format!("Failed to copy sidecar file: {}", e))?;
    } else {
        let default_metadata = ImageMetadata::default();
        let json_string =
            serde_json::to_string_pretty(&default_metadata).map_err(|e| e.to_string())?;
        crate::session_bridge::write_editor_sidecar(new_sidecar_path, json_string)
            .map_err(|e| e.to_string())?;
    }

    if let Some(album_id) = target_album_id {
        let _ = add_to_album(album_id, vec![new_virtual_path.clone()], app_handle);
    }

    Ok(new_virtual_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn read(path: &Path) -> String {
        fs::read_to_string(path).unwrap()
    }

    /// A Trash folder that records what it was given (and refuses `refuse`).
    struct FakeTrash {
        dir: PathBuf,
        got: std::sync::Mutex<Vec<PathBuf>>,
        refuse: Option<PathBuf>,
    }
    impl FakeTrash {
        fn new(dir: PathBuf, refuse: Option<PathBuf>) -> Self {
            fs::create_dir_all(&dir).unwrap();
            Self {
                dir,
                got: Default::default(),
                refuse,
            }
        }
    }
    impl rawtonic_fileops::SystemTrash for FakeTrash {
        fn trash(&self, path: &Path) -> anyhow::Result<String> {
            if self.refuse.as_deref() == Some(path) {
                anyhow::bail!("Trash full");
            }
            let mut got = self.got.lock().unwrap();
            let item = self.dir.join(got.len().to_string());
            fs::rename(path, &item)?;
            got.push(path.to_path_buf());
            Ok(item.display().to_string())
        }
        fn restore(&self, _: &str, _: &Path) -> anyhow::Result<()> {
            Ok(())
        }
        fn contains(&self, _: &str) -> bool {
            true
        }
    }

    #[test]
    fn clearing_sidecars_moves_them_to_the_trash_and_names_any_it_could_not() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        for name in [
            "a.jpg",
            "a.jpg.rrdata",
            "b.jpg",
            "b.jpg.rrexif",
            "c.jpg.rrdata",
        ] {
            write(&root.join(name), name);
        }
        let trash = FakeTrash::new(tmp.path().join("Trash"), Some(root.join("c.jpg.rrdata")));
        let error =
            clear_all_sidecars_blocking(root.to_string_lossy().into_owned(), &trash).unwrap_err();
        assert!(error.contains("2 sidecar files moved"), "{error}");
        assert!(error.contains("c.jpg.rrdata: Trash full"), "{error}");
        let mut got = trash.got.into_inner().unwrap();
        got.sort();
        assert_eq!(got, [root.join("a.jpg.rrdata"), root.join("b.jpg.rrexif")]);
        // In the Trash, not deleted; the refused one stays where it was.
        assert_eq!(fs::read_dir(tmp.path().join("Trash")).unwrap().count(), 2);
        assert!(root.join("c.jpg.rrdata").is_file());
        // The photos themselves are never touched.
        assert!(root.join("a.jpg").is_file() && root.join("b.jpg").is_file());

        let trash = FakeTrash::new(tmp.path().join("Trash2"), None);
        assert_eq!(
            clear_all_sidecars_blocking(root.to_string_lossy().into_owned(), &trash),
            Ok(1)
        );
    }

    #[test]
    fn a_new_virtual_copy_never_takes_an_existing_copys_id() {
        let tmp = tempfile::tempdir().unwrap();
        let photo = tmp.path().join("photo.raf");
        write(&photo, "raw");
        let (_, taken) = parse_virtual_path(&format!("{}?vc=aaaaaa", photo.display()));
        write(&taken, "the existing copy's edit");
        let mut ids = ["aaaaaa", "bbbbbb"].into_iter().map(String::from);
        let (path, sidecar) =
            new_copy_path(&photo, |p| p.exists(), || ids.next().unwrap()).unwrap();
        assert!(path.ends_with("?vc=bbbbbb"));
        assert_ne!(sidecar, taken);
        assert_eq!(read(&taken), "the existing copy's edit");
        // Only taken ids: an error, nothing replaced.
        assert!(new_copy_path(&photo, |_| true, || "aaaaaa".to_string()).is_err());
    }

    #[test]
    fn photos_of_the_same_name_copied_together_both_arrive() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let (a, b, dest) = (
            root.join("A/photo.jpg"),
            root.join("B/photo.jpg"),
            root.join("C"),
        );
        write(&a, "a");
        write(&root.join("A/photo.jpg.rrdata"), "edits a");
        write(&b, "b");
        write(&root.join("B/photo.jpg.rrdata"), "edits b");
        fs::create_dir(&dest).unwrap();

        copy_files_blocking(
            vec![
                a.to_string_lossy().into_owned(),
                b.to_string_lossy().into_owned(),
            ],
            dest.to_string_lossy().into_owned(),
        )
        .unwrap();
        // In the order given: the second one gets a numbered name, with its
        // own sidecar; nothing is replaced.
        assert_eq!(read(&dest.join("photo.jpg")), "a");
        assert_eq!(read(&dest.join("photo.jpg.rrdata")), "edits a");
        assert_eq!(read(&dest.join("photo_copy_1.jpg")), "b");
        assert_eq!(read(&dest.join("photo_copy_1.jpg.rrdata")), "edits b");
        assert_eq!(fs::read_dir(&dest).unwrap().count(), 4);
    }

    #[test]
    fn copying_never_replaces_a_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let (a, dest) = (root.join("A/photo.jpg"), root.join("C"));
        write(&a, "a");
        write(&root.join("A/photo.jpg.rrdata"), "edits a");
        // Only the sidecar's name is taken: the copy stops before writing.
        write(&dest.join("photo.jpg.rrdata"), "other edits");
        let error = copy_files_blocking(
            vec![a.to_string_lossy().into_owned()],
            dest.to_string_lossy().into_owned(),
        )
        .unwrap_err();
        assert!(error.contains("already exists"), "{error}");
        assert!(!dest.join("photo.jpg").exists());
        assert_eq!(read(&dest.join("photo.jpg.rrdata")), "other edits");

        // The write itself refuses a file that appeared after planning.
        write(&dest.join("late.jpg"), "late");
        assert!(copy_to_new(&a, &dest.join("late.jpg")).is_err());
        assert_eq!(read(&dest.join("late.jpg")), "late");
    }

    #[test]
    fn clearing_sidecars_leaves_session_photos_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let session = root.join("Job");
        rawtonic_catalog::Library::create(&session, "Job", rawtonic_catalog::Mode::Session)
            .unwrap();
        let photo = session.join("Capture/A.RAF");
        write(&photo, "a");
        rawtonic_catalog::Library::open(&session)
            .unwrap()
            .add_asset(&photo)
            .unwrap();
        for sidecar in ["Capture/A.RAF.rrdata", "Capture/A.RAF.c0ffee.rrdata"] {
            write(&session.join(sidecar), "{}");
        }
        write(&session.join("Capture/A.RAF.rrexif"), "{}");
        write(&root.join("Folder/B.jpg"), "b");
        write(&root.join("Folder/B.jpg.rrdata"), "{}");

        let trash = FakeTrash::new(tmp.path().join("Trash"), None);
        let deleted =
            clear_all_sidecars_blocking(root.to_string_lossy().into_owned(), &trash).unwrap();
        // The session keeps the edits (its database is what counts); the
        // folder's sidecar and the EXIF cache go.
        assert_eq!(deleted, 2);
        assert!(session.join("Capture/A.RAF.rrdata").is_file());
        assert!(session.join("Capture/A.RAF.c0ffee.rrdata").is_file());
        assert!(!session.join("Capture/A.RAF.rrexif").exists());
        assert!(!root.join("Folder/B.jpg.rrdata").exists());
        assert!(root.join("Folder/B.jpg").is_file());
    }

    #[test]
    fn a_copy_into_its_own_folder_takes_a_name_free_for_all_its_files() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let a = root.join("A/photo.jpg");
        write(&a, "a");
        write(&root.join("A/photo.jpg.rrdata"), "edits");
        // A stray sidecar holds the first numbered name.
        write(&root.join("A/photo_copy_1.jpg.rrdata"), "stray");
        let plan = plan_copies(std::slice::from_ref(&a), &root.join("A")).unwrap();
        assert_eq!(
            plan,
            vec![
                (a.clone(), root.join("A/photo_copy_2.jpg")),
                (
                    root.join("A/photo.jpg.rrdata"),
                    root.join("A/photo_copy_2.jpg.rrdata")
                ),
            ]
        );
    }
}
