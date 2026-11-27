//! The profile a Linux desktop assigns to the display a window is on:
//!
//! - KWin on Wayland (KDE Plasma 6) colour-manages windows itself when the
//!   display has an ICC or EDID colour profile or HDR on ([`crate::kwin`]):
//!   the preview then stays sRGB ([`SystemProfile::ManagedBySystem`]);
//! - else colord's default profile for that display device (GNOME and
//!   colord-aware X11 desktops store their choice there);
//! - else the ICC profile an X11 server publishes for it (`_ICC_PROFILE`
//!   for the first monitor, `_ICC_PROFILE_n` for monitor n, the X Color
//!   Management convention).
//!
//! The window's display is found by its monitor's position: on X11 among
//! XRandR's monitors, on KWin among the layouts of its output
//! configuration. Elsewhere on Wayland it is not known: colord's first
//! display is used. GNOME's own colour management (mutter) is not detected.

use std::time::Duration;

use dbus::Path;
use dbus::blocking::Connection;
use dbus::blocking::stdintf::org_freedesktop_dbus::Properties;

use crate::SystemProfile;

const COLORD: &str = "org.freedesktop.ColorManager";
const TIMEOUT: Duration = Duration::from_millis(500);

/// colord's default profile of the display named `output` (a connector
/// such as `eDP-1`; none if colord does not know it, so that display's own
/// X11 profile is looked at next), or with no name of its first display.
/// `Err` when colord is not reachable.
fn colord_profile(output: Option<&str>) -> Result<Option<String>, String> {
    let connection = Connection::new_system().map_err(|e| e.to_string())?;
    let manager = connection.with_proxy(COLORD, "/org/freedesktop/ColorManager", TIMEOUT);
    let by_name = output.and_then(|output| {
        manager
            .method_call::<(Path<'static>,), _, _, _>(
                COLORD,
                "FindDeviceByProperty",
                ("XRANDR_name", output),
            )
            .ok()
            .map(|(device,)| device)
    });
    let device = match by_name {
        Some(device) => device,
        None if output.is_some() => return Ok(None),
        None => {
            let (devices,): (Vec<Path<'static>>,) = manager
                .method_call(COLORD, "GetDevicesByKind", ("display",))
                .map_err(|e| e.to_string())?;
            match devices.into_iter().next() {
                Some(device) => device,
                None => return Ok(None),
            }
        }
    };
    let device = connection.with_proxy(COLORD, device, TIMEOUT);
    let profiles: Vec<Path<'static>> = device
        .get(&format!("{COLORD}.Device"), "Profiles")
        .map_err(|e| e.to_string())?;
    let Some(profile) = profiles.into_iter().next() else {
        return Ok(None);
    };
    let profile = connection.with_proxy(COLORD, profile, TIMEOUT);
    let file: String = profile
        .get(&format!("{COLORD}.Profile"), "Filename")
        .map_err(|e| e.to_string())?;
    Ok((!file.is_empty()).then_some(file))
}

/// The X11 monitors (XRandR): each one's output name and position.
fn x11_monitors() -> Vec<(String, (i32, i32))> {
    if std::env::var_os("DISPLAY").is_none() {
        return Vec::new();
    }
    let (Ok(xlib), Ok(xrandr)) = (x11_dl::xlib::Xlib::open(), x11_dl::xrandr::Xrandr::open())
    else {
        return Vec::new();
    };
    let mut found = Vec::new();
    // SAFETY: the display is opened and closed here; the monitor list and
    // the names are copied out before they are freed.
    unsafe {
        let display = (xlib.XOpenDisplay)(std::ptr::null());
        if display.is_null() {
            return found;
        }
        let root = (xlib.XDefaultRootWindow)(display);
        let mut count = 0;
        let monitors = (xrandr.XRRGetMonitors)(display, root, 1, &mut count);
        if !monitors.is_null() {
            for monitor in std::slice::from_raw_parts(monitors, count.max(0) as usize) {
                let name = (xlib.XGetAtomName)(display, monitor.name);
                if name.is_null() {
                    continue;
                }
                let text = std::ffi::CStr::from_ptr(name)
                    .to_string_lossy()
                    .into_owned();
                (xlib.XFree)(name.cast());
                found.push((text, (monitor.x, monitor.y)));
            }
            (xrandr.XRRFreeMonitors)(monitors);
        }
        (xlib.XCloseDisplay)(display);
    }
    found
}

/// The bytes of the X11 root window's `_ICC_PROFILE` atom (monitor 0) or
/// `_ICC_PROFILE_n` (monitor n), if an X server is reachable and has one.
fn x11_profile(monitor: usize) -> Option<Vec<u8>> {
    std::env::var_os("DISPLAY")?;
    let xlib = x11_dl::xlib::Xlib::open().ok()?;
    // SAFETY: the display is opened and closed here; the property data is
    // copied out before XFree.
    unsafe {
        let display = (xlib.XOpenDisplay)(std::ptr::null());
        if display.is_null() {
            return None;
        }
        let root = (xlib.XDefaultRootWindow)(display);
        let name = match monitor {
            0 => c"_ICC_PROFILE".to_owned(),
            n => std::ffi::CString::new(format!("_ICC_PROFILE_{n}")).expect("no NUL"),
        };
        let atom = (xlib.XInternAtom)(display, name.as_ptr(), 1);
        let mut bytes = None;
        if atom != 0 {
            let mut actual_type = 0;
            let mut format = 0;
            let mut items = 0;
            let mut after = 0;
            let mut data = std::ptr::null_mut();
            let status = (xlib.XGetWindowProperty)(
                display,
                root,
                atom,
                0,
                // In 32-bit units: up to 64 MB.
                16 * 1024 * 1024,
                0,
                x11_dl::xlib::AnyPropertyType as u64,
                &mut actual_type,
                &mut format,
                &mut items,
                &mut after,
                &mut data,
            );
            if status == 0 && !data.is_null() {
                if format == 8 && items > 0 {
                    bytes = Some(std::slice::from_raw_parts(data, items as usize).to_vec());
                }
                (xlib.XFree)(data.cast());
            }
        }
        (xlib.XCloseDisplay)(display);
        bytes
    }
}

/// Where a window is: its monitor's position (logical and device px) and
/// every monitor's logical position and size (the desktop's layout).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WindowOutput {
    pub logical: Option<(i32, i32)>,
    pub physical: Option<(i32, i32)>,
    pub monitors: Vec<crate::kwin::Monitor>,
}

/// What the desktop says about the colours of the display a window is on,
/// and that display's name when it is known.
pub fn profile_of_window(window: &WindowOutput) -> (SystemProfile, Option<String>) {
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    let kde = std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|d| d.split(':').any(|d| d == "KDE"));
    let mut name = None;
    if wayland && kde {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config"))
            })
            .and_then(|dir| std::fs::read_to_string(dir.join("kwinoutputconfig.json")).ok());
        if let Some(config) = config {
            let output = window
                .logical
                .and_then(|at| crate::kwin::output_at(&config, &window.monitors, at));
            // The window's output unknown: if KWin manages a display of a
            // layout it keeps for these monitors, converting could be
            // converting twice.
            let managing = match &output {
                Some(output) => output.converts.then(|| output.clone()),
                None => crate::kwin::layout_output_converts(&config, &window.monitors),
            };
            if managing.is_some() {
                return (
                    SystemProfile::ManagedBySystem,
                    managing.map(|o| o.connector),
                );
            }
            name = output.map(|o| o.connector);
        }
    }
    let mut x11_index = None;
    if !wayland || name.is_none() {
        let monitors = x11_monitors();
        if let Some(at) = window.physical
            && let Some(index) = monitors.iter().position(|(_, p)| *p == at)
        {
            x11_index = Some(index);
            name = name.or_else(|| Some(monitors[index].0.clone()));
        }
    }
    let profile = lookup(name.as_deref(), x11_index.unwrap_or(0));
    (profile, name)
}

/// What the desktop says about the colours of the display named `output`.
pub fn profile_of_output(output: Option<&str>) -> SystemProfile {
    lookup(output, 0)
}

fn lookup(output: Option<&str>, x11_monitor: usize) -> SystemProfile {
    let colord = colord_profile(output);
    if let Ok(Some(path)) = &colord {
        return match std::fs::read(path) {
            Ok(icc) => SystemProfile::Assigned {
                icc,
                location: path.clone(),
            },
            Err(error) => SystemProfile::Unavailable(format!("{path}: {error}")),
        };
    }
    if let Some(icc) = x11_profile(x11_monitor) {
        return SystemProfile::Assigned {
            icc,
            location: match x11_monitor {
                0 => "X11 _ICC_PROFILE".into(),
                n => format!("X11 _ICC_PROFILE_{n}"),
            },
        };
    }
    match colord {
        Ok(_) => SystemProfile::NoneAssigned,
        Err(why) => SystemProfile::Unavailable(format!("colord not reachable: {why}")),
    }
}
