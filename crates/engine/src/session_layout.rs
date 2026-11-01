//! Session folder detection. The layout itself is defined by `rawtonic-catalog`;
//! the engine only needs to find a session's own profile and cache folders.
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

/// The session folder holding `path` (a folder with `session.json` and
/// `session.sqlite` above it), if any.
pub fn session_root(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .skip(1)
        .find(|dir| {
            (dir.join("session.json").is_file() && dir.join("session.sqlite").is_file())
                || (dir.join("catalog.json").is_file() && dir.join("catalog.sqlite").is_file())
        })
        .map(Path::to_path_buf)
}

/// The photographs each library keeps outside its own folder (a session's
/// photos the app moved to another drive, ADR 0016), as whoever opened the
/// library registered them from its records.
static OUTSIDE: RwLock<Vec<(PathBuf, HashSet<PathBuf>)>> = RwLock::new(Vec::new());

/// Registers the photographs (and companion files) the library at `library`
/// keeps outside its folder, replacing what was registered for it; none
/// removes the library.
pub fn set_outside_photos(library: &Path, photos: HashSet<PathBuf>) {
    let mut outside = OUTSIDE.write().unwrap_or_else(|e| e.into_inner());
    outside.retain(|(dir, _)| dir != library);
    if !photos.is_empty() {
        outside.push((library.to_path_buf(), photos));
    }
}

/// The folders the open session keeps outside its folder (its Capture,
/// Selects or Output folder on another drive, G38), as the app registered
/// them from its `session.json`.
static OUTSIDE_FOLDERS: RwLock<Vec<(PathBuf, Vec<PathBuf>)>> = RwLock::new(Vec::new());

/// Registers the folders the session at `library` keeps outside its
/// folder, replacing what was registered for it; none removes it.
pub fn set_outside_folders(library: &Path, folders: Vec<PathBuf>) {
    let mut registered = OUTSIDE_FOLDERS.write().unwrap_or_else(|e| e.into_inner());
    registered.retain(|(dir, _)| dir != library);
    if !folders.is_empty() {
        registered.push((library.to_path_buf(), folders));
    }
}

/// The session that registered a folder outside it holding `path` (the
/// folder itself or anything below it).
pub fn outside_folder_owner(path: &Path) -> Option<PathBuf> {
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

/// The library (session or catalog folder) whose resources — `profiles/`,
/// `resources/` — a recipe of the photograph at `path` names: the library
/// folder holding it, else the library that registered it as one of its
/// photographs outside its folder. `path` may also be the photograph's
/// editor sidecar (`<file>.rrdata`, `<file>.<copy>.rrdata`).
pub fn library_root(path: &Path) -> Option<PathBuf> {
    session_root(path)
        .or_else(|| registered_owner(path))
        .or_else(|| outside_folder_owner(path))
}

fn registered_owner(path: &Path) -> Option<PathBuf> {
    let outside = OUTSIDE.read().unwrap_or_else(|e| e.into_inner());
    if outside.is_empty() {
        return None;
    }
    let candidates = photo_candidates(path);
    let owner = |candidate: &Path| {
        outside
            .iter()
            .find(|(_, photos)| photos.contains(candidate))
            .map(|(dir, _)| dir.clone())
    };
    candidates
        .iter()
        .find_map(|c| owner(c))
        .or_else(|| candidates.iter().find_map(|c| owner(&registered_form(c))))
}

/// `path`, or for an editor sidecar the photograph it belongs to.
fn photo_candidates(path: &Path) -> Vec<PathBuf> {
    let mut candidates = vec![path.to_path_buf()];
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return candidates;
    };
    if let Some(stem) = name.strip_suffix(".rrdata") {
        candidates.push(path.with_file_name(stem));
        // A virtual copy's sidecar: `<file>.<6 hex digits>.rrdata`.
        if let Some((file, copy)) = stem.rsplit_once('.')
            && copy.len() == 6
            && copy.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        {
            candidates.push(path.with_file_name(file));
        }
    }
    candidates
}

/// The form libraries record paths in: canonical, also for a file (or a
/// drive) that is gone.
fn registered_form(path: &Path) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    match (
        path.parent().and_then(|p| p.canonicalize().ok()),
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
        let drive = drive.canonicalize().unwrap();
        let photo = drive.join("Day 1/A.ARW");
        std::fs::write(&photo, b"a").unwrap();
        assert_eq!(library_root(&photo), None);
        set_outside_folders(&session, vec![drive.clone()]);
        assert_eq!(library_root(&photo), Some(session.clone()));
        assert_eq!(
            library_root(&photo.with_file_name("A.ARW.rrdata")),
            Some(session.clone())
        );
        assert_eq!(outside_folder_owner(&drive), Some(session.clone()));
        assert_eq!(outside_folder_owner(&dir.path().join("Drive/B.ARW")), None);
        set_outside_folders(&session, Vec::new());
        assert_eq!(library_root(&photo), None);
    }

    #[test]
    fn photos_outside_their_session_belong_to_the_library_that_registered_them() {
        let dir = tempfile::tempdir().unwrap();
        let session = dir.path().join("Job");
        std::fs::create_dir_all(session.join("Capture")).unwrap();
        std::fs::write(session.join("session.json"), "{}").unwrap();
        std::fs::write(session.join("session.sqlite"), "").unwrap();
        let drive = dir.path().join("Drive/Shoots");
        std::fs::create_dir_all(&drive).unwrap();
        let photo = drive.join("A.NEF");
        std::fs::write(&photo, "a").unwrap();
        let neighbour = drive.join("B.NEF");
        assert_eq!(
            library_root(&session.join("Capture/A.NEF")),
            Some(session.clone())
        );
        assert_eq!(library_root(&photo), None);

        set_outside_photos(&session, HashSet::from([photo.canonicalize().unwrap()]));
        assert_eq!(library_root(&photo), Some(session.clone()));
        assert_eq!(
            library_root(&drive.join("A.NEF.rrdata")),
            Some(session.clone())
        );
        assert_eq!(
            library_root(&drive.join("A.NEF.0a1b2c.rrdata")),
            Some(session.clone())
        );
        // Only its own photographs, never their neighbours.
        assert_eq!(library_root(&neighbour), None);
        assert_eq!(library_root(&drive.join("B.NEF.rrdata")), None);

        set_outside_photos(&session, HashSet::new());
        assert_eq!(library_root(&photo), None);
    }
}
