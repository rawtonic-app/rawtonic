// SPDX-License-Identifier: AGPL-3.0-or-later
//! Crash records for the crash report on the next start (ADR 0017,
//! channel 2). A running app keeps a marker; a Rust panic and a fatal
//! signal each leave a small note beside it. A clean exit removes all
//! three. The next start turns notes left by a process that is gone into
//! one crash record of allowlisted fields — version, OS, GPU, the panic's
//! message and location, backtrace frames reduced to function and file
//! names, the previous run's last log lines, scrubbed — and asks once
//! whether to send it. Records are kept 30 days at most; nothing here
//! sends anything.

use crate::scrub::Scrubber;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Records older than this are deleted unsent.
pub const KEEP_DAYS: u64 = 30;
/// Lines of the previous run's log a record keeps.
pub const LOG_LINES: usize = 200;
/// Log tail and backtrace, in UTF-16 units (the relay's cap).
pub const DIAGNOSTICS_CAP: usize = 24 * 1024;
const MESSAGE_CAP: usize = 2_000;
const LOCATION_CAP: usize = 300;
const MAX_FRAMES: usize = 64;

const MARKER: &str = "running.json";
const PANIC_NOTE: &str = "panic.json";
const SIGNAL_NOTE: &str = "signal.txt";
const RECORD_PREFIX: &str = "crash-";

/// The `app` block every payload carries (ADR 0017).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub version: String,
    pub os: String,
    pub os_version: String,
    pub arch: String,
}

impl AppInfo {
    /// This build on this computer; `os_version` as the system reports it.
    pub fn new(version: &str, os_version: Option<&str>) -> Self {
        let os = match std::env::consts::OS {
            "macos" => "macos",
            "windows" => "windows",
            _ => "linux",
        };
        Self {
            version: version.to_string(),
            os: os.to_string(),
            os_version: os_version.map_or_else(|| "0".to_string(), major_minor),
            arch: std::env::consts::ARCH.to_string(),
        }
    }
}

/// "26.5.1" → "26.5", "11 (26100)" → "11", "rolling" → "0": the relay
/// takes a major and an optional minor version only.
pub fn major_minor(raw: &str) -> String {
    let mut parts = raw.trim().split('.');
    let number = |part: Option<&str>| {
        let digits: String = part?
            .chars()
            .take_while(char::is_ascii_digit)
            .take(3)
            .collect();
        (!digits.is_empty()).then_some(digits)
    };
    let Some(major) = number(parts.next()) else {
        return "0".to_string();
    };
    // A minor only right after the major's digits ("11 (26100)" has none).
    let major_whole = raw.trim().split('.').next().unwrap_or("").len() == major.len();
    match number(parts.next()) {
        Some(minor) if major_whole => format!("{major}.{minor}"),
        _ => major,
    }
}

/// The graphics adapter: a vendor from a fixed list and its name as the
/// driver reports it, reduced to the characters the relay takes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gpu {
    pub vendor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl Gpu {
    /// From the adapter's PCI vendor id and name.
    pub fn new(pci_vendor: u32, name: &str) -> Self {
        let vendor = match pci_vendor {
            0x106b => "apple",
            0x1002 | 0x1022 => "amd",
            0x8086 => "intel",
            0x10de => "nvidia",
            0x5143 | 0x17cb => "qualcomm",
            // Metal reports no PCI id for Apple's own GPUs.
            _ if name.trim_start().starts_with("Apple") => "apple",
            _ => "other",
        };
        Self {
            vendor: vendor.to_string(),
            name: plain_name(name, 100),
        }
    }
}

/// `text` reduced to one line of letters, digits and the punctuation the
/// relay's names allow, or nothing when no character is left.
pub fn plain_name(text: &str, max: usize) -> Option<String> {
    let kept: String = text
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .filter(|c| c.is_alphanumeric() || " ._()+/,&'-".contains(*c))
        .collect();
    let squeezed = kept
        .split(' ')
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let cut: String = squeezed.chars().take(max).collect();
    let cut = cut.trim().to_string();
    (!cut.is_empty()).then_some(cut)
}

/// What a running app writes at start (and again once its GPU is known).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RunMarker {
    pid: u32,
    started: u64,
    app: AppInfo,
    #[serde(default)]
    gpu: Option<Gpu>,
}

/// The first panic of a run, reduced as the hook writes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PanicNote {
    pid: u32,
    at: u64,
    message: String,
    location: String,
    #[serde(default)]
    backtrace: Option<String>,
}

/// One crash, as it is kept until it is sent, refused or 30 days old.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrashRecord {
    pub id: String,
    /// UTC date of the crash, YYYY-MM-DD.
    pub crashed_on: String,
    /// When the record was made (Unix seconds); it is pruned by this.
    pub created: u64,
    pub app: AppInfo,
    #[serde(default)]
    pub gpu: Option<Gpu>,
    pub message: String,
    pub location: String,
    #[serde(default)]
    pub backtrace: Option<String>,
    #[serde(default)]
    pub log_tail: Option<String>,
    /// The user was asked about it once (Not now keeps it until it expires).
    #[serde(default)]
    pub asked: bool,
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The UTC date of a Unix time, YYYY-MM-DD.
pub fn utc_date(secs: u64) -> String {
    // Howard Hinnant's days-to-civil algorithm.
    let z = (secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// The longest end of `text` within `cap` UTF-16 units, starting at a line.
fn keep_end(text: &str, cap: usize) -> &str {
    if utf16_len(text) <= cap {
        return text;
    }
    let mut units = 0;
    let mut start = text.len();
    for (at, c) in text.char_indices().rev() {
        units += c.len_utf16();
        if units > cap {
            break;
        }
        start = at;
    }
    let rest = &text[start..];
    rest.find('\n').map_or(rest, |nl| &rest[nl + 1..])
}

/// The longest start of `text` within `cap` UTF-16 units, ending at a line.
fn keep_start(text: &str, cap: usize) -> &str {
    if utf16_len(text) <= cap {
        return text;
    }
    let mut units = 0;
    let mut end = 0;
    for (at, c) in text.char_indices() {
        units += c.len_utf16();
        if units > cap {
            break;
        }
        end = at + c.len_utf8();
    }
    let head = &text[..end];
    head.rfind('\n').map_or(head, |nl| &head[..nl])
}

fn cap_chars(text: &str, cap: usize) -> String {
    keep_start(text, cap).to_string()
}

/// The last [`LOG_LINES`] lines of a log, scrubbed and within the cap.
pub fn log_tail(log: &str, scrubber: &Scrubber) -> Option<String> {
    let lines: Vec<&str> = log.lines().collect();
    let tail = lines[lines.len().saturating_sub(LOG_LINES)..].join("\n");
    let scrubbed = scrubber.scrub(&tail);
    let kept = keep_end(&scrubbed, DIAGNOSTICS_CAP).trim_end().to_string();
    (!kept.trim().is_empty()).then_some(kept)
}

/// A source path reduced to its file name: `src/a/b.rs:12:5` → `b.rs:12:5`.
pub fn file_name_only(location: &str) -> String {
    let name = location.rsplit(['/', '\\']).next().unwrap_or(location);
    name.to_string()
}

/// Frames of the panic machinery between the hook and the code that
/// panicked.
fn is_panic_machinery(function: &str) -> bool {
    [
        "std::panicking::",
        "core::panicking::",
        "rust_begin_unwind",
        "__rust_end_short_backtrace",
        "std::sys::backtrace::",
        "core::result::unwrap_failed",
        "core::option::unwrap_failed",
        "core::option::expect_failed",
    ]
    .iter()
    .any(|marker| function.contains(marker))
}

/// A backtrace as `std::backtrace::Backtrace` prints it, reduced to
/// `N: function (file.rs:line:column)` lines: paths become file names,
/// frames without a symbol (stripped builds) and the panic machinery
/// before the panicking frame are left out. `None` when no frame is left.
pub fn reduce_backtrace(text: &str) -> Option<String> {
    let mut frames: Vec<(String, Option<String>)> = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(at) = trimmed.strip_prefix("at ") {
            if let Some(last) = frames.last_mut()
                && last.1.is_none()
            {
                last.1 = Some(file_name_only(at.trim()));
            }
            continue;
        }
        let Some((number, function)) = trimmed.split_once(": ") else {
            continue;
        };
        if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        // "0x1045f3a2c - name" on some platforms: the address goes.
        let function = function
            .split_once(" - ")
            .filter(|(address, _)| address.starts_with("0x"))
            .map_or(function, |(_, name)| name)
            .trim();
        frames.push((function.to_string(), None));
    }
    // The capture and the hook come first, then the panic machinery: the
    // panicking code starts after the first run of machinery frames near
    // the top (`std::panicking::try` further down is the thread's start).
    let skip = frames
        .iter()
        .take(32)
        .position(|(function, _)| is_panic_machinery(function))
        .map_or(0, |first| {
            first
                + frames[first..]
                    .iter()
                    .take_while(|(function, _)| is_panic_machinery(function))
                    .count()
        });
    let lines: Vec<String> = frames
        .into_iter()
        .skip(skip)
        .filter(|(function, _)| !function.is_empty() && !function.contains("<unknown>"))
        .take(MAX_FRAMES)
        .enumerate()
        .map(|(n, (function, at))| match at {
            Some(at) => format!("{n}: {function} ({at})"),
            None => format!("{n}: {function}"),
        })
        .collect();
    if lines.is_empty() {
        return None;
    }
    Some(keep_start(&lines.join("\n"), DIAGNOSTICS_CAP).to_string())
}

fn signal_name(signal: i32) -> String {
    match signal {
        4 => "SIGILL".into(),
        6 => "SIGABRT".into(),
        8 => "SIGFPE".into(),
        // SIGBUS is 10 on macOS and 7 on Linux.
        10 if cfg!(target_os = "macos") => "SIGBUS".into(),
        7 if cfg!(target_os = "linux") => "SIGBUS".into(),
        11 => "SIGSEGV".into(),
        other => format!("signal {other}"),
    }
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    let bytes = serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?;
    rawtonic_metadata::atomic_write(path, bytes).map_err(std::io::Error::other)
}

fn remove(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// A record id is digits, letters and dashes only (it names a file).
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn random_suffix() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish()
}

/// What this start knows about itself.
#[derive(Debug, Clone)]
pub struct ThisRun {
    pub pid: u32,
    pub app: AppInfo,
    pub now: u64,
}

/// The crash folder of one app (its config folder's `crash-reports`).
#[derive(Debug, Clone)]
pub struct CrashStore {
    dir: PathBuf,
}

impl CrashStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    fn record_path(&self, id: &str) -> Option<PathBuf> {
        valid_id(id).then(|| self.path(&format!("{RECORD_PREFIX}{id}.json")))
    }

    /// At start: turns a panic or fatal signal the previous run left into a
    /// crash record (when `keep`, i.e. the user did not say Never), clears
    /// the previous run's files and marks this run as running. `alive`
    /// tells whether a process id still runs; `previous_log` reads the
    /// previous run's log before this run replaces it. Returns the new
    /// record, if one was made. Another running instance's marker is left
    /// alone (and nothing is recorded or marked).
    pub fn start(
        &self,
        run: &ThisRun,
        alive: impl Fn(u32) -> bool,
        previous_log: impl FnOnce() -> Option<String>,
        scrubber: &Scrubber,
        keep: bool,
    ) -> std::io::Result<Option<CrashRecord>> {
        std::fs::create_dir_all(&self.dir)?;
        let marker: Option<RunMarker> = read_json(&self.path(MARKER));
        if let Some(marker) = &marker
            && marker.pid != run.pid
            && alive(marker.pid)
        {
            return Ok(None);
        }
        let panic: Option<PanicNote> = read_json(&self.path(PANIC_NOTE));
        let panic = panic.filter(|note| marker.as_ref().is_none_or(|m| m.pid == note.pid));
        let signal_path = self.path(SIGNAL_NOTE);
        let signal = std::fs::read_to_string(&signal_path)
            .ok()
            .and_then(|text| text.trim().parse::<i32>().ok())
            .map(|signal| {
                let at = std::fs::metadata(&signal_path)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map_or(run.now, |d| d.as_secs());
                (signal, at)
            });

        let mut record = None;
        if keep && (panic.is_some() || signal.is_some()) {
            let at = panic
                .as_ref()
                .map(|p| p.at)
                .or(signal.map(|(_, at)| at))
                .unwrap_or(run.now);
            let (message, location, backtrace) = match (&panic, signal) {
                (Some(p), _) => (p.message.clone(), p.location.clone(), p.backtrace.clone()),
                (None, Some((signal, _))) => (
                    format!(
                        "Rawtonic was ended by {} (no Rust panic was recorded; the fault was in native code such as a system library or graphics driver).",
                        signal_name(signal)
                    ),
                    "native code".to_string(),
                    None,
                ),
                (None, None) => unreachable!("checked above"),
            };
            let new = CrashRecord {
                id: format!("{}-{:016x}", run.now, random_suffix()),
                crashed_on: utc_date(at),
                created: run.now,
                app: marker
                    .as_ref()
                    .map_or_else(|| run.app.clone(), |m| m.app.clone()),
                gpu: marker.as_ref().and_then(|m| m.gpu.clone()),
                message: cap_chars(&scrubber.scrub(&message), MESSAGE_CAP),
                location: cap_chars(&scrubber.scrub(&location), LOCATION_CAP),
                backtrace: backtrace
                    .map(|b| keep_start(&scrubber.scrub(&b), DIAGNOSTICS_CAP).to_string()),
                log_tail: previous_log().and_then(|log| log_tail(&log, scrubber)),
                asked: false,
            };
            if let Some(path) = self.record_path(&new.id) {
                write_json(&path, &new)?;
                record = Some(new);
            }
        }
        remove(&self.path(PANIC_NOTE));
        remove(&signal_path);
        write_json(
            &self.path(MARKER),
            &RunMarker {
                pid: run.pid,
                started: run.now,
                app: run.app.clone(),
                gpu: None,
            },
        )?;
        Ok(record)
    }

    /// Adds the GPU to this run's marker once it is known.
    pub fn note_gpu(&self, pid: u32, gpu: Gpu) -> std::io::Result<()> {
        let path = self.path(MARKER);
        match read_json::<RunMarker>(&path) {
            Some(mut marker) if marker.pid == pid => {
                marker.gpu = Some(gpu);
                write_json(&path, &marker)
            }
            _ => Ok(()),
        }
    }

    /// A clean exit: this run's marker and notes go, so the next start
    /// asks nothing.
    pub fn clean_exit(&self, pid: u32) {
        let marker: Option<RunMarker> = read_json(&self.path(MARKER));
        if marker.is_some_and(|m| m.pid == pid) {
            remove(&self.path(MARKER));
            remove(&self.path(PANIC_NOTE));
            remove(&self.path(SIGNAL_NOTE));
        }
    }

    /// Records not yet sent, newest first; those older than [`KEEP_DAYS`]
    /// (or unreadable) are deleted.
    pub fn pending(&self, now: u64) -> Vec<CrashRecord> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut records = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !(name.starts_with(RECORD_PREFIX) && name.ends_with(".json")) {
                continue;
            }
            match read_json::<CrashRecord>(&entry.path()) {
                Some(record) if now.saturating_sub(record.created) <= KEEP_DAYS * 86_400 => {
                    records.push(record)
                }
                _ => remove(&entry.path()),
            }
        }
        records.sort_by(|a, b| b.created.cmp(&a.created).then(b.id.cmp(&a.id)));
        records
    }

    /// The user was asked about this record (Not now).
    pub fn mark_asked(&self, id: &str) -> std::io::Result<()> {
        let Some(path) = self.record_path(id) else {
            return Ok(());
        };
        match read_json::<CrashRecord>(&path) {
            Some(mut record) => {
                record.asked = true;
                write_json(&path, &record)
            }
            None => Ok(()),
        }
    }

    /// Deletes one record (sent, or the user does not want it).
    pub fn remove(&self, id: &str) {
        if let Some(path) = self.record_path(id) {
            remove(&path);
        }
    }

    /// Deletes every record (Never).
    pub fn remove_all(&self) {
        for record in self.pending(0) {
            self.remove(&record.id);
        }
    }

    /// Deletes the notes of this run (recording was turned off).
    fn remove_notes(&self) {
        remove(&self.path(PANIC_NOTE));
        remove(&self.path(SIGNAL_NOTE));
    }
}

// --- The hooks: a panic hook and fatal-signal handlers write notes for
// the next start. They are armed once the store is known and write only
// while recording is on (the user did not choose Never).

static ARMED: Mutex<Option<PathBuf>> = Mutex::new(None);
static NOTED: AtomicBool = AtomicBool::new(false);
static EXITING: AtomicBool = AtomicBool::new(false);
static RECORDING: AtomicBool = AtomicBool::new(false);

/// Turns the notes on or off (Never, or the preference). Off also deletes
/// the notes this run already wrote.
pub fn set_recording(store: &CrashStore, on: bool) {
    RECORDING.store(on, Ordering::SeqCst);
    if !on {
        store.remove_notes();
    }
}

/// Arms the hooks for `store` (the panic note, and on Unix the signal note).
pub fn arm(store: &CrashStore) {
    if let Ok(mut armed) = ARMED.lock() {
        *armed = Some(store.dir.clone());
    }
    #[cfg(unix)]
    signals::install(&store.path(SIGNAL_NOTE));
}

/// The app is ending cleanly: a fatal signal from here on (a library's
/// teardown) is not a crash.
pub fn exiting() {
    EXITING.store(true, Ordering::SeqCst);
}

/// Called from the app's panic hook: writes the first panic of the run as
/// a note (message, location reduced to a file name, reduced backtrace).
pub fn note_panic(info: &std::panic::PanicHookInfo<'_>) {
    if !RECORDING.load(Ordering::SeqCst) {
        return;
    }
    let Some(dir) = ARMED.try_lock().ok().and_then(|armed| armed.clone()) else {
        return;
    };
    if NOTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let message = if let Some(s) = info.payload().downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = info.payload().downcast_ref::<String>() {
        s.clone()
    } else {
        "panic with a non-text payload".to_string()
    };
    let location = info.location().map_or_else(
        || "unknown".to_string(),
        |loc| {
            format!(
                "{}:{}:{}",
                file_name_only(loc.file()),
                loc.line(),
                loc.column()
            )
        },
    );
    let backtrace = reduce_backtrace(&std::backtrace::Backtrace::force_capture().to_string());
    let note = PanicNote {
        pid: std::process::id(),
        at: now_secs(),
        message: cap_chars(message.trim(), MESSAGE_CAP),
        location,
        backtrace,
    };
    let _ = write_json(&dir.join(PANIC_NOTE), &note);
}

/// Writes the signal note from a signal handler (async-signal-safe: no
/// allocation, only open/write/close on a path prepared in advance). The
/// first fatal signal wins; none is written after [`exiting`] or while
/// recording is off.
pub fn note_signal(signal: i32) {
    #[cfg(unix)]
    signals::note(signal);
    #[cfg(not(unix))]
    let _ = signal;
}

#[cfg(unix)]
mod signals {
    use super::{EXITING, RECORDING};
    use std::ffi::CString;
    use std::sync::atomic::{AtomicPtr, Ordering};

    /// The signal note's path, leaked once per arming (a few bytes).
    static PATH: AtomicPtr<libc::c_char> = AtomicPtr::new(std::ptr::null_mut());
    const FATAL: [libc::c_int; 5] = [
        libc::SIGSEGV,
        libc::SIGBUS,
        libc::SIGILL,
        libc::SIGFPE,
        libc::SIGABRT,
    ];
    /// The handlers that were there before ours, called after the note.
    static mut PREVIOUS: [Option<libc::sigaction>; 5] = [None; 5];

    pub(super) fn install(path: &std::path::Path) {
        use std::os::unix::ffi::OsStrExt;
        let Ok(path) = CString::new(path.as_os_str().as_bytes()) else {
            return;
        };
        let old = PATH.swap(path.into_raw(), Ordering::SeqCst);
        if !old.is_null() {
            // Handlers are installed once; a later arming only moves the note.
            return;
        }
        for (slot, signal) in FATAL.iter().enumerate() {
            // SAFETY: sigaction with a zeroed struct, our handler and an
            // empty mask; the previous action is kept to be chained to.
            unsafe {
                let mut action: libc::sigaction = std::mem::zeroed();
                action.sa_sigaction = handler as *const () as libc::sighandler_t;
                action.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK;
                libc::sigemptyset(&mut action.sa_mask);
                let mut previous: libc::sigaction = std::mem::zeroed();
                if libc::sigaction(*signal, &action, &mut previous) == 0 {
                    PREVIOUS[slot] = Some(previous);
                }
            }
        }
    }

    pub(super) fn note(signal: libc::c_int) {
        if EXITING.load(Ordering::SeqCst) || !RECORDING.load(Ordering::SeqCst) {
            return;
        }
        let path = PATH.load(Ordering::SeqCst);
        if path.is_null() {
            return;
        }
        // Two ASCII digits at most; written without allocating.
        let mut digits = [0u8; 4];
        let mut len = 0;
        let mut value = signal.unsigned_abs();
        loop {
            digits[len] = b'0' + (value % 10) as u8;
            len += 1;
            value /= 10;
            if value == 0 || len == digits.len() {
                break;
            }
        }
        digits[..len].reverse();
        // SAFETY: open/write/close are async-signal-safe; the path is a
        // leaked, NUL-terminated CString. O_EXCL keeps the first signal.
        unsafe {
            let fd = libc::open(
                path,
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
                0o600 as libc::c_uint,
            );
            if fd >= 0 {
                libc::write(fd, digits.as_ptr().cast(), len);
                libc::close(fd);
            }
        }
    }

    extern "C" fn handler(
        signal: libc::c_int,
        info: *mut libc::siginfo_t,
        context: *mut libc::c_void,
    ) {
        note(signal);
        let slot = FATAL.iter().position(|s| *s == signal);
        // SAFETY: chains to the handler that was installed before ours
        // (Rust's stack-overflow handler, the app's SIGABRT handler), or
        // restores the default action and raises the signal again.
        unsafe {
            let previous = slot.and_then(|slot| PREVIOUS[slot]);
            match previous {
                Some(previous)
                    if previous.sa_sigaction != libc::SIG_DFL
                        && previous.sa_sigaction != libc::SIG_IGN =>
                {
                    if previous.sa_flags & libc::SA_SIGINFO != 0 {
                        let chained: extern "C" fn(
                            libc::c_int,
                            *mut libc::siginfo_t,
                            *mut libc::c_void,
                        ) = std::mem::transmute(previous.sa_sigaction);
                        chained(signal, info, context);
                    } else {
                        let chained: extern "C" fn(libc::c_int) =
                            std::mem::transmute(previous.sa_sigaction);
                        chained(signal);
                    }
                }
                _ => {
                    let mut default: libc::sigaction = std::mem::zeroed();
                    default.sa_sigaction = libc::SIG_DFL;
                    libc::sigaction(signal, &default, std::ptr::null_mut());
                    libc::raise(signal);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scrubber() -> Scrubber {
        Scrubber::new(Some("anna"), Some("studio"))
    }

    fn run(pid: u32, now: u64) -> ThisRun {
        ThisRun {
            pid,
            app: AppInfo {
                version: "0.1.0-alpha.4".into(),
                os: "macos".into(),
                os_version: "26.5".into(),
                arch: "aarch64".into(),
            },
            now,
        }
    }

    const DAY: u64 = 86_400;
    const T0: u64 = 1_792_800_000; // 2026-10-24

    #[test]
    fn os_versions_keep_major_and_minor() {
        assert_eq!(major_minor("26.5.1"), "26.5");
        assert_eq!(major_minor("15"), "15");
        assert_eq!(major_minor("11 (26100)"), "11");
        assert_eq!(major_minor("24.04"), "24.04");
        assert_eq!(major_minor("rolling"), "0");
        assert_eq!(major_minor(""), "0");
        assert_eq!(AppInfo::new("1", None).os_version, "0");
    }

    #[test]
    fn gpus_get_a_vendor_and_a_plain_name() {
        assert_eq!(
            Gpu::new(0, "Apple M5 Pro"),
            Gpu {
                vendor: "apple".into(),
                name: Some("Apple M5 Pro".into())
            }
        );
        assert_eq!(Gpu::new(0x10de, "NVIDIA GeForce RTX 3080").vendor, "nvidia");
        assert_eq!(
            Gpu::new(0x8086, "Intel(R) UHD Graphics™ 630\n")
                .name
                .as_deref(),
            Some("Intel(R) UHD Graphics 630")
        );
        assert_eq!(Gpu::new(0x1234, "™").name, None);
        assert_eq!(Gpu::new(0x1234, "x").vendor, "other");
    }

    #[test]
    fn utc_dates() {
        assert_eq!(utc_date(0), "1970-01-01");
        assert_eq!(utc_date(T0), "2026-10-24");
        assert_eq!(utc_date(951_782_400), "2000-02-29");
    }

    #[test]
    fn backtraces_lose_paths_machinery_and_unknown_frames() {
        let text = "   0: std::backtrace_rs::backtrace::libunwind::trace
             at /rustc/abc/library/std/src/../../backtrace/src/backtrace/libunwind.rs:117:9
   1: std::backtrace::Backtrace::create
             at /rustc/abc/library/std/src/backtrace.rs:331:13
   2: rawtonic_contribute::crash::note_panic
             at /Users/anna/rawtonic/crates/contribute/src/crash.rs:400:20
   3: std::panicking::rust_panic_with_hook
             at /rustc/abc/library/std/src/panicking.rs:841:13
   4: core::panicking::panic_fmt
             at /rustc/abc/library/core/src/panicking.rs:75:14
   5: rapidraw_lib::image_processing::render
             at /Users/anna/rawtonic/src-tauri/src/image_processing.rs:1200:9
   6: <unknown>
   7: 0x1045f3a2c - tokio::runtime::task::harness::poll
   8: start_thread
";
        let reduced = reduce_backtrace(text).unwrap();
        assert_eq!(
            reduced,
            "0: rapidraw_lib::image_processing::render (image_processing.rs:1200:9)\n\
             1: tokio::runtime::task::harness::poll\n\
             2: start_thread"
        );
        assert!(!reduced.contains("anna"));
        assert_eq!(reduce_backtrace("   0: <unknown>\n   1: <unknown>\n"), None);
        assert_eq!(reduce_backtrace("disabled backtrace"), None);
    }

    #[test]
    fn log_tails_keep_the_last_lines_scrubbed_and_capped() {
        let log: String = (0..500)
            .map(|n| format!("line {n} /Users/anna/Pictures/x{n}.NEF\n"))
            .collect();
        let tail = log_tail(&log, &scrubber()).unwrap();
        assert_eq!(tail.lines().count(), LOG_LINES);
        assert!(tail.starts_with("line 300 <path>.NEF"));
        assert!(tail.ends_with("line 499 <path>.NEF"));
        let long: String = (0..400)
            .map(|n| format!("{n} {}\n", "é".repeat(200)))
            .collect();
        let tail = log_tail(&long, &scrubber()).unwrap();
        assert!(utf16_len(&tail) <= DIAGNOSTICS_CAP);
        assert!(tail.ends_with(&"é".repeat(200)));
        assert!(
            tail.lines()
                .next()
                .unwrap()
                .chars()
                .next()
                .unwrap()
                .is_ascii_digit()
        );
        assert_eq!(log_tail("\n\n", &scrubber()), None);
    }

    fn store() -> (tempfile::TempDir, CrashStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = CrashStore::new(dir.path().join("crash-reports"));
        (dir, store)
    }

    #[test]
    fn a_clean_exit_leaves_nothing_to_ask_about() {
        let (_dir, store) = store();
        let none = store
            .start(&run(10, T0), |_| false, || None, &scrubber(), true)
            .unwrap();
        assert_eq!(none, None);
        store.clean_exit(10);
        let none = store
            .start(
                &run(11, T0 + 60),
                |_| false,
                || panic!("not read"),
                &scrubber(),
                true,
            )
            .unwrap();
        assert_eq!(none, None);
        // An exit that left no panic or signal (a forced quit, power loss)
        // is not reported either.
        let none = store
            .start(&run(12, T0 + 120), |_| false, || None, &scrubber(), true)
            .unwrap();
        assert_eq!(none, None);
        assert!(store.pending(T0).is_empty());
    }

    #[test]
    fn a_panic_becomes_one_scrubbed_record_with_the_previous_log() {
        let (_dir, store) = store();
        store
            .start(&run(10, T0 - DAY), |_| false, || None, &scrubber(), true)
            .unwrap();
        store.note_gpu(10, Gpu::new(0, "Apple M5 Pro")).unwrap();
        write_json(
            &store.path(PANIC_NOTE),
            &PanicNote {
                pid: 10,
                at: T0 - DAY + 30,
                message: "failed to open /Users/anna/Pictures/a.CR3 on studio".into(),
                location: "decode.rs:12:5".into(),
                backtrace: Some("0: rawdecode::open (decode.rs:12:5)".into()),
            },
        )
        .unwrap();
        let record = store
            .start(
                &run(11, T0),
                |_| false,
                || Some("opening /Users/anna/Pictures/a.CR3\nPANIC!".into()),
                &scrubber(),
                true,
            )
            .unwrap()
            .unwrap();
        assert_eq!(record.crashed_on, "2026-10-23");
        assert_eq!(record.message, "failed to open <path>.CR3");
        assert_eq!(record.location, "decode.rs:12:5");
        assert_eq!(record.gpu.as_ref().unwrap().vendor, "apple");
        assert_eq!(
            record.log_tail.as_deref(),
            Some("opening <path>.CR3\nPANIC!")
        );
        assert_eq!(store.pending(T0), vec![record.clone()]);
        // Asked once: the record stays (Not now) but is marked.
        store.mark_asked(&record.id).unwrap();
        assert!(store.pending(T0)[0].asked);
        // The next start does not report the same crash again.
        store.clean_exit(11);
        assert_eq!(
            store
                .start(&run(12, T0 + 1), |_| false, || None, &scrubber(), true)
                .unwrap(),
            None
        );
        // 30 days later it is gone unsent.
        assert_eq!(store.pending(T0 + KEEP_DAYS * DAY).len(), 1);
        assert!(store.pending(T0 + KEEP_DAYS * DAY + 1).is_empty());
        assert!(!store.record_path(&record.id).unwrap().exists());
    }

    #[test]
    fn never_keeps_no_record_and_another_running_instance_is_left_alone() {
        let (_dir, store) = store();
        store
            .start(&run(10, T0), |_| false, || None, &scrubber(), true)
            .unwrap();
        std::fs::write(store.path(SIGNAL_NOTE), "11").unwrap();
        // Process 10 still runs: its files are not ours to judge.
        assert_eq!(
            store
                .start(&run(11, T0), |pid| pid == 10, || None, &scrubber(), true)
                .unwrap(),
            None
        );
        assert!(store.path(SIGNAL_NOTE).exists());
        // The user said Never: nothing is kept, the note still goes.
        assert_eq!(
            store
                .start(&run(12, T0), |_| false, || None, &scrubber(), false)
                .unwrap(),
            None
        );
        assert!(!store.path(SIGNAL_NOTE).exists());
        assert!(store.pending(T0).is_empty());
    }

    #[test]
    fn a_fatal_signal_without_a_panic_is_recorded_as_native() {
        let (_dir, store) = store();
        store
            .start(&run(10, T0), |_| false, || None, &scrubber(), true)
            .unwrap();
        std::fs::write(store.path(SIGNAL_NOTE), "11").unwrap();
        let record = store
            .start(&run(11, T0 + 5), |_| false, || None, &scrubber(), true)
            .unwrap()
            .unwrap();
        assert!(record.message.contains("SIGSEGV"));
        assert_eq!(record.location, "native code");
        assert_eq!(record.backtrace, None);
        store.remove(&record.id);
        assert!(store.pending(T0).is_empty());
        // Ids name files: nothing else is touched.
        store.remove("../running");
        assert!(store.path(MARKER).exists());
    }

    /// The panic hook writes the first panic of the run as a note that
    /// the next start turns into a record.
    #[test]
    fn the_panic_hook_notes_the_first_panic() {
        let (_dir, store) = store();
        store
            .start(
                &run(std::process::id(), T0),
                |_| false,
                || None,
                &scrubber(),
                true,
            )
            .unwrap();
        arm(&store);
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(note_panic));
        // Never: a panic leaves nothing.
        set_recording(&store, false);
        let ignored = std::panic::catch_unwind(|| panic!("while off"));
        assert!(ignored.is_err());
        assert!(!store.path(PANIC_NOTE).exists());
        set_recording(&store, true);
        let first = std::panic::catch_unwind(|| panic!("boom at /Users/anna/secret.NEF"));
        let second = std::panic::catch_unwind(|| panic!("later"));
        std::panic::set_hook(previous);
        assert!(first.is_err() && second.is_err());
        let note: PanicNote = read_json(&store.path(PANIC_NOTE)).unwrap();
        assert_eq!(note.pid, std::process::id());
        assert_eq!(note.message, "boom at /Users/anna/secret.NEF");
        assert!(note.location.starts_with("crash.rs:"), "{}", note.location);
        // The first frame is the code that panicked, not the machinery.
        let backtrace = note.backtrace.unwrap();
        let first = backtrace.lines().next().unwrap();
        assert!(
            first.contains("the_panic_hook_notes_the_first_panic"),
            "{backtrace}"
        );
        assert!(!backtrace.contains('/'), "{backtrace}");
        let record = store
            .start(&run(1, T0 + 9), |_| false, || None, &scrubber(), true)
            .unwrap()
            .unwrap();
        assert_eq!(record.message, "boom at <path>.NEF");
        assert!(record.location.starts_with("crash.rs:"));
    }

    /// A fatal signal in a child process leaves the signal note (the child
    /// is this test binary, run again with the variable set).
    #[cfg(unix)]
    #[test]
    fn a_fatal_signal_leaves_a_note() {
        if let Ok(dir) = std::env::var("RAWTONIC_CRASH_CHILD_DIR") {
            let store = CrashStore::new(dir);
            arm(&store);
            let on = std::env::var("RAWTONIC_CRASH_CHILD_RECORDING").is_ok();
            set_recording(&store, on);
            std::process::abort();
        }
        let (_dir, store) = store();
        std::fs::create_dir_all(store.dir()).unwrap();
        let child = |recording: bool| {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "crash::tests::a_fatal_signal_leaves_a_note",
                    "--nocapture",
                ])
                .env("RAWTONIC_CRASH_CHILD_DIR", store.dir());
            if recording {
                command.env("RAWTONIC_CRASH_CHILD_RECORDING", "1");
            }
            command.status().unwrap()
        };
        // Never: the abort leaves no note.
        assert!(!child(false).success());
        assert!(!store.path(SIGNAL_NOTE).exists());
        assert!(!child(true).success());
        assert_eq!(
            std::fs::read_to_string(store.path(SIGNAL_NOTE)).unwrap(),
            libc::SIGABRT.to_string()
        );
    }
}
