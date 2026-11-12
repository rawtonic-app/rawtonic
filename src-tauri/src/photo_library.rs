//! Which library (session or catalog folder) a photograph belongs to, as
//! the app knows it: the library folder holding it (`LibraryLayout`), else
//! the open session that keeps one of its folders outside its own (its
//! Capture, Selects or Output folder on another drive, G38). The engine and
//! export take the answer as data (`library: Option<&Path>`); they do not
//! know the layout.
use rawtonic_catalog::layout::LibraryLayout;
use rawtonic_fs::Canonical;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

/// The folders the open session keeps outside its folder, as the app
/// registered them from its `session.json`.
static OUTSIDE_FOLDERS: RwLock<Vec<(PathBuf, Vec<PathBuf>)>> = RwLock::new(Vec::new());

/// Registers the folders the session at `library` keeps outside its
/// folder, replacing what was registered for it; none removes it.
pub(crate) fn set_outside_folders(library: &Path, folders: Vec<PathBuf>) {
    let mut registered = OUTSIDE_FOLDERS.write().unwrap_or_else(|e| e.into_inner());
    registered.retain(|(dir, _)| dir != library);
    if !folders.is_empty() {
        registered.push((library.to_path_buf(), folders));
    }
}

/// The session that registered a folder outside it holding `path` (the
/// folder itself or anything below it).
pub(crate) fn outside_folder_owner(path: &Path) -> Option<PathBuf> {
    let registered = OUTSIDE_FOLDERS.read().unwrap_or_else(|e| e.into_inner());
    if registered.is_empty() {
        return None;
    }
    let path = registered_form(path);
    registered
        .iter()
        .find(|(_, folders)| folders.iter().any(|folder| path.starts_with(folder)))
        .map(|(dir, _)| dir.clone())
}

/// The library folder whose files (camera profiles in `profiles/`,
/// `resources/`) a recipe of the photograph at `path` names: the library
/// folder holding it, else the open session that keeps its folder outside.
/// `path` may also be the photograph's editor sidecar, which lies beside it.
pub(crate) fn library_of(path: &Path) -> Option<PathBuf> {
    LibraryLayout::holding(path)
        .map(LibraryLayout::into_directory)
        .or_else(|| outside_folder_owner(path))
}

/// [`library_of`] for a path as the webview and the caches name it.
pub(crate) fn library_of_str(path: &str) -> Option<PathBuf> {
    library_of(Path::new(path))
}

/// The form libraries record paths in: canonical, also for a file (or a
/// drive) that is gone.
fn registered_form(path: &Path) -> PathBuf {
    if let Ok(canonical) = path.canonical() {
        return canonical;
    }
    match (
        path.parent().and_then(|p| p.canonical().ok()),
        path.file_name(),
    ) {
        (Some(parent), Some(name)) => parent.join(name),
        _ => path.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sessions_folder_outside_it_names_the_session_as_the_photos_library() {
        let dir = tempfile::tempdir().unwrap();
        let session = dir.path().join("Job");
        let drive = dir.path().join("Drive/Capture");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::create_dir_all(drive.join("Day 1")).unwrap();
        let drive = drive.canonical().unwrap();
        let photo = drive.join("Day 1/A.ARW");
        std::fs::write(&photo, b"a").unwrap();
        assert_eq!(library_of(&photo), None);
        set_outside_folders(&session, vec![drive.clone()]);
        assert_eq!(library_of(&photo), Some(session.clone()));
        assert_eq!(
            library_of(&photo.with_file_name("A.ARW.rrdata")),
            Some(session.clone())
        );
        assert_eq!(outside_folder_owner(&drive), Some(session.clone()));
        assert_eq!(outside_folder_owner(&dir.path().join("Drive/B.ARW")), None);
        set_outside_folders(&session, Vec::new());
        assert_eq!(library_of(&photo), None);
    }

    #[test]
    fn a_photo_of_a_half_created_library_still_belongs_to_it() {
        let dir = tempfile::tempdir().unwrap();
        let session = dir.path().join("Job");
        std::fs::create_dir_all(session.join("Capture")).unwrap();
        let photo = session.join("Capture/A.NEF");
        assert_eq!(library_of(&photo), None);
        // The manifest without its database: a damaged library, not a
        // plain folder, so its camera profiles and previews stay its own.
        std::fs::write(
            session.join(rawtonic_catalog::layout::SESSION_MANIFEST),
            "{}",
        )
        .unwrap();
        assert_eq!(library_of(&photo), Some(session.clone()));
        assert_eq!(library_of_str(&photo.to_string_lossy()), Some(session));
    }
}
