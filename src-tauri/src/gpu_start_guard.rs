// SPDX-License-Identifier: AGPL-3.0-or-later
//! The GPU start's crash guard (task WSF). While a device starts (adapter
//! pick, device creation, the first processor's compile) a flag file names
//! the backends being tried and the step; a crash leaves it for the next
//! start, which then leaves those backends out for one start and says so.
//! The start after that tries the user's choice again, once; if it crashes
//! again, the fallback is kept until the user turns it back in Preferences
//! ("Use automatic again"). A fallback backend that crashes too is left out
//! as well, with a new notice. The processing backend preference itself is
//! never changed: what is left out lives in `gpu-start.json` beside it, so
//! Preferences can show it and nothing changes without the user knowing.
//!
//! Earlier versions switched the preference to OpenGL silently after such a
//! crash; a start that finds OpenGL set says so once.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use wgpu::Backends;

pub(crate) const FLAG_FILE: &str = ".gpu_init_crash_flag";
pub(crate) const STATE_FILE: &str = "gpu-start.json";

/// What the flag holds while a device starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Flag {
    pid: u32,
    /// The backends being tried, as `WGPU_BACKEND` lists them.
    backends: String,
    /// "adapter", "device" or "compile".
    stage: String,
}

/// A start that crashed while starting the device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StartCrash {
    /// The backends it was trying (empty: not known, an older version's flag).
    pub backends: Backends,
    pub stage: String,
}

/// The flag left by an earlier start.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FlagCheck {
    Clean,
    /// The flag belongs to a process that is still running (a concurrent
    /// launch or headless export), not to a crashed one.
    InProgressElsewhere,
    Crashed(StartCrash),
}

/// Backends as a comma list ("vulkan,dx12"), in wgpu's order.
pub(crate) fn names(backends: Backends) -> String {
    wgpu::Backend::ALL
        .into_iter()
        .filter(|backend| backends.contains((*backend).into()))
        .map(wgpu::Backend::to_str)
        .collect::<Vec<_>>()
        .join(",")
}

fn parse(list: &str) -> Backends {
    if list.trim().is_empty() {
        Backends::empty()
    } else {
        Backends::from_comma_list(list) - Backends::NOOP
    }
}

/// Reads the flag; `alive` tells whether a process id still runs.
pub(crate) fn check_flag(path: &Path, alive: impl Fn(u32) -> bool) -> FlagCheck {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return FlagCheck::Clean;
    };
    let flag: Option<Flag> = serde_json::from_str(&contents).ok();
    // Earlier versions wrote the process id only.
    let owner = flag
        .as_ref()
        .map(|f| f.pid)
        .or_else(|| contents.trim().parse::<u32>().ok());
    if owner.is_some_and(|pid| pid != std::process::id() && alive(pid)) {
        return FlagCheck::InProgressElsewhere;
    }
    FlagCheck::Crashed(StartCrash {
        backends: flag
            .as_ref()
            .map_or(Backends::empty(), |f| parse(&f.backends)),
        stage: flag.map_or_else(|| "unknown".to_string(), |f| f.stage),
    })
}

/// Held while a flag is written and while a clean exit removes it: true
/// once the app ends cleanly, after which no flag is written again (a
/// quit during the first compile is not a crash).
static EXITING: Mutex<bool> = Mutex::new(false);
/// This run's flag file (set at start-up), removed by a clean exit.
static FLAG_PATH: OnceLock<PathBuf> = OnceLock::new();

pub(crate) fn set_flag_path(path: PathBuf) {
    let _ = FLAG_PATH.set(path);
}

/// A clean exit: this process's flag goes and none is written again.
pub(crate) fn clean_exit() {
    if let Some(path) = FLAG_PATH.get() {
        exit_with(&EXITING, path);
    }
}

fn exit_with(gate: &Mutex<bool>, path: &Path) {
    let mut exiting = gate
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *exiting = true;
    let ours = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Flag>(&text).ok())
        .is_some_and(|flag| flag.pid == std::process::id());
    if ours {
        let _ = std::fs::remove_file(path);
    }
}

/// Writes the flag for the step about to run (atomic: a crash while
/// writing it leaves the previous step's flag, or none); nothing after a
/// clean exit began.
pub(crate) fn write_flag(path: &Path, backends: Backends, stage: &str) {
    if write_flag_with(&EXITING, path, backends, stage) {
        crate::contribute::note_gpu_phase(Some(format!("{stage} on {}", names(backends))));
    }
}

fn write_flag_with(gate: &Mutex<bool>, path: &Path, backends: Backends, stage: &str) -> bool {
    let exiting = gate
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if *exiting {
        return false;
    }
    let flag = Flag {
        pid: std::process::id(),
        backends: names(backends),
        stage: stage.to_string(),
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(bytes) = serde_json::to_vec(&flag) {
        let _ = rawtonic_fs::atomic_write(path, bytes);
    }
    true
}

pub(crate) fn clear_flag(path: &Path) {
    let _ = std::fs::remove_file(path);
    crate::contribute::note_gpu_phase(None);
}

/// Where the guard stands across starts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Step {
    /// Nothing left out.
    #[default]
    Clean,
    /// A start crashed; this start leaves its backends out, the next one
    /// tries the user's choice again.
    FellBack,
    /// This start tries the user's choice again after a fallback.
    Retrying,
    /// The user's choice crashed again: left out until the user turns it
    /// back (Preferences).
    Kept,
}

/// `gpu-start.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GuardState {
    #[serde(default)]
    pub step: Step,
    /// The backends left out (comma list).
    #[serde(default)]
    pub avoid: String,
    /// The backends of the newest crash.
    #[serde(default)]
    pub crashed: String,
    /// The step the newest crash happened in.
    #[serde(default)]
    pub crashed_stage: String,
    /// UTC date of the newest crash.
    #[serde(default)]
    pub crashed_on: Option<String>,
    /// The processing backend preference when the fallback began: a
    /// different choice made since ends it.
    #[serde(default)]
    pub preference: Option<String>,
    /// An OpenGL preference was looked at once (an earlier version may
    /// have set it after a crash).
    #[serde(default)]
    pub gl_checked: bool,
}

/// What a start shows about the guard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub(crate) enum Notice {
    /// The previous start crashed on `crashed`; this start leaves `avoid`
    /// out. `kept`: it is kept until the user turns it back; else the next
    /// start tries the user's choice again.
    FellBack {
        crashed: String,
        stage: String,
        avoid: String,
        kept: bool,
    },
    /// OpenGL is the preference, perhaps from an earlier version's silent
    /// switch after a crash.
    OpenGlPreference,
}

/// The outcome for this start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Decision {
    /// Left out of this start's device start.
    pub avoid: Backends,
    pub notice: Option<Notice>,
    /// To be saved, when it changed.
    pub next: GuardState,
}

fn normalized(preference: Option<&str>) -> Option<String> {
    preference.filter(|p| *p != "auto").map(str::to_string)
}

/// The guard's step for this start: `crash` is what the flag says about the
/// previous start, `preference` the processing backend preference.
pub(crate) fn decide(
    state: &GuardState,
    crash: Option<&StartCrash>,
    preference: Option<&str>,
    gl_compiled: bool,
    today: &str,
) -> Decision {
    let preference = normalized(preference);
    let mut next = state.clone();
    // A choice made in Preferences since the fallback began ends it.
    if next.step != Step::Clean && next.preference != preference {
        next = GuardState {
            gl_checked: next.gl_checked,
            ..GuardState::default()
        };
    }
    let mut notice = None;
    let avoided = parse(&next.avoid);
    match crash {
        Some(crash) => {
            let left_out = avoided | crash.backends;
            next.step = match next.step {
                // A first crash, or a fallback backend that crashed too:
                // the next start tries the user's choice again.
                Step::Clean | Step::FellBack => Step::FellBack,
                // The user's choice crashed again, or the kept fallback
                // crashed too: kept.
                Step::Retrying | Step::Kept => Step::Kept,
            };
            next.avoid = names(left_out);
            next.crashed = names(crash.backends);
            next.crashed_stage = crash.stage.clone();
            next.crashed_on = Some(today.to_string());
            next.preference = preference.clone();
            notice = Some(Notice::FellBack {
                crashed: next.crashed.clone(),
                stage: crash.stage.clone(),
                avoid: next.avoid.clone(),
                kept: next.step == Step::Kept,
            });
        }
        None => {
            next.step = match next.step {
                Step::FellBack => Step::Retrying,
                // The retry started: the crash was a one-off.
                Step::Retrying | Step::Clean => Step::Clean,
                Step::Kept => Step::Kept,
            };
            if next.step == Step::Clean {
                next = GuardState {
                    gl_checked: next.gl_checked,
                    ..GuardState::default()
                };
            }
        }
    }
    let avoid = match next.step {
        Step::FellBack | Step::Kept => parse(&next.avoid),
        Step::Clean | Step::Retrying => Backends::empty(),
    };
    if !next.gl_checked {
        next.gl_checked = true;
        if notice.is_none() && gl_compiled && preference.as_deref() == Some("gl") {
            notice = Some(Notice::OpenGlPreference);
        }
    }
    Decision {
        avoid,
        notice,
        next,
    }
}

/// What is left out while another process starts its GPU (its flag is not
/// a crash): the state's current set, nothing advanced.
pub(crate) fn current_avoid(state: &GuardState) -> Backends {
    match state.step {
        Step::FellBack | Step::Kept => parse(&state.avoid),
        Step::Clean | Step::Retrying => Backends::empty(),
    }
}

pub(crate) fn state_path(config_dir: &Path) -> PathBuf {
    config_dir.join(STATE_FILE)
}

pub(crate) fn read_state(path: &Path) -> GuardState {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub(crate) fn write_state(path: &Path, state: &GuardState) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(state).map_err(|e| e.to_string())?;
    rawtonic_fs::atomic_write(path, bytes).map_err(|e| e.to_string())
}

/// The guard at this start: reads the flag and the state, decides, saves
/// and clears the flag. Returns the decision and what to log once the
/// logger exists.
pub(crate) fn at_start(
    config_dir: &Path,
    preference: Option<&str>,
    gl_compiled: bool,
    today: &str,
    alive: impl Fn(u32) -> bool,
) -> (Decision, Vec<String>) {
    let flag = config_dir.join(FLAG_FILE);
    let state_file = state_path(config_dir);
    let state = read_state(&state_file);
    let mut log = Vec::new();
    let decision = match check_flag(&flag, alive) {
        FlagCheck::InProgressElsewhere => {
            log.push(
                "Another Rawtonic process is starting the GPU; not treated as a crash.".into(),
            );
            return (
                Decision {
                    avoid: current_avoid(&state),
                    notice: None,
                    next: state,
                },
                log,
            );
        }
        FlagCheck::Clean => decide(&state, None, preference, gl_compiled, today),
        FlagCheck::Crashed(crash) => {
            let _ = std::fs::remove_file(&flag);
            log.push(format!(
                "The previous run ended while starting the GPU ({} step, backends: {}).",
                crash.stage,
                if crash.backends.is_empty() {
                    "not known".to_string()
                } else {
                    names(crash.backends)
                }
            ));
            decide(&state, Some(&crash), preference, gl_compiled, today)
        }
    };
    if decision.next != state
        && let Err(error) = write_state(&state_file, &decision.next)
    {
        log.push(format!("Could not save the GPU start state: {error}"));
    }
    match decision.next.step {
        Step::FellBack | Step::Kept => log.push(format!(
            "GPU backends left out at this start after a crash: {} ({})",
            decision.next.avoid,
            if decision.next.step == Step::Kept {
                "kept until Preferences → Processing → Use automatic again"
            } else {
                "the next start tries the chosen backend again"
            }
        )),
        Step::Retrying => log.push(format!(
            "Trying the chosen GPU backend again after a crash on {}.",
            decision.next.crashed
        )),
        Step::Clean => {}
    }
    (decision, log)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TODAY: &str = "2026-11-06";

    fn crash(backends: Backends) -> StartCrash {
        StartCrash {
            backends,
            stage: "device".into(),
        }
    }

    /// Runs one start: the flag's crash (if any) against the saved state.
    fn start(state: &GuardState, crashed: Option<Backends>, preference: Option<&str>) -> Decision {
        decide(state, crashed.map(crash).as_ref(), preference, true, TODAY)
    }

    #[test]
    fn a_crash_falls_back_once_then_the_choice_is_tried_again() {
        let clean = GuardState {
            gl_checked: true,
            ..GuardState::default()
        };
        // Vulkan crashed: this start leaves it out and says so.
        let first = start(&clean, Some(Backends::VULKAN), None);
        assert_eq!(first.avoid, Backends::VULKAN);
        assert_eq!(first.next.step, Step::FellBack);
        assert_eq!(
            first.notice,
            Some(Notice::FellBack {
                crashed: "vulkan".into(),
                stage: "device".into(),
                avoid: "vulkan".into(),
                kept: false
            })
        );
        assert_eq!(first.next.crashed_on.as_deref(), Some(TODAY));
        // The next start tries the automatic choice again, quietly.
        let retry = start(&first.next, None, None);
        assert_eq!(retry.avoid, Backends::empty());
        assert_eq!(retry.next.step, Step::Retrying);
        assert_eq!(retry.notice, None);
        // It started: the crash was a one-off, nothing is left out.
        let after = start(&retry.next, None, None);
        assert_eq!(after.next, clean);
        assert_eq!(after.avoid, Backends::empty());
    }

    #[test]
    fn a_second_crash_of_the_choice_keeps_the_fallback_until_the_user_turns_it_back() {
        let s = GuardState::default();
        let first = start(&s, Some(Backends::VULKAN), None);
        let retry = start(&first.next, None, None);
        let again = start(&retry.next, Some(Backends::VULKAN), None);
        assert_eq!(again.next.step, Step::Kept);
        assert_eq!(again.avoid, Backends::VULKAN);
        assert!(matches!(
            again.notice,
            Some(Notice::FellBack { kept: true, .. })
        ));
        // Kept across starts, shown once.
        let later = start(&again.next, None, None);
        assert_eq!(later.next.step, Step::Kept);
        assert_eq!(later.avoid, Backends::VULKAN);
        assert_eq!(later.notice, None);
        // A choice made in Preferences ends it (Use automatic again resets
        // the state file; choosing a backend does too).
        let chosen = start(&later.next, None, Some("dx12"));
        assert_eq!(chosen.next.step, Step::Clean);
        assert_eq!(chosen.avoid, Backends::empty());
    }

    #[test]
    fn a_fallback_backend_that_crashes_too_is_never_kept_silently() {
        let first = start(&GuardState::default(), Some(Backends::VULKAN), None);
        // DX12, used instead, crashed too: both are left out, with a notice.
        let second = start(&first.next, Some(Backends::DX12), None);
        assert_eq!(second.avoid, Backends::VULKAN | Backends::DX12);
        assert_eq!(second.next.step, Step::FellBack);
        assert_eq!(
            second.notice,
            Some(Notice::FellBack {
                crashed: "dx12".into(),
                stage: "device".into(),
                avoid: "vulkan,dx12".into(),
                kept: false
            })
        );
        // And when the kept fallback crashes, it is left out as well.
        let retry = start(&second.next, None, None);
        let kept = start(&retry.next, Some(Backends::VULKAN), None);
        assert_eq!(kept.next.step, Step::Kept);
        let kept_too = start(&kept.next, Some(Backends::GL), None);
        assert_eq!(kept_too.next.step, Step::Kept);
        assert_eq!(
            kept_too.avoid,
            Backends::VULKAN | Backends::DX12 | Backends::GL
        );
        assert!(matches!(
            kept_too.notice,
            Some(Notice::FellBack { kept: true, .. })
        ));
    }

    #[test]
    fn an_explicit_choice_that_crashed_is_left_out_but_not_changed() {
        let first = start(&GuardState::default(), Some(Backends::DX12), Some("dx12"));
        assert_eq!(first.avoid, Backends::DX12);
        assert_eq!(first.next.preference.as_deref(), Some("dx12"));
        // The same choice: the retry follows.
        let retry = start(&first.next, None, Some("dx12"));
        assert_eq!(retry.next.step, Step::Retrying);
        // "auto" and no preference are the same choice.
        let auto = start(&GuardState::default(), Some(Backends::VULKAN), Some("auto"));
        assert_eq!(start(&auto.next, None, None).next.step, Step::Retrying);
    }

    #[test]
    fn an_open_gl_preference_is_mentioned_once() {
        let first = start(&GuardState::default(), None, Some("gl"));
        assert_eq!(first.notice, Some(Notice::OpenGlPreference));
        assert!(first.next.gl_checked);
        assert_eq!(start(&first.next, None, Some("gl")).notice, None);
        // Not where OpenGL is not built in, and not for another choice.
        let mac = decide(&GuardState::default(), None, Some("gl"), false, TODAY);
        assert_eq!(mac.notice, None);
        let other = start(&GuardState::default(), None, Some("vulkan"));
        assert_eq!(other.notice, None);
        assert!(other.next.gl_checked);
    }

    #[test]
    fn a_crash_with_unknown_backends_is_shown_and_leaves_nothing_out() {
        let first = start(&GuardState::default(), Some(Backends::empty()), None);
        assert_eq!(first.avoid, Backends::empty());
        assert!(
            matches!(first.notice, Some(Notice::FellBack { ref crashed, .. }) if crashed.is_empty())
        );
    }

    #[test]
    fn the_flag_names_the_backends_and_step_and_the_start_reads_it() {
        let dir = tempfile::tempdir().unwrap();
        let flag = dir.path().join(FLAG_FILE);
        assert_eq!(check_flag(&flag, |_| false), FlagCheck::Clean);

        write_flag(&flag, Backends::VULKAN | Backends::DX12, "adapter");
        // This process's own flag is a crash once it is gone.
        assert_eq!(
            check_flag(&flag, |_| true),
            FlagCheck::Crashed(StartCrash {
                backends: Backends::VULKAN | Backends::DX12,
                stage: "adapter".into()
            })
        );
        // Another process's flag while it runs is not.
        std::fs::write(&flag, r#"{"pid":4242,"backends":"gl","stage":"device"}"#).unwrap();
        assert_eq!(
            check_flag(&flag, |pid| pid == 4242),
            FlagCheck::InProgressElsewhere
        );
        // An earlier version's flag: the process id only, backends unknown.
        std::fs::write(&flag, "4242").unwrap();
        assert_eq!(
            check_flag(&flag, |_| false),
            FlagCheck::Crashed(StartCrash {
                backends: Backends::empty(),
                stage: "unknown".into()
            })
        );
        clear_flag(&flag);
        assert!(!flag.exists());
    }

    /// A quit while the first processor compiles is a clean exit: the flag
    /// goes and a step after it writes none, so the next start sees no crash.
    #[test]
    fn a_clean_exit_during_the_start_leaves_no_flag() {
        let dir = tempfile::tempdir().unwrap();
        let flag = dir.path().join(FLAG_FILE);
        let gate = Mutex::new(false);
        assert!(write_flag_with(&gate, &flag, Backends::VULKAN, "compile"));
        exit_with(&gate, &flag);
        assert!(!flag.exists());
        assert!(!write_flag_with(&gate, &flag, Backends::VULKAN, "device"));
        assert_eq!(check_flag(&flag, |_| false), FlagCheck::Clean);
        // Another process's flag is left alone.
        std::fs::write(&flag, r#"{"pid":4242,"backends":"gl","stage":"device"}"#).unwrap();
        exit_with(&Mutex::new(false), &flag);
        assert!(flag.exists());
    }

    #[test]
    fn at_start_saves_the_state_clears_the_flag_and_logs_what_happened() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(FLAG_FILE),
            r#"{"pid":4242,"backends":"vulkan","stage":"compile"}"#,
        )
        .unwrap();
        let (decision, log) = at_start(dir.path(), None, true, TODAY, |_| false);
        assert_eq!(decision.avoid, Backends::VULKAN);
        assert!(!dir.path().join(FLAG_FILE).exists());
        assert_eq!(read_state(&state_path(dir.path())), decision.next);
        assert!(
            log.iter()
                .any(|line| line.contains("compile step, backends: vulkan")),
            "{log:?}"
        );
        assert!(log.iter().any(|line| line.contains("left out")), "{log:?}");

        // The next start retries; a third clears the state.
        let (retry, _) = at_start(dir.path(), None, true, TODAY, |_| false);
        assert_eq!(retry.avoid, Backends::empty());
        let (clean, _) = at_start(dir.path(), None, true, TODAY, |_| false);
        assert_eq!(clean.next.step, Step::Clean);
        assert_eq!(read_state(&state_path(dir.path())).step, Step::Clean);
    }
}
