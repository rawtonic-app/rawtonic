//! Session folder detection. The layout itself is defined by `rawtonic-catalog`;
//! the engine only needs to find a session's own profile and cache folders.
use std::path::{Path, PathBuf};

/// The session folder holding `path` (a folder with `session.json` and
/// `session.sqlite` above it), if any.
pub fn session_root(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .skip(1)
        .find(|dir| dir.join("session.json").is_file() && dir.join("session.sqlite").is_file())
        .map(Path::to_path_buf)
}
