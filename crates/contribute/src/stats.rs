// SPDX-License-Identifier: AGPL-3.0-or-later
//! Anonymous usage statistics (ADR 0017, channel 4): off unless the user
//! switched them on. While on, the app keeps per ISO week (UTC) only which
//! camera makes/models were opened (each once), which tools from a fixed
//! list were used at all, and the app, OS and GPU vendor of that week. At
//! the first start of a later week each finished week the relay still takes
//! (up to two weeks after its end) is sent once; older ones are dropped.
//! Nothing is kept while off, and switching off deletes what was kept, so
//! the next send does not happen. No id, no counts, no times.

use crate::crash::{AppInfo, plain_name};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// The tools counted as used / not used (services/relay/src/schema.ts).
pub const TOOLS: &[&str] = &[
    "basic",
    "curves",
    "color",
    "colorGrading",
    "sharpen",
    "noiseReduction",
    "aiDenoise",
    "lensCorrection",
    "transform",
    "crop",
    "masks",
    "aiMasks",
    "retouch",
    "redEye",
    "relight",
    "generative",
    "presets",
    "copySettings",
    "snapshots",
    "merge",
    "export",
    "batchExport",
    "tether",
    "ingest",
    "search",
    "culling",
];

/// GPU vendors the relay takes.
const GPU_VENDORS: &[&str] = &["apple", "amd", "intel", "nvidia", "qualcomm", "other"];
/// Cameras per week the relay takes.
pub const MAX_CAMERAS: usize = 50;
/// How long after its end the relay takes a week.
pub const ACCEPT_DAYS: u64 = 14;
const DAY: u64 = 86_400;

/// Days since 1970-01-01 of a civil date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The Monday (days since the epoch) that starts ISO week 1 of `year`: the
/// week holding January 4th.
fn week_one(year: i64) -> i64 {
    let jan4 = days_from_civil(year, 1, 4);
    // 1970-01-01 was a Thursday: weekday 0 = Monday.
    let weekday = (jan4 + 3).rem_euclid(7);
    jan4 - weekday
}

/// The ISO week (UTC) of a Unix time, `YYYY-Www`.
pub fn iso_week(secs: u64) -> String {
    let days = (secs / DAY) as i64;
    let year = {
        // The ISO year is the civil year of this week's Thursday.
        let thursday = days - (days + 3).rem_euclid(7) + 3;
        let mut year = 1970 + thursday.div_euclid(365);
        while days_from_civil(year, 1, 1) > thursday {
            year -= 1;
        }
        while days_from_civil(year + 1, 1, 1) <= thursday {
            year += 1;
        }
        year
    };
    let week = (days - week_one(year)).div_euclid(7) + 1;
    format!("{year:04}-W{week:02}")
}

/// When `week` ends (the next Monday 00:00 UTC, Unix seconds), or None for
/// a week that does not exist.
pub fn week_end(week: &str) -> Option<u64> {
    let (year, number) = week.split_once("-W")?;
    if year.len() != 4 || number.len() != 2 {
        return None;
    }
    let year: i64 = year.parse().ok()?;
    let number: i64 = number.parse().ok()?;
    if number < 1 {
        return None;
    }
    let start = week_one(year) + (number - 1) * 7;
    let start_secs = u64::try_from(start).ok()? * DAY;
    (iso_week(start_secs) == week).then_some(start_secs + 7 * DAY)
}

/// Whether the relay takes `week` at `now`: ended, and not long ago.
pub fn week_open(week: &str, now: u64) -> bool {
    week_end(week).is_some_and(|end| end <= now && now - end <= ACCEPT_DAYS * DAY)
}

/// What one week kept.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Week {
    /// The app as last seen that week.
    pub app: Option<AppInfo>,
    pub gpu_vendor: Option<String>,
    /// Camera (make, model) pairs opened, each once.
    pub cameras: BTreeSet<(String, String)>,
    pub tools: BTreeSet<String>,
}

/// A body that was sent, kept for "Show what is sent".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sent {
    pub week: String,
    pub body: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct File {
    #[serde(default)]
    weeks: BTreeMap<String, Week>,
    #[serde(default)]
    last_sent: Option<Sent>,
}

/// The `POST /stats` payload (schema 1) of a week.
pub fn payload(week: &str, kept: &Week, app: &AppInfo) -> Value {
    let app = kept.app.as_ref().unwrap_or(app);
    let gpu = kept
        .gpu_vendor
        .as_deref()
        .filter(|v| GPU_VENDORS.contains(v))
        .unwrap_or("other");
    let cameras: Vec<Value> = kept
        .cameras
        .iter()
        .take(MAX_CAMERAS)
        .map(|(make, model)| json!({"make": make, "model": model}))
        .collect();
    let tools: serde_json::Map<String, Value> = kept
        .tools
        .iter()
        .filter(|t| TOOLS.contains(&t.as_str()))
        .map(|t| (t.clone(), Value::Bool(true)))
        .collect();
    json!({
        "schema": 1,
        "week": week,
        "app": app,
        "gpuVendor": gpu,
        "cameras": cameras,
        "tools": tools,
    })
}

/// The weekly counters on disk (one JSON file, written atomically).
pub struct StatsStore {
    path: PathBuf,
    on: AtomicBool,
    /// Counts the switches off: work started before one is void, even when
    /// statistics are switched on again meanwhile.
    consent: AtomicU64,
    lock: Mutex<()>,
    /// This run's app and GPU vendor, known also while off: every week
    /// recorded later starts with them.
    seen: Mutex<(Option<AppInfo>, Option<String>)>,
}

impl StatsStore {
    pub fn new(path: impl Into<PathBuf>, on: bool) -> Self {
        let store = Self {
            path: path.into(),
            on: AtomicBool::new(on),
            consent: AtomicU64::new(0),
            lock: Mutex::new(()),
            seen: Mutex::new((None, None)),
        };
        if !on {
            let _ = store.forget_weeks();
        }
        store
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn is_on(&self) -> bool {
        self.on.load(Ordering::SeqCst)
    }

    /// The consent a send starts under, if statistics are on.
    pub fn ticket(&self) -> Option<u64> {
        self.is_on().then(|| self.consent.load(Ordering::SeqCst))
    }

    /// Whether the consent of `ticket` still holds: on, and not switched
    /// off since (also not off and on again).
    pub fn still(&self, ticket: u64) -> bool {
        self.is_on() && self.consent.load(Ordering::SeqCst) == ticket
    }

    fn read(&self) -> File {
        std::fs::read(&self.path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn write(&self, file: &File) -> Result<(), String> {
        let result = serde_json::to_vec_pretty(file)
            .map_err(|e| e.to_string())
            .and_then(|bytes| {
                rawtonic_metadata::atomic_write(&self.path, bytes).map_err(|e| e.to_string())
            });
        if let Err(e) = &result {
            log::warn!("Usage statistics not saved: {e}");
        }
        result
    }

    /// Changes this week's record while on; writes only when it changed.
    fn update(&self, now: u64, change: impl FnOnce(&mut Week)) {
        if !self.is_on() {
            return;
        }
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        // Switched off while waiting for the lock: nothing is kept.
        if !self.is_on() {
            return;
        }
        let mut file = self.read();
        let week = file.weeks.entry(iso_week(now)).or_default();
        let before = week.clone();
        {
            let seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
            if week.app.is_none() {
                week.app = seen.0.clone();
            }
            if week.gpu_vendor.is_none() {
                week.gpu_vendor = seen.1.clone();
            }
        }
        change(week);
        if *week != before || !self.path.exists() {
            let _ = self.write(&file);
        }
    }

    /// This run used the app this week (also with no photo opened).
    pub fn note_run(&self, now: u64, app: &AppInfo) {
        self.seen.lock().unwrap_or_else(|e| e.into_inner()).0 = Some(app.clone());
        self.update(now, |week| week.app = Some(app.clone()));
    }

    pub fn note_gpu_vendor(&self, now: u64, vendor: &str) {
        self.seen.lock().unwrap_or_else(|e| e.into_inner()).1 = Some(vendor.to_string());
        self.update(now, |week| week.gpu_vendor = Some(vendor.to_string()));
    }

    /// A photo of this camera was opened (names as the camera library
    /// reports them, reduced to what the relay takes).
    pub fn note_camera(&self, now: u64, make: &str, model: &str) {
        let (Some(make), Some(model)) = (plain_name(make, 64), plain_name(model, 64)) else {
            return;
        };
        self.update(now, |week| {
            if week.cameras.len() < MAX_CAMERAS {
                week.cameras.insert((make, model));
            }
        });
    }

    /// Tools from [`TOOLS`] used; other names are ignored.
    pub fn note_tools(&self, now: u64, tools: &[String]) {
        let known: Vec<&String> = tools
            .iter()
            .filter(|t| TOOLS.contains(&t.as_str()))
            .collect();
        if known.is_empty() {
            return;
        }
        self.update(now, |week| {
            for tool in known {
                week.tools.insert(tool.clone());
            }
        });
    }

    /// Switches recording on or off at once. Off stops at once and deletes
    /// the statistics file — every kept week and the last sent body; an
    /// error says it could not be deleted. On starts from no file — nothing
    /// from before a switch-off is ever sent — and stays off when an old file
    /// cannot be deleted.
    pub fn set_on(&self, on: bool) -> Result<(), String> {
        if on {
            self.forget_weeks()?;
            self.on.store(true, Ordering::SeqCst);
            Ok(())
        } else {
            self.consent.fetch_add(1, Ordering::SeqCst);
            self.on.store(false, Ordering::SeqCst);
            self.forget_weeks()
        }
    }

    /// Deletes the statistics file (the app's own, in its config folder).
    fn forget_weeks(&self) -> Result<(), String> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => {
                log::warn!("Usage statistics not deleted: {e}");
                Err(e.to_string())
            }
        }
    }

    pub fn last_sent(&self) -> Option<Sent> {
        self.read().last_sent
    }

    /// Every body kept to be sent, in send order: the finished weeks the
    /// relay still takes (sent at the next start), then this week so far.
    pub fn pending(&self, now: u64, app: &AppInfo) -> Vec<(String, Value)> {
        let current = iso_week(now);
        let mut weeks: Vec<(String, Value)> = self
            .read()
            .weeks
            .iter()
            .filter(|(week, _)| **week == current || week_open(week, now))
            .map(|(week, kept)| (week.clone(), payload(week, kept, app)))
            .collect();
        // BTreeMap order is week order; the current week is the newest.
        weeks.sort_by(|a, b| a.0.cmp(&b.0));
        weeks
    }

    /// The finished weeks to send now, oldest first; weeks the relay no
    /// longer takes are dropped. Empty while off.
    pub fn due(&self, now: u64) -> Vec<String> {
        if !self.is_on() {
            return Vec::new();
        }
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut file = self.read();
        let current = iso_week(now);
        let before = file.weeks.len();
        file.weeks
            .retain(|week, _| *week == current || week_open(week, now));
        if file.weeks.len() != before {
            let _ = self.write(&file);
        }
        file.weeks
            .keys()
            .filter(|week| **week != current)
            .cloned()
            .collect()
    }

    /// Takes `week` out to send it: from here it is not kept, so a send
    /// whose outcome is unknown is never repeated (at most once).
    /// None when its removal could not be saved: then it is not sent.
    pub fn take(&self, week: &str) -> Option<Week> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut file = self.read();
        let kept = file.weeks.remove(week)?;
        self.write(&file).ok()?;
        Some(kept)
    }

    /// A taken week that certainly did not arrive (no connection, or the
    /// relay's limit), kept again for the next start — unless statistics
    /// were switched off meanwhile.
    pub fn put_back(&self, week: &str, kept: Week, ticket: u64) {
        if !self.still(ticket) {
            return;
        }
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        if !self.still(ticket) {
            return;
        }
        let mut file = self.read();
        file.weeks.entry(week.to_string()).or_insert(kept);
        let _ = self.write(&file);
    }

    /// `body` of `week` was sent under `ticket`: kept for "Show what is
    /// sent" — unless statistics were switched off since (then nothing of
    /// it is written back).
    pub fn sent(&self, week: &str, body: String, ticket: u64) {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        if !self.still(ticket) {
            return;
        }
        let mut file = self.read();
        file.last_sent = Some(Sent {
            week: week.to_string(),
            body,
        });
        let _ = self.write(&file);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> AppInfo {
        AppInfo {
            version: "0.1.0-alpha.4".into(),
            os: "macos".into(),
            os_version: "27.0".into(),
            arch: "aarch64".into(),
        }
    }

    /// 2026-10-21 12:00 UTC, a Wednesday of week 43.
    const WED: u64 = 1_792_584_000;

    #[test]
    fn tools_are_the_relays() {
        let schema = include_str!("../../../services/relay/src/schema.ts");
        let list = schema
            .split("export const TOOLS = [")
            .nth(1)
            .and_then(|rest| rest.split(']').next())
            .unwrap();
        let relay: Vec<&str> = list
            .split(',')
            .map(|item| item.trim().trim_matches('"'))
            .filter(|item| !item.is_empty())
            .collect();
        assert_eq!(relay, TOOLS);
    }

    #[test]
    fn iso_weeks_match_the_calendar() {
        assert_eq!(iso_week(WED), "2026-W43");
        // 2021-01-03 (Sunday) belongs to 2020's week 53; 2021-01-04 starts W01.
        assert_eq!(iso_week(1_609_675_200), "2020-W53");
        assert_eq!(iso_week(1_609_718_400), "2021-W01");
        // 2024-12-30 (Monday) is in 2025's week 1.
        assert_eq!(iso_week(1_735_560_000), "2025-W01");
        assert_eq!(week_end("2026-W43"), Some(1_792_972_800)); // Mon 2026-10-26
        assert_eq!(week_end("2020-W53"), Some(1_609_718_400));
        assert_eq!(week_end("2021-W53"), None);
        assert_eq!(week_end("2026-W00"), None);
        for secs in (0..3_000).map(|d| 1_600_000_000 + d * 86_400 / 3) {
            let week = iso_week(secs);
            let end = week_end(&week).unwrap();
            assert!(end - 7 * DAY <= secs && secs < end, "{week} {secs}");
        }
    }

    #[test]
    fn keeps_nothing_while_off_and_forgets_when_switched_off() {
        let dir = tempfile::tempdir().unwrap();
        let store = StatsStore::new(dir.path().join("stats.json"), false);
        store.note_camera(WED, "SONY", "ILCE-7M3");
        assert!(!store.path().exists());
        store.set_on(true).unwrap();
        store.note_camera(WED, "SONY", "ILCE-7M3");
        assert_eq!(store.pending(WED, &app()).len(), 1);
        store.sent("2026-W42", "{}".into(), store.ticket().unwrap());
        store.set_on(false).unwrap();
        // Everything goes, also the last sent body: no file is left.
        assert!(!store.path().exists());
        assert!(store.pending(WED, &app()).is_empty());
        assert_eq!(store.last_sent(), None);
        assert!(store.due(WED + 7 * DAY).is_empty());
        // Off at start (a preference saved off): the weeks are gone too.
        let on = StatsStore::new(dir.path().join("b.json"), true);
        on.note_tools(WED, &["curves".into()]);
        let off = StatsStore::new(dir.path().join("b.json"), false);
        off.set_on(true).unwrap();
        assert!(off.pending(WED, &app()).is_empty());
    }

    #[test]
    fn a_week_is_its_cameras_once_and_its_tools_as_yes() {
        let dir = tempfile::tempdir().unwrap();
        let store = StatsStore::new(dir.path().join("stats.json"), true);
        store.note_run(WED, &app());
        store.note_gpu_vendor(WED, "apple");
        for _ in 0..3 {
            store.note_camera(WED, "NIKON CORPORATION", "NIKON Z 6");
        }
        store.note_camera(WED, "SONY", "ILCE-7M3<script>");
        store.note_tools(
            WED,
            &[
                "curves".into(),
                "masks".into(),
                "curves".into(),
                "wat".into(),
            ],
        );
        let body = store.pending(WED, &app()).pop().unwrap().1;
        assert_eq!(
            body,
            json!({
                "schema": 1,
                "week": "2026-W43",
                "app": {"version": "0.1.0-alpha.4", "os": "macos", "osVersion": "27.0", "arch": "aarch64"},
                "gpuVendor": "apple",
                "cameras": [
                    {"make": "NIKON CORPORATION", "model": "NIKON Z 6"},
                    {"make": "SONY", "model": "ILCE-7M3script"}
                ],
                "tools": {"curves": true, "masks": true}
            })
        );
    }

    #[test]
    fn sends_finished_weeks_the_relay_still_takes_at_most_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = StatsStore::new(dir.path().join("stats.json"), true);
        store.note_run(WED - 14 * DAY, &app()); // W41: ended 2026-10-12, open until 10-26
        store.note_run(WED - 7 * DAY, &app()); // W42
        store.note_run(WED, &app()); // W43, the current week
        assert_eq!(store.due(WED), ["2026-W41", "2026-W42"]);
        let later = WED + 5 * DAY; // Monday 2026-10-26, W44
        // The preview lists every body that will leave, in send order.
        let pending: Vec<String> = store
            .pending(later, &app())
            .into_iter()
            .map(|(w, _)| w)
            .collect();
        assert_eq!(pending, ["2026-W42", "2026-W43"]);
        assert_eq!(store.due(later), ["2026-W42", "2026-W43"]);
        // Taken to send: not kept any more, whatever the outcome.
        let ticket = store.ticket().unwrap();
        assert!(store.take("2026-W42").is_some());
        assert_eq!(store.take("2026-W42"), None);
        store.sent("2026-W42", "{}".into(), ticket);
        assert_eq!(store.due(later), ["2026-W43"]);
        assert_eq!(store.last_sent().unwrap().week, "2026-W42");
        // Not reached: kept again, unless switched off meanwhile — also
        // off and on again during the send.
        let w43 = store.take("2026-W43").unwrap();
        store.put_back("2026-W43", w43.clone(), ticket);
        assert_eq!(store.due(later), ["2026-W43"]);
        let w43 = store.take("2026-W43").unwrap();
        store.set_on(false).unwrap();
        store.set_on(true).unwrap();
        assert!(!store.still(ticket));
        store.put_back("2026-W43", w43, ticket);
        assert!(store.due(later).is_empty());
        // A send that succeeds after a switch-off writes nothing back.
        store.sent("2026-W43", "{}".into(), ticket);
        assert!(!store.path().exists());
        assert_eq!(store.last_sent(), None);
        // Unsent past the relay's window: dropped, not sent late.
        store.note_run(later, &app());
        assert!(store.due(later + 30 * DAY).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn weeks_that_could_not_be_deleted_on_opt_out_never_become_due() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let store = StatsStore::new(dir.path().join("stats.json"), true);
        store.note_run(WED, &app());
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
        let off = store.set_on(false);
        let on = store.set_on(true);
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        if off.is_err() {
            // Reported, and switching on again could not clear them either:
            // it stays off.
            assert!(on.is_err());
            assert!(!store.is_on());
            assert!(store.due(WED + 7 * DAY).is_empty());
        }
        // Switched on once the folder is writable: it starts empty.
        store.set_on(true).unwrap();
        assert!(store.due(WED + 7 * DAY).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_week_whose_removal_cannot_be_saved_is_not_sent() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let store = StatsStore::new(dir.path().join("stats.json"), true);
        store.note_run(WED, &app());
        let later = WED + 7 * DAY;
        assert_eq!(store.due(later), ["2026-W43"]);
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
        let taken = store.take("2026-W43");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        // Root may write anyway; then the removal was saved.
        if taken.is_none() {
            assert_eq!(store.due(later), ["2026-W43"]);
        } else {
            assert!(store.due(later).is_empty());
        }
    }

    #[test]
    fn a_week_recorded_after_the_gpu_was_known_names_its_vendor() {
        let dir = tempfile::tempdir().unwrap();
        let store = StatsStore::new(dir.path().join("stats.json"), false);
        store.note_run(WED, &app());
        store.note_gpu_vendor(WED, "apple");
        store.set_on(true).unwrap();
        store.note_tools(WED, &["export".into()]);
        let body = store.pending(WED, &app()).pop().unwrap().1;
        assert_eq!(body["gpuVendor"], "apple");
        assert_eq!(body["app"]["version"], "0.1.0-alpha.4");
        // A new week in a long session starts with them too.
        store.note_tools(WED + 7 * DAY, &["crop".into()]);
        assert_eq!(
            store.pending(WED + 7 * DAY, &app()).pop().unwrap().1["gpuVendor"],
            "apple"
        );
    }
}
