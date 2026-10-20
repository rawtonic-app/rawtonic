//! Journaled, undoable rename, move and trash for the Library (G40).
//!
//! Commands plan every file an operation touches (the photo, its editor
//! sidecars, virtual copies, the EXIF cache and an XMP no other photo
//! shares), carry the plan out through `rawtonic-fileops` and then bring the
//! session/catalog database and the app's albums in line. Inside a session,
//! "delete" moves files into the session's own `Trash/` folder (Capture
//! One's session Trash); elsewhere, and from that folder, they go to the
//! system Trash. Undone operations can be redone until a new one runs.
//! The only deletion is the user's explicit Empty Trash of a session's
//! `Trash/` folder (`empty_session_trash`, journaled, not undoable).
use crate::file_management::{AlbumItem, add_to_album, get_albums, sync_album_path_changes};
use crate::formats::is_supported_image_file;
use rawtonic_catalog::{Library, session_for};
use rawtonic_fileops::{Journal, Operation, State, Step, SystemTrash, Target};
use serde::Serialize;
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
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
        Ok(rawtonic_fileops::rename_no_clobber(
            Path::new(item),
            original,
        )?)
    }
    fn contains(&self, item: &str) -> bool {
        fs::symlink_metadata(item).is_ok()
    }
}

#[cfg(any(windows, all(unix, not(target_os = "macos"))))]
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

fn with_journal<T>(
    app: &AppHandle,
    f: impl FnOnce(&Journal) -> anyhow::Result<T>,
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
            let counted =
                is_item(&step.from) || matches!(&step.to, Target::Path { path } if path.is_dir());
            if !counted {
                continue;
            }
            summary.items += 1;
            match &step.to {
                Target::SystemTrash { .. } => summary.system_trash += 1,
                Target::Path { path } => {
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
    let name = path.file_name()?.to_str()?.strip_suffix(".rrdata")?;
    let (image, id) = name.rsplit_once('.')?;
    if id.is_empty() || !is_supported_image_file(image) {
        return None;
    }
    Some(format!(
        "{}?vc={id}",
        path.with_file_name(image).to_string_lossy()
    ))
}

/// The XMP sidecar of `image` (`name.xmp`, shared by RAW+JPEG pairs).
fn xmp_for(image: &Path) -> PathBuf {
    image.with_extension("xmp")
}

/// Every file that belongs to `images` and moves with them: editor sidecars,
/// virtual copies, the EXIF cache, a RAW's companion JPEG
/// (`session_bridge::companion_of`), and an XMP whose every photo is
/// included.
pub fn associated_files(images: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let selected: HashSet<&PathBuf> = images.iter().collect();
    let mut files = BTreeSet::new();
    for image in images {
        for file in crate::file_management::find_all_associated_files(image)? {
            files.insert(file);
        }
        let companion = crate::session_bridge::companion_of(image);
        if let Some(companion) = &companion {
            files.insert(companion.clone());
        }
        let xmp = xmp_for(image);
        if !xmp.is_file() || files.contains(&xmp) {
            continue;
        }
        // Another photo with this name (RAW+JPEG) keeps its XMP; a
        // companion is no photo of its own.
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
            .filter(|p| p.join("session.json").is_file())
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
    // Album memberships of what goes to the system Trash, for undo.
    let trashed: Vec<String> = steps
        .iter()
        .filter(|s| matches!(s.to, Target::SystemTrash { .. }))
        .filter_map(|s| item_key(&s.from))
        .collect();
    let memberships = album_memberships(app, &trashed);
    let op = with_journal(app, |journal| {
        let op = journal.execute(kind, label, steps, &PlatformTrash)?;
        if !memberships.is_empty() {
            journal.set_context(&op.id, serde_json::json!({ "albums": memberships }))?;
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
    let op = with_journal(app, |journal| journal.undo(&last.id, &PlatformTrash))?;
    sync_pending(app, Some(&op.id));
    Ok(Some(op))
}

/// Redoes the file operation undone last; `None` when there is none. The
/// plan is validated again first: a redo refuses, changing nothing, when a
/// target was taken or a source is gone since the undo.
pub fn redo_last(app: &AppHandle) -> Result<Option<Operation>, String> {
    let Some(next) = with_journal(app, |journal| journal.last_redoable())? else {
        return Ok(None);
    };
    let op = with_journal(app, |journal| journal.redo(&next.id, &PlatformTrash))?;
    sync_pending(app, Some(&op.id));
    Ok(Some(op))
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
/// about it for `current` only, once.
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
            let to = match &step.to {
                Target::Path { path } => Some(path.clone()),
                Target::SystemTrash { .. } => None,
            };
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
                                || i.rsplit_once("?vc=")
                                    .is_some_and(|(base, _)| wanted.contains(base))
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
    if let Ok(tree) = get_albums(app.clone()) {
        walk(&tree, &wanted, &mut found);
    }
    found
}

/// Brings the session/catalog database and the albums in line with the
/// operation's state, then records that in the journal. When the database
/// could not follow, or must not yet (`follows` false, see
/// `sync_in_order`), the operation stays unsynced (retried later; every
/// step is idempotent) and the error is returned. Albums follow at once in
/// either case, in the order of the operations.
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
    for (from, to) in op.renames() {
        let (a, b) = if forward { (from, to) } else { (to, from) };
        if b.is_dir() {
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
                let _ = add_to_album(album, images, app.clone());
            }
        }
    }
    catalog?;
    if let Err(e) = with_journal(app, |journal| journal.mark_synced(&op.id)) {
        log::warn!("File operation '{}' synced but not recorded: {e}", op.label);
    }
    Ok(())
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
    for (from, to) in op.renames() {
        let (a, b) = if forward { (from, to) } else { (to, from) };
        if !(b.is_dir() || is_supported_image_file(&b)) {
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
            Change::Missing(path, missing) => lib.set_missing_at(path, *missing).map(|_| ()),
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
    let mut carried = 0;
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
                    carried += crate::preview_proxy::carry_over(&a.join(below), entry.path());
                }
            }
        } else if is_supported_image_file(&b) {
            carried += crate::preview_proxy::carry_over(&a, &b);
        }
    }
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
            trash.file_name() == Some(SESSION_TRASH.as_ref()) && s.join("session.json").is_file()
        })
        .ok_or_else(|| format!("{} is not a session's Trash", trash.display()))?;
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
    for file in op.removed.iter().filter(|f| is_supported_image_file(f)) {
        crate::preview_proxy::discard_source(file, &kinds);
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

/// What the session's Trash folder holds, for the Empty Trash dialog.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrashContents {
    /// Photographs and virtual copies.
    pub photos: usize,
    /// Every file (sidecars, XMP and anything else included).
    pub files: usize,
    /// Top-level entries (files and folders).
    pub entries: usize,
    /// Set when the Trash was emptied but some files could not be deleted
    /// yet (they are out of the Trash and deleted at the next start).
    pub note: Option<String>,
}

/// The `Trash/` folder of the session at `session`, which must be a
/// session (its `session.json`), by its real path.
fn session_trash_dir(session: &str) -> Result<PathBuf, String> {
    let dir = PathBuf::from(session)
        .canonicalize()
        .map_err(|e| format!("Cannot open the session {session}: {e}"))?;
    if !dir.join("session.json").is_file() {
        return Err(format!("{} is not a session", dir.display()));
    }
    Ok(dir.join(SESSION_TRASH))
}

/// What emptying the session's Trash would delete; `None` when it has no
/// Trash folder.
fn session_trash_plan(session: &str) -> Result<Option<rawtonic_fileops::PurgePlan>, String> {
    let trash = session_trash_dir(session)?;
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
    }
}

/// Empties the session's Trash for good if it still holds what the user
/// confirmed (`expected`): refuses, deleting nothing, when it changed.
pub fn empty_trash(
    app: &AppHandle,
    session: &str,
    expected: &TrashContents,
) -> Result<TrashContents, String> {
    let plan = session_trash_plan(session)?
        .filter(|plan| !plan.entries.is_empty())
        .ok_or("The session's Trash is already empty.")?;
    let mut contents = contents_of(&plan);
    if (contents.photos, contents.files, contents.entries)
        != (expected.photos, expected.files, expected.entries)
    {
        return Err(
            "The session's Trash changed since it was counted; nothing was deleted.".into(),
        );
    }
    let label = if contents.photos == 1 {
        "Empty Trash (1 photo)".to_string()
    } else {
        format!("Empty Trash ({} photos)", contents.photos)
    };
    let op = with_journal(app, |journal| journal.purge("emptyTrash", &label, &plan))?;
    sync_pending(app, Some(&op.id));
    if op.state != State::Purged {
        log::warn!("'{}' is not finished: {:?}", op.label, op.note);
        contents.note = op.note.clone();
    }
    Ok(contents)
}

/// What the session's Trash folder holds (zero when it has none).
#[tauri::command]
pub async fn session_trash_contents(session: String) -> Result<TrashContents, String> {
    tauri::async_runtime::spawn_blocking(move || {
        Ok(session_trash_plan(&session)?
            .map(|plan| contents_of(&plan))
            .unwrap_or_default())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Permanently deletes everything in the session's Trash folder, after the
/// user confirmed the counts in `session_trash_contents`. Not undoable.
#[tauri::command]
pub async fn empty_session_trash(
    app: AppHandle,
    session: String,
    photos: usize,
    files: usize,
    entries: usize,
) -> Result<TrashContents, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let expected = TrashContents {
            photos,
            files,
            entries,
            note: None,
        };
        empty_trash(&app, &session, &expected)
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
        if path.contains("?vc=") {
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
        let xmp = xmp_for(image);
        if all.contains(&xmp) && assigned.insert(xmp.clone()) {
            group.push(xmp);
        }
        groups.push(group);
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
        dir.canonicalize().unwrap()
    }

    #[test]
    fn virtual_copy_sidecars_name_their_copy() {
        assert_eq!(
            album_key(Path::new("/p/DSCF0135.RAF.9b8208.rrdata")).as_deref(),
            Some("/p/DSCF0135.RAF?vc=9b8208")
        );
        assert_eq!(album_key(Path::new("/p/DSCF0135.RAF.rrdata")), None);
        assert_eq!(album_key(Path::new("/p/DSCF0135.xmp")), None);
    }

    /// A RAW's recorded companion JPEG moves and goes to the Trash with it,
    /// and so does the XMP (the companion is no photo of its own); a JPEG
    /// of the same name that is not its companion keeps the XMP.
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
        assert!(!files.contains(&capture.join("B.xmp")));
        assert_eq!(
            crate::session_bridge::companions_in_folder(&capture),
            HashSet::from([capture.join("A.JPG")])
        );
        assert!(crate::session_bridge::companions_in_folder(&dir.join("Selects")).is_empty());
    }

    #[test]
    fn associated_files_keep_a_shared_xmp_with_the_remaining_photo() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        for name in [
            "A.RAF",
            "A.JPG",
            "A.xmp",
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
            !files.contains(&dir.join("A.xmp")),
            "A.JPG stays and keeps A.xmp"
        );
        assert!(!files.contains(&dir.join("A.JPG")));
        let both = associated_files(&[dir.join("A.RAF"), dir.join("A.JPG")]).unwrap();
        assert!(
            both.contains(&dir.join("A.xmp")),
            "both photos go, so does their XMP"
        );
    }

    #[test]
    fn session_files_go_to_the_session_trash_and_from_there_to_the_system_trash() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = session(tmp.path());
        let photo = dir.join("Capture/A.RAF");
        fs::write(&photo, "a").unwrap();
        let sidecar = dir.join("Capture/A.RAF.rrdata");
        fs::write(&sidecar, "{}").unwrap();
        let outside = tmp.path().canonicalize().unwrap().join("B.RAF");
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
    fn emptying_the_session_trash_deletes_its_files_and_forgets_their_records() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = session(tmp.path());
        let kept = dir.join("Capture/K.RAF");
        fs::write(&kept, "k").unwrap();
        let lib = Library::open(&dir).unwrap();
        let kept_asset = lib.add_asset(&kept).unwrap();
        let journal = Journal::open(&tmp.path().join("journal.sqlite")).unwrap();
        let none = &rawtonic_fileops::NoSystemTrash;
        let mut trashed = Vec::new();
        for name in ["A.RAF", "B.ARW"] {
            let photo = dir.join("Capture").join(name);
            fs::write(&photo, name).unwrap();
            fs::write(dir.join(format!("Capture/{name}.rrdata")), "{}").unwrap();
            trashed.push(lib.add_asset(&photo).unwrap());
            let groups = vec![associated_files(std::slice::from_ref(&photo)).unwrap()];
            let op = journal
                .execute("trash", "Delete 1 photo", plan_trash(&groups), none)
                .unwrap();
            sync_catalog(&op, true).unwrap();
            journal.mark_synced(&op.id).unwrap();
        }
        let session_path = dir.to_string_lossy().into_owned();
        let plan = session_trash_plan(&session_path).unwrap().unwrap();
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
        let plan = session_trash_plan(&session_path).unwrap().unwrap();
        assert_eq!(contents_of(&plan), TrashContents::default());
    }

    #[test]
    fn only_a_session_has_a_trash_to_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let folder = tmp.path().join("Not a session");
        fs::create_dir_all(folder.join("Trash")).unwrap();
        fs::write(folder.join("Trash/A.RAF"), "a").unwrap();
        let error = session_trash_plan(&folder.to_string_lossy()).unwrap_err();
        assert!(error.contains("is not a session"), "{error}");
        // A session without a Trash folder has nothing in it.
        let dir = session(tmp.path());
        fs::remove_dir(dir.join("Trash")).unwrap();
        assert!(
            session_trash_plan(&dir.to_string_lossy())
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

    /// Uses the real system Trash (and puts the file back): run by hand.
    #[test]
    #[ignore]
    fn platform_trash_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp
            .path()
            .canonicalize()
            .unwrap()
            .join("rawtonic-trash-test.txt");
        fs::write(&file, "x").unwrap();
        let item = PlatformTrash.trash(&file).unwrap();
        assert!(!file.exists());
        assert!(PlatformTrash.contains(&item), "{item}");
        PlatformTrash.restore(&item, &file).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "x");
        assert!(!PlatformTrash.contains(&item));
    }
}
