//! Which session owns a photograph outside every library folder (a
//! session's photographs the app moved to another drive, ADR 0016): one
//! recorded fact and one lookup. Each session records its outside
//! photographs in its database (external roots); the app keeps a copy of
//! those records for every session it has read, in the engine's registry
//! (`rawtonic_engine::session_layout`, which resolves recipe resources by
//! it) and in `outside-photos.json` in the app's config folder, so the
//! owner is known whichever library is open, after a restart and while the
//! session's own drive is away. `record` reads a session's records again;
//! a session that cannot be read keeps what was recorded.
use rawtonic_catalog::Library;
use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
    sync::Mutex,
};

/// Where the copy of the records is kept (unset: not kept, as in tests).
static RECORD_FILE: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Sets where the records are kept and loads what is recorded there.
pub(crate) fn load_records(file: PathBuf) {
    if let Ok(text) = std::fs::read(&file) {
        match serde_json::from_slice::<BTreeMap<PathBuf, Vec<PathBuf>>>(&text) {
            Ok(sessions) => {
                for (dir, photos) in sessions {
                    rawtonic_engine::session_layout::set_outside_photos(
                        &dir,
                        photos.into_iter().collect(),
                    );
                }
            }
            Err(e) => log::warn!("{} not read: {e}", file.display()),
        }
    }
    *RECORD_FILE.lock().unwrap_or_else(|e| e.into_inner()) = Some(file);
}

/// Reads again which photographs the session `dir` keeps outside its folder
/// and records them; fails, keeping what was recorded, when the session
/// cannot be read (its drive is away, its database cannot be opened). A
/// catalog has none (it manages every path).
pub(crate) fn record(dir: &Path) -> Result<(), String> {
    if dir.join("catalog.json").is_file() {
        return Ok(());
    }
    if !dir.join("session.json").is_file() {
        return Err(format!("The session {} is not available", dir.display()));
    }
    let photos: HashSet<PathBuf> = Library::open(dir)
        .and_then(|lib| lib.outside_photos())
        .map_err(|e| format!("The session {} cannot be read: {e:#}", dir.display()))?
        .into_iter()
        .collect();
    rawtonic_engine::session_layout::set_outside_photos(dir, photos);
    persist();
    Ok(())
}

/// Writes the records (atomically; one writer at a time).
fn persist() {
    let file = RECORD_FILE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(file) = file.as_ref() else {
        return;
    };
    let sessions: BTreeMap<PathBuf, Vec<PathBuf>> =
        rawtonic_engine::session_layout::outside_photos_snapshot()
            .into_iter()
            .collect();
    let written = serde_json::to_vec(&sessions)
        .map_err(anyhow::Error::from)
        .and_then(|bytes| rawtonic_metadata::atomic_write(file, bytes));
    if let Err(e) = written {
        log::warn!("{} not written: {e:#}", file.display());
    }
}

/// The session that keeps the photograph at `path` (or its editor sidecar)
/// outside its folder, as recorded.
pub(crate) fn session_of(path: &Path) -> Option<PathBuf> {
    rawtonic_engine::session_layout::registered_library(path)
}

/// Every session that keeps photographs at or below `folder`, as recorded.
pub(crate) fn sessions_below(folder: &Path) -> Vec<PathBuf> {
    let mut all = rawtonic_engine::session_layout::registered_libraries_below(folder);
    if all.is_empty()
        && let Ok(real) = folder.canonicalize()
    {
        all = rawtonic_engine::session_layout::registered_libraries_below(&real);
    }
    all
}

/// Where the records are kept now (tests).
#[cfg(test)]
pub(crate) fn set_record_file_for_test(file: Option<PathBuf>) {
    *RECORD_FILE.lock().unwrap_or_else(|e| e.into_inner()) = file;
}
