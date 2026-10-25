//! Which session owns a photograph outside every library folder (a
//! session's photographs the app moved to another drive, ADR 0016): one
//! recorded fact and one lookup. Each session records its outside
//! photographs in its database (external roots); the app keeps a copy of
//! those records for every session it has read, in the engine's registry
//! (`rawtonic_engine::session_layout`, which resolves recipe resources by
//! it) and in `outside-photos.json` in the app's config folder, so the
//! owner is known whichever library is open, after a restart and while the
//! session's own drive is away. `record` reads a session's records again;
//! a session that cannot be read keeps what was recorded. A session is
//! known by its identity (the id in `session.json`), not its folder: found
//! again in another folder (renamed or moved while closed), its records
//! there replace those of the folder it left.
use rawtonic_catalog::Library;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
    sync::Mutex,
};

/// Where the copy of the records is kept (unset: not kept, as in tests).
static RECORD_FILE: Mutex<Option<PathBuf>> = Mutex::new(None);

/// The identity of every session read, by folder, and where a session
/// found in another folder went (old folder → new).
#[derive(Default, Serialize, Deserialize)]
struct Identities {
    ids: BTreeMap<PathBuf, String>,
    moved: BTreeMap<PathBuf, PathBuf>,
}

static IDENTITIES: Mutex<Identities> = Mutex::new(Identities {
    ids: BTreeMap::new(),
    moved: BTreeMap::new(),
});

/// What `outside-photos.json` holds.
#[derive(Default, Serialize, Deserialize)]
struct Records {
    /// Each session's photographs outside its folder.
    sessions: BTreeMap<PathBuf, Vec<PathBuf>>,
    #[serde(flatten)]
    identities: Identities,
}

fn identities() -> std::sync::MutexGuard<'static, Identities> {
    IDENTITIES.lock().unwrap_or_else(|e| e.into_inner())
}

/// Sets where the records are kept and loads what is recorded there.
pub(crate) fn load_records(file: PathBuf) {
    if let Ok(text) = std::fs::read(&file) {
        match serde_json::from_slice::<Records>(&text) {
            Ok(records) => {
                for (dir, photos) in records.sessions {
                    rawtonic_engine::session_layout::set_outside_photos(
                        &dir,
                        photos.into_iter().collect(),
                    );
                }
                *identities() = records.identities;
            }
            Err(e) => log::warn!("{} not read: {e}", file.display()),
        }
    }
    *RECORD_FILE.lock().unwrap_or_else(|e| e.into_inner()) = Some(file);
}

/// Reads again which photographs the session `dir` keeps outside its folder
/// and records them; fails, keeping what was recorded, when the session
/// cannot be read (its drive is away, its database cannot be opened). A
/// catalog has none (it manages every path). The same session recorded in
/// another folder that no longer holds it (renamed or moved while closed)
/// is this one: its records are dropped and that folder leads here.
pub(crate) fn record(dir: &Path) -> Result<(), String> {
    if dir.join("catalog.json").is_file() {
        return Ok(());
    }
    if !dir.join("session.json").is_file() {
        return Err(format!("The session {} is not available", dir.display()));
    }
    let lib = Library::open(dir)
        .map_err(|e| format!("The session {} cannot be read: {e:#}", dir.display()))?;
    let photos: HashSet<PathBuf> = lib
        .outside_photos()
        .map_err(|e| format!("The session {} cannot be read: {e:#}", dir.display()))?
        .into_iter()
        .collect();
    let id = lib.manifest.id.clone();
    drop(lib);
    {
        let mut known = identities();
        let left: Vec<PathBuf> = known
            .ids
            .iter()
            .filter(|&(other, other_id)| {
                other.as_path() != dir && *other_id == id && session_id(other).as_ref() != Some(&id)
            })
            .map(|(other, _)| other.clone())
            .collect();
        // Folders that led to one of those lead here now.
        for to in known.moved.values_mut() {
            if left.contains(to) {
                *to = dir.to_path_buf();
            }
        }
        for other in left {
            rawtonic_engine::session_layout::set_outside_photos(&other, HashSet::new());
            known.ids.remove(&other);
            known.moved.insert(other, dir.to_path_buf());
        }
        // A session's own folder again (renamed back).
        known.moved.remove(dir);
        known.ids.insert(dir.to_path_buf(), id);
    }
    rawtonic_engine::session_layout::set_outside_photos(dir, photos);
    persist();
    Ok(())
}

/// The identity in the session manifest at `dir`, when one is there.
fn session_id(dir: &Path) -> Option<String> {
    let manifest = std::fs::read(dir.join("session.json")).ok()?;
    let manifest: rawtonic_catalog::Manifest = serde_json::from_slice(&manifest).ok()?;
    Some(manifest.id)
}

/// Where the session recorded at `dir` is now: the folder it was found in
/// again when it left `dir` (renamed or moved while closed), else `dir`.
pub(crate) fn now_at(dir: &Path) -> PathBuf {
    identities()
        .moved
        .get(dir)
        .cloned()
        .unwrap_or_else(|| dir.to_path_buf())
}

/// Writes the records (atomically; one writer at a time).
fn persist() {
    let file = RECORD_FILE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(file) = file.as_ref() else {
        return;
    };
    let known = identities();
    let records = Records {
        sessions: rawtonic_engine::session_layout::outside_photos_snapshot()
            .into_iter()
            .collect(),
        identities: Identities {
            ids: known.ids.clone(),
            moved: known.moved.clone(),
        },
    };
    drop(known);
    let written = serde_json::to_vec(&records)
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
    *identities() = Identities::default();
}
