//! "Move the whole session to …" (G40, option (c)): photographs never move
//! out of their session; where a move would take them out, the Library
//! offers to move the whole session folder instead. App glue over
//! `rawtonic_catalog::session_move` (where a session may go) and the file
//! operation journal (one journaled, no-clobber, undoable step: a rename on
//! the same drive, a verified copy on another, the original trashed only
//! after the copy was verified).
use crate::file_operations::OperationSummary;
use rawtonic_catalog::session_move::{check_session_move, plan_session_move};
use rawtonic_fileops::{Operation, State, Step, Target};
use serde::Serialize;
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter};

/// The kind the journal records a whole-session move under.
pub const KIND: &str = "moveSession";

/// The event naming a session folder's old and new place after it moved,
/// or its move was undone or redone: the Library opens it there when it is
/// the open one, and its recent-libraries entry follows.
pub const SESSION_MOVED: &str = "session-moved";

#[derive(Debug, Clone, Serialize)]
pub struct SessionMoved {
    pub from: String,
    pub to: String,
}

/// What the Library offers instead of moving photographs out of their
/// session: the whole session `session` into `destination`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionMoveOffer {
    /// The session folder, and its name.
    pub session: String,
    pub name: String,
    /// The folder it would go into.
    pub destination: String,
    /// The session goes to another drive (copied, checked, then its
    /// original trashed), with what that copies and the drive's space.
    pub cross_volume: bool,
    pub bytes: u64,
    pub drive: String,
    pub free_bytes: u64,
    /// Why the session cannot go there either (nothing is changed then).
    pub refusal: Option<String>,
    /// An export of the session waits, runs or is paused: it is finished
    /// or cancelled first (`EXPORTS_PENDING`).
    pub exports_pending: bool,
}

/// The offer to move the session `session` into `destination`, with why it
/// cannot be done when it cannot. Whether the database is open in another
/// program is asked when the move runs, since this one may hold it now.
pub(crate) fn offer(session: &Path, destination: &Path) -> SessionMoveOffer {
    let mut offer = SessionMoveOffer {
        session: session.to_string_lossy().into_owned(),
        name: session
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        destination: destination.to_string_lossy().into_owned(),
        exports_pending: crate::export_queue::has_unfinished_job_in(session),
        ..SessionMoveOffer::default()
    };
    let planned = match check_session_move(session, destination) {
        Ok(planned) => planned,
        Err(e) => {
            offer.refusal = Some(format!("{e:#}"));
            return offer;
        }
    };
    offer.name = planned.name;
    match Step::moving(&planned.from, &planned.to) {
        Ok(step) => {
            let plan = crate::file_management::other_drive_plan(
                &[step],
                0,
                &destination.to_string_lossy(),
            );
            offer.cross_volume = plan.cross_volume;
            offer.bytes = plan.bytes;
            offer.drive = plan.drive;
            offer.free_bytes = plan.free_bytes;
            offer.refusal = plan.refusal;
        }
        Err(e) => offer.refusal = Some(format!("{e:#}")),
    }
    offer
}

/// Why moving a session is refused while an export of it is waiting,
/// running or paused: the job would find its photographs gone (it is not
/// moved with them). The Library offers to open the export queue.
pub const EXPORTS_PENDING: &str =
    "Finish or cancel the exports of this session first; nothing was moved.";

/// Why nothing may move the session at `session` now: a watched folder or
/// a tethered camera writes into the open session, or an export of it is
/// waiting, running or paused.
fn busy(session: &Path) -> Option<&'static str> {
    if crate::hot_folder::hot_folder_status().is_some() {
        Some("Stop the watched folder before moving the session; nothing was moved.")
    } else if crate::camera_tethering::tether_status().is_some() {
        Some("Disconnect the tethered camera before moving the session; nothing was moved.")
    } else if crate::export_queue::has_unfinished_job_in(session) {
        Some(EXPORTS_PENDING)
    } else {
        None
    }
}

const IMPORT_BUSY: &str =
    "Wait for the import or library copy to finish before moving the session; nothing was moved.";

/// How long a move waits for this program's own background readers of the
/// session (content hashes, capture facts) to let go of its database.
const RELEASE_WAIT: Duration = Duration::from_secs(3);

/// Dropped after a whole-session move (or its undo/redo) that stopped the
/// background passes (content hashes, capture facts), whether it was done,
/// refused or failed: they run again for the open library at its place as
/// it is now.
#[must_use]
pub(crate) struct ResumePasses;

impl Drop for ResumePasses {
    fn drop(&mut self) {
        if let Some(dir) = crate::session_bridge::active_library_directory() {
            crate::folder_sync::start_checksum_pass(dir.clone());
            crate::library_search::start_facts_pass(dir);
        }
    }
}

/// Stops this program's background passes and waits until no connection
/// holds the session's database at `dir` any more; refuses when one still
/// does (another program has the session open).
fn wait_until_free<T>(dir: &Path, check: impl Fn() -> anyhow::Result<T>) -> Result<T, String> {
    crate::folder_sync::stop_checksum_pass();
    crate::library_search::stop_facts_pass();
    let started = Instant::now();
    loop {
        match check() {
            Ok(done) => return Ok(done),
            Err(e) if started.elapsed() >= RELEASE_WAIT => {
                log::warn!("Session at {} still in use: {e:#}", dir.display());
                return Err(format!("{e:#}"));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

/// Moves the whole session `session` into the folder `destination` (the
/// Library's "Move the whole session to …"): checked first, run as one
/// journaled step, and the open session is opened again at its new place
/// (`SESSION_MOVED`). Fails clearly, changing nothing, when it cannot be
/// done (see `plan_session_move`; too little space on the other drive).
#[tauri::command]
pub async fn move_session(
    session: String,
    destination: String,
    app: AppHandle,
) -> Result<OperationSummary, String> {
    crate::file_management::blocking(move || {
        if let Some(reason) = busy(Path::new(&session)) {
            return Err(reason.to_string());
        }
        crate::session_bridge::holding_import_gate(IMPORT_BUSY, || {
            let session = Path::new(&session);
            let destination = Path::new(&destination);
            // Refusals other than "in use" come at once, without waiting.
            check_session_move(session, destination).map_err(|e| format!("{e:#}"))?;
            let _resume = ResumePasses;
            let planned = wait_until_free(session, || plan_session_move(session, destination))?;
            let step = Step::moving(&planned.from, &planned.to).map_err(|e| format!("{e:#}"))?;
            let label = format!(
                "Move session {} to {}",
                planned.name,
                destination
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
            );
            let op = crate::file_operations::run(&app, KIND, &label, vec![step])?;
            followed(&app, &op);
            Ok((&op).into())
        })
    })
    .await?
}

/// The open library when it is a catalog other than the moved session (at
/// `from` before, `to` after): its records of photographs in the session
/// (absolute: a catalog keeps photos anywhere) follow the move. A closed
/// catalog cannot follow; it shows them as missing until relinked.
pub(crate) fn open_catalog_other_than(from: &Path, to: &Path) -> Option<PathBuf> {
    crate::session_bridge::active_library_directory()
        .filter(|dir| dir != from && dir != to && dir.join("catalog.json").is_file())
}

/// The session folder's place before and after `op` as it stands now
/// (done: moved; undone: back), `None` when it is in neither state.
fn places(op: &Operation) -> Option<(PathBuf, PathBuf)> {
    let step = op.steps.first()?;
    let to = match &step.to {
        Target::Path { path } | Target::OtherVolume { path, .. } => path.clone(),
        Target::SystemTrash { .. } => return None,
    };
    match op.state {
        State::Done => Some((step.from.clone(), to)),
        State::Undone => Some((to, step.from.clone())),
        _ => None,
    }
}

/// Before an undo (`forward` false) or redo of the whole-session move
/// `op`: the session must not be open anywhere, as for the move itself.
/// Keep what it returns until the undo or redo is done.
pub(crate) fn before_reversal(
    op: &Operation,
    forward: bool,
) -> Result<Option<ResumePasses>, String> {
    if op.kind != KIND {
        return Ok(None);
    }
    let Some(step) = op.steps.first() else {
        return Ok(None);
    };
    let Some(target) = step.target() else {
        return Ok(None);
    };
    let now = if forward { &step.from } else { target };
    if let Some(reason) = busy(now) {
        return Err(reason.to_string());
    }
    let resume = ResumePasses;
    wait_until_free(now, || {
        anyhow::ensure!(
            !rawtonic_catalog::session_move::in_use_elsewhere(now)?,
            "The session at {} is open in another program (another Rawtonic window or the rawtonic command); close it there and try again. Nothing was moved.",
            now.display()
        );
        Ok(())
    })?;
    Ok(Some(resume))
}

/// After a whole-session move, undo or redo: the open session follows to
/// its new place and the Library hears where it went.
pub(crate) fn followed(app: &AppHandle, op: &Operation) {
    if op.kind != KIND {
        return;
    }
    let Some((from, to)) = places(op) else {
        return;
    };
    if let Err(e) = crate::session_bridge::session_moved(app, &from, &to) {
        log::warn!(
            "The moved session could not be remembered at {}: {e}",
            to.display()
        );
    }
    let _ = app.emit(
        SESSION_MOVED,
        SessionMoved {
            from: from.to_string_lossy().into_owned(),
            to: to.to_string_lossy().into_owned(),
        },
    );
}
