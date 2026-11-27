//! The monitor profile of each native display window (G23/G62, MCM1;
//! rawtonic-display-profile holds the lookups and the transform).
//!
//! On Windows each window's native display converts its frames into the
//! profile Windows assigns to the monitor the window is on (or the ICC
//! profile chosen in Preferences instead), read again whenever the window
//! moves, its display's scale changes or it gets the focus back (a profile
//! changed in Colour Management meanwhile). On macOS ColorSync matches the
//! windows to the display; on Linux the webview shows the previews (no
//! native display), so the profile is only looked up and named in
//! Preferences.

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
    /// with HDR or Auto colour management); "converted": the preview is
    /// converted into `profile`; "srgb": shown as sRGB (an sRGB monitor, no
    /// profile, or one that cannot be used: see `notice`); "notApplied":
    /// found, but not applied by Rawtonic on this system (Linux).
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
    /// Per window label: the newest lookup started; an older one that
    /// finishes later is dropped.
    lookups: HashMap<String, u64>,
}

/// What a window was last given: the profile behind its transform (None:
/// no transform) and its status.
type Shown = (Option<Vec<u8>>, MonitorStatus);

impl Applied {
    /// Starts a lookup for window `label`: its number.
    #[cfg_attr(not(windows), allow(dead_code))]
    fn start(&mut self, label: &str) -> u64 {
        let next = self.lookups.get(label).copied().unwrap_or(0) + 1;
        self.lookups.insert(label.to_string(), next);
        next
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
        override_available: cfg!(windows),
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
            let notice = "The chosen profile is not used while Windows manages this display's colours (HDR or Auto colour management).".to_string();
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

/// The profile chosen in Preferences: its path and bytes.
#[cfg_attr(not(windows), allow(dead_code))]
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
    #[cfg(not(windows))]
    let _ = (app, label);
}

/// Applies what [`resolve`] makes of `system` and `chosen` to window
/// `label`, unless a newer lookup than `lookup` was started meanwhile.
#[cfg_attr(not(windows), allow(dead_code))]
fn apply(
    app: &AppHandle,
    label: &str,
    lookup: u64,
    system: SystemProfile,
    chosen: Option<(String, Result<Vec<u8>, String>)>,
) {
    let (lut, icc, status) = resolve(system, chosen);
    // The profile behind the transform (None: no transform) and the status.
    let now = (icc.filter(|_| lut.is_some()), status.clone());
    // Held while installing, so lookups finishing together install in order.
    let mut applied = lock_recover(&APPLIED);
    let applied = applied.get_or_insert_with(Applied::default);
    if !applied.newest(label, lookup) || !applied.changed(label, now) {
        return;
    }
    let installed = set_transform(app, label, lut.as_ref());
    let status = shown_status(status, lut.is_some(), installed);
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

/// The status of a transform that `converts` (or not) once set on the
/// window: without a native display (the native renderer off) the webview
/// draws the preview, which Rawtonic does not convert.
fn shown_status(mut status: MonitorStatus, converts: bool, installed: bool) -> MonitorStatus {
    if converts && !installed {
        status.mode = "notApplied";
        status.notice = Some(
            "The native renderer is off (Preferences > Processing): the webview draws the preview, which Rawtonic does not convert into this profile."
                .into(),
        );
    }
    status
}

/// Sets `lut` on window `label`'s native display; whether it has one.
#[cfg_attr(not(windows), allow(dead_code))]
fn set_transform(
    app: &AppHandle,
    label: &str,
    lut: Option<&rawtonic_display_profile::DisplayLut>,
) -> bool {
    use tauri::Manager;
    if label == crate::second_window::LABEL {
        return crate::second_window::set_monitor_transform(app, lut);
    }
    let Some(context) = app.state::<crate::state::gpu::GpuState>().context() else {
        return false;
    };
    let mut display = lock_recover(&context.display);
    match display.as_mut() {
        Some(display) => {
            display.set_monitor_transform(&context.device, &context.queue, lut);
            true
        }
        None => false,
    }
}

/// The main window's status, for Preferences: as last applied (Windows),
/// or looked up now (Linux).
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

#[cfg(target_os = "linux")]
fn main_status() -> MonitorStatus {
    let system = rawtonic_display_profile::linux::profile_of_output(None);
    let (_, icc, mut status) = resolve(system, None);
    if icc.is_some() {
        status.mode = "notApplied";
        status.notice = Some(
            "The first display's profile (with several displays the window's may differ). On Linux the webview shows the preview as sRGB: Rawtonic does not convert it into this profile; the desktop may."
                .into(),
        );
    }
    status
}

#[cfg(windows)]
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
        assert_eq!(off.mode, "notApplied");
        assert!(off.notice.is_some());
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
