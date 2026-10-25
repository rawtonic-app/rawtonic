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

/// Whether `test` holds for the photographs registered for `library`
/// (false when none are).
pub fn outside_photos_match(library: &Path, test: impl FnOnce(&HashSet<PathBuf>) -> bool) -> bool {
    let outside = OUTSIDE.read().unwrap_or_else(|e| e.into_inner());
    outside
        .iter()
        .find(|(dir, _)| dir == library)
        .is_some_and(|(_, photos)| test(photos))
}

/// The library that registered photographs at or below `folder` (a folder
/// it keeps photographs in outside its own), if any.
pub fn registered_owner_below(folder: &Path) -> Option<PathBuf> {
    let outside = OUTSIDE.read().unwrap_or_else(|e| e.into_inner());
    let forms = [folder.to_path_buf(), registered_form(folder)];
    outside
        .iter()
        .find(|(_, photos)| {
            photos
                .iter()
                .any(|photo| forms.iter().any(|f| photo.starts_with(f)))
        })
        .map(|(dir, _)| dir.clone())
}

/// The library (session or catalog folder) whose resources — `profiles/`,
/// `resources/` — a recipe of the photograph at `path` names: the library
/// folder holding it, else the library that registered it as one of its
/// photographs outside its folder. `path` may also be the photograph's
/// editor sidecar (`<file>.rrdata`, `<file>.<copy>.rrdata`).
pub fn library_root(path: &Path) -> Option<PathBuf> {
    session_root(path).or_else(|| registered_owner(path))
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
