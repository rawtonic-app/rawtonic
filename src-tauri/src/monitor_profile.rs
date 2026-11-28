//! The monitor profile of each native display window (G23/G62, MCM1;
//! rawtonic-display-profile holds the lookups and the transform).
//!
//! On Windows each window's native display converts its frames into the
//! profile Windows assigns to the monitor the window is on (or the ICC
//! profile chosen in Preferences instead), read again whenever the window
//! moves, its display's scale changes or it gets the focus back (a profile
//! changed in Colour Management meanwhile). On macOS ColorSync matches the
//! windows to the display. On Linux the webview shows the previews and the
//! Library's thumbnails as device values, so the main window's transform
//! converts both before they reach it (`browser_colour.rs`), unless the
//! compositor colour-manages the window itself (KWin with a colour
//! profile): then nothing is converted, as on Windows with HDR or Auto
//! colour management. On Windows WebView2 shows the Library's thumbnails
//! through the system's profile itself; a profile chosen in Preferences
//! reaches them through [`webview_thumbnails`].

use std::collections::HashMap;
use std::sync::Mutex;

use rawtonic_display_profile::{ProfileError, SystemProfile};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::state::lock::lock_recover;

/// Sent with the main window's [`MonitorStatus`] whenever it changes.
const STATUS_EVENT: &str = "monitor-profile-changed";

/// How the preview's colours reach a window's monitor, for Preferences.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorStatus {
    /// "system": the OS matches the window to the display (macOS; Windows
    /// with HDR or Auto colour management; Linux with KWin managing it); "converted": the preview is
    /// converted into `profile`; "srgb": shown as sRGB (an sRGB monitor, no
    /// profile, or one that cannot be used: see `notice`); "rendererOff": `profile` would be used, but the native renderer is
    /// off, so the webview's preview is not converted.
    pub mode: &'static str,
    /// The profile's name, if one was found.
    pub profile: Option<String>,
    /// Where it was found (a path, colord, X11).
    pub location: Option<String>,
    /// Whether it is the profile chosen in Preferences.
    pub from_override: bool,
    /// Why a profile found is not used, or that none could be looked up.
    pub notice: Option<String>,
    /// Whether this system takes a profile chosen in Preferences.
    pub override_available: bool,
}

#[derive(Default)]
struct Applied {
    /// Per window label: the profile bytes last turned into its transform
    /// (None: no transform) and its status.
    windows: HashMap<String, Shown>,
    /// Per window label: the status as installed (what Preferences shows).
    reported: HashMap<String, MonitorStatus>,
    /// Per window label: what the last lookup found (the system's answer
    /// and the chosen profile), so an unchanged one builds nothing.
    inputs: HashMap<String, Inputs>,
    /// Per window label: the newest lookup started; an older one that
    /// finishes later is dropped.
    lookups: HashMap<String, u64>,
    /// Linux: why the newest lookup leaves the colours to the desktop.
    managed_notice: Option<String>,
}

/// What a lookup found: the system's answer and the chosen profile.
type Inputs = (SystemProfile, Option<(String, Result<Vec<u8>, String>)>);

/// What a window was last given: the profile behind its transform (None:
/// no transform) and its status.
type Shown = (Option<Vec<u8>>, MonitorStatus);

impl Applied {
    /// Starts a lookup for window `label`: its number.
    #[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
    fn start(&mut self, label: &str) -> u64 {
        let next = self.lookups.get(label).copied().unwrap_or(0) + 1;
        self.lookups.insert(label.to_string(), next);
        next
    }

    /// Whether lookup `lookup` for `label` has anything to do: it is the
    /// newest, and it found something other than what is installed. Only
    /// an installed result is noted ([`Applied::commit`]), so a lookup
    /// dropped for a newer one never makes that newer one skip its work.
    #[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
    fn fresh(&self, label: &str, lookup: u64, inputs: &Inputs) -> bool {
        self.newest(label, lookup) && self.inputs.get(label) != Some(inputs)
    }

    /// Lookup `lookup`'s result for `label` is being installed: whether it
    /// is still the newest (else it is dropped); notes what it found.
    fn commit(&mut self, label: &str, lookup: u64, inputs: Inputs) -> bool {
        if !self.newest(label, lookup) {
            return false;
        }
        self.inputs.insert(label.to_string(), inputs);
        true
    }

    /// Whether lookup `lookup` is still the newest for `label`.
    fn newest(&self, label: &str, lookup: u64) -> bool {
        self.lookups.get(label).copied().unwrap_or(0) == lookup
    }

    /// Notes `now` for window `label`; whether it differs from what the
    /// window was last given (so the transform is set again).
    fn changed(&mut self, label: &str, now: Shown) -> bool {
        if self.windows.get(label) == Some(&now) {
            return false;
        }
        self.windows.insert(label.to_string(), now);
        true
    }
}

static APPLIED: Mutex<Option<Applied>> = Mutex::new(None);

fn status_of(
    profile: Option<&[u8]>,
    location: Option<String>,
    from_override: bool,
    mode: &'static str,
    notice: Option<String>,
) -> MonitorStatus {
    MonitorStatus {
        mode,
        profile: profile.and_then(rawtonic_display_profile::description),
        location,
        from_override,
        notice,
        override_available: cfg!(any(windows, target_os = "linux")),
    }
}

/// What a window on a monitor with `system` (or the chosen `chosen`
/// profile: its path and bytes, or why it could not be read) shows: its
/// transform and status. A profile that cannot be used falls back to sRGB
/// with a notice, never to an error.
fn resolve(
    system: SystemProfile,
    chosen: Option<(String, Result<Vec<u8>, String>)>,
) -> (
    Option<rawtonic_display_profile::DisplayLut>,
    Option<Vec<u8>>,
    MonitorStatus,
) {
    let unusable = |name: &str, error: &ProfileError| {
        format!("{name} cannot be used ({error}); the preview is shown as sRGB.")
    };
    let (icc, location, from_override) = match (chosen, system) {
        // Windows converts sRGB windows itself (HDR, Auto colour
        // management): values converted here would be converted twice.
        (Some((path, _)), SystemProfile::ManagedBySystem) => {
            let notice = if cfg!(windows) {
                "The chosen profile is not used while Windows manages this display's colours (HDR or Auto colour management)."
            } else {
                "The chosen profile is not used while the desktop manages this display's colours (a colour profile set in its display settings)."
            }
            .to_string();
            return (
                None,
                None,
                status_of(None, Some(path), true, "system", Some(notice)),
            );
        }
        (Some((path, Ok(icc))), _) => (icc, path, true),
        (Some((path, Err(why))), _) => {
            let notice = format!(
                "The chosen profile {path} could not be read ({why}); the preview is shown as sRGB."
            );
            return (
                None,
                None,
                status_of(None, Some(path), true, "srgb", Some(notice)),
            );
        }
        (None, SystemProfile::ManagedBySystem) => {
            return (None, None, status_of(None, None, false, "system", None));
        }
        (None, SystemProfile::NoneAssigned) => {
            return (None, None, status_of(None, None, false, "srgb", None));
        }
        (None, SystemProfile::Unavailable(why)) => {
            let notice = format!(
                "The monitor profile could not be looked up ({why}); the preview is shown as sRGB."
            );
            return (
                None,
                None,
                status_of(None, None, false, "srgb", Some(notice)),
            );
        }
        (None, SystemProfile::Assigned { icc, location }) => (icc, location, false),
    };
    match rawtonic_display_profile::display_lut(rawtonic_display_profile::Encoding::Srgb, &icc) {
        Ok(Some(lut)) => {
            let status = status_of(Some(&icc), Some(location), from_override, "converted", None);
            (Some(lut), Some(icc), status)
        }
        Ok(None) => {
            let status = status_of(Some(&icc), Some(location), from_override, "srgb", None);
            (None, Some(icc), status)
        }
        Err(error) => {
            let name = rawtonic_display_profile::description(&icc).unwrap_or(location.clone());
            let notice = unusable(&name, &error);
            log::warn!("Monitor profile {location}: {error}");
            let status = status_of(
                Some(&icc),
                Some(location),
                from_override,
                "srgb",
                Some(notice),
            );
            (None, Some(icc), status)
        }
    }
}

/// The table for the Library's thumbnails in a webview that converts them
/// from sRGB into the system's profile itself (Windows, WebView2; notes:
/// benchmark-results/wqa8-2026-11-28 row 6), for the main window whose
/// [`resolve`]d status is `status`: only while a chosen profile is in use
/// (else None: the webview's own conversion is the system's, which is what
/// the window shows). With the system's profile unknown or unusable
/// nothing is converted, and the notice says so.
fn webview_thumbnails(
    system: &SystemProfile,
    chosen: Option<&(String, Result<Vec<u8>, String>)>,
    status: &MonitorStatus,
) -> (Option<rawtonic_display_profile::DisplayLut>, Option<String>) {
    // Not in use: none chosen, unreadable or unusable (its own notice), or
    // the system manages the display's colours.
    let Some((_, Ok(icc))) = chosen else {
        return (None, None);
    };
    if !status.from_override || status.mode == "system" || status.notice.is_some() {
        return (None, None);
    }
    let not_converted = |why: String| {
        Some(format!(
            "The Library's thumbnails are shown through the system's profile, not the chosen one: {why}."
        ))
    };
    let system_icc = match system {
        SystemProfile::Assigned { icc, .. } => Some(icc.as_slice()),
        SystemProfile::NoneAssigned => None,
        SystemProfile::Unavailable(why) => {
            return (
                None,
                not_converted(format!(
                    "the system's profile could not be looked up ({why})"
                )),
            );
        }
        SystemProfile::ManagedBySystem => return (None, None),
    };
    match rawtonic_display_profile::managed_webview_lut(icc, system_icc) {
        Ok(lut) => (lut, None),
        Err(error) => {
            let name = system_icc
                .and_then(rawtonic_display_profile::description)
                .unwrap_or_else(|| "the system's profile".into());
            log::warn!("Thumbnails for the chosen monitor profile: {error}");
            (
                None,
                not_converted(format!("{name} cannot be used ({error})")),
            )
        }
    }
}

/// The profile chosen in Preferences: its path and bytes.
#[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
fn chosen(app: &AppHandle) -> Option<(String, Result<Vec<u8>, String>)> {
    let path = crate::state::settings::current(app)
        .monitor_profile
        .clone()?;
    let read = std::fs::read(&path).map_err(|e| e.to_string());
    Some((path, read))
}

/// A native display was (re)made for window `label` (start, GPU recovery,
/// the second window opened): it gets its transform from scratch.
pub fn display_created(app: &AppHandle, label: &str) {
    if let Some(applied) = lock_recover(&APPLIED).as_mut() {
        applied.windows.remove(label);
        applied.reported.remove(label);
        applied.inputs.remove(label);
    }
    refresh(app, label);
}

/// Looks up the profile of the native display window `label` again and,
/// when it changed, converts that window's frames into it. Off the main
/// thread; a no-op on systems without a native display transform.
pub fn refresh(app: &AppHandle, label: &str) {
    #[cfg(windows)]
    {
        use tauri::Manager;
        let Some(window) = app.get_webview_window(label) else {
            return;
        };
        let Ok(hwnd) = window.hwnd() else { return };
        let hwnd = hwnd.0 as isize;
        let lookup = lock_recover(&APPLIED)
            .get_or_insert_with(Applied::default)
            .start(label);
        let (app, label) = (app.clone(), label.to_string());
        tauri::async_runtime::spawn_blocking(move || {
            let monitor = rawtonic_display_profile::windows::monitor_of(hwnd);
            let system = rawtonic_display_profile::windows::profile_of_monitor(monitor);
            apply(&app, &label, lookup, system, chosen(&app));
        });
    }
    #[cfg(target_os = "linux")]
    {
        use tauri::Manager;
        let Some(window) = app.get_webview_window(label) else {
            return;
        };
        let output = window_output(&window);
        let lookup = lock_recover(&APPLIED)
            .get_or_insert_with(Applied::default)
            .start(label);
        let (app, label) = (app.clone(), label.to_string());
        tauri::async_runtime::spawn_blocking(move || {
            let (system, name) = rawtonic_display_profile::linux::profile_of_window(&output);
            let managed = (system == SystemProfile::ManagedBySystem).then(|| {
                format!(
                    "The desktop (KWin) converts the colours of {} itself, from the colour profile set in System Settings > Display & Monitor: Rawtonic leaves the preview in sRGB so it is not converted twice.",
                    name.as_deref().unwrap_or("this display")
                )
            });
            lock_recover(&APPLIED)
                .get_or_insert_with(Applied::default)
                .managed_notice = managed;
            apply(&app, &label, lookup, system, chosen(&app));
        });
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    let _ = (app, label);
}

/// Wayland tells a window no move, and GTK 3 no output change when the
/// scale stays the same: every 2 s each window's monitor is read again, and
/// a window that is on another monitor (or whose desktop layout changed)
/// looks its profile up again.
#[cfg(target_os = "linux")]
pub fn watch_outputs(app: &AppHandle) {
    let app = app.clone();
    let spawned = std::thread::Builder::new()
        .name("monitor-output-watch".into())
        .spawn(move || {
            use tauri::Manager;
            let mut last: HashMap<String, rawtonic_display_profile::linux::WindowOutput> =
                HashMap::new();
            loop {
                std::thread::sleep(std::time::Duration::from_secs(2));
                for label in ["main", crate::second_window::LABEL] {
                    let Some(window) = app.get_webview_window(label) else {
                        last.remove(label);
                        continue;
                    };
                    // The first look too: the window may have moved before
                    // it (an unchanged answer builds nothing).
                    let now = window_output(&window);
                    if last.get(label) != Some(&now) {
                        refresh(&app, label);
                    }
                    last.insert(label.to_string(), now);
                }
            }
        });
    if let Err(error) = spawned {
        log::warn!("Monitor output watch not started: {error}");
    }
}

/// Where `window` is: its monitor's position (logical and device px) and
/// the logical position of every monitor. Read on the main thread (GTK).
#[cfg(target_os = "linux")]
fn window_output(window: &tauri::WebviewWindow) -> rawtonic_display_profile::linux::WindowOutput {
    let logical = |m: &tauri::Monitor| {
        let at = m.position().to_logical::<f64>(m.scale_factor());
        (at.x.round() as i32, at.y.round() as i32)
    };
    let rect = |m: &tauri::Monitor| {
        let size = m.size().to_logical::<f64>(m.scale_factor());
        (
            logical(m),
            (size.width.round() as i32, size.height.round() as i32),
        )
    };
    let current = window.current_monitor().ok().flatten();
    rawtonic_display_profile::linux::WindowOutput {
        logical: current.as_ref().map(logical),
        physical: current.as_ref().map(|m| (m.position().x, m.position().y)),
        monitors: window
            .available_monitors()
            .unwrap_or_default()
            .iter()
            .map(rect)
            .collect(),
    }
}

/// Applies what [`resolve`] makes of `system` and `chosen` to window
/// `label`, unless a newer lookup than `lookup` was started meanwhile.
#[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
fn apply(
    app: &AppHandle,
    label: &str,
    lookup: u64,
    system: SystemProfile,
    chosen: Option<(String, Result<Vec<u8>, String>)>,
) {
    // An unchanged answer (focus back, a move within one monitor) or an
    // older lookup builds nothing.
    let inputs = (system, chosen);
    if !lock_recover(&APPLIED)
        .get_or_insert_with(Applied::default)
        .fresh(label, lookup, &inputs)
    {
        return;
    }
    let (lut, icc, mut status) = resolve(inputs.0.clone(), inputs.1.clone());
    // Windows: the main window's thumbnails under a chosen profile (a
    // runtime check, so every platform's build type-checks it).
    let thumbnails = (cfg!(windows) && label == "main").then(|| {
        let (thumbnails, notice) = webview_thumbnails(&inputs.0, inputs.1.as_ref(), &status);
        if let Some(notice) = notice {
            status.notice = Some(match status.notice.take() {
                Some(earlier) => format!("{earlier} {notice}"),
                None => notice,
            });
        }
        thumbnails
    });
    // The profile behind the transform (None: no transform) and the status.
    let now = (icc.filter(|_| lut.is_some()), status.clone());
    // Taken before APPLIED: `attach_display` holds the context while it
    // notes a new display here (one lock order).
    let context = {
        use tauri::Manager;
        app.state::<crate::state::gpu::GpuState>().context()
    };
    // Held while installing, so lookups finishing together install in order.
    let mut applied = lock_recover(&APPLIED);
    let applied = applied.get_or_insert_with(Applied::default);
    if !applied.commit(label, lookup, inputs) {
        return;
    }
    // A new system profile under the same chosen one changes the
    // thumbnails' table, not the window's transform or status. Without a
    // native display (the native renderer off) Develop's preview is the
    // webview's too, shown through the system's profile: so are they.
    if let Some(thumbnails) = thumbnails.map(|t| t.filter(|_| has_native_display(context.as_ref())))
        && crate::browser_colour::set_thumbnails(thumbnails.as_ref())
    {
        log::info!(
            "Library thumbnails: {}",
            if thumbnails.is_some() {
                "converted for the chosen monitor profile"
            } else {
                "as the webview shows them"
            }
        );
        let _ = app.emit(
            crate::browser_colour::CHANGED_EVENT,
            crate::browser_colour::generation(),
        );
    }
    if !applied.changed(label, now) {
        return;
    }
    let installed = set_transform(app, context.as_ref(), label, lut.as_ref());
    let mut status = shown_status(status, lut.is_some(), installed);
    if status.mode == "system" && status.notice.is_none() {
        status.notice = applied.managed_notice.clone();
    }
    log::info!(
        "Monitor profile of window {label}: {} {:?} ({:?})",
        status.mode,
        status.profile,
        status.location
    );
    applied.reported.insert(label.to_string(), status.clone());
    if label == "main" {
        let _ = app.emit(STATUS_EVENT, &status);
    }
}

/// Whether the main window has a native display (Develop's photo is drawn
/// by the native present pass, not the webview).
fn has_native_display(context: Option<&crate::gpu_processing::GpuContext>) -> bool {
    context.is_some_and(|context| lock_recover(&context.display).is_some())
}

/// The status of a transform that `converts` (or not) once set on the
/// window: without a native display (the native renderer off) the webview
/// draws the preview, which Rawtonic does not convert.
fn shown_status(mut status: MonitorStatus, converts: bool, installed: bool) -> MonitorStatus {
    if converts && !installed {
        status.mode = "rendererOff";
        status.notice = Some(
            "The native renderer is off (Preferences > Processing): the webview draws the preview, which Rawtonic does not convert into this profile."
                .into(),
        );
    }
    status
}

/// Sets `lut` on window `label`'s native display; whether it has one.
#[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
fn set_transform(
    app: &AppHandle,
    context: Option<&crate::gpu_processing::GpuContext>,
    label: &str,
    lut: Option<&rawtonic_display_profile::DisplayLut>,
) -> bool {
    // Linux has no native display: the webview is the display; its frames
    // (and the main window's thumbnails) are converted (browser_colour.rs).
    // A change renders the main window's frame again, which the second
    // window mirrors.
    #[cfg(target_os = "linux")]
    {
        if crate::browser_colour::set(label, lut) {
            let _ = app.emit(
                crate::browser_colour::CHANGED_EVENT,
                crate::browser_colour::generation(),
            );
            // Its own frames (a locked photograph, a 100 % region) too.
            if label == crate::second_window::LABEL
                && let Some(context) = context
            {
                crate::second_window::develop_again(app, context);
            }
        }
        true
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = app;
        set_native_transform(context, label, lut)
    }
}

/// Sets `lut` on window `label`'s native display; whether it has one.
#[cfg(not(target_os = "linux"))]
fn set_native_transform(
    context: Option<&crate::gpu_processing::GpuContext>,
    label: &str,
    lut: Option<&rawtonic_display_profile::DisplayLut>,
) -> bool {
    let Some(context) = context else {
        return false;
    };
    if label == crate::second_window::LABEL {
        return crate::second_window::set_monitor_transform(context, lut);
    }
    let mut display = lock_recover(&context.display);
    match display.as_mut() {
        Some(display) => {
            display.set_monitor_transform(&context.device, &context.queue, lut);
            true
        }
        None => false,
    }
}

/// The main window's status, for Preferences: as last applied.
#[tauri::command]
pub async fn monitor_profile_status() -> Result<MonitorStatus, String> {
    tauri::async_runtime::spawn_blocking(main_status)
        .await
        .map_err(|e| e.to_string())
}

#[cfg(target_os = "macos")]
fn main_status() -> MonitorStatus {
    status_of(None, None, false, "system", None)
}

#[cfg(any(windows, target_os = "linux"))]
fn main_status() -> MonitorStatus {
    lock_recover(&APPLIED)
        .as_ref()
        .and_then(|applied| applied.reported.get("main"))
        .cloned()
        .unwrap_or_else(|| status_of(None, None, false, "srgb", None))
}

/// Chooses the ICC profile at `path` for the preview instead of the
/// system's (None: the system's again). A profile that cannot be used is
/// refused with the reason, and nothing changes.
#[tauri::command]
pub async fn monitor_profile_choose(
    app_handle: AppHandle,
    path: Option<String>,
) -> Result<MonitorStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        if let Some(path) = &path {
            let icc = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
            rawtonic_display_profile::display_lut(rawtonic_display_profile::Encoding::Srgb, &icc)
                .map_err(|e| e.to_string())?;
        }
        crate::app_settings::persist_monitor_profile(&app_handle, path)?;
        for label in ["main", crate::second_window::LABEL] {
            refresh(&app_handle, label);
        }
        Ok::<_, String>(())
    })
    .await
    .map_err(|e| e.to_string())??;
    monitor_profile_status().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use moxcms::ColorProfile;

    fn p3() -> Vec<u8> {
        ColorProfile::new_display_p3().encode().unwrap()
    }

    #[test]
    fn a_profile_is_applied_again_only_when_it_changed() {
        let mut applied = Applied::default();
        let shown = |icc: Vec<u8>| {
            let (lut, icc, status) = resolve(
                SystemProfile::Assigned {
                    icc,
                    location: "p.icc".into(),
                },
                None,
            );
            (icc.filter(|_| lut.is_some()), status)
        };
        assert!(applied.changed("main", shown(p3())));
        // Focus back, the same monitor and profile: nothing is redone.
        assert!(!applied.changed("main", shown(p3())));
        // Moved to a monitor with another profile, or the profile changed.
        let adobe = ColorProfile::new_adobe_rgb().encode().unwrap();
        assert!(applied.changed("main", shown(adobe.clone())));
        // Each window keeps its own.
        assert!(applied.changed("second-view", shown(adobe)));
        assert!(applied.changed("main", shown(p3())));
        // A new display (display_created) is given its transform afresh.
        applied.windows.remove("main");
        assert!(applied.changed("main", shown(p3())));
    }

    #[test]
    fn a_chosen_profile_waits_while_windows_manages_colour() {
        let (lut, _, status) = resolve(
            SystemProfile::ManagedBySystem,
            Some(("/x/p3.icc".into(), Ok(p3()))),
        );
        assert!(lut.is_none());
        assert_eq!(status.mode, "system");
        assert!(status.from_override && status.notice.is_some());
    }

    #[test]
    fn a_transform_without_a_native_display_is_reported_as_not_applied() {
        let (_, _, status) = resolve(
            SystemProfile::Assigned {
                icc: p3(),
                location: "p3.icc".into(),
            },
            None,
        );
        assert_eq!(shown_status(status.clone(), true, true).mode, "converted");
        let off = shown_status(status, true, false);
        assert_eq!(off.mode, "rendererOff");
        assert!(off.notice.is_some());
    }

    fn found(icc: Vec<u8>) -> Inputs {
        (
            SystemProfile::Assigned {
                icc,
                location: "p.icc".into(),
            },
            None,
        )
    }

    #[test]
    fn an_unchanged_lookup_builds_nothing() {
        let mut applied = Applied::default();
        let first = applied.start("main");
        assert!(applied.fresh("main", first, &found(p3())));
        assert!(applied.commit("main", first, found(p3())));
        let again = applied.start("main");
        assert!(!applied.fresh("main", again, &found(p3())));
        let other = applied.start("main");
        let adobe = ColorProfile::new_adobe_rgb().encode().unwrap();
        assert!(applied.fresh("main", other, &found(adobe)));
    }

    #[test]
    fn a_lookup_dropped_mid_drag_never_hides_the_new_monitor() {
        // On the P3 monitor; dragged onto an Adobe RGB one: the first
        // lookup there is overtaken by the next move while it builds.
        let mut applied = Applied::default();
        let on_p3 = applied.start("main");
        assert!(applied.commit("main", on_p3, found(p3())));
        let adobe = ColorProfile::new_adobe_rgb().encode().unwrap();
        let first = applied.start("main");
        assert!(applied.fresh("main", first, &found(adobe.clone())));
        let second = applied.start("main");
        // The second still has the new monitor's profile to install…
        assert!(applied.fresh("main", second, &found(adobe.clone())));
        // …and the first, finishing later, is dropped.
        assert!(!applied.commit("main", first, found(adobe.clone())));
        assert!(applied.commit("main", second, found(adobe.clone())));
        let focus = applied.start("main");
        assert!(!applied.fresh("main", focus, &found(adobe)));
    }

    #[test]
    fn an_older_lookup_finishing_late_is_dropped() {
        let mut applied = Applied::default();
        let first = applied.start("main");
        let second = applied.start("main");
        assert!(!applied.newest("main", first));
        assert!(applied.newest("main", second));
        assert!(applied.newest("second-view", 0));
    }

    #[test]
    fn a_wide_gamut_profile_converts() {
        let (lut, icc, status) = resolve(
            SystemProfile::Assigned {
                icc: p3(),
                location: "C:\\p3.icm".into(),
            },
            None,
        );
        assert!(lut.is_some());
        assert_eq!(icc, Some(p3()));
        assert_eq!(status.mode, "converted");
        assert!(!status.from_override);
    }

    #[test]
    fn the_chosen_profile_wins_over_the_system() {
        let (lut, _, status) = resolve(
            SystemProfile::NoneAssigned,
            Some(("/x/p3.icc".into(), Ok(p3()))),
        );
        assert!(lut.is_some());
        assert!(status.from_override);
        assert_eq!(status.location.as_deref(), Some("/x/p3.icc"));
    }

    #[test]
    fn unusable_or_missing_profiles_show_srgb_with_a_notice() {
        let gray = ColorProfile::new_gray_with_gamma(2.2).encode().unwrap();
        for (system, chosen) in [
            (
                SystemProfile::Assigned {
                    icc: gray,
                    location: "gray.icc".into(),
                },
                None,
            ),
            (
                SystemProfile::Assigned {
                    icc: b"junk".to_vec(),
                    location: "junk.icc".into(),
                },
                None,
            ),
            (SystemProfile::Unavailable("no colord".into()), None),
            (
                SystemProfile::NoneAssigned,
                Some(("gone.icc".into(), Err("not found".into()))),
            ),
        ] {
            let (lut, _, status) = resolve(system, chosen);
            assert!(lut.is_none());
            assert_eq!(status.mode, "srgb");
            assert!(status.notice.is_some(), "{status:?}");
        }
    }

    /// The thumbnails' table and notice for `system` under `chosen`, as
    /// `apply` makes them.
    fn thumbnails(
        system: SystemProfile,
        chosen: Option<(String, Result<Vec<u8>, String>)>,
    ) -> (Option<rawtonic_display_profile::DisplayLut>, Option<String>) {
        let (_, _, status) = resolve(system.clone(), chosen.clone());
        webview_thumbnails(&system, chosen.as_ref(), &status)
    }

    #[test]
    fn thumbnails_follow_a_chosen_profile_only() {
        let adobe = ColorProfile::new_adobe_rgb().encode().unwrap();
        let system = || SystemProfile::Assigned {
            icc: p3(),
            location: "C:\\panel.icm".into(),
        };
        let chosen = |icc: &[u8]| Some(("D:\\wide.icc".to_string(), Ok(icc.to_vec())));
        // No choice: the webview's own conversion, nothing here.
        assert_eq!(thumbnails(system(), None), (None, None));
        // The system's own profile chosen: nothing either.
        assert_eq!(thumbnails(system(), chosen(&p3())), (None, None));
        // Another one: the table that ends at its values.
        let (lut, notice) = thumbnails(system(), chosen(&adobe));
        assert_eq!(
            lut,
            rawtonic_display_profile::managed_webview_lut(&adobe, Some(&p3())).unwrap()
        );
        assert!(lut.is_some() && notice.is_none());
        let (lut, _) = thumbnails(SystemProfile::NoneAssigned, chosen(&adobe));
        assert!(lut.is_some());
        // Windows manages the display, or the chosen file is unusable:
        // nothing, the status already says why.
        assert_eq!(
            thumbnails(SystemProfile::ManagedBySystem, chosen(&adobe)),
            (None, None)
        );
        assert_eq!(thumbnails(system(), chosen(b"junk")), (None, None));
        assert_eq!(
            thumbnails(system(), Some(("gone.icc".into(), Err("not found".into())))),
            (None, None)
        );
    }

    #[test]
    fn thumbnails_stay_as_shown_when_the_system_profile_is_unknown() {
        let adobe = ColorProfile::new_adobe_rgb().encode().unwrap();
        let chosen = Some(("wide.icc".to_string(), Ok(adobe)));
        for system in [
            SystemProfile::Unavailable("no device".into()),
            SystemProfile::Assigned {
                icc: b"junk".to_vec(),
                location: "C:\\junk.icm".into(),
            },
        ] {
            let (lut, notice) = thumbnails(system, chosen.clone());
            assert!(lut.is_none());
            assert!(notice.is_some_and(|n| n.contains("thumbnails")), "a notice");
        }
    }

    #[test]
    fn an_srgb_monitor_or_none_is_no_transform_and_no_notice() {
        let srgb = ColorProfile::new_srgb().encode().unwrap();
        for system in [
            SystemProfile::Assigned {
                icc: srgb,
                location: "sRGB Color Space Profile.icm".into(),
            },
            SystemProfile::NoneAssigned,
        ] {
            let (lut, _, status) = resolve(system, None);
            assert!(lut.is_none());
            assert_eq!(status.mode, "srgb");
            assert_eq!(status.notice, None);
        }
        let (lut, _, status) = resolve(SystemProfile::ManagedBySystem, None);
        assert!(lut.is_none());
        assert_eq!(status.mode, "system");
    }
}
