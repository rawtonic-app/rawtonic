//! Synchronize Folder: finds what changed on disk outside Rawtonic in a
//! folder of the library (or the whole library) and brings the library in
//! line, with the user's review in between.
//!
//! [`Library::scan_folder_sync`] lists the folder and compares it with the
//! library. It reads only; files are hashed and nothing is written. It
//! classifies every difference:
//! - **verified move**: an unknown file whose full SHA-256 equals the one on
//!   record for a missing photograph (renamed, moved between folders, or
//!   both). Also a file renamed only in letter case on a case-insensitive
//!   volume.
//! - **probable move**: size and sampled fingerprint match a missing
//!   photograph, but there is no hash on record to prove it.
//! - **copy**: the same bytes as a photograph that is still there.
//! - **contested**: matches a missing photograph and is also a copy of one
//!   that is there, or several files and photographs match one another.
//!   The user picks.
//! - **new**: no match.
//! - **removed**: a photograph whose file is gone, with nothing found for it,
//!   below a folder the scan listed and whose library folder (the session
//!   or catalog folder, or a catalog folder it is in) is there.
//! - **unavailable**: a photograph on a disconnected drive, in a folder that
//!   could not be read, or whose library folder is not there or empty (a
//!   drive or network share that is not connected, also where the system
//!   gives no drive identity: Windows, SMB/NAS shares). It is never treated
//!   as removed, and a removal is refused for it when applied.
//! - **error**: a file or folder that could not be read.
//!
//! [`Library::scan_files_sync`] classifies given files only (no folder is
//! listed, only files matching a photograph by size and sampled
//! fingerprint are hashed): what a library opening uses to follow renames.
//!
//! [`Library::apply_folder_sync`] applies what the user accepted and checks
//! each action against disk and database first, skipping what changed since
//! the scan. Moves and removals commit in one transaction, so ratings,
//! keywords, recipes, versions and albums follow a moved photograph. New
//! files are added with their sidecars in one more transaction
//! (`add_files`, import batch "sync"); when that fails, the moves and
//! removals stay applied and the files are reported skipped with the error.
//! Files on disk are never moved, modified or deleted; a removal only
//! removes the library's entry.
use crate::checksum::{sha256_file, stamp};
use crate::{Library, Mode, RelinkReport, add::ImportKind, fingerprint, resolve_moved};
use anyhow::{Context, Result, bail, ensure};
use rusqlite::TransactionBehavior;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

/// Folders of a library folder that hold no photographs of their own
/// (caches, masks, Trash, outputs, previews). A scan does not go into them.
pub const INTERNAL_FOLDERS: &[&str] = &[
    ".cache",
    "masks",
    "resources",
    "profiles",
    "Trash",
    "Output",
    "previews",
    "smart-previews",
];

/// The formats a scan looks at when the caller does not say (the app passes
/// its own list).
pub fn is_photo(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .is_some_and(|ext| {
            [
                "arw", "dng", "cr3", "cr2", "nef", "raf", "rw2", "orf", "pef", "jpg", "jpeg",
                "png", "tif", "tiff", "heic", "heif", "webp", "avif", "jxl",
            ]
            .contains(&ext.as_str())
        })
}

/// A scan was cancelled (`SyncOptions::cancel`).
#[derive(Debug)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Synchronization cancelled")
    }
}

impl std::error::Error for Cancelled {}

/// A file as a scan read it. An action whose file reads differently when
/// applied is skipped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileIdentity {
    pub size: u64,
    /// Modification time (seconds).
    pub mtime: i64,
    pub fingerprint: String,
}

impl FileIdentity {
    pub fn read(path: &Path) -> Result<Self> {
        let (size, mtime) =
            stamp(path).with_context(|| format!("{} is not a file", path.display()))?;
        Ok(Self {
            size,
            mtime,
            fingerprint: fingerprint(path)?,
        })
    }
}

/// A photograph whose file is now at `to`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncMove {
    pub asset_id: String,
    pub from: PathBuf,
    pub to: PathBuf,
    pub identity: FileIdentity,
    /// The full content hash equals the one on record. Otherwise only size
    /// and sampled fingerprint matched.
    pub verified: bool,
}

/// A file no photograph of the library is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncNewFile {
    pub path: PathBuf,
    pub identity: FileIdentity,
    /// Photographs (still there) it is a byte-for-byte copy of.
    pub copy_of: Vec<PathBuf>,
}

/// A file the scan cannot decide on: it may be any of `moves`, or a copy of
/// `copy_of`, or a new photograph. The user picks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncContested {
    pub path: PathBuf,
    pub identity: FileIdentity,
    pub moves: Vec<SyncMove>,
    pub copy_of: Vec<PathBuf>,
}

/// A photograph whose file is gone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncMissing {
    pub asset_id: String,
    pub path: PathBuf,
    /// The disconnected drive it is on (unavailable photographs).
    pub volume: Option<String>,
}

/// Something that could not be read, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncIssue {
    pub path: PathBuf,
    pub reason: String,
}

/// What a scan found, for the user to review.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncPlan {
    /// The folders scanned.
    pub folders: Vec<PathBuf>,
    /// Photograph files looked at.
    pub files: usize,
    pub verified_moves: Vec<SyncMove>,
    pub probable_moves: Vec<SyncMove>,
    pub new_files: Vec<SyncNewFile>,
    pub copies: Vec<SyncNewFile>,
    pub contested: Vec<SyncContested>,
    pub removed: Vec<SyncMissing>,
    pub unavailable: Vec<SyncMissing>,
    pub errors: Vec<SyncIssue>,
}

impl SyncPlan {
    /// Whether the scan found nothing to act on.
    pub fn is_empty(&self) -> bool {
        self.verified_moves.is_empty()
            && self.probable_moves.is_empty()
            && self.new_files.is_empty()
            && self.copies.is_empty()
            && self.contested.is_empty()
            && self.removed.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SyncPhase {
    /// Listing folders (`done` files found, no total yet).
    Listing,
    /// Reading files no photograph is (`done` of `total`).
    Checking,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SyncProgress {
    pub phase: SyncPhase,
    pub done: usize,
    pub total: usize,
}

/// How a scan runs.
pub struct SyncOptions<'a> {
    /// Which files are photographs.
    pub accept: &'a dyn Fn(&Path) -> bool,
    /// Stops the scan between files and hash chunks (`Cancelled`).
    pub cancel: &'a AtomicBool,
    pub progress: &'a dyn Fn(SyncProgress),
}

/// One of the accepted changes, as the scan found it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SyncAction {
    /// The photograph `asset_id`, registered at `from`, is now `to`.
    #[serde(rename_all = "camelCase")]
    Move {
        asset_id: String,
        from: PathBuf,
        to: PathBuf,
        identity: FileIdentity,
    },
    /// Add the file at `path` as a new photograph.
    #[serde(rename_all = "camelCase")]
    Add {
        path: PathBuf,
        identity: FileIdentity,
    },
    /// Remove the photograph `asset_id` (its file at `path` is gone) from
    /// the library. The library entry only; nothing on disk is touched.
    #[serde(rename_all = "camelCase")]
    Remove { asset_id: String, path: PathBuf },
}

impl From<&SyncMove> for SyncAction {
    fn from(change: &SyncMove) -> Self {
        Self::Move {
            asset_id: change.asset_id.clone(),
            from: change.from.clone(),
            to: change.to.clone(),
            identity: change.identity.clone(),
        }
    }
}

/// What applying did.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncApplied {
    /// (registered place, new place) of every photograph moved.
    pub moved: Vec<(PathBuf, PathBuf)>,
    pub added: Vec<PathBuf>,
    pub removed: Vec<PathBuf>,
    /// Actions not applied because disk or library changed since the scan
    /// (or a file cannot be added), with why.
    pub skipped: Vec<SyncIssue>,
    /// Why the new files could not be added, when adding them failed as a
    /// whole after the moves and removals were applied (each file is in
    /// `skipped` too).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub add_error: Option<String>,
}

impl SyncApplied {
    /// The moves as a relink report (previews and derived sidecars follow
    /// them as after a relink).
    pub fn relinked(&self) -> RelinkReport {
        RelinkReport {
            relinked: self.moved.clone(),
            ..RelinkReport::default()
        }
    }
}

/// A registered photograph as a scan sees it.
struct Known {
    id: String,
    path: PathBuf,
    size: u64,
    mtime: i64,
    fingerprint: String,
    sha256: Option<String>,
}

/// What a scan listed, to be classified.
struct Scanned<'a> {
    known: Vec<Known>,
    /// The disconnected drive of every photograph on one.
    disconnected: HashMap<String, String>,
    /// The scope: photographs below it are reported removed or unavailable.
    folders: &'a [PathBuf],
    /// The folders of the scope that were there and listed.
    walked: &'a [PathBuf],
    /// Photograph files found.
    files: Vec<PathBuf>,
    /// Folders that could not be read.
    failed: Vec<SyncIssue>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Present,
    Missing,
    Unavailable,
}

/// What one unknown file matched.
enum Found {
    New,
    Copy(Vec<usize>),
    /// Missing photographs it may be, each with whether it is verified.
    Moved(Vec<(usize, bool)>),
    /// Missing photographs it may be while it is also a copy of a
    /// photograph that is there.
    Contested(Vec<(usize, bool)>, Vec<usize>),
}

fn cancelled(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        return Err(Cancelled.into());
    }
    Ok(())
}

/// The same file under two names (a case-only rename on a case-insensitive
/// volume, or a hard link).
fn same_file(a: &Path, b: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match (fs::metadata(a), fs::metadata(b)) {
            (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
            _ => false,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (a, b);
        false
    }
}

/// `a` and `b` differ in letter case only.
fn case_only_rename(a: &Path, b: &Path) -> bool {
    a != b && a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
}

/// `path` is `folder` or below it.
fn below(path: &Path, folder: &Path) -> bool {
    path.starts_with(folder)
}

/// Whether `folder` is a folder with anything in it. An empty one may be
/// the mount point a drive or share left behind (`/mnt/nas` on Linux) and
/// is no evidence that the photographs registered in it are gone.
fn folder_there(folder: &Path) -> bool {
    fs::read_dir(folder).is_ok_and(|mut entries| entries.next().is_some())
}

/// What a registered photograph is as far as its file goes.
fn state_of(k: &Known, disconnected: &HashMap<String, String>, failed: &[SyncIssue]) -> State {
    if k.path.is_file() {
        State::Present
    } else if disconnected.contains_key(&k.id) || failed.iter().any(|f| below(&k.path, &f.path)) {
        State::Unavailable
    } else {
        State::Missing
    }
}

/// The library folders a removal must see: the library's own folder and
/// every external catalog folder, each checked once.
struct Anchors {
    directory: PathBuf,
    roots: Vec<PathBuf>,
    there: HashMap<PathBuf, bool>,
}

impl Anchors {
    /// Whether the photograph at `path`, whose file is gone, may be removed
    /// as far as its folders go: its library folder (the library's own
    /// folder, or a catalog folder it is in) is there and not empty. A
    /// photograph whose catalog folder is gone (a drive or share that is not
    /// connected, also without any drive identity) never is; for a folder
    /// deleted inside a library folder that is there, it is.
    fn allow(&mut self, path: &Path) -> bool {
        if below(path, &self.directory) {
            return true;
        }
        for root in self.roots.iter().filter(|root| below(path, root)) {
            if *self
                .there
                .entry(root.clone())
                .or_insert_with(|| folder_there(root))
            {
                return true;
            }
        }
        false
    }
}

impl Library {
    /// The folders a synchronization of `folder` (or of the whole library
    /// with `None`) lists: for a session its folder, for a catalog its own
    /// folder and every catalog folder outside it (the topmost ones).
    pub fn sync_scope(&self, folder: Option<&Path>) -> Result<Vec<PathBuf>> {
        if let Some(folder) = folder {
            let folder = resolve_moved(folder);
            ensure!(
                self.manifest.mode == Mode::Catalog || below(&folder, &self.directory),
                "{} is not a folder of this session",
                folder.display()
            );
            return Ok(vec![folder]);
        }
        let mut folders = vec![self.directory.clone()];
        if self.manifest.mode == Mode::Catalog {
            let roots: Vec<String> = self
                .conn
                .prepare("SELECT path FROM roots WHERE external=1 ORDER BY path")?
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            folders.extend(roots.into_iter().map(PathBuf::from));
        }
        folders.sort();
        let mut topmost: Vec<PathBuf> = Vec::new();
        for folder in folders {
            if !topmost.iter().any(|top| below(&folder, top)) {
                topmost.push(folder);
            }
        }
        Ok(topmost)
    }

    fn known_assets(&self) -> Result<Vec<Known>> {
        let mut query = self.conn.prepare(
            "SELECT a.id,a.rel_path,r.path,r.external,a.size,a.mtime,a.fingerprint,a.sha256
             FROM assets a JOIN roots r ON r.id=a.root_id ORDER BY a.rowid",
        )?;
        let rows = query.query_map([], |r| {
            Ok(Known {
                id: r.get(0)?,
                path: self.join_root(r.get(2)?, r.get(3)?, r.get(1)?),
                size: r.get::<_, i64>(4)? as u64,
                mtime: r.get(5)?,
                fingerprint: r.get(6)?,
                sha256: r.get(7)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The library folders a removal must see (see [`Anchors`]).
    fn anchors(&self) -> Result<Anchors> {
        let roots: Vec<String> = self
            .conn
            .prepare("SELECT path FROM roots WHERE external=1")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(Anchors {
            directory: self.directory.clone(),
            roots: roots.into_iter().map(PathBuf::from).collect(),
            there: HashMap::new(),
        })
    }

    /// The disconnected drive of every asset whose drive is disconnected.
    fn disconnected(&self) -> Result<HashMap<String, String>> {
        Ok(self
            .offline_rows("1=1", &[])?
            .into_iter()
            .filter_map(|asset| Some((asset.id, asset.volume?)))
            .collect())
    }

    /// Lists `folder` recursively: photograph files in name order. Hidden
    /// entries, symlinks, the library's internal folders and other
    /// libraries inside it are left out; folders that cannot be read are
    /// reported in `failed`.
    fn walk(
        &self,
        folder: &Path,
        options: &SyncOptions<'_>,
        files: &mut Vec<PathBuf>,
        failed: &mut Vec<SyncIssue>,
    ) -> Result<()> {
        let mut pending = vec![folder.to_path_buf()];
        while let Some(directory) = pending.pop() {
            cancelled(options.cancel)?;
            if directory != self.directory
                && (directory.join("session.json").is_file()
                    || directory.join("catalog.json").is_file())
            {
                continue;
            }
            let entries = match fs::read_dir(&directory) {
                Ok(entries) => entries,
                Err(e) => {
                    failed.push(SyncIssue {
                        path: directory,
                        reason: e.to_string(),
                    });
                    continue;
                }
            };
            let mut entries: Vec<_> = entries.flatten().collect();
            entries.sort_by_key(|e| e.file_name());
            let mut subfolders = Vec::new();
            for entry in entries {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with('.') {
                    continue;
                }
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                let path = entry.path();
                if kind.is_dir() {
                    if below(&path, &self.directory) && INTERNAL_FOLDERS.contains(&name.as_ref()) {
                        continue;
                    }
                    subfolders.push(path);
                } else if kind.is_file() && (options.accept)(&path) {
                    files.push(path);
                    if files.len().is_multiple_of(256) {
                        (options.progress)(SyncProgress {
                            phase: SyncPhase::Listing,
                            done: files.len(),
                            total: 0,
                        });
                    }
                }
            }
            // Depth first, in name order.
            pending.extend(subfolders.into_iter().rev());
        }
        Ok(())
    }

    /// Scans `folder` (or the whole library) and returns what changed for
    /// the user to review. Reads only. A folder that is not there fails,
    /// unless its drive is disconnected: then its photographs are reported
    /// unavailable. Photographs below a folder of the scope that is not
    /// there (a drive or share that is not connected, with or without a
    /// drive identity) are unavailable, never removed.
    pub fn scan_folder_sync(
        &self,
        folder: Option<&Path>,
        options: &SyncOptions<'_>,
    ) -> Result<SyncPlan> {
        let folders = self.sync_scope(folder)?;
        let known = self.known_assets()?;
        let disconnected = self.disconnected()?;
        if let (Some(_), [only]) = (folder, folders.as_slice())
            && !only.is_dir()
            && !known
                .iter()
                .any(|k| below(&k.path, only) && disconnected.contains_key(&k.id))
        {
            bail!(
                "{} is not there. If it was moved or renamed, use Locate Folder; if it is on a drive, connect the drive.",
                only.display()
            );
        }
        let mut files = Vec::new();
        let mut failed = Vec::new();
        let mut walked = Vec::new();
        for folder in folders.iter().filter(|f| f.is_dir()) {
            self.walk(folder, options, &mut files, &mut failed)?;
            walked.push(folder.clone());
        }
        self.classify(
            Scanned {
                known,
                disconnected,
                folders: &folders,
                walked: &walked,
                files,
                failed,
            },
            options,
        )
    }

    /// Classifies `files` alone, as a scan would: which are photographs of
    /// the library moved or renamed there, copies, or new. No folder is
    /// listed and nothing is reported removed or unavailable; only files
    /// with the size and sampled fingerprint of a photograph are hashed,
    /// and only those photographs' files are looked at. Reads only.
    pub fn scan_files_sync(
        &self,
        files: &[PathBuf],
        options: &SyncOptions<'_>,
    ) -> Result<SyncPlan> {
        self.classify(
            Scanned {
                known: self.known_assets()?,
                disconnected: self.disconnected()?,
                folders: &[],
                walked: &[],
                files: files.to_vec(),
                failed: Vec::new(),
            },
            options,
        )
    }

    /// A scan's findings for the photograph files listed.
    fn classify(&self, scanned: Scanned<'_>, options: &SyncOptions<'_>) -> Result<SyncPlan> {
        let Scanned {
            known,
            mut disconnected,
            folders,
            walked,
            files,
            failed,
        } = scanned;
        let mut plan = SyncPlan {
            folders: folders.to_vec(),
            ..SyncPlan::default()
        };
        let in_scope = |path: &Path| folders.iter().any(|f| below(path, f));
        plan.files = files.len();
        let paths: HashSet<&Path> = known.iter().map(|k| k.path.as_path()).collect();
        // Looked at when needed: only candidates of a file found and the
        // photographs in the scope have their file looked at.
        let mut states: Vec<Option<State>> = vec![None; known.len()];
        let mut by_key: HashMap<(u64, &str), Vec<usize>> = HashMap::new();
        for (index, k) in known.iter().enumerate() {
            by_key
                .entry((k.size, k.fingerprint.as_str()))
                .or_default()
                .push(index);
        }
        let unknown: Vec<PathBuf> = files
            .into_iter()
            .filter(|f| !paths.contains(f.as_path()))
            .collect();
        let total = unknown.len();
        // Content hashes of present photographs, read once.
        let mut present_hash: HashMap<usize, Option<String>> = HashMap::new();
        let mut found: Vec<(PathBuf, FileIdentity, Found)> = Vec::new();
        for (n, file) in unknown.into_iter().enumerate() {
            cancelled(options.cancel)?;
            (options.progress)(SyncProgress {
                phase: SyncPhase::Checking,
                done: n,
                total,
            });
            let identity = match FileIdentity::read(&file) {
                Ok(identity) => identity,
                Err(e) => {
                    plan.errors.push(SyncIssue {
                        path: file,
                        reason: format!("{e:#}"),
                    });
                    continue;
                }
            };
            let candidates = by_key
                .get(&(identity.size, identity.fingerprint.as_str()))
                .cloned()
                .unwrap_or_default();
            if candidates.is_empty() {
                found.push((file, identity, Found::New));
                continue;
            }
            for &i in &candidates {
                states[i].get_or_insert_with(|| state_of(&known[i], &disconnected, &failed));
            }
            let state = |i: usize| states[i].unwrap_or(State::Unavailable);
            // Renamed only in letter case: the photograph's own file. A hard
            // link under another name is the same file too, but the
            // photograph is still at its place: a copy (below), not a move.
            if let Some(&alias) = candidates.iter().find(|&&i| {
                state(i) == State::Present
                    && case_only_rename(&known[i].path, &file)
                    && same_file(&known[i].path, &file)
            }) {
                found.push((file, identity, Found::Moved(vec![(alias, true)])));
                continue;
            }
            let hash = match sha256_file(&file, options.cancel) {
                Ok(Some(hash)) => hash,
                Ok(None) => return Err(Cancelled.into()),
                Err(e) => {
                    plan.errors.push(SyncIssue {
                        path: file,
                        reason: format!("{e:#}"),
                    });
                    continue;
                }
            };
            let mut moves = Vec::new();
            let mut copies = Vec::new();
            for &i in &candidates {
                let k = &known[i];
                match state(i) {
                    // A hash on record that differs: another file.
                    State::Missing => match &k.sha256 {
                        Some(recorded) if *recorded == hash => moves.push((i, true)),
                        Some(_) => {}
                        None => moves.push((i, false)),
                    },
                    State::Present => {
                        let theirs = match present_hash.get(&i) {
                            Some(hash) => hash.clone(),
                            None => {
                                let current = stamp(&k.path) == Some((k.size, k.mtime));
                                let hash = match (&k.sha256, current) {
                                    (Some(recorded), true) => Some(recorded.clone()),
                                    _ => sha256_file(&k.path, options.cancel).ok().flatten(),
                                };
                                cancelled(options.cancel)?;
                                present_hash.insert(i, hash.clone());
                                hash
                            }
                        };
                        if theirs.as_deref() == Some(hash.as_str()) {
                            copies.push(i);
                        }
                    }
                    // Never a move (its file may well be on the disconnected
                    // drive); proven the same bytes, a copy of it.
                    State::Unavailable => {
                        if k.sha256.as_deref() == Some(hash.as_str()) {
                            copies.push(i);
                        }
                    }
                }
            }
            let what = match (moves.is_empty(), copies.is_empty()) {
                (true, true) => Found::New,
                (true, false) => Found::Copy(copies),
                (false, true) => Found::Moved(moves),
                (false, false) => Found::Contested(moves, copies),
            };
            found.push((file, identity, what));
        }
        (options.progress)(SyncProgress {
            phase: SyncPhase::Checking,
            done: total,
            total,
        });
        // A missing photograph claimed by more than one file is the user's
        // to settle.
        let mut claims: HashMap<usize, usize> = HashMap::new();
        for (_, _, what) in &found {
            if let Found::Moved(moves) | Found::Contested(moves, _) = what {
                for (i, _) in moves {
                    *claims.entry(*i).or_default() += 1;
                }
            }
        }
        let to_move = |i: usize, verified: bool, file: &Path, identity: &FileIdentity| SyncMove {
            asset_id: known[i].id.clone(),
            from: known[i].path.clone(),
            to: file.to_path_buf(),
            identity: identity.clone(),
            verified,
        };
        let paths_of = |indices: &[usize]| -> Vec<PathBuf> {
            indices.iter().map(|&i| known[i].path.clone()).collect()
        };
        for (file, identity, what) in found {
            match what {
                Found::New => plan.new_files.push(SyncNewFile {
                    path: file,
                    identity,
                    copy_of: Vec::new(),
                }),
                Found::Copy(of) => plan.copies.push(SyncNewFile {
                    copy_of: paths_of(&of),
                    path: file,
                    identity,
                }),
                Found::Moved(moves) if moves.len() == 1 && claims[&moves[0].0] == 1 => {
                    let (i, verified) = moves[0];
                    let change = to_move(i, verified, &file, &identity);
                    if verified {
                        plan.verified_moves.push(change);
                    } else {
                        plan.probable_moves.push(change);
                    }
                }
                Found::Moved(moves) => plan.contested.push(SyncContested {
                    moves: moves
                        .iter()
                        .map(|&(i, verified)| to_move(i, verified, &file, &identity))
                        .collect(),
                    copy_of: Vec::new(),
                    path: file,
                    identity,
                }),
                Found::Contested(moves, of) => plan.contested.push(SyncContested {
                    moves: moves
                        .iter()
                        .map(|&(i, verified)| to_move(i, verified, &file, &identity))
                        .collect(),
                    copy_of: paths_of(&of),
                    path: file,
                    identity,
                }),
            }
        }
        // A drive disconnected during the scan is no evidence of anything
        // missing.
        disconnected.extend(self.disconnected()?);
        for (i, k) in known.iter().enumerate() {
            if states[i] == Some(State::Missing) && disconnected.contains_key(&k.id) {
                states[i] = Some(State::Unavailable);
            }
        }
        let claimed: HashSet<usize> = claims.keys().copied().collect();
        // Looked at after listing and hashing, so a drive or share that went
        // away meanwhile is not taken for deleted photographs either.
        let walked: Vec<&PathBuf> = walked.iter().filter(|f| f.is_dir()).collect();
        let mut anchors = self.anchors()?;
        for (i, k) in known.iter().enumerate() {
            if !in_scope(&k.path) {
                continue;
            }
            let state = *states[i].get_or_insert_with(|| state_of(k, &disconnected, &failed));
            let entry = || SyncMissing {
                asset_id: k.id.clone(),
                path: k.path.clone(),
                volume: disconnected.get(&k.id).cloned(),
            };
            match state {
                State::Missing if claimed.contains(&i) => {}
                // Gone from a folder that was listed and is there: removed.
                State::Missing
                    if walked.iter().any(|f| below(&k.path, f)) && anchors.allow(&k.path) =>
                {
                    plan.removed.push(entry())
                }
                // Its folder is not there: a drive or share not connected
                // (without a drive identity the system cannot tell).
                State::Missing | State::Unavailable => plan.unavailable.push(entry()),
                State::Present => {}
            }
        }
        plan.errors.extend(failed);
        let index: HashMap<&str, usize> = known
            .iter()
            .enumerate()
            .map(|(i, k)| (k.id.as_str(), i))
            .collect();
        let still_missing = |change: &SyncMove| {
            index.get(change.asset_id.as_str()).is_some_and(|&i| {
                states[i] == Some(State::Missing) || same_file(&known[i].path, &change.to)
            })
        };
        plan.verified_moves.retain(still_missing);
        plan.probable_moves.retain(still_missing);
        for contested in &mut plan.contested {
            contested.moves.retain(still_missing);
        }
        plan.verified_moves.sort_by(|a, b| a.to.cmp(&b.to));
        plan.probable_moves.sort_by(|a, b| a.to.cmp(&b.to));
        plan.new_files.sort_by(|a, b| a.path.cmp(&b.path));
        plan.copies.sort_by(|a, b| a.path.cmp(&b.path));
        plan.contested.sort_by(|a, b| a.path.cmp(&b.path));
        plan.removed.sort_by(|a, b| a.path.cmp(&b.path));
        plan.unavailable.sort_by(|a, b| a.path.cmp(&b.path));
        plan.errors.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(plan)
    }

    /// Applies the accepted `actions` of a scan. Every action is checked
    /// against disk and library first and skipped (with why) when anything
    /// changed since the scan: the photograph was moved, edited out of the
    /// library or found again, the file changed or is gone, another
    /// photograph took its place, its drive went away. Moves and removals
    /// commit together in one transaction, new files in another, as one
    /// import batch. Nothing on disk is moved, modified or deleted.
    pub fn apply_folder_sync(&self, actions: &[SyncAction]) -> Result<SyncApplied> {
        let mut applied = SyncApplied::default();
        let mut moves: Vec<(String, PathBuf, PathBuf)> = Vec::new();
        let mut removals: Vec<(String, PathBuf)> = Vec::new();
        let mut additions: Vec<PathBuf> = Vec::new();
        let mut taken_assets: HashSet<String> = HashSet::new();
        let mut taken_files: BTreeSet<PathBuf> = BTreeSet::new();
        let mut skipped: Vec<SyncIssue> = Vec::new();
        let mut skip = |path: &Path, reason: &str| {
            skipped.push(SyncIssue {
                path: path.to_path_buf(),
                reason: reason.to_string(),
            })
        };
        let mut anchors = self.anchors()?;
        // Moves first: a file a move takes is not added as well.
        let mut ordered: Vec<&SyncAction> = actions
            .iter()
            .filter(|a| matches!(a, SyncAction::Move { .. }))
            .collect();
        ordered.extend(
            actions
                .iter()
                .filter(|a| !matches!(a, SyncAction::Move { .. })),
        );
        for action in ordered {
            match action {
                SyncAction::Move {
                    asset_id,
                    from,
                    to,
                    identity,
                } => {
                    if let Err(reason) = self.check_move(asset_id, from, to, identity) {
                        skip(to, &reason);
                    } else if taken_assets.contains(asset_id) || taken_files.contains(to) {
                        skip(to, "Another accepted change takes this photograph or file");
                    } else {
                        taken_assets.insert(asset_id.clone());
                        taken_files.insert(to.clone());
                        moves.push((asset_id.clone(), from.clone(), to.clone()));
                    }
                }
                SyncAction::Remove { asset_id, path } => {
                    if let Err(reason) = self.check_removal(asset_id, path, &mut anchors) {
                        skip(path, &reason);
                    } else if taken_assets.insert(asset_id.clone()) {
                        removals.push((asset_id.clone(), path.clone()));
                    } else {
                        skip(path, "Another accepted change takes this photograph");
                    }
                }
                SyncAction::Add { path, identity } => {
                    if taken_files.contains(path) {
                        skip(path, "Another accepted change takes this file");
                        continue;
                    }
                    match FileIdentity::read(path) {
                        Ok(now) if now == *identity => {}
                        _ => {
                            skip(path, "The file changed or is gone since the scan");
                            continue;
                        }
                    }
                    if self.asset_at(path)?.is_some() {
                        skip(path, "The file is in the library already");
                        continue;
                    }
                    taken_files.insert(path.clone());
                    additions.push(path.clone());
                }
            }
        }
        if !moves.is_empty() || !removals.is_empty() {
            let places: Vec<(String, PathBuf)> = moves
                .iter()
                .map(|(id, _, to)| (id.clone(), to.clone()))
                .collect();
            let tx =
                rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
            for (id, _) in &removals {
                tx.execute(
                    "DELETE FROM masks WHERE version_id IN (SELECT id FROM versions WHERE asset_id=?)",
                    [id],
                )?;
                tx.execute("DELETE FROM assets WHERE id=?", [id])?;
            }
            // Also drops external roots left without photographs.
            self.apply_places(&tx, &places)?;
            tx.commit()?;
            applied.moved = moves.into_iter().map(|(_, from, to)| (from, to)).collect();
            applied.removed = removals.into_iter().map(|(_, path)| path).collect();
        }
        if !additions.is_empty() {
            let _batch = self.import_batch(ImportKind::Sync);
            match self.add_files(&additions) {
                Ok(report) => {
                    applied.added = report.added.into_iter().map(|a| a.path).collect();
                    for (path, reason) in report.failed {
                        skip(&path, &reason);
                    }
                }
                // The moves and removals are committed: report them with
                // the files left out rather than fail as if nothing happened.
                Err(e) => {
                    let reason = format!("Not added: {e:#}");
                    for path in &additions {
                        skip(path, &reason);
                    }
                    applied.add_error = Some(format!("{e:#}"));
                }
            }
        }
        applied.skipped = skipped;
        Ok(applied)
    }

    fn check_move(
        &self,
        id: &str,
        from: &Path,
        to: &Path,
        identity: &FileIdentity,
    ) -> std::result::Result<(), String> {
        let asset = self
            .asset(id)
            .map_err(|_| "The photograph is no longer in the library".to_string())?;
        if asset.path != from {
            return Err("The photograph moved in the library since the scan".into());
        }
        if from.is_file() && !same_file(from, to) {
            return Err("The photograph's file is back at its place".into());
        }
        match FileIdentity::read(to) {
            Ok(now) if now == *identity => {}
            _ => return Err("The file changed or is gone since the scan".into()),
        }
        if asset.size != identity.size || asset.fingerprint != identity.fingerprint {
            return Err("The file is not this photograph".into());
        }
        match self.asset_at(to) {
            Ok(Some(other)) if other.id != asset.id => {
                return Err("The file is another photograph of the library now".into());
            }
            Ok(_) => {}
            Err(e) => return Err(e.to_string()),
        }
        match self.place_for(to) {
            Ok(Some(_)) => Ok(()),
            _ => Err("Session photographs must be inside the session folder".into()),
        }
    }

    fn check_removal(
        &self,
        id: &str,
        path: &Path,
        anchors: &mut Anchors,
    ) -> std::result::Result<(), String> {
        let asset = self
            .asset(id)
            .map_err(|_| "The photograph is no longer in the library".to_string())?;
        if asset.path != path {
            return Err("The photograph moved in the library since the scan".into());
        }
        if path.is_file() {
            return Err("The photograph's file is back".into());
        }
        match self.offline_asset(id) {
            Ok(offline) if offline.volume.is_none() => {}
            Ok(_) => return Err("The photograph's drive is not connected".into()),
            Err(e) => return Err(e.to_string()),
        }
        // Without a drive identity (Windows, network shares) a drive or
        // share that is not connected only shows as its folder being gone.
        if !anchors.allow(path) {
            return Err(
                "The photograph's folder is not there: its drive or share may not be connected"
                    .into(),
            );
        }
        Ok(())
    }
}
