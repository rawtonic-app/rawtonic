//! Journaled, undoable rename, move and trash for the Library (G40).
//!
//! Commands plan every file an operation touches (the photo, its editor
//! sidecars, virtual copies, the EXIF cache and an XMP no other photo
//! shares), carry the plan out through `rawtonic-fileops` and then bring the
//! session/catalog database and the app's albums in line. Inside a session,
//! "delete" moves files into the session's own `Trash/` folder (Capture
//! One's session Trash); elsewhere, and from that folder, they go to the
//! system Trash. Undone operations can be redone until a new one runs.
//! Empty Trash uses the same system Trash path. Only a separate explicit
//! permanent-deletion confirmation may invoke purge. A move
//! to another drive copies, verifies and then trashes the originals (ADR
//! 0016); its progress goes to the Library, which can stop it.
use crate::file_management::{
    AlbumItem, add_to_album_blocking, get_albums_blocking, sync_album_path_changes,
};
use crate::formats::is_supported_image_file;
use rawtonic_catalog::layout::SESSION_MANIFEST;
use rawtonic_catalog::{Library, session_for};
#[cfg(any(not(windows), test))]
use rawtonic_fileops::SystemTrash;
use rawtonic_fileops::{
    CopyObserver, FileOpsError, Journal, KeptCopy, Operation, State, Step, Target,
};
use rawtonic_fs::Canonical;
use rawtonic_metadata::photo_ref::{self, PhotoRef};
use rawtonic_metadata::sidecars::is_developed;
use serde::Serialize;
use std::{
    cell::Cell,
    collections::{BTreeSet, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter, Manager};

/// The session folder that receives deleted files, as in a Capture One session.
pub const SESSION_TRASH: &str = "Trash";

/// The system Trash. Records where each item went so it can be put back.
pub struct PlatformTrash;

#[cfg(target_os = "macos")]
impl SystemTrash for PlatformTrash {
    fn trash(&self, path: &Path) -> anyhow::Result<String> {
        use objc2_foundation::{NSFileManager, NSString, NSURL};
        let text = path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("Path is not valid UTF-8"))?;
        let url = NSURL::fileURLWithPath(&NSString::from_str(text));
        let mut resulting = None;
        NSFileManager::defaultManager()
            .trashItemAtURL_resultingItemURL_error(&url, Some(&mut resulting))
            .map_err(|e| anyhow::anyhow!("{}", e.localizedDescription()))?;
        let trashed = resulting
            .and_then(|url| url.path())
            .ok_or_else(|| anyhow::anyhow!("The Trash did not report where the item went"))?;
        Ok(trashed.to_string())
    }
    fn restore(&self, item: &str, original: &Path) -> anyhow::Result<()> {
        Ok(rawtonic_fs::rename_no_clobber(Path::new(item), original).map(drop)?)
    }
    fn contains(&self, item: &str) -> bool {
        fs::symlink_metadata(item).is_ok()
    }
}

#[cfg(windows)]
mod windows_trash;

#[cfg(all(unix, not(target_os = "macos")))]
impl SystemTrash for PlatformTrash {
    fn trash(&self, path: &Path) -> anyhow::Result<String> {
        use trash::os_limited;
        let before: HashSet<_> = os_limited::list()?.into_iter().map(|i| i.id).collect();
        trash::delete(path)?;
        // The new item for this path: the freedesktop/Windows Trash knows
        // each item's original location.
        let item = os_limited::list()?
            .into_iter()
            .filter(|i| !before.contains(&i.id) && i.original_path() == path)
            .max_by_key(|i| i.time_deleted)
            .ok_or_else(|| anyhow::anyhow!("The trashed item was not found in the Trash"))?;
        Ok(item.id.to_string_lossy().into_owned())
    }
    fn restore(&self, item: &str, original: &Path) -> anyhow::Result<()> {
        use trash::os_limited;
        let found = os_limited::list()?
            .into_iter()
            .find(|i| i.id.to_string_lossy() == item && i.original_path() == original)
            .ok_or_else(|| anyhow::anyhow!("{} is no longer in the Trash", original.display()))?;
        os_limited::restore_all([found])?;
        Ok(())
    }
    fn contains(&self, item: &str) -> bool {
        trash::os_limited::list()
            .map(|items| items.iter().any(|i| i.id.to_string_lossy() == item))
            .unwrap_or(false)
    }
}

static JOURNAL: Mutex<Option<Journal>> = Mutex::new(None);

/// Stop for the file operation that is running: each run gets a number
/// (sent with its progress), and Stop names the run it is for, so a Stop
/// button left from a finished operation never stops the next one.
struct StopSwitch {
    running: AtomicU64,
    next: AtomicU64,
    /// The run Stop was pressed for: one value, so no interleaving of
    /// Stop and a new run can stop that new run.
    stopped: AtomicU64,
}

impl StopSwitch {
    const fn new() -> Self {
        Self {
            running: AtomicU64::new(0),
            next: AtomicU64::new(1),
            stopped: AtomicU64::new(0),
        }
    }
    /// A run begins (holding the journal's lock); returns its number.
    fn begin(&self) -> u64 {
        let run = self.next.fetch_add(1, Ordering::SeqCst);
        self.running.store(run, Ordering::SeqCst);
        run
    }
    /// The run is over: a Stop for it does nothing any more.
    fn end(&self, run: u64) {
        let _ = self
            .running
            .compare_exchange(run, 0, Ordering::SeqCst, Ordering::SeqCst);
    }
    fn stop(&self, run: u64) {
        if run != 0 && self.running.load(Ordering::SeqCst) == run {
            self.stopped.store(run, Ordering::SeqCst);
        }
    }
    fn stopped(&self, run: u64) -> bool {
        run != 0 && self.stopped.load(Ordering::SeqCst) == run
    }
}

static STOP: StopSwitch = StopSwitch::new();

/// The event with the bytes copied so far by a move to another drive (or
/// its undo or redo).
pub const FILE_OPERATION_PROGRESS: &str = "file-operation-progress";

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CopyProgress {
    /// The run, for Stop.
    run: u64,
    label: String,
    done: u64,
    total: u64,
}

/// Sends the copy progress to the frontend (at most ten times a second)
/// and passes Stop on. File operations run one at a time (the journal's
/// lock): `start` begins this run there and the run ends when it is
/// dropped, so Stop pressed for one operation never reaches another.
struct ProgressEvents<'a> {
    app: &'a AppHandle,
    label: String,
    last: Cell<Option<Instant>>,
    run: Cell<u64>,
}

impl<'a> ProgressEvents<'a> {
    fn new(app: &'a AppHandle, label: &str) -> Self {
        Self {
            app,
            label: label.to_string(),
            last: Cell::new(None),
            run: Cell::new(0),
        }
    }
}

impl ProgressEvents<'_> {
    fn start(&self) {
        self.run.set(STOP.begin());
    }
}

impl Drop for ProgressEvents<'_> {
    fn drop(&mut self) {
        STOP.end(self.run.get());
    }
}

impl CopyObserver for ProgressEvents<'_> {
    fn copied(&self, done: u64, total: u64) {
        let now = Instant::now();
        if done < total
            && self
                .last
                .get()
                .is_some_and(|last| now - last < Duration::from_millis(100))
        {
            return;
        }
        self.last.set(Some(now));
        let _ = self.app.emit(
            FILE_OPERATION_PROGRESS,
            CopyProgress {
                run: self.run.get(),
                label: self.label.clone(),
                done,
                total,
            },
        );
    }
    fn cancelled(&self) -> bool {
        STOP.stopped(self.run.get())
    }
}

/// Stops the copying of the move to another drive (or its undo or redo)
/// numbered `run` in its progress: what was copied is removed and nothing
/// is moved. Does nothing once that run is over.
#[tauri::command]
pub fn stop_file_operation(run: u64) {
    STOP.stop(run);
}

/// `with_journal` for the session folders' glue (`session_folders`).
pub(crate) fn with_journal_pub<T>(
    app: &AppHandle,
    f: impl FnOnce(&Journal) -> Result<T, FileOpsError>,
) -> Result<T, String> {
    with_journal(app, f)
}

fn with_journal<T>(
    app: &AppHandle,
    f: impl FnOnce(&Journal) -> Result<T, FileOpsError>,
) -> Result<T, String> {
    let mut guard = JOURNAL.lock().map_err(|e| e.to_string())?;
    if guard.is_none() {
        let path = app
            .path()
            .app_data_dir()
            .map_err(|e| e.to_string())?
            .join("file-operations.sqlite");
        *guard = Some(Journal::open(&path).map_err(|e| format!("{e:#}"))?);
    }
    f(guard.as_ref().expect("journal opened")).map_err(|e| format!("{e:#}"))
}

/// What the frontend shows about an operation.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationSummary {
    pub id: String,
    pub kind: String,
    pub label: String,
    /// Photos and folders the operation moved (sidecars not counted).
    pub items: usize,
    /// How many went to the session Trash, and how many to the system Trash.
    pub session_trash: usize,
    pub system_trash: usize,
    /// New paths of renamed or moved photos (where they are now: after an
    /// undo, their old places).
    pub new_paths: Vec<String>,
    /// Photos and folders now at another place, in the direction just
    /// applied (an undo reverses the operation's steps), so the frontend's
    /// path-keyed state (selection, open photo, picks, thumbnails) follows
    /// them. Sidecars and virtual copies are not listed: copies follow
    /// their photo.
    pub moves: Vec<PathMove>,
    /// What the user should know, e.g. originals a move to another drive
    /// kept in place.
    pub note: Option<String>,
}

/// A photo or folder that moved from `from` to `to`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PathMove {
    pub from: String,
    pub to: String,
}

impl From<&Operation> for OperationSummary {
    fn from(op: &Operation) -> Self {
        let is_item = |p: &Path| is_supported_image_file(p) || album_key(p).is_some() || p.is_dir();
        let mut summary = Self {
            id: op.id.clone(),
            kind: op.kind.clone(),
            label: op.label.clone(),
            items: 0,
            session_trash: 0,
            system_trash: 0,
            new_paths: Vec::new(),
            moves: Vec::new(),
            note: op.note.clone(),
        };
        // Done: from → to; undone: back from to → from; otherwise (rolled
        // back, interrupted) nothing is anywhere new.
        let applied = |from: &Path, to: &Path| -> Option<(PathBuf, PathBuf)> {
            match op.state {
                State::Done => Some((from.to_path_buf(), to.to_path_buf())),
                State::Undone => Some((to.to_path_buf(), from.to_path_buf())),
                _ => None,
            }
        };
        for step in &op.steps {
            // A folder moved to another drive is no longer at `from` (its
            // original went to the Trash).
            let counted = is_item(&step.from)
                || matches!(&step.to, Target::Path { path } | Target::OtherVolume { path, .. } if path.is_dir());
            if !counted {
                continue;
            }
            summary.items += 1;
            match &step.to {
                Target::SystemTrash { .. } => summary.system_trash += 1,
                Target::Path { path } | Target::OtherVolume { path, .. } => {
                    if op.kind == "trash" {
                        summary.session_trash += 1;
                    }
                    let Some((from, to)) = applied(&step.from, path) else {
                        continue;
                    };
                    let photo = is_supported_image_file(&to);
                    if !(photo || to.is_dir()) {
                        continue;
                    }
                    if photo && op.kind != "trash" {
                        summary.new_paths.push(to.to_string_lossy().into_owned());
                    }
                    summary.moves.push(PathMove {
                        from: from.to_string_lossy().into_owned(),
                        to: to.to_string_lossy().into_owned(),
                    });
                }
            }
        }
        summary
    }
}

/// `<image>.<copy id>.rrdata` → `<image>?vc=<copy id>` (how albums and the
/// frontend name a virtual copy); `None` for anything else.
fn album_key(path: &Path) -> Option<String> {
    let name = path
        .file_name()?
        .to_str()?
        .strip_suffix(photo_ref::SIDECAR_SUFFIX)?;
    let (image, id) = name.rsplit_once('.')?;
    if id.is_empty() || !is_supported_image_file(image) {
        return None;
    }
    Some(PhotoRef::of_path(&path.with_file_name(image), Some(id)).format())
}

/// The XMP sidecar `image` has (`rawtonic_metadata::sidecars`): a RAW's
/// is `<stem>.xmp`, a JPEG beside its RAW has `<name>.JPG.xmp`.
fn xmp_for(image: &Path) -> Option<PathBuf> {
    rawtonic_metadata::existing_sidecar(image)
}

/// The XMP sidecars of `images` that take another name in `dest`: a lone
/// JPEG's `A.xmp` moved beside a file of its stem (its RAW, there already or
/// moved with it) becomes `A.JPG.xmp`, so the RAW never takes it for its own
/// there (`rawtonic_metadata::sidecars`).
pub fn xmp_targets(images: &[PathBuf], dest: &Path) -> HashMap<PathBuf, PathBuf> {
    let there = rawtonic_metadata::sidecars::Sidecars::new();
    let mut targets = HashMap::new();
    for image in images {
        let (Some(name), Some(stem)) = (image.file_name(), image.file_stem()) else {
            continue;
        };
        if !is_developed(image) {
            continue;
        }
        let Some(xmp) = rawtonic_metadata::existing_sidecar(image) else {
            continue;
        };
        let stem_form = xmp
            .file_stem()
            .is_some_and(|s| s.eq_ignore_ascii_case(stem));
        let partner_moves = images.iter().any(|other| {
            other != image
                && other
                    .file_stem()
                    .is_some_and(|s| s.eq_ignore_ascii_case(stem))
        });
        if stem_form && (partner_moves || there.has_partner(&dest.join(name))) {
            targets.insert(xmp, full_name_sidecar(&dest.join(name)));
        }
    }
    targets
}

/// Renames a move into `dest` needs first: a JPEG there alone whose
/// `A.xmp` would become the RAW's (or be no one's) once another photo of its
/// name is moved in beside it takes `A.JPG.xmp`
/// (`xmp_targets` for the JPEG that stays).
pub fn partner_xmp_renames(images: &[PathBuf], dest: &Path) -> Vec<(PathBuf, PathBuf)> {
    let mut renames = Vec::new();
    let Ok(entries) = fs::read_dir(dest) else {
        return renames;
    };
    let there: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    // Any photo of its name arriving (a RAW, or a rendered TIFF beside a
    // JPEG) makes the JPEG's sidecar the full-name one.
    for image in images {
        let Some(stem) = image.file_stem() else {
            continue;
        };
        for jpeg in there.iter().filter(|p| {
            is_developed(p)
                && p.is_file()
                && p.file_stem().is_some_and(|s| s.eq_ignore_ascii_case(stem))
        }) {
            // Its sidecar now, while it is alone there.
            if let Some(xmp) = rawtonic_metadata::existing_sidecar(jpeg)
                && xmp
                    .file_stem()
                    .is_some_and(|s| s.eq_ignore_ascii_case(stem))
                && !renames.iter().any(|(from, _)| from == &xmp)
            {
                renames.push((xmp, full_name_sidecar(jpeg)));
            }
        }
    }
    renames
}

/// `A.JPG.xmp` for `A.JPG`.
fn full_name_sidecar(image: &Path) -> PathBuf {
    let mut name = image.file_name().unwrap_or_default().to_os_string();
    name.push(".xmp");
    image.with_file_name(name)
}

/// Every file that belongs to `images` and moves with them: editor sidecars,
/// virtual copies, the EXIF cache, a RAW's companion JPEG
/// (`session_bridge::companion_of`), and its XMP sidecar, unless another
/// photo left behind has a sidecar of that name too (a CR3 and a DNG of one
/// stem): then it stays with the remaining photo.
pub fn associated_files(images: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let selected: HashSet<&PathBuf> = images.iter().collect();
    let sidecars = rawtonic_metadata::sidecars::Sidecars::new();
    let mut files = BTreeSet::new();
    for image in images {
        if let Some(packet) = sidecars.ambiguous_packet(image) {
            return Err(format!(
                "File operation blocked by ambiguous XMP ownership at {}",
                packet.display()
            ));
        }
        for file in crate::file_management::find_all_associated_files(image)? {
            files.insert(file);
        }
        let companion = crate::session_bridge::companion_of(image);
        if let Some(companion) = &companion {
            files.insert(companion.clone());
        }
        let Some(xmp) = sidecars.existing(image) else {
            continue;
        };
        // Listed already (a JPEG's own `A.JPG.xmp`), maybe under its stored
        // spelling (`A.JPG.XMP` on a case-insensitive volume, where the
        // name asked for is not in the folder's listing). On a
        // case-sensitive volume `a.xmp` beside `A.xmp` is a file of its own.
        let parent = image.parent().ok_or("No parent folder")?;
        let spelled_so = || {
            fs::read_dir(parent)
                .into_iter()
                .flatten()
                .flatten()
                .any(|e| Some(e.file_name().as_os_str()) == xmp.file_name())
        };
        let listed = files.contains(&xmp)
            || (files.iter().any(|f| {
                f.parent() == xmp.parent()
                    && f.file_name().zip(xmp.file_name()).is_some_and(|(a, b)| {
                        a.to_string_lossy()
                            .eq_ignore_ascii_case(&b.to_string_lossy())
                    })
            }) && !spelled_so());
        if !xmp.is_file() || listed {
            continue;
        }
        // A JPEG partner's sidecar is its own (`A.JPG.xmp`), not this one;
        // a companion is no photo of its own.
        let stem = image.file_stem();
        let shared = fs::read_dir(image.parent().ok_or("No parent folder")?)
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .any(|p| {
                p.file_stem() == stem
                    && is_supported_image_file(&p)
                    && !selected.contains(&p)
                    && companion.as_ref() != Some(&p)
                    && xmp_for(&p).as_ref() == Some(&xmp)
            });
        if !shared {
            files.insert(xmp);
        }
    }
    Ok(files.into_iter().collect())
}

/// Plans a move to the Trash: session files into the session's `Trash/`
/// (keeping their folder below the session), everything else and files
/// already in the session Trash to the system Trash. `groups` are the files
/// of one photo each; a photo whose name is taken in the session Trash goes
/// into a numbered subfolder there, with all its files.
pub fn plan_trash(groups: &[Vec<PathBuf>]) -> Vec<Step> {
    let mut steps = Vec::new();
    let mut taken = HashSet::new();
    for group in groups {
        let targets: Vec<Option<PathBuf>> = group.iter().map(|f| session_trash_target(f)).collect();
        let free = targets
            .iter()
            .flatten()
            .all(|t| !t.exists() && !taken.contains(t));
        let mut n = 1;
        let targets: Vec<Option<PathBuf>> = if free {
            targets
        } else {
            loop {
                let candidate: Vec<Option<PathBuf>> = targets
                    .iter()
                    .map(|t| {
                        t.as_ref().map(|t| {
                            t.parent()
                                .unwrap_or(Path::new(""))
                                .join(n.to_string())
                                .join(t.file_name().unwrap_or_default())
                        })
                    })
                    .collect();
                if candidate
                    .iter()
                    .flatten()
                    .all(|t| !t.exists() && !taken.contains(t))
                {
                    break candidate;
                }
                n += 1;
            }
        };
        for (file, target) in group.iter().zip(targets) {
            match target {
                Some(target) => {
                    taken.insert(target.clone());
                    steps.push(Step::rename(file, target));
                }
                None => steps.push(Step::system_trash(file)),
            }
        }
    }
    steps
}

/// `session/Trash/<path below the session>`, or `None` outside a session or
/// for something already in the session Trash.
fn session_trash_target(path: &Path) -> Option<PathBuf> {
    let session = session_for(path).or_else(|| {
        // A folder directly in the session (session_for looks above the parent).
        path.parent()
            .filter(|p| p.join(SESSION_MANIFEST).is_file())
            .map(Path::to_path_buf)
    })?;
    let relative = path.strip_prefix(&session).ok()?;
    if relative.starts_with(SESSION_TRASH) || relative.as_os_str().is_empty() {
        return None;
    }
    Some(session.join(SESSION_TRASH).join(relative))
}

/// Runs a planned operation and syncs the database and albums.
pub fn run(
    app: &AppHandle,
    kind: &str,
    label: &str,
    steps: Vec<Step>,
) -> Result<Operation, String> {
    run_with_context(app, kind, label, steps, None)
}

/// `run`, with `context` kept with the operation under
/// `session_folders::CONTEXT_KEY` (the folder role a Move contents part went
/// with), for its undo and redo to follow (`sync`).
pub fn run_with_context(
    app: &AppHandle,
    kind: &str,
    label: &str,
    steps: Vec<Step>,
    context: Option<serde_json::Value>,
) -> Result<Operation, String> {
    // Album memberships of what goes to the system Trash, for undo.
    let trashed: Vec<String> = steps
        .iter()
        .filter(|s| matches!(s.to, Target::SystemTrash { .. }))
        .filter_map(|s| item_key(&s.from))
        .collect();
    let memberships = album_memberships(app, &trashed);
    let progress = ProgressEvents::new(app, label);
    let op = with_journal(app, |journal| {
        progress.start();
        let newest = journal.history(1)?.pop().map(|op| op.id);
        let result = journal.execute_with(kind, label, steps, &PlatformTrash, &progress);
        // A failed run is the newest operation, if it was journaled.
        let id = match &result {
            Ok(op) => Some(op.id.clone()),
            Err(_) => journal
                .history(1)
                .ok()
                .and_then(|mut ops| ops.pop())
                .map(|op| op.id)
                .filter(|id| Some(id) != newest.as_ref()),
        };
        if let Some(id) = id {
            notify_kept(app, journal, &id, &[]);
        }
        let op = result?;
        let mut stored = serde_json::Map::new();
        if !memberships.is_empty() {
            stored.insert("albums".into(), serde_json::json!(memberships));
        }
        if let Some(context) = &context {
            stored.insert(crate::session_folders::CONTEXT_KEY.into(), context.clone());
        }
        if !stored.is_empty() {
            journal.set_context(&op.id, serde_json::Value::Object(stored))?;
        }
        journal.operation(&op.id)
    })?;
    sync_pending(app, Some(&op.id));
    Ok(op)
}

/// Undoes the newest finished operation; `None` when there is none.
pub fn undo_last(app: &AppHandle) -> Result<Option<Operation>, String> {
    let Some(last) = with_journal(app, |journal| journal.last_undoable())? else {
        return Ok(None);
    };
    let _resume = crate::session_move::before_reversal(
        &app.state::<crate::state::export::ExportState>(),
        &last,
        false,
    )?;
    let progress = ProgressEvents::new(app, &last.label);
    let op = with_journal(app, |journal| {
        progress.start();
        let before = journal.kept_copies(&last.id)?;
        let result = journal.undo_with(&last.id, &PlatformTrash, &progress);
        notify_kept(app, journal, &last.id, &before);
        result
    })?;
    sync_pending(app, Some(&op.id));
    crate::session_move::followed(app, &op);
    Ok(Some(op))
}

/// Redoes the file operation undone last; `None` when there is none. The
/// plan is validated again first: a redo refuses, changing nothing, when a
/// target was taken or a source is gone since the undo.
pub fn redo_last(app: &AppHandle) -> Result<Option<Operation>, String> {
    let Some(next) = with_journal(app, |journal| journal.last_redoable())? else {
        return Ok(None);
    };
    let _resume = crate::session_move::before_reversal(
        &app.state::<crate::state::export::ExportState>(),
        &next,
        true,
    )?;
    let progress = ProgressEvents::new(app, &next.label);
    let op = with_journal(app, |journal| {
        progress.start();
        let before = journal.kept_copies(&next.id)?;
        let result = journal.redo_with(&next.id, &PlatformTrash, &progress);
        notify_kept(app, journal, &next.id, &before);
        result
    })?;
    sync_pending(app, Some(&op.id));
    crate::session_move::followed(app, &op);
    Ok(Some(op))
}

/// The event naming the copies a run left on the other drive, in place
/// (ADR 0016): the Library offers to show each one and to move it to the
/// Trash ([`trash_kept_copy`]).
pub const FILE_OPERATION_KEPT_COPIES: &str = "file-operation-kept-copies";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct KeptCopies {
    operation_id: String,
    copies: Vec<KeptCopyView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct KeptCopyView {
    path: String,
    source: String,
    /// Verified complete (otherwise perhaps only its start).
    complete: bool,
}

/// The copies of `after` that were not left before the run (`before`).
fn newly_kept(id: &str, before: &[KeptCopy], after: Vec<KeptCopy>) -> Option<KeptCopies> {
    let copies: Vec<_> = after
        .into_iter()
        .filter(|copy| !before.contains(copy))
        .map(|copy| KeptCopyView {
            path: copy.path.to_string_lossy().into_owned(),
            source: copy.source.to_string_lossy().into_owned(),
            complete: copy.complete,
        })
        .collect();
    (!copies.is_empty()).then(|| KeptCopies {
        operation_id: id.to_string(),
        copies,
    })
}

/// Tells the Library which copies the run of operation `id` left.
fn notify_kept(app: &AppHandle, journal: &Journal, id: &str, before: &[KeptCopy]) {
    match journal.kept_copies(id) {
        Ok(after) => {
            if let Some(kept) = newly_kept(id, before, after) {
                let _ = app.emit(FILE_OPERATION_KEPT_COPIES, kept);
            }
        }
        Err(e) => log::warn!("File operation journal could not list kept copies: {e:#}"),
    }
}

/// Moves a copy a file operation left on another drive to the system Trash
/// (the Library's offer after [`FILE_OPERATION_KEPT_COPIES`]); not undoable
/// (see `Journal::trash_kept_copy`); it is taken back by dragging it out
/// of the Trash.
/// Refused, changing nothing, for any file the journal does not record as
/// left by that operation.
#[tauri::command]
pub async fn trash_kept_copy(
    app: AppHandle,
    operation_id: String,
    path: String,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        with_journal(&app, |journal| {
            journal.trash_kept_copy(&operation_id, Path::new(&path), &PlatformTrash)
        })
        .map(drop)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// What the frontend shows when the database could not follow an operation.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CatalogSyncFailure {
    label: String,
    error: String,
}

/// Syncs every operation the records do not follow yet, oldest first: the
/// one just run (`current`) and any whose sync failed before (a locked
/// database), so the catalog catches up in order. A failure leaves the
/// operation pending for the next operation or start-up; the user hears
/// about it for `current` only, once. Call with nothing that waits for the
/// main thread held: the album changes take the album list's write lock.
fn sync_pending(app: &AppHandle, current: Option<&str>) {
    let pending = match with_journal(app, |journal| journal.unsynced()) {
        Ok(pending) => pending,
        Err(e) => {
            log::error!("File operation journal could not list pending syncs: {e}");
            return;
        }
    };
    let mut caught_up = Vec::new();
    for (op, result) in sync_in_order(&pending, |op, follows| sync(app, op, follows)) {
        match result {
            Ok(()) if current != Some(op.id.as_str()) => caught_up.push(op.label.clone()),
            Ok(()) => {}
            Err(error) => {
                log::warn!("Catalog could not follow '{}': {error}", op.label);
                if current == Some(op.id.as_str()) {
                    let _ = app.emit(
                        "file-operation-sync-failed",
                        CatalogSyncFailure {
                            label: op.label.clone(),
                            error,
                        },
                    );
                }
            }
        }
    }
    notify_caught_up(app, caught_up);
}

/// The event telling the frontend that the catalog caught up on earlier
/// operations whose sync had failed: what it counts (offline photographs,
/// Missing Photographs, folders) and lists is read again.
pub const FILE_OPERATIONS_CAUGHT_UP: &str = "file-operations-caught-up";

fn notify_caught_up(app: &AppHandle, labels: Vec<String>) {
    if labels.is_empty() {
        return;
    }
    log::info!("Catalog caught up on {labels:?}");
    let _ = app.emit(FILE_OPERATIONS_CAUGHT_UP, labels);
}

/// Why an operation's catalog sync waits (`sync_in_order`).
const WAITS_FOR_EARLIER: &str =
    "an earlier change to these files is not in the catalog yet; both follow together";

/// Every path an operation touched, before and after.
fn touched(op: &Operation) -> Vec<PathBuf> {
    op.steps
        .iter()
        .flat_map(|step| {
            let to = step.target().map(Path::to_path_buf);
            std::iter::once(step.from.clone()).chain(to)
        })
        .collect()
}

/// Runs `sync(op, follows)` over `pending` (oldest first) and returns each
/// result. An operation that touches a path (or a folder above or below
/// it) that an earlier operation with a failed sync touched gets `follows`
/// false: its catalog changes wait, since replaying the earlier one after
/// it would put the records where the later one already moved the files
/// away from (rename a→b fails to sync, b→c syncs and finds no record at b,
/// the retry of a→b then leaves the record at b). Unrelated operations
/// still follow at once.
fn sync_in_order(
    pending: &[Operation],
    mut sync: impl FnMut(&Operation, bool) -> Result<(), String>,
) -> Vec<(&Operation, Result<(), String>)> {
    let mut blocked: Vec<PathBuf> = Vec::new();
    pending
        .iter()
        .map(|op| {
            let paths = touched(op);
            let waits = paths
                .iter()
                .any(|p| blocked.iter().any(|b| p.starts_with(b) || b.starts_with(p)));
            let result = sync(op, !waits);
            if result.is_err() {
                blocked.extend(paths);
            }
            (op, result)
        })
        .collect()
}

/// Undoes the newest file operation (Edit ▸ Undo in the Library).
#[tauri::command]
pub async fn undo_file_operation(app: AppHandle) -> Result<Option<OperationSummary>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        undo_last(&app).map(|op| op.as_ref().map(OperationSummary::from))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Redoes the file operation undone last (Edit ▸ Redo in the Library).
#[tauri::command]
pub async fn redo_file_operation(app: AppHandle) -> Result<Option<OperationSummary>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        redo_last(&app).map(|op| op.as_ref().map(OperationSummary::from))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Rolls back operations a crash interrupted and finishes syncs a crash cut
/// short. Called once at start-up.
pub fn recover(app: &AppHandle) {
    match with_journal(app, |journal| journal.recover(&PlatformTrash)) {
        Ok(pending) => {
            for op in &pending {
                log::warn!(
                    "File operation journal: bringing records in line with '{}' ({:?}){}",
                    op.label,
                    op.state,
                    op.note
                        .as_deref()
                        .map(|n| format!(": {n}"))
                        .unwrap_or_default()
                );
            }
            let mut caught_up = Vec::new();
            for (op, result) in sync_in_order(&pending, |op, follows| sync(app, op, follows)) {
                match result {
                    Ok(()) => caught_up.push(op.label.clone()),
                    Err(e) => log::warn!("Catalog could not follow '{}' yet: {e}", op.label),
                }
            }
            notify_caught_up(app, caught_up);
        }
        Err(e) => log::error!("File operation journal recovery failed: {e}"),
    }
}

/// The album/frontend name of a photo or virtual copy file; `None` for sidecars.
fn item_key(path: &Path) -> Option<String> {
    if is_supported_image_file(path) {
        Some(path.to_string_lossy().into_owned())
    } else {
        album_key(path)
    }
}

fn album_memberships(app: &AppHandle, items: &[String]) -> HashMap<String, Vec<String>> {
    let mut found = HashMap::new();
    if items.is_empty() {
        return found;
    }
    let wanted: HashSet<&str> = items.iter().map(String::as_str).collect();
    fn walk(nodes: &[AlbumItem], wanted: &HashSet<&str>, found: &mut HashMap<String, Vec<String>>) {
        for node in nodes {
            match node {
                AlbumItem::Album { id, images, .. } => {
                    let hits: Vec<String> = images
                        .iter()
                        .filter(|i| {
                            wanted.contains(i.as_str())
                                || photo_ref::is_copy(i) && wanted.contains(photo_ref::source_of(i))
                        })
                        .cloned()
                        .collect();
                    if !hits.is_empty() {
                        found.insert(id.clone(), hits);
                    }
                }
                AlbumItem::Group { children, .. } => walk(children, wanted, found),
            }
        }
    }
    if let Ok(tree) = get_albums_blocking(app.clone()) {
        walk(&tree, &wanted, &mut found);
    }
    found
}

/// Brings the session/catalog database and the albums in line with the
/// operation's state, then records that in the journal. When the database
/// could not follow, or must not yet (`follows` false, see
/// `sync_in_order`), the operation stays unsynced (retried later; every
/// step is idempotent) and the error is returned. Albums follow at once in
/// either case, in the order of the operations. It takes the album list's
/// write lock (`state::library_files`), which every caller must not hold on
/// the main thread: they all run on blocking workers, as do the albums'
/// commands and the presets' imports.
fn sync(app: &AppHandle, op: &Operation, follows: bool) -> Result<(), String> {
    if matches!(op.state, State::PurgeCommitted | State::Purged) {
        // Emptied for good (or committed to be): records, previews and
        // album entries of everything that was in the Trash go.
        let catalog = if follows {
            forget_purged(op)
        } else {
            Err(WAITS_FOR_EARLIER.to_string())
        };
        let deletions: HashSet<String> = op
            .purged()
            .iter()
            .chain(&op.removed)
            .filter_map(|p| item_key(p).or_else(|| Some(p.to_string_lossy().into_owned())))
            .collect();
        if !deletions.is_empty() {
            sync_album_path_changes(app, None, Some(&deletions), None);
        }
        catalog?;
        if let Err(e) = with_journal(app, |journal| journal.mark_synced(&op.id)) {
            log::warn!("File operation '{}' synced but not recorded: {e}", op.label);
        }
        return Ok(());
    }
    let forward = op.state == State::Done;
    if !matches!(op.state, State::Done | State::Undone) {
        return Ok(());
    }
    let catalog = if follows {
        sync_catalog(op, forward)
    } else {
        Err(WAITS_FOR_EARLIER.to_string())
    };

    let mut renames = HashMap::new();
    let mut folder_renames = Vec::new();
    for (a, b, folder) in moved(op, forward) {
        if folder {
            folder_renames.push((
                a.to_string_lossy().into_owned(),
                b.to_string_lossy().into_owned(),
            ));
        } else if let (Some(a), Some(b)) = (item_key(&a), item_key(&b)) {
            renames.insert(a, b);
        }
    }
    if !renames.is_empty() {
        sync_album_path_changes(app, Some(&renames), None, None);
    }
    for (a, b) in &folder_renames {
        sync_album_path_changes(app, None, None, Some((a, b)));
    }
    let trashed: HashSet<String> = op
        .trashed()
        .iter()
        .filter_map(|p| item_key(p).or_else(|| Some(p.to_string_lossy().into_owned())))
        .collect();
    if !trashed.is_empty() {
        if forward {
            sync_album_path_changes(app, None, Some(&trashed), None);
        } else if let Some(albums) = op
            .context
            .as_ref()
            .and_then(|c| c.get("albums"))
            .and_then(|a| serde_json::from_value::<HashMap<String, Vec<String>>>(a.clone()).ok())
        {
            for (album, images) in albums {
                let _ = add_to_album_blocking(&album, &images, app);
            }
        }
    }
    // The folder role a Move contents part went with follows its undo and
    // redo (C1Q1, G38), and the Library shows the roles as they now are; a
    // role that cannot follow is said, the records' sync goes on.
    use crate::session_folders::Followed;
    match with_journal(app, |journal| {
        Ok(crate::session_folders::follow(journal, op, forward))
    }) {
        Ok(Followed::Changed { session }) => {
            let _ = app.emit(
                SESSION_FOLDERS_CHANGED,
                SessionFoldersChanged {
                    session,
                    refused: None,
                },
            );
        }
        Ok(Followed::Refused { session, error }) => {
            log::warn!("'{}': the folder role stays: {error}", op.label);
            let _ = app.emit(
                SESSION_FOLDERS_CHANGED,
                SessionFoldersChanged {
                    session,
                    refused: Some(error),
                },
            );
        }
        Ok(Followed::Unchanged) => {}
        Err(e) => log::warn!("'{}': the folder role could not follow: {e}", op.label),
    }
    catalog?;
    if let Err(e) = with_journal(app, |journal| journal.mark_synced(&op.id)) {
        log::warn!("File operation '{}' synced but not recorded: {e}", op.label);
    }
    Ok(())
}

/// The event telling the Library that a session's folder roles changed with
/// a file operation's undo or redo, or could not (`refused`: why).
pub const SESSION_FOLDERS_CHANGED: &str = "session-folders-changed";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionFoldersChanged {
    session: PathBuf,
    refused: Option<String>,
}

/// The (old, new) places of what the operation moved, in the direction its
/// records follow (`forward`: done; else undone), and whether each is a
/// folder: a folder moved to another drive by the journal, since that drive
/// may be offline when the records follow (after a crash, at the next
/// start); anything else by what is on disk.
fn moved(op: &Operation, forward: bool) -> Vec<(PathBuf, PathBuf, bool)> {
    op.steps
        .iter()
        .filter_map(|step| {
            let to = step.target()?.to_path_buf();
            let journaled = matches!(&step.to, Target::OtherVolume { copy, .. } if copy.folder);
            let (a, b) = if forward {
                (step.from.clone(), to)
            } else {
                (to, step.from.clone())
            };
            let folder = journaled || b.is_dir();
            Some((a, b, folder))
        })
        .collect()
}

/// Session/catalog records follow renamed and moved photos and folders, and
/// mark photos sent to the system Trash as missing (present again on undo).
/// Every change is tried; the failures are returned together. Running it
/// again is harmless: a relocation finds nothing left at the old place.
pub fn sync_catalog(op: &Operation, forward: bool) -> Result<(), String> {
    enum Change {
        Relocate(PathBuf, PathBuf),
        Missing(PathBuf, bool),
    }
    let mut changes: Vec<(PathBuf, Change)> = Vec::new();
    for (a, b, folder) in moved(op, forward) {
        // A whole session moved keeps its own records (its paths are
        // relative); an open catalog holding photos in it follows it.
        if op.kind == crate::session_move::KIND {
            if let Some(catalog) = crate::session_move::open_catalog_other_than(&a, &b) {
                changes.push((catalog, Change::Relocate(a, b)));
            }
            continue;
        }
        if !(folder || is_supported_image_file(&b)) {
            continue;
        }
        let library = crate::session_bridge::library_dir_for(&a)
            .or_else(|| crate::session_bridge::library_dir_for(&b));
        if let Some(dir) = library {
            changes.push((dir, Change::Relocate(a, b)));
        }
    }
    for path in op.trashed() {
        // Photos and folders; sidecars have no record of their own.
        let sidecar = path
            .extension()
            .is_some_and(|e| ["rrdata", "rrexif", "xmp"].contains(&e.to_string_lossy().as_ref()));
        if let (false, Some(dir)) = (sidecar, crate::session_bridge::library_dir_for(&path)) {
            changes.push((dir, Change::Missing(path, forward)));
        }
    }
    let mut libraries: HashMap<PathBuf, Result<Library, String>> = HashMap::new();
    let mut failures: Vec<String> = Vec::new();
    for (dir, change) in changes {
        let lib = match libraries
            .entry(dir.clone())
            .or_insert_with(|| Library::open(&dir).map_err(|e| format!("{e:#}")))
        {
            Ok(lib) => lib,
            Err(e) => {
                failures.push(e.clone());
                continue;
            }
        };
        let result = match &change {
            Change::Relocate(a, b) => lib.relocate(a, b).map(|_| ()),
            Change::Missing(path, missing) => lib.set_trashed_at(path, *missing).map(|_| ()),
        };
        if let Err(e) = result {
            failures.push(format!("{e:#}"));
        }
    }
    drop(libraries);
    carry_previews(op, forward);
    // Virtual copies the library keeps follow their sidecars: a deleted
    // copy is forgotten, one put back is kept again.
    let mut sidecars: Vec<PathBuf> = op
        .renames()
        .into_iter()
        .flat_map(|(from, to)| [from, to])
        .chain(op.trashed())
        .filter(|path| crate::session_bridge::copy_for_sidecar(path).is_some())
        .collect();
    sidecars.sort();
    sidecars.dedup();
    for sidecar in sidecars {
        crate::session_bridge::reconcile_copy(&sidecar);
    }
    if failures.is_empty() {
        return Ok(());
    }
    failures.dedup();
    Err(failures.join("; "))
}

/// Previews (Standard and 1:1) follow photographs and folders renamed or
/// moved by the app, as they follow a relink: only preview files move,
/// never replacing one.
fn carry_previews(op: &Operation, forward: bool) {
    let mut moves: Vec<(PathBuf, PathBuf)> = Vec::new();
    for (from, to) in op.renames() {
        let (a, b) = if forward { (from, to) } else { (to, from) };
        if b.is_dir() {
            for entry in walkdir::WalkDir::new(&b)
                .follow_links(false)
                .into_iter()
                .flatten()
                .filter(|e| e.file_type().is_file() && is_supported_image_file(e.path()))
            {
                if let Ok(below) = entry.path().strip_prefix(&b) {
                    moves.push((a.join(below), entry.path().to_path_buf()));
                }
            }
        } else if is_supported_image_file(&b) {
            moves.push((a.to_path_buf(), b.to_path_buf()));
        }
    }
    // One listing of each previews folder for the whole move.
    let carried =
        crate::preview_proxy::carry_over_all(moves.iter().map(|(a, b)| (a.as_path(), b.as_path())));
    if carried > 0 {
        log::info!("{carried} previews followed '{}'", op.label);
    }
}

/// The session catalog forgets what an emptied session Trash held (every
/// photograph at or below each of its entries), and their previews go.
/// Running it again finds nothing.
pub(crate) fn forget_purged(op: &Operation) -> Result<(), String> {
    let entries = op.purged();
    let Some(trash) = entries.first().and_then(|e| e.parent()) else {
        return Ok(());
    };
    let session = trash
        .parent()
        .filter(|s| {
            trash.file_name() == Some(SESSION_TRASH.as_ref())
                && rawtonic_catalog::layout::LibraryLayout::at(s).is_some()
        })
        .ok_or_else(|| format!("{} is not a library's Trash", trash.display()))?;
    let lib = Library::open(session).map_err(|e| format!("{e:#}"))?;
    let mut failures = Vec::new();
    for entry in &entries {
        if let Err(e) = lib.forget_at(entry) {
            failures.push(format!("{e:#}"));
        }
    }
    let kinds = [
        crate::preview_proxy::ProxyKind::Standard,
        crate::preview_proxy::ProxyKind::Full,
    ];
    crate::preview_proxy::discard_sources(
        op.removed
            .iter()
            .filter(|f| is_supported_image_file(f))
            .map(PathBuf::as_path),
        &kinds,
    );
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

/// What the library's Trash folder holds, for the Empty Trash dialog.
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrashContents {
    /// Photographs and virtual copies.
    pub photos: usize,
    /// Every file (sidecars, XMP and anything else included).
    pub files: usize,
    /// Top-level entries (files and folders).
    pub entries: usize,
    /// Permanent purge only: committed entries awaiting cleanup.
    pub note: Option<String>,
    /// Successful, independently undoable moves; a failed entry stays put.
    pub operations: Vec<OperationSummary>,
    pub failures: Vec<String>,
    /// Successful entries relative to the library Trash (preserves the UI path spelling).
    pub moved_entries: Vec<PathBuf>,
    pub remaining: Option<Box<TrashContents>>,
}

/// The real `Trash/` folder of a library identified by its manifest.
fn library_trash_dir(session: &str) -> Result<PathBuf, String> {
    let dir = PathBuf::from(session)
        .canonical()
        .map_err(|e| format!("Cannot open the library {session}: {e}"))?;
    if rawtonic_catalog::layout::LibraryLayout::at(&dir).is_none() {
        return Err(format!("{} is not a library", dir.display()));
    }
    Ok(dir.join(SESSION_TRASH))
}

/// What emptying the library's Trash would delete; `None` when it has no
/// Trash folder.
fn library_trash_plan(session: &str) -> Result<Option<rawtonic_fileops::PurgePlan>, String> {
    let trash = library_trash_dir(session)?;
    if fs::symlink_metadata(&trash).is_err() {
        return Ok(None);
    }
    rawtonic_fileops::plan_purge(&trash)
        .map(Some)
        .map_err(|e| format!("{e:#}"))
}

fn contents_of(plan: &rawtonic_fileops::PurgePlan) -> TrashContents {
    TrashContents {
        photos: plan.files.iter().filter(|f| item_key(f).is_some()).count(),
        files: plan.files.len(),
        entries: plan.entries.len(),
        note: None,
        operations: Vec::new(),
        failures: Vec::new(),
        moved_entries: Vec::new(),
        remaining: None,
    }
}

/// Empties the library Trash through the system Trash, unless the user
/// separately confirmed permanent deletion. Refuses when counts changed.
pub fn empty_trash(
    app: &AppHandle,
    session: &str,
    expected: &TrashContents,
    permanently: bool,
) -> Result<TrashContents, String> {
    let plan = library_trash_plan(session)?
        .filter(|plan| !plan.entries.is_empty())
        .ok_or("The library's Trash is already empty.")?;
    let mut contents = contents_of(&plan);
    if (contents.photos, contents.files, contents.entries)
        != (expected.photos, expected.files, expected.entries)
    {
        return Err(
            "The library's Trash changed since it was counted; nothing was deleted.".into(),
        );
    }
    let label = if contents.photos == 1 {
        "Empty Trash (1 photo)".to_string()
    } else {
        format!("Empty Trash ({} photos)", contents.photos)
    };
    if permanently {
        // Only the separate, explicit permanent-deletion confirmation reaches here.
        let op = with_journal(app, |journal| journal.purge("emptyTrash", &label, &plan))?;
        sync_pending(app, Some(&op.id));
        if op.state != State::Purged {
            contents.note = op.note.clone();
        }
    } else {
        // One journal transaction per entry: failure never rolls back earlier
        // successful moves, and no failed move can fall through to purge.
        let (operations, failures) = move_trash_entries(&plan, |entry| {
            let entry_label = format!(
                "Empty Trash: {}",
                entry.file_name().unwrap_or_default().to_string_lossy()
            );
            run(app, "trash", &entry_label, vec![Step::system_trash(entry)])
        });
        contents.moved_entries = operations
            .iter()
            .flat_map(Operation::trashed)
            .filter_map(|entry| entry.strip_prefix(&plan.folder).ok().map(Path::to_path_buf))
            .collect();
        contents.operations = operations.iter().map(OperationSummary::from).collect();
        contents.failures = failures;
        contents.remaining =
            library_trash_plan(session)?.map(|remaining| Box::new(contents_of(&remaining)));
    }
    Ok(contents)
}

/// Shared session/catalog path, with the journal/OS adapter injected for tests.
fn move_trash_entries(
    plan: &rawtonic_fileops::PurgePlan,
    mut move_entry: impl FnMut(&Path) -> Result<Operation, String>,
) -> (Vec<Operation>, Vec<String>) {
    let mut moved = Vec::new();
    let mut failed = Vec::new();
    for entry in &plan.entries {
        match move_entry(entry) {
            Ok(op) => moved.push(op),
            Err(error) => {
                let files = plan
                    .files
                    .iter()
                    .filter(|file| file.starts_with(entry))
                    .map(|file| file.display().to_string())
                    .collect::<Vec<_>>();
                failed.push(format!(
                    "{}: {error}\n{}",
                    entry.display(),
                    files.join("\n")
                ));
            }
        }
    }
    (moved, failed)
}

/// What the library's Trash folder holds (zero when it has none).
#[tauri::command]
pub async fn session_trash_contents(session: String) -> Result<TrashContents, String> {
    tauri::async_runtime::spawn_blocking(move || {
        Ok(library_trash_plan(&session)?
            .map(|plan| contents_of(&plan))
            .unwrap_or_default())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Moves library Trash entries to the system Trash. Permanent deletion
/// requires the separate confirmation (`permanently` defaults to false).
#[tauri::command]
pub async fn empty_session_trash(
    app: AppHandle,
    session: String,
    photos: usize,
    files: usize,
    entries: usize,
    permanently: Option<bool>,
) -> Result<TrashContents, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let expected = TrashContents {
            photos,
            files,
            entries,
            ..TrashContents::default()
        };
        empty_trash(&app, &session, &expected, permanently.unwrap_or(false))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Trashes photos (and their virtual copies, sidecars, XMP) or, for
/// `?vc=` paths, only that virtual copy.
pub fn trash_photos(
    app: &AppHandle,
    paths: &[String],
    with_same_name: bool,
) -> Result<Operation, String> {
    let mut groups: Vec<Vec<PathBuf>> = Vec::new();
    let mut seen = HashSet::new();
    let mut images = Vec::new();
    for path in paths {
        let (source, sidecar) = crate::file_management::parse_virtual_path(path);
        if photo_ref::is_copy(path) {
            if sidecar.exists() && seen.insert(sidecar.clone()) {
                groups.push(vec![sidecar]);
            }
        } else if source.exists() && seen.insert(source.clone()) {
            images.push(source);
        }
    }
    if with_same_name {
        // Every photo sharing a selected photo's name (RAW+JPEG pairs).
        let mut extra = Vec::new();
        for image in &images {
            let Some(parent) = image.parent() else {
                continue;
            };
            for entry in fs::read_dir(parent).map_err(|e| e.to_string())?.flatten() {
                let p = entry.path();
                if p.file_stem() == image.file_stem()
                    && is_supported_image_file(&p)
                    && seen.insert(p.clone())
                {
                    extra.push(p);
                }
            }
        }
        images.extend(extra);
    }
    // One group per photo; a shared XMP goes with the last of its photos.
    let all = associated_files(&images)?;
    let mut assigned: HashSet<PathBuf> = groups.iter().flatten().cloned().collect();
    for image in &images {
        let mut group = Vec::new();
        for file in crate::file_management::find_all_associated_files(image)?
            .into_iter()
            .chain(crate::session_bridge::companion_of(image))
        {
            if assigned.insert(file.clone()) {
                group.push(file);
            }
        }
        if let Some(xmp) = xmp_for(image)
            && all.contains(&xmp)
            && assigned.insert(xmp.clone())
        {
            group.push(xmp);
        }
        groups.push(group);
    }
    let photos: Vec<_> = groups
        .iter()
        .flatten()
        .filter(|path| is_supported_image_file(path))
        .cloned()
        .collect();
    if let Some(directory) = crate::session_bridge::active_library_directory() {
        crate::session_bridge::with_library_blocking(&directory, |lib| {
            let registered: Vec<_> = lib
                .index_entries()
                .map_err(|e| e.to_string())?
                .into_keys()
                .collect();
            rawtonic_fileops::refuse_shared_photos(&photos, &registered).map_err(|e| e.to_string())
        })?;
    }
    if groups.is_empty() {
        return Err("Nothing to delete was found.".into());
    }
    let count = paths.len();
    let label = if count == 1 {
        "Delete 1 photo".to_string()
    } else {
        format!("Delete {count} photos")
    };
    run(app, "trash", &label, plan_trash(&groups))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(root: &Path) -> PathBuf {
        let dir = root.join("Job");
        rawtonic_catalog::Library::create(&dir, "Job", rawtonic_catalog::Mode::Session).unwrap();
        dir.canonical().unwrap()
    }

    /// Only the copies a run left are offered, not those an earlier run
    /// of the same operation left.
    #[test]
    fn only_newly_kept_copies_are_sent() {
        let copy = |name: &str, secs| KeptCopy {
            path: PathBuf::from(format!("/Volumes/X/{name}")),
            source: PathBuf::from(format!("/Photos/{name}")),
            complete: false,
            born: rawtonic_fileops::Born { secs, nanos: 0 },
        };
        assert_eq!(newly_kept("op", &[], Vec::new()), None);
        let earlier = vec![copy("A.RAF", 1)];
        assert_eq!(newly_kept("op", &earlier, earlier.clone()), None);
        // B is new; A again under its name is another file now.
        let sent = newly_kept("op", &earlier, vec![copy("A.RAF", 2), copy("B.RAF", 1)]).unwrap();
        assert_eq!(sent.operation_id, "op");
        let paths: Vec<_> = sent.copies.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(paths, ["/Volumes/X/A.RAF", "/Volumes/X/B.RAF"]);
        assert_eq!(sent.copies[1].source, "/Photos/B.RAF");
    }

    /// Stop reaches only the run it names, and only while it runs.
    #[test]
    fn stop_reaches_only_the_run_it_names() {
        let switch = StopSwitch::new();
        let a = switch.begin();
        switch.stop(a);
        assert!(switch.stopped(a));
        switch.end(a);
        let b = switch.begin();
        assert!(!switch.stopped(b), "a new run starts unstopped");
        switch.stop(a);
        assert!(!switch.stopped(b), "a Stop left from the last run");
        // A Stop for A that read A as running, then landed after B began.
        switch.stopped.store(a, Ordering::SeqCst);
        assert!(!switch.stopped(b), "a late Stop for the last run");
        switch.stop(b);
        assert!(switch.stopped(b));
        switch.end(b);
        switch.stop(b);
        let c = switch.begin();
        assert!(!switch.stopped(c));
    }

    #[test]
    fn virtual_copy_sidecars_name_their_copy() {
        // Paths in the platform's own form, as the library stores them: the
        // key is the image's path, not a string taken apart.
        let folder = tempfile::tempdir().unwrap();
        let photo = folder.path().join("DSCF0135.RAF");
        let key = album_key(&folder.path().join("DSCF0135.RAF.9b8208.rrdata"));
        assert_eq!(key, Some(format!("{}?vc=9b8208", photo.to_string_lossy())));
        assert_eq!(album_key(&folder.path().join("DSCF0135.RAF.rrdata")), None);
        assert_eq!(album_key(&folder.path().join("DSCF0135.xmp")), None);
        // A key and a photo path compare equal only in the same form, so
        // the library's native path matches its virtual copy's key.
        let key_path = key.unwrap();
        let (image, id) = key_path.rsplit_once("?vc=").unwrap();
        assert_eq!(Path::new(image), photo);
        assert!(is_supported_image_file(image));
        assert_eq!(id, "9b8208");
    }

    /// A session photo's virtual copy is named after the photo's native
    /// path, and that key parses back to the photo beside it on every
    /// platform (a literal such as `/p/…` would need the platform's
    /// separators); a key that is not one of an image is still refused.
    #[test]
    fn a_virtual_copy_key_names_the_photo_beside_its_sidecar() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = session(tmp.path());
        let photo = dir.join("Capture/B.NEF");
        fs::create_dir_all(photo.parent().unwrap()).unwrap();
        fs::write(&photo, "b").unwrap();
        let sidecar = dir.join("Capture/B.NEF.a1b2c3.rrdata");
        fs::write(&sidecar, "{}").unwrap();
        let key = album_key(&sidecar).unwrap();
        let (image, id) = key.rsplit_once("?vc=").unwrap();
        assert_eq!((Path::new(image), id), (photo.as_path(), "a1b2c3"));
        // The sidecar is a virtual copy of the photo in the same folder.
        assert_eq!(
            Path::new(image).parent(),
            sidecar.parent(),
            "the key names the photo beside the sidecar, not the sidecar"
        );
        // Still refused: a copy id must follow an image name.
        assert_eq!(album_key(&dir.join("Capture/B.NEF..rrdata")), None);
        assert_eq!(album_key(&dir.join("Capture/B.NEF.notes.txt.rrdata")), None);
    }

    /// A RAW's recorded companion JPEG moves and goes to the Trash with it,
    /// and so does the XMP; a JPEG of the same name that is not its
    /// companion stays, and the RAW's XMP goes with the RAW all the same.
    #[test]
    fn a_companion_jpeg_belongs_to_its_raw() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = session(tmp.path());
        let capture = dir.join("Capture");
        for name in ["A.RAF", "A.JPG", "A.xmp", "B.RAF", "B.JPG", "B.xmp"] {
            fs::write(capture.join(name), name).unwrap();
        }
        let lib = rawtonic_catalog::Library::open(&dir).unwrap();
        let planned = lib
            .plan_add(&capture.join("A.RAF"), Default::default())
            .unwrap()
            .with_companion("A.JPG")
            .unwrap();
        lib.insert_planned(vec![planned]).unwrap();
        lib.add_files(&[capture.join("B.RAF"), capture.join("B.JPG")])
            .unwrap();
        let files = associated_files(&[capture.join("A.RAF"), capture.join("B.RAF")]).unwrap();
        assert!(files.contains(&capture.join("A.JPG")));
        assert!(files.contains(&capture.join("A.xmp")));
        assert!(!files.contains(&capture.join("B.JPG")));
        assert!(files.contains(&capture.join("B.xmp")));
        assert_eq!(
            crate::session_bridge::companions_in_folder(&capture),
            HashSet::from([capture.join("A.JPG")])
        );
        assert!(crate::session_bridge::companions_in_folder(&dir.join("Selects")).is_empty());
    }

    /// A photograph reached through a link: the JPEG of its name beside
    /// the link is not its companion (that is beside the link's target).
    #[cfg(unix)]
    #[test]
    fn a_linked_photo_never_takes_the_jpeg_beside_the_link() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = session(tmp.path());
        let capture = dir.join("Capture");
        let links = capture.join("Links");
        fs::create_dir_all(&links).unwrap();
        fs::write(capture.join("A.RAF"), "raw").unwrap();
        fs::write(capture.join("A.JPG"), "jpeg").unwrap();
        let lib = rawtonic_catalog::Library::open(&dir).unwrap();
        let planned = lib
            .plan_add(&capture.join("A.RAF"), Default::default())
            .unwrap()
            .with_companion("A.JPG")
            .unwrap();
        lib.insert_planned(vec![planned]).unwrap();
        std::os::unix::fs::symlink(capture.join("A.RAF"), links.join("A.RAF")).unwrap();
        fs::write(links.join("A.JPG"), "someone else's").unwrap();
        assert_eq!(
            crate::session_bridge::companion_of(&links.join("A.RAF")),
            None
        );
        assert_eq!(
            crate::session_bridge::companion_of(&capture.join("A.RAF")),
            Some(capture.join("A.JPG"))
        );
    }

    #[test]
    fn uppercase_xmp_follows_its_raw_also_beside_a_jpeg() {
        let tmp = tempfile::tempdir().unwrap();
        let photo = tmp.path().join("upper.arw");
        let upper = photo.with_extension("XMP");
        fs::write(&photo, b"synthetic original").unwrap();
        fs::write(&upper, b"sidecar").unwrap();
        if photo.with_extension("xmp").exists() {
            return;
        }
        assert!(
            associated_files(std::slice::from_ref(&photo))
                .unwrap()
                .contains(&upper)
        );
        let jpeg = photo.with_extension("jpg");
        fs::write(&jpeg, b"partner").unwrap();
        // The stem sidecar is the RAW's: it goes with the RAW alone, never
        // with the JPEG alone.
        assert!(
            associated_files(std::slice::from_ref(&photo))
                .unwrap()
                .contains(&upper)
        );
        assert!(
            !associated_files(std::slice::from_ref(&jpeg))
                .unwrap()
                .contains(&upper)
        );
        assert!(associated_files(&[photo, jpeg]).unwrap().contains(&upper));
    }

    #[test]
    fn each_partner_of_a_raw_jpeg_pair_moves_with_its_own_xmp() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        for name in [
            "A.RAF",
            "A.JPG",
            "A.xmp",
            "A.JPG.xmp",
            "A.RAF.rrdata",
            "A.RAF.abc123.rrdata",
            "B.ARW",
            "B.xmp",
        ] {
            fs::write(dir.join(name), name).unwrap();
        }
        let files = associated_files(&[dir.join("A.RAF"), dir.join("B.ARW")]).unwrap();
        assert!(files.contains(&dir.join("A.RAF.rrdata")));
        assert!(files.contains(&dir.join("A.RAF.abc123.rrdata")));
        assert!(files.contains(&dir.join("B.xmp")), "B's own XMP moves");
        assert!(
            files.contains(&dir.join("A.xmp")),
            "the RAW's XMP moves with it"
        );
        assert!(!files.contains(&dir.join("A.JPG")));
        assert!(
            !files.contains(&dir.join("A.JPG.xmp")),
            "the JPEG keeps its own"
        );
        let jpeg = associated_files(&[dir.join("A.JPG")]).unwrap();
        assert!(jpeg.contains(&dir.join("A.JPG.xmp")));
        assert!(!jpeg.contains(&dir.join("A.xmp")), "never the RAW's");
    }

    #[test]
    fn a_lone_jpegs_xmp_moved_beside_its_raw_takes_its_own_name() {
        let tmp = tempfile::tempdir().unwrap();
        let (from, to) = (tmp.path().join("from"), tmp.path().join("to"));
        fs::create_dir_all(&from).unwrap();
        fs::create_dir_all(&to).unwrap();
        for name in ["A.JPG", "A.xmp", "B.JPG", "B.xmp"] {
            fs::write(from.join(name), name).unwrap();
        }
        fs::write(to.join("A.NEF"), b"raw").unwrap();
        let images = [from.join("A.JPG"), from.join("B.JPG")];
        let targets = xmp_targets(&images, &to);
        assert_eq!(
            targets.get(&from.join("A.xmp")),
            Some(&to.join("A.JPG.xmp"))
        );
        assert_eq!(
            targets.get(&from.join("B.xmp")),
            None,
            "B stays alone there"
        );
    }

    #[test]
    fn a_pair_brought_together_by_a_move_keeps_each_its_own_xmp() {
        let tmp = tempfile::tempdir().unwrap();
        let (jpegs, raws, to) = (
            tmp.path().join("jpegs"),
            tmp.path().join("raws"),
            tmp.path().join("to"),
        );
        for dir in [&jpegs, &raws, &to] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(jpegs.join("A.JPG"), b"jpg").unwrap();
        fs::write(jpegs.join("A.xmp"), b"jpeg's").unwrap();
        fs::write(raws.join("A.NEF"), b"raw").unwrap();
        // Both moved in one operation into an empty folder.
        let images = [jpegs.join("A.JPG"), raws.join("A.NEF")];
        assert_eq!(
            xmp_targets(&images, &to).get(&jpegs.join("A.xmp")),
            Some(&to.join("A.JPG.xmp"))
        );
        assert!(partner_xmp_renames(&images, &to).is_empty());
        // The RAW moved alone into the folder that holds the JPEG and its
        // XMP: the JPEG's takes its own name there first.
        fs::write(to.join("B.JPG"), b"jpg").unwrap();
        fs::write(to.join("B.xmp"), b"jpeg's").unwrap();
        fs::write(raws.join("B.NEF"), b"raw").unwrap();
        assert_eq!(
            partner_xmp_renames(&[raws.join("B.NEF")], &to),
            [(to.join("B.xmp"), to.join("B.JPG.xmp"))]
        );
    }

    /// (g) of review round 3: a rendered TIFF of a JPEG's name moved in
    /// beside it: the JPEG's `A.xmp` takes its own full name first, and the
    /// TIFF never takes it.
    #[test]
    fn a_tiff_moved_beside_a_lone_jpeg_leaves_the_jpegs_xmp_its_own() {
        let tmp = tempfile::tempdir().unwrap();
        let (from, to) = (tmp.path().join("from"), tmp.path().join("to"));
        fs::create_dir_all(&from).unwrap();
        fs::create_dir_all(&to).unwrap();
        fs::write(from.join("A.TIF"), b"rendered").unwrap();
        fs::write(to.join("A.JPG"), b"jpg").unwrap();
        fs::write(to.join("A.xmp"), b"jpeg's").unwrap();
        let images = [from.join("A.TIF")];
        assert_eq!(
            partner_xmp_renames(&images, &to),
            [(to.join("A.xmp"), to.join("A.JPG.xmp"))]
        );
        assert!(xmp_targets(&images, &to).is_empty());
        assert!(
            !associated_files(&images)
                .unwrap()
                .contains(&to.join("A.xmp"))
        );
        // After the move the TIFF has no sidecar of its own yet, the JPEG
        // has its own.
        fs::rename(to.join("A.xmp"), to.join("A.JPG.xmp")).unwrap();
        fs::rename(from.join("A.TIF"), to.join("A.TIF")).unwrap();
        assert_eq!(rawtonic_metadata::existing_sidecar(&to.join("A.TIF")), None);
        assert_eq!(
            rawtonic_metadata::existing_sidecar(&to.join("A.JPG")),
            Some(to.join("A.JPG.xmp"))
        );
    }

    #[test]
    fn a_jpegs_own_xmp_is_moved_once_whatever_its_letter_case() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        for name in ["A.CR3", "A.JPG", "A.JPG.XMP"] {
            fs::write(dir.join(name), name).unwrap();
        }
        let files = associated_files(&[dir.join("A.JPG")]).unwrap();
        let xmps = files
            .iter()
            .filter(|f| f.extension().is_some_and(|e| e.eq_ignore_ascii_case("xmp")))
            .count();
        assert_eq!(xmps, 1, "{files:?}");
    }

    #[test]
    fn a_sidecar_two_raws_share_stays_with_the_one_left_behind() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        for name in ["A.CR3", "A.DNG", "A.xmp"] {
            fs::write(dir.join(name), name).unwrap();
        }
        let one = associated_files(&[dir.join("A.CR3")]).unwrap();
        assert!(
            !one.contains(&dir.join("A.xmp")),
            "A.DNG stays and keeps A.xmp"
        );
        let both = associated_files(&[dir.join("A.CR3"), dir.join("A.DNG")]).unwrap();
        assert!(both.contains(&dir.join("A.xmp")), "both go, so does it");
    }

    #[test]
    fn session_files_go_to_the_session_trash_and_from_there_to_the_system_trash() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = session(tmp.path());
        let photo = dir.join("Capture/A.RAF");
        fs::write(&photo, "a").unwrap();
        let sidecar = dir.join("Capture/A.RAF.rrdata");
        fs::write(&sidecar, "{}").unwrap();
        let outside = tmp.path().canonical().unwrap().join("B.RAF");
        fs::write(&outside, "b").unwrap();
        let steps = plan_trash(&[vec![photo.clone(), sidecar.clone()], vec![outside.clone()]]);
        assert_eq!(
            steps,
            vec![
                Step::rename(&photo, dir.join("Trash/Capture/A.RAF")),
                Step::rename(&sidecar, dir.join("Trash/Capture/A.RAF.rrdata")),
                Step::system_trash(&outside),
            ]
        );
        // Already in the session Trash: the system Trash.
        let in_trash = dir.join("Trash/Capture/C.RAF");
        fs::create_dir_all(in_trash.parent().unwrap()).unwrap();
        fs::write(&in_trash, "c").unwrap();
        assert_eq!(
            plan_trash(&[vec![in_trash.clone()]]),
            vec![Step::system_trash(&in_trash)]
        );
        // A taken name moves the whole photo into a numbered folder.
        fs::write(dir.join("Trash/Capture/A.RAF"), "older").unwrap();
        let steps = plan_trash(&[vec![photo.clone(), sidecar.clone()]]);
        assert_eq!(
            steps,
            vec![
                Step::rename(&photo, dir.join("Trash/Capture/1/A.RAF")),
                Step::rename(&sidecar, dir.join("Trash/Capture/1/A.RAF.rrdata")),
            ]
        );
        // A session folder goes to the session Trash as a whole.
        let folder = dir.join("Selects");
        fs::create_dir_all(&folder).unwrap();
        assert_eq!(
            plan_trash(&[vec![folder.clone()]]),
            vec![Step::rename(&folder, dir.join("Trash/Selects"))]
        );
    }

    /// C1Q1 (G38): undoing "Move contents" gives the Capture role back to
    /// the folder the photos returned to, and redo gives it to the new
    /// folder again; a folder chosen in between keeps the role, and a role
    /// that cannot go back is refused without holding up the records.
    #[test]
    fn move_contents_undo_and_redo_carry_the_folder_role() {
        use crate::session_folders::{Followed, follow};
        use rawtonic_catalog::session_folders::FolderRole;
        let tmp = tempfile::tempdir().unwrap();
        let dir = session(tmp.path());
        let (old, new) = (dir.join("Capture"), dir.join("Capture2"));
        fs::create_dir_all(&new).unwrap();
        let photo = old.join("A.NEF");
        fs::write(&photo, "a").unwrap();
        Library::open(&dir).unwrap().add_asset(&photo).unwrap();
        let folder = |role| Library::open(&dir).unwrap().session_folder(role).path;
        let set = |role, path: &Path| {
            Library::open(&dir)
                .unwrap()
                .set_session_folder(role, path)
                .unwrap();
        };
        let journal = Journal::open(&tmp.path().join("journal.sqlite")).unwrap();
        let nt = &rawtonic_fileops::NoSystemTrash;
        // As Session Folders does it: the role first, then the contents.
        let move_contents = || {
            set(FolderRole::Capture, &new);
            let steps = rawtonic_fileops::plan_move_contents(&old, &new)
                .unwrap()
                .remove(0);
            let op = journal
                .execute("move", "Move contents to Capture2", steps, nt)
                .unwrap();
            let context = crate::session_folders::move_context(&dir, &old, &new).unwrap();
            journal
                .set_context(
                    &op.id,
                    serde_json::json!({ crate::session_folders::CONTEXT_KEY: context }),
                )
                .unwrap();
            let op = journal.operation(&op.id).unwrap();
            sync_catalog(&op, true).unwrap();
            assert_eq!(follow(&journal, &op, true), Followed::Unchanged);
            op
        };
        let undo = |op: &Operation| {
            let op = journal.undo(&op.id, nt).unwrap();
            sync_catalog(&op, false).unwrap();
            let followed = follow(&journal, &op, false);
            assert!(photo.is_file());
            (op, followed)
        };
        let redo = |op: &Operation| {
            let op = journal.redo(&op.id, nt).unwrap();
            sync_catalog(&op, true).unwrap();
            let followed = follow(&journal, &op, true);
            assert!(new.join("A.NEF").is_file());
            (op, followed)
        };

        let op = move_contents();
        assert_eq!(folder(FolderRole::Capture), new);
        let (op, followed) = undo(&op);
        assert_eq!(
            followed,
            Followed::Changed {
                session: dir.clone()
            }
        );
        assert_eq!(
            folder(FolderRole::Capture),
            old,
            "undo puts the Capture role back"
        );
        // Running it again (a retried sync) changes nothing more.
        assert_eq!(follow(&journal, &op, false), Followed::Unchanged);
        let (op, followed) = redo(&op);
        assert_eq!(
            followed,
            Followed::Changed {
                session: dir.clone()
            }
        );
        assert_eq!(
            folder(FolderRole::Capture),
            new,
            "redo gives it to the new folder again"
        );

        // A folder chosen after the move keeps the role through undo and redo.
        let other = dir.join("Elsewhere");
        fs::create_dir_all(&other).unwrap();
        set(FolderRole::Capture, &other);
        let (op, followed) = undo(&op);
        assert_eq!(followed, Followed::Unchanged);
        let (op, followed) = redo(&op);
        assert_eq!(followed, Followed::Unchanged);
        assert_eq!(folder(FolderRole::Capture), other.canonical().unwrap());
        let (_, _) = undo(&op);

        // Undo, then the role chosen by hand (elsewhere and back to the old
        // folder): redo leaves it there.
        let op = move_contents();
        let (op, followed) = undo(&op);
        assert_eq!(
            followed,
            Followed::Changed {
                session: dir.clone()
            }
        );
        set(FolderRole::Capture, &other);
        set(FolderRole::Capture, &old);
        crate::session_folders::chosen_by_hand(&journal, &dir, FolderRole::Capture);
        let (op, followed) = redo(&op);
        assert_eq!(followed, Followed::Unchanged);
        assert_eq!(folder(FolderRole::Capture), old);
        let (_, _) = undo(&op);

        // The role put back by hand after the move: redo leaves it there.
        let op = move_contents();
        set(FolderRole::Capture, &old);
        let (op, followed) = undo(&op);
        assert_eq!(followed, Followed::Unchanged);
        let (op, followed) = redo(&op);
        assert_eq!(followed, Followed::Unchanged);
        assert_eq!(folder(FolderRole::Capture), old);
        let (_, _) = undo(&op);
        set(FolderRole::Capture, &old);

        // The old folder took another role since: the files come back, the
        // Capture role stays, and the records followed all the same.
        let op = move_contents();
        set(FolderRole::Selects, &old);
        let (_, followed) = undo(&op);
        assert!(matches!(followed, Followed::Refused { .. }), "{followed:?}");
        assert_eq!(folder(FolderRole::Capture), new);
        let lib = Library::open(&dir).unwrap();
        assert!(lib.asset_at(&photo).unwrap().is_some());
    }

    #[test]
    fn trashing_a_jpeg_takes_its_xmp_beside_other_editors_settings() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = session(tmp.path());
        let capture = dir.join("Capture");
        let jpeg = capture.join("A.JPG");
        for (name, contents) in [
            ("A.JPG", "jpeg"),
            ("A.NEF", "raw"),
            ("A.JPG.xmp", "jpeg packet"),
            ("A.xmp", "raw packet"),
            ("A.JPG.dop", "jpeg editor settings"),
            ("A.NEF.pp3", "raw editor settings"),
        ] {
            fs::write(capture.join(name), contents).unwrap();
        }
        let files = associated_files(std::slice::from_ref(&jpeg)).unwrap();
        let journal = Journal::open(&tmp.path().join("journal.sqlite")).unwrap();
        let trash = rawtonic_fileops::NoSystemTrash;
        let op = journal
            .execute("trash", "Delete 1 photo", plan_trash(&[files]), &trash)
            .unwrap();
        let in_trash = dir.join("Trash/Capture");
        assert!(in_trash.join("A.JPG").is_file());
        assert_eq!(
            fs::read_to_string(in_trash.join("A.JPG.xmp")).unwrap(),
            "jpeg packet"
        );
        assert_eq!(
            fs::read_to_string(capture.join("A.xmp")).unwrap(),
            "raw packet"
        );
        assert!(capture.join("A.JPG.dop").is_file());
        assert!(capture.join("A.NEF.pp3").is_file());
        journal.undo(&op.id, &trash).unwrap();
        assert!(jpeg.is_file());
        assert_eq!(
            fs::read_to_string(capture.join("A.JPG.xmp")).unwrap(),
            "jpeg packet"
        );
    }

    #[test]
    fn trash_and_undo_keep_the_catalog_record_and_its_edits() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = session(tmp.path());
        let photo = dir.join("Capture/A.RAF");
        fs::write(&photo, "a").unwrap();
        let lib = Library::open(&dir).unwrap();
        let asset = lib.add_asset(&photo).unwrap();
        let mut meta = lib.metadata(&asset.id).unwrap();
        meta.rating = 5;
        lib.set_metadata(&asset.id, &meta).unwrap();
        let journal = Journal::open(&tmp.path().join("journal.sqlite")).unwrap();
        let groups = vec![associated_files(std::slice::from_ref(&photo)).unwrap()];
        let op = journal
            .execute(
                "trash",
                "Delete 1 photo",
                plan_trash(&groups),
                &rawtonic_fileops::NoSystemTrash,
            )
            .unwrap();
        sync_catalog(&op, true).unwrap();
        let trashed = dir.join("Trash/Capture/A.RAF");
        assert!(trashed.is_file() && !photo.exists());
        let moved = lib.asset_at(&trashed).unwrap().unwrap();
        assert_eq!((moved.id.as_str(), moved.rating), (asset.id.as_str(), 5));
        let summary = OperationSummary::from(&op);
        assert_eq!((summary.items, summary.session_trash), (1, 1));

        let op = journal
            .undo(&op.id, &rawtonic_fileops::NoSystemTrash)
            .unwrap();
        sync_catalog(&op, false).unwrap();
        assert!(photo.is_file());
        assert!(
            !dir.join("Trash/Capture").exists(),
            "the created folder goes again"
        );
        assert!(dir.join("Trash").is_dir(), "the session keeps its Trash");
        assert_eq!(lib.asset_at(&photo).unwrap().unwrap().id, asset.id);
    }

    /// Find Duplicates' Move to Trash (`duplicates.rs`): only the marked
    /// copy goes, its group goes with it, and undo brings both back without
    /// reading the files again; the kept and unrelated photographs are not
    /// touched.
    #[test]
    fn trashing_a_duplicate_and_undoing_it_restore_its_group() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = session(tmp.path());
        let jpeg = |app: &str, scan: &[u8]| {
            let mut out = vec![0xFF, 0xD8, 0xFF, 0xE1];
            out.extend(((app.len() + 2) as u16).to_be_bytes());
            out.extend(app.as_bytes());
            out.extend([0xFF, 0xDA, 0, 3, 9]);
            out.extend(scan);
            out.extend([0xFF, 0xD9]);
            out
        };
        let (a, b, c) = (
            dir.join("Capture/A.jpg"),
            dir.join("Capture/B.jpg"),
            dir.join("Capture/C.jpg"),
        );
        fs::write(&a, jpeg("rating 0", b"scan")).unwrap();
        fs::write(&b, jpeg("rating 5, keywords", b"scan")).unwrap();
        fs::write(&c, jpeg("rating 0", b"another scan")).unwrap();
        let lib = Library::open(&dir).unwrap();
        for photo in [&a, &b, &c] {
            lib.add_asset(photo).unwrap();
        }
        let untouched = |p: &PathBuf| fs::read(p).unwrap();
        let (a_bytes, c_bytes) = (untouched(&a), untouched(&c));
        let none = std::sync::atomic::AtomicBool::new(false);
        lib.fill_image_hashes(2, &none, &mut |_, _| {}).unwrap();
        let group = |lib: &Library| -> Vec<Vec<PathBuf>> {
            lib.duplicate_groups()
                .unwrap()
                .into_iter()
                .map(|g| g.into_iter().map(|m| m.path).collect())
                .collect()
        };
        assert_eq!(group(&lib), [[a.clone(), b.clone()]]);
        assert!(
            lib.confirm_duplicates(std::slice::from_ref(&a), std::slice::from_ref(&b))
                .unwrap()
                .is_empty()
        );

        let journal = Journal::open(&tmp.path().join("journal.sqlite")).unwrap();
        let groups = vec![associated_files(std::slice::from_ref(&b)).unwrap()];
        let op = journal
            .execute(
                "trash",
                "Delete 1 photo",
                plan_trash(&groups),
                &rawtonic_fileops::NoSystemTrash,
            )
            .unwrap();
        sync_catalog(&op, true).unwrap();
        assert!(!b.exists() && dir.join("Trash/Capture/B.jpg").is_file());
        assert!(group(&lib).is_empty());
        assert_eq!(
            (untouched(&a), untouched(&c)),
            (a_bytes.clone(), c_bytes.clone())
        );

        let op = journal
            .undo(&op.id, &rawtonic_fileops::NoSystemTrash)
            .unwrap();
        sync_catalog(&op, false).unwrap();
        assert!(b.is_file());
        assert_eq!(
            lib.image_hash_backlog().unwrap(),
            0,
            "nothing to read again"
        );
        assert_eq!(group(&lib), [[a.clone(), b.clone()]]);
        assert_eq!((untouched(&a), untouched(&c)), (a_bytes, c_bytes));
    }

    /// Delete into the session Trash, undo, redo: the files go back into
    /// the Trash and the catalog record (id, rating) follows each step.
    #[test]
    fn redo_of_a_session_trash_keeps_the_catalog_record() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = session(tmp.path());
        let photo = dir.join("Capture/A.RAF");
        fs::write(&photo, "a").unwrap();
        let lib = Library::open(&dir).unwrap();
        let asset = lib.add_asset(&photo).unwrap();
        let mut meta = lib.metadata(&asset.id).unwrap();
        meta.rating = 3;
        lib.set_metadata(&asset.id, &meta).unwrap();
        let journal = Journal::open(&tmp.path().join("journal.sqlite")).unwrap();
        let none = &rawtonic_fileops::NoSystemTrash;
        let groups = vec![associated_files(std::slice::from_ref(&photo)).unwrap()];
        let op = journal
            .execute("trash", "Delete 1 photo", plan_trash(&groups), none)
            .unwrap();
        sync_catalog(&op, true).unwrap();
        let op = journal.undo(&op.id, none).unwrap();
        sync_catalog(&op, false).unwrap();
        assert_eq!(lib.asset_at(&photo).unwrap().unwrap().id, asset.id);

        let op = journal.redo(&op.id, none).unwrap();
        assert_eq!(op.state, State::Done);
        sync_catalog(&op, true).unwrap();
        let trashed = dir.join("Trash/Capture/A.RAF");
        assert!(trashed.is_file() && !photo.exists());
        let moved = lib.asset_at(&trashed).unwrap().unwrap();
        assert_eq!((moved.id.as_str(), moved.rating), (asset.id.as_str(), 3));
        assert!(lib.asset_at(&photo).unwrap().is_none());
        let summary = OperationSummary::from(&op);
        assert_eq!((summary.items, summary.session_trash), (1, 1));
    }

    /// Emptying the session Trash: its counts, then the files deleted and
    /// the records forgotten, with photographs elsewhere in the session
    /// untouched; a second sync is harmless.
    #[test]
    fn emptying_library_trash_deletes_its_files_and_forgets_their_records() {
        for mode in [
            rawtonic_catalog::Mode::Session,
            rawtonic_catalog::Mode::Catalog,
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let dir = tmp.path().join("Library");
            Library::create(&dir, "Library", mode).unwrap();
            fs::create_dir_all(dir.join("Capture")).unwrap();
            let kept = dir.join("Capture/K.RAF");
            fs::write(&kept, "k").unwrap();
            let lib = Library::open(&dir).unwrap();
            let kept_asset = lib.add_asset(&kept).unwrap();
            let journal = Journal::open(&tmp.path().join("journal.sqlite")).unwrap();
            let mut trashed = Vec::new();
            for name in ["A.RAF", "B.ARW"] {
                let photo = dir.join("Capture").join(name);
                let sidecar = dir.join(format!("Capture/{name}.rrdata"));
                fs::write(&photo, name).unwrap();
                fs::write(&sidecar, "{}").unwrap();
                let asset = lib.add_asset(&photo).unwrap();
                let mut metadata = lib.metadata(&asset.id).unwrap();
                metadata.rating = 4;
                lib.set_metadata(&asset.id, &metadata).unwrap();
                lib.save_editor_state(
                    &asset.id,
                    &serde_json::json!({"adjustments":{"exposure":1}}),
                )
                .unwrap();
                let op = journal
                    .execute(
                        "trash",
                        "Move to Trash",
                        plan_trash(&[vec![photo, sidecar]]),
                        &rawtonic_fileops::NoSystemTrash,
                    )
                    .unwrap();
                sync_catalog(&op, true).unwrap();
                trashed.push(asset);
            }
            let session_path = dir.to_string_lossy().into_owned();
            let plan = library_trash_plan(&session_path).unwrap().unwrap();
            let contents = contents_of(&plan);
            assert_eq!(
                (contents.photos, contents.files, contents.entries),
                (2, 4, 1)
            );

            let op = journal.purge("emptyTrash", "Empty Trash", &plan).unwrap();
            assert_eq!(op.state, State::Purged);
            forget_purged(&op).unwrap();
            forget_purged(&op).unwrap();
            assert_eq!(fs::read_dir(dir.join("Trash")).unwrap().count(), 0);
            for asset in &trashed {
                assert!(
                    lib.asset(&asset.id).is_err(),
                    "record of {:?} kept",
                    asset.path
                );
            }
            assert_eq!(lib.asset_at(&kept).unwrap().unwrap().id, kept_asset.id);
            assert_eq!(fs::read_to_string(&kept).unwrap(), "k");
            // Nothing before it can be undone any more.
            assert!(journal.last_undoable().unwrap().is_none());
            // Nothing left to count.
            let plan = library_trash_plan(&session_path).unwrap().unwrap();
            let contents = contents_of(&plan);
            assert_eq!(
                (contents.photos, contents.files, contents.entries),
                (0, 0, 0)
            );
        }
    }

    #[test]
    fn emptying_library_trash_uses_system_trash_and_undo_preserves_records() {
        for mode in [
            rawtonic_catalog::Mode::Session,
            rawtonic_catalog::Mode::Catalog,
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let dir = tmp.path().join("Library");
            Library::create(&dir, "Library", mode).unwrap();
            fs::create_dir_all(dir.join("Capture")).unwrap();
            let kept = dir.join("Capture/K.RAF");
            fs::write(&kept, "k").unwrap();
            let lib = Library::open(&dir).unwrap();
            let kept_asset = lib.add_asset(&kept).unwrap();
            let journal = Journal::open(&tmp.path().join("journal.sqlite")).unwrap();
            let mut trashed = Vec::new();
            for name in ["A.RAF", "B.ARW"] {
                let photo = dir.join("Capture").join(name);
                let sidecar = dir.join(format!("Capture/{name}.rrdata"));
                fs::write(&photo, name).unwrap();
                fs::write(&sidecar, "{}").unwrap();
                let asset = lib.add_asset(&photo).unwrap();
                let mut metadata = lib.metadata(&asset.id).unwrap();
                metadata.rating = 4;
                lib.set_metadata(&asset.id, &metadata).unwrap();
                lib.save_editor_state(
                    &asset.id,
                    &serde_json::json!({"adjustments":{"exposure":1}}),
                )
                .unwrap();
                let op = journal
                    .execute(
                        "trash",
                        "Move to Trash",
                        plan_trash(&[vec![photo, sidecar]]),
                        &rawtonic_fileops::NoSystemTrash,
                    )
                    .unwrap();
                sync_catalog(&op, true).unwrap();
                trashed.push(asset);
            }
            let session_path = dir.to_string_lossy().into_owned();
            let plan = library_trash_plan(&session_path).unwrap().unwrap();
            let contents = contents_of(&plan);
            assert_eq!(
                (contents.photos, contents.files, contents.entries),
                (2, 4, 1)
            );

            let trash = BesideTrash(tmp.path().join("os-trash"));
            let (ops, errors) = move_trash_entries(&plan, |entry| {
                let op = journal
                    .execute(
                        "trash",
                        "Empty Trash",
                        vec![Step::system_trash(entry)],
                        &trash,
                    )
                    .map_err(|e| format!("{e:#}"))?;
                sync_catalog(&op, true)?;
                Ok(op)
            });
            assert!(errors.is_empty(), "{errors:?}");
            assert_eq!(fs::read_dir(dir.join("Trash")).unwrap().count(), 0);
            assert_eq!(fs::read_dir(&trash.0).unwrap().count(), 1);
            for asset in &trashed {
                assert!(lib.asset(&asset.id).unwrap().missing);
            }
            for op in ops.iter().rev() {
                let undone = journal.undo(&op.id, &trash).unwrap();
                sync_catalog(&undone, false).unwrap();
            }
            for asset in &trashed {
                assert!(!lib.asset(&asset.id).unwrap().missing);
                assert_eq!(lib.metadata(&asset.id).unwrap().rating, 4);
                assert!(lib.editor_state(&asset.id).unwrap().is_some());
            }
            assert_eq!(lib.asset_at(&kept).unwrap().unwrap().id, kept_asset.id);
            assert_eq!(fs::read_to_string(&kept).unwrap(), "k");
            assert_eq!(
                contents_of(&library_trash_plan(&session_path).unwrap().unwrap()).files,
                4
            );
        }
    }

    #[test]
    fn unavailable_trash_never_purges_and_partial_success_stays_moved() {
        for mode in [
            rawtonic_catalog::Mode::Session,
            rawtonic_catalog::Mode::Catalog,
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let dir = tmp.path().canonical().unwrap().join("Library");
            Library::create(&dir, "Library", mode).unwrap();
            let lib = Library::open(&dir).unwrap();
            let a = dir.join("Trash/A.RAF");
            let b = dir.join("Trash/B.RAF");
            fs::create_dir_all(a.parent().unwrap()).unwrap();
            fs::write(&a, "a").unwrap();
            fs::write(&b, "b").unwrap();
            let aa = lib.add_asset(&a).unwrap();
            let bb = lib.add_asset(&b).unwrap();
            let journal = Journal::open(&tmp.path().join("journal.sqlite")).unwrap();
            let plan = library_trash_plan(dir.to_str().unwrap()).unwrap().unwrap();
            let (ops, errors) = move_trash_entries(&plan, |entry| {
                journal
                    .execute(
                        "trash",
                        "Empty Trash",
                        vec![Step::system_trash(entry)],
                        &rawtonic_fileops::NoSystemTrash,
                    )
                    .map_err(|e| format!("{e:#}"))
            });
            assert!(ops.is_empty());
            assert_eq!(errors.len(), 2);
            assert_eq!(fs::read_to_string(&a).unwrap(), "a");
            assert_eq!(fs::read_to_string(&b).unwrap(), "b");
            let trash = BesideTrash(tmp.path().join("os-trash"));
            let (ops, errors) = move_trash_entries(&plan, |entry| {
                if entry == b {
                    return Err("Volume has no system Trash".into());
                }
                let op = journal
                    .execute(
                        "trash",
                        "Empty Trash",
                        vec![Step::system_trash(entry)],
                        &trash,
                    )
                    .map_err(|e| format!("{e:#}"))?;
                sync_catalog(&op, true)?;
                Ok(op)
            });
            assert_eq!(ops.len(), 1);
            assert_eq!(errors.len(), 1);
            assert!(errors[0].contains("B.RAF"));
            assert!(!a.exists());
            assert_eq!(fs::read_to_string(&b).unwrap(), "b");
            assert!(lib.asset(&aa.id).unwrap().missing);
            assert!(!lib.asset(&bb.id).unwrap().missing);
            let undo = journal.undo(&ops[0].id, &trash).unwrap();
            sync_catalog(&undo, false).unwrap();
            assert_eq!(fs::read_to_string(&a).unwrap(), "a");
        }
    }

    #[test]
    fn only_a_session_has_a_trash_to_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let folder = tmp.path().join("Not a session");
        fs::create_dir_all(folder.join("Trash")).unwrap();
        fs::write(folder.join("Trash/A.RAF"), "a").unwrap();
        let error = library_trash_plan(&folder.to_string_lossy()).unwrap_err();
        assert!(error.contains("is not a library"), "{error}");
        // A session without a Trash folder has nothing in it.
        let dir = session(tmp.path());
        fs::remove_dir(dir.join("Trash")).unwrap();
        assert!(
            library_trash_plan(&dir.to_string_lossy())
                .unwrap()
                .is_none()
        );
        assert!(folder.join("Trash/A.RAF").is_file());
    }

    /// A catalog that cannot be written reports the failure (the journal
    /// keeps the operation unsynced); the retry later catches up, and a
    /// second run changes nothing.
    #[cfg(unix)]
    #[test]
    fn a_failed_catalog_sync_is_reported_and_a_retry_catches_up() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = session(tmp.path());
        let photo = dir.join("Capture/A.RAF");
        fs::write(&photo, "a").unwrap();
        let asset = Library::open(&dir).unwrap().add_asset(&photo).unwrap();
        let journal = Journal::open(&tmp.path().join("journal.sqlite")).unwrap();
        let renamed = dir.join("Capture/B.RAF");
        let op = journal
            .execute(
                "rename",
                "Rename 1 photo",
                vec![Step::rename(&photo, &renamed)],
                &rawtonic_fileops::NoSystemTrash,
            )
            .unwrap();

        let database = dir.join("session.sqlite");
        let writable = fs::metadata(&database).unwrap().permissions();
        fs::set_permissions(&database, fs::Permissions::from_mode(0o444)).unwrap();
        let failed = sync_catalog(&op, true);
        // SQLite gives the -wal/-shm files it made the database's mode.
        for file in ["session.sqlite", "session.sqlite-wal", "session.sqlite-shm"] {
            if dir.join(file).exists() {
                fs::set_permissions(dir.join(file), writable.clone()).unwrap();
            }
        }
        assert!(failed.is_err(), "{failed:?}");
        assert_eq!(journal.unsynced().unwrap().len(), 1);

        sync_catalog(&op, true).unwrap();
        sync_catalog(&op, true).unwrap();
        let lib = Library::open(&dir).unwrap();
        assert_eq!(lib.asset_at(&renamed).unwrap().unwrap().id, asset.id);
        assert!(lib.asset_at(&photo).unwrap().is_none());
    }

    /// Rename a→b whose sync fails, then b→c: the second waits for the
    /// first, so the retry does not leave the record at b (where no file
    /// is); an unrelated rename follows at once.
    #[test]
    fn a_later_operation_on_the_same_photo_waits_for_an_earlier_failed_sync() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = session(tmp.path());
        let (a, b, c) = (
            dir.join("Capture/A.RAF"),
            dir.join("Capture/B.RAF"),
            dir.join("Capture/C.RAF"),
        );
        let (x, y) = (dir.join("Capture/X.RAF"), dir.join("Capture/Y.RAF"));
        fs::write(&a, "a").unwrap();
        fs::write(&x, "x").unwrap();
        let lib = Library::open(&dir).unwrap();
        let asset = lib.add_asset(&a).unwrap();
        let other = lib.add_asset(&x).unwrap();
        let journal = Journal::open(&tmp.path().join("journal.sqlite")).unwrap();
        let rename = |from: &Path, to: &Path| {
            journal
                .execute(
                    "rename",
                    "Rename 1 photo",
                    vec![Step::rename(from, to)],
                    &rawtonic_fileops::NoSystemTrash,
                )
                .unwrap()
        };
        let first = rename(&a, &b);
        let second = rename(&b, &c);
        let unrelated = rename(&x, &y);
        let pending = journal.unsynced().unwrap();
        assert_eq!(pending.len(), 3);

        // The database is locked for the first one only.
        let mut follows = Vec::new();
        let results = sync_in_order(&pending, |op, may_follow| {
            follows.push(may_follow);
            if op.id == first.id {
                return Err("database is locked".into());
            }
            if !may_follow {
                return Err(WAITS_FOR_EARLIER.into());
            }
            sync_catalog(op, true)?;
            journal.mark_synced(&op.id).map_err(|e| e.to_string())
        });
        assert_eq!(follows, [true, false, true]);
        assert!(results[0].1.is_err() && results[1].1.is_err() && results[2].1.is_ok());
        assert_eq!(results[2].0.id, unrelated.id);
        assert_eq!(lib.asset_at(&a).unwrap().unwrap().id, asset.id);
        assert_eq!(lib.asset_at(&y).unwrap().unwrap().id, other.id);

        // Later both follow, in order: the record ends where the file is.
        let pending = journal.unsynced().unwrap();
        let ids: Vec<&str> = pending.iter().map(|op| op.id.as_str()).collect();
        assert_eq!(ids, [first.id.as_str(), second.id.as_str()]);
        let results = sync_in_order(&pending, |op, may_follow| {
            assert!(may_follow);
            sync_catalog(op, true)?;
            journal.mark_synced(&op.id).map_err(|e| e.to_string())
        });
        assert!(results.iter().all(|(_, r)| r.is_ok()));
        assert!(c.is_file());
        assert_eq!(lib.asset_at(&c).unwrap().unwrap().id, asset.id);
        assert!(lib.asset_at(&a).unwrap().is_none() && lib.asset_at(&b).unwrap().is_none());
        assert!(journal.unsynced().unwrap().is_empty());
    }

    #[test]
    fn moves_and_renames_carry_previews_and_virtual_copies_along() {
        use crate::preview_proxy::{ProxyKind, proxy_path_of};
        let tmp = tempfile::tempdir().unwrap();
        let dir = session(tmp.path());
        let photo = dir.join("Capture/A.RAF");
        fs::write(&photo, "a").unwrap();
        let lib = Library::open(&dir).unwrap();
        let asset = lib.add_asset(&photo).unwrap();
        let copy = dir.join("Capture/A.RAF.c0ffee.rrdata");
        crate::session_bridge::write_editor_sidecar(
            &copy,
            br#"{"version":1,"rating":4,"adjustments":{}}"#,
        )
        .unwrap();
        let standard = proxy_path_of(&photo, ProxyKind::Standard).unwrap();
        let full = proxy_path_of(&photo, ProxyKind::Full).unwrap();
        fs::create_dir_all(standard.parent().unwrap()).unwrap();
        fs::write(&standard, "standard preview").unwrap();
        fs::write(&full, "1:1 preview").unwrap();
        let journal = Journal::open(&tmp.path().join("journal.sqlite")).unwrap();

        // Moved into Selects with its virtual copy: previews follow.
        let moved = dir.join("Selects/A.RAF");
        let op = journal
            .execute(
                "move",
                "Move 1 photo",
                vec![
                    Step::rename(&photo, &moved),
                    Step::rename(&copy, dir.join("Selects/A.RAF.c0ffee.rrdata")),
                ],
                &rawtonic_fileops::NoSystemTrash,
            )
            .unwrap();
        sync_catalog(&op, true).unwrap();
        let moved_standard = proxy_path_of(&moved, ProxyKind::Standard).unwrap();
        assert_ne!(moved_standard, standard);
        assert_eq!(fs::read(&moved_standard).unwrap(), b"standard preview");
        assert!(proxy_path_of(&moved, ProxyKind::Full).unwrap().is_file());
        assert!(!standard.exists());
        assert_eq!(lib.asset_at(&moved).unwrap().unwrap().id, asset.id);
        assert_eq!(lib.copies(&asset.id).unwrap(), ["c0ffee"]);

        // Undone: they go back with the photograph.
        let op = journal
            .undo(&op.id, &rawtonic_fileops::NoSystemTrash)
            .unwrap();
        sync_catalog(&op, false).unwrap();
        assert_eq!(fs::read(&standard).unwrap(), b"standard preview");
        assert!(full.is_file());

        // A renamed folder carries the previews of everything in it.
        let renamed = dir.join("Capture 2026");
        let op = journal
            .execute(
                "rename",
                "Rename folder",
                vec![Step::rename(dir.join("Capture"), &renamed)],
                &rawtonic_fileops::NoSystemTrash,
            )
            .unwrap();
        sync_catalog(&op, true).unwrap();
        assert!(
            proxy_path_of(&renamed.join("A.RAF"), ProxyKind::Standard)
                .unwrap()
                .is_file()
        );
        let op = journal
            .undo(&op.id, &rawtonic_fileops::NoSystemTrash)
            .unwrap();
        sync_catalog(&op, false).unwrap();
        assert!(standard.is_file());

        // Deleting the virtual copy (into the session Trash) forgets it;
        // undo keeps it again.
        let op = journal
            .execute(
                "trash",
                "Delete 1 photo",
                plan_trash(&[vec![copy.clone()]]),
                &rawtonic_fileops::NoSystemTrash,
            )
            .unwrap();
        sync_catalog(&op, true).unwrap();
        assert!(lib.copies(&asset.id).unwrap().is_empty());
        let op = journal
            .undo(&op.id, &rawtonic_fileops::NoSystemTrash)
            .unwrap();
        sync_catalog(&op, false).unwrap();
        assert_eq!(lib.copies(&asset.id).unwrap(), ["c0ffee"]);
    }

    /// The summary of a rename, and of its undo, lists where photos and
    /// folders went in the direction just applied; sidecars and virtual
    /// copies are left to follow their photo.
    #[test]
    fn summaries_carry_the_path_moves_of_an_operation_and_of_its_undo() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = session(tmp.path());
        let photo = dir.join("Capture/A.RAF");
        let copy = dir.join("Capture/A.RAF.c0ffee.rrdata");
        fs::write(&photo, "a").unwrap();
        fs::write(&copy, "{}").unwrap();
        let journal = Journal::open(&tmp.path().join("journal.sqlite")).unwrap();
        let renamed = dir.join("Capture/A_renamed.RAF");
        let op = journal
            .execute(
                "rename",
                "Rename 1 photo",
                vec![
                    Step::rename(&photo, &renamed),
                    Step::rename(&copy, dir.join("Capture/A_renamed.RAF.c0ffee.rrdata")),
                ],
                &rawtonic_fileops::NoSystemTrash,
            )
            .unwrap();
        let s = |p: &Path| p.to_string_lossy().into_owned();
        let summary = OperationSummary::from(&op);
        assert_eq!(
            summary.moves,
            [PathMove {
                from: s(&photo),
                to: s(&renamed)
            }]
        );
        assert_eq!(summary.new_paths, [s(&renamed)]);

        let undone = journal
            .undo(&op.id, &rawtonic_fileops::NoSystemTrash)
            .unwrap();
        let summary = OperationSummary::from(&undone);
        assert_eq!(
            summary.moves,
            [PathMove {
                from: s(&renamed),
                to: s(&photo)
            }]
        );
        assert_eq!(summary.new_paths, [s(&photo)]);

        // A folder rename and its undo.
        let folder = dir.join("Capture 2026");
        let op = journal
            .execute(
                "rename",
                "Rename folder",
                vec![Step::rename(dir.join("Capture"), &folder)],
                &rawtonic_fileops::NoSystemTrash,
            )
            .unwrap();
        let capture = dir.join("Capture");
        assert_eq!(
            OperationSummary::from(&op).moves,
            [PathMove {
                from: s(&capture),
                to: s(&folder)
            }]
        );
        let undone = journal
            .undo(&op.id, &rawtonic_fileops::NoSystemTrash)
            .unwrap();
        let summary = OperationSummary::from(&undone);
        assert_eq!(
            summary.moves,
            [PathMove {
                from: s(&folder),
                to: s(&capture)
            }]
        );
        assert!(summary.new_paths.is_empty());
        let json = serde_json::to_value(&summary).unwrap();
        assert_eq!(json["moves"][0]["to"], s(&capture));
    }

    /// A Trash folder of its own for copy moves in tests.
    struct BesideTrash(PathBuf);
    impl SystemTrash for BesideTrash {
        fn trash(&self, path: &Path) -> anyhow::Result<String> {
            fs::create_dir_all(&self.0)?;
            let item = self.0.join(uuid::Uuid::new_v4().to_string());
            fs::rename(path, &item)?;
            Ok(item.to_string_lossy().into_owned())
        }
        fn restore(&self, item: &str, original: &Path) -> anyhow::Result<()> {
            Ok(rawtonic_fs::rename_no_clobber(Path::new(item), original).map(drop)?)
        }
        fn contains(&self, item: &str) -> bool {
            Path::new(item).exists()
        }
    }

    #[test]
    fn system_trash_sync_hides_album_membership_and_undo_restores_it() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = session(tmp.path());
        let photo = dir.join("Capture/A.RAF");
        fs::write(&photo, "synthetic photo").unwrap();
        let lib = Library::open(&dir).unwrap();
        let asset = lib.add_asset(&photo).unwrap();
        let album = lib.create_collection("Kept", None).unwrap();
        lib.add_to_collection(&album.id, std::slice::from_ref(&asset.id))
            .unwrap();
        let journal = Journal::open(&tmp.path().join("journal.sqlite")).unwrap();
        let trash = BesideTrash(tmp.path().join(".trash"));
        let op = journal
            .execute(
                "trash",
                "Delete 1 photo",
                vec![Step::system_trash(&photo)],
                &trash,
            )
            .unwrap();
        sync_catalog(&op, true).unwrap();
        assert!(lib.asset(&asset.id).unwrap().missing);
        assert!(lib.collection_paths(&album.id).unwrap().is_empty());
        assert_eq!(lib.collections().unwrap()[0].count, 0);
        let op = journal.undo(&op.id, &trash).unwrap();
        sync_catalog(&op, false).unwrap();
        assert_eq!(lib.collection_paths(&album.id).unwrap(), [photo]);
        assert_eq!(lib.collections().unwrap()[0].count, 1);
        assert!(!lib.asset(&asset.id).unwrap().missing);
    }

    /// The records follow a folder moved to another drive also when that
    /// drive is offline when they do (a crash before the sync, the next
    /// start without the drive): the journal says it was a folder.
    #[test]
    fn records_follow_a_folder_moved_to_another_drive_while_that_drive_is_offline() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = session(tmp.path());
        let photo = dir.join("Capture/Day 1/A.RAF");
        fs::create_dir_all(photo.parent().unwrap()).unwrap();
        fs::write(&photo, "a").unwrap();
        let lib = Library::open(&dir).unwrap();
        let asset = lib.add_asset(&photo).unwrap();
        let trash = BesideTrash(tmp.path().join(".trash"));
        let journal = Journal::open(&tmp.path().join("journal.sqlite")).unwrap();
        let moved = dir.join("Selects/Day 1");
        let op = journal
            .execute(
                "moveFolder",
                "Move folder Day 1 to Selects",
                vec![Step::other_volume(dir.join("Capture/Day 1"), &moved)],
                &trash,
            )
            .unwrap();
        // The drive goes away before the records follow.
        let away = tmp.path().join("away");
        fs::rename(&moved, &away).unwrap();
        sync_catalog(&op, true).unwrap();
        fs::rename(&away, &moved).unwrap();
        let found = lib.asset_at(&moved.join("A.RAF")).unwrap();
        assert_eq!(found.map(|a| a.id), Some(asset.id));
    }

    /// A folder copied to another drive (its original then in the Trash),
    /// its undo and its redo: each summary moves the paths that way.
    #[test]
    fn summaries_of_a_folder_moved_to_another_drive_and_back_carry_its_move() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonical().unwrap();
        let (folder, archive) = (root.join("Shoot"), root.join("Archive"));
        fs::create_dir_all(folder.join("Day 1")).unwrap();
        fs::write(folder.join("Day 1/A.RAF"), "a").unwrap();
        fs::create_dir(&archive).unwrap();
        let trash = BesideTrash(root.join(".trash"));
        let journal = Journal::open(&root.join("journal.sqlite")).unwrap();
        // The copy move itself, also on one volume.
        let moved = archive.join("Shoot");
        let op = journal
            .execute(
                "moveFolder",
                "Move folder Shoot to Archive",
                vec![Step::other_volume(&folder, &moved)],
                &trash,
            )
            .unwrap();
        assert!(!folder.exists());
        let s = |p: &Path| p.to_string_lossy().into_owned();
        let there = [PathMove {
            from: s(&folder),
            to: s(&moved),
        }];
        let back = [PathMove {
            from: s(&moved),
            to: s(&folder),
        }];
        assert_eq!(OperationSummary::from(&op).moves, there);
        let undone = journal.undo(&op.id, &trash).unwrap();
        assert_eq!(OperationSummary::from(&undone).moves, back);
        let redone = journal.redo(&op.id, &trash).unwrap();
        assert_eq!(OperationSummary::from(&redone).moves, there);
        assert_eq!(OperationSummary::from(&redone).items, 1);
    }

    /// The Trash of the first folder holding a path.
    struct TrashPerDrive(Vec<(PathBuf, BesideTrash)>);
    impl TrashPerDrive {
        fn of(&self, path: &Path) -> &BesideTrash {
            &self
                .0
                .iter()
                .find(|(dir, _)| path.starts_with(dir))
                .unwrap()
                .1
        }
    }
    impl SystemTrash for TrashPerDrive {
        fn trash(&self, path: &Path) -> anyhow::Result<String> {
            self.of(path).trash(path)
        }
        fn restore(&self, item: &str, original: &Path) -> anyhow::Result<()> {
            self.of(original).restore(item, original)
        }
        fn contains(&self, item: &str) -> bool {
            self.0.iter().any(|(_, trash)| trash.contains(item))
        }
    }

    /// A catalog's RAW+JPEG photo (rating, edit sidecar) copied to a folder
    /// on another drive stays the same photo there: online, paired, rated;
    /// undo brings it back and leaves nothing in the drive's folder.
    /// `RAWTONIC_TEST_OTHER_VOLUME` names a folder on another drive (e.g. an
    /// attached disk image) to copy to.
    #[test]
    fn a_catalog_photo_moved_to_another_drive_stays_online_paired_and_rated() {
        let tmp = tempfile::tempdir().unwrap();
        let drive = match std::env::var_os("RAWTONIC_TEST_OTHER_VOLUME") {
            Some(dir) => tempfile::tempdir_in(dir).unwrap(),
            None => tempfile::tempdir().unwrap(),
        };
        let root = tmp.path().canonical().unwrap();
        let lib = Library::create(
            &root.join("Catalog"),
            "Catalog",
            rawtonic_catalog::Mode::Catalog,
        )
        .unwrap();
        let shoot = root.join("Shoot");
        let archive = drive.path().canonical().unwrap().join("Archive");
        fs::create_dir_all(&shoot).unwrap();
        fs::create_dir_all(&archive).unwrap();
        let files = ["A.NEF", "A.JPG", "A.NEF.rrdata"];
        for name in files {
            fs::write(shoot.join(name), name).unwrap();
        }
        let asset = lib.add_asset(&shoot.join("A.NEF")).unwrap();
        lib.set_companion(&asset.id, Some("A.JPG")).unwrap();
        let rated = rawtonic_metadata::Metadata {
            rating: 4,
            ..Default::default()
        };
        lib.set_metadata(&asset.id, &rated).unwrap();
        let steps = files
            .iter()
            .map(|name| Step::other_volume(shoot.join(name), archive.join(name)))
            .collect();
        // Each drive's own Trash, as the system's.
        let drive_trash = BesideTrash(drive.path().join(".trash"));
        let boot_trash = BesideTrash(root.join(".trash"));
        let trash = TrashPerDrive(vec![
            (archive.clone(), drive_trash),
            (root.clone(), boot_trash),
        ]);
        let journal = Journal::open(&root.join("journal.sqlite")).unwrap();
        // The records follow as `sync_catalog` has them follow (it finds
        // the open catalog by the app's state).
        let follow = |op: &Operation, forward: bool| {
            for (a, b, _) in moved(op, forward) {
                if is_supported_image_file(&b) {
                    lib.relocate(&a, &b).unwrap();
                }
            }
        };
        let op = journal
            .execute("move", "Move 1 photo to Archive", steps, &trash)
            .unwrap();
        follow(&op, true);
        let there = lib.asset_at(&archive.join("A.NEF")).unwrap().unwrap();
        assert_eq!(there.id, asset.id);
        assert!(!there.missing);
        assert_eq!(there.rating, 4);
        assert_eq!(
            lib.companion_verified(&there).unwrap(),
            Some(archive.join("A.JPG"))
        );
        assert!(lib.offline_assets().unwrap().is_empty());
        assert_eq!(lib.assets(None).unwrap().len(), 1);

        let undone = journal.undo(&op.id, &trash).unwrap();
        follow(&undone, false);
        let back = lib.asset_at(&shoot.join("A.NEF")).unwrap().unwrap();
        assert_eq!(
            (back.id.as_str(), back.missing, back.rating),
            (asset.id.as_str(), false, 4)
        );
        assert_eq!(
            lib.companion_verified(&back).unwrap(),
            Some(shoot.join("A.JPG"))
        );
        for name in files {
            assert_eq!(fs::read_to_string(shoot.join(name)).unwrap(), name);
        }
        let left: Vec<_> = fs::read_dir(&archive).unwrap().flatten().collect();
        assert!(left.is_empty(), "{left:?}");
    }

    /// Uses the real system Trash (and puts the file back): run by hand.
    /// `RAWTONIC_TEST_TRASH_DIR` names a folder on another drive (e.g. an
    /// attached disk image), whose own Trash must take the item.
    #[test]
    #[ignore]
    fn platform_trash_round_trip() {
        let tmp = match std::env::var_os("RAWTONIC_TEST_TRASH_DIR") {
            Some(dir) => tempfile::tempdir_in(dir).unwrap(),
            None => tempfile::tempdir().unwrap(),
        };
        let file = tmp
            .path()
            .canonical()
            .unwrap()
            .join("rawtonic-trash-test.txt");
        fs::write(&file, "x").unwrap();
        let item = PlatformTrash.trash(&file).unwrap();
        assert!(!file.exists());
        assert!(PlatformTrash.contains(&item), "{item}");
        #[cfg(unix)]
        if std::env::var_os("RAWTONIC_TEST_TRASH_DIR").is_some() {
            use std::os::unix::fs::MetadataExt;
            let dev = |p: &Path| fs::metadata(p).unwrap().dev();
            assert_eq!(
                dev(Path::new(&item)),
                dev(tmp.path()),
                "{item}: another drive's Trash"
            );
        }
        PlatformTrash.restore(&item, &file).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "x");
        assert!(!PlatformTrash.contains(&item));
    }
}
