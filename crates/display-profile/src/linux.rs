//! The profile a Linux desktop assigns to a display: colord's default
//! profile for the display device (GNOME and KDE store their choice there),
//! else the ICC profile an X11 session publishes in the root window's
//! `_ICC_PROFILE` atom. A Wayland session without colord has neither.

use std::time::Duration;

use dbus::Path;
use dbus::blocking::Connection;
use dbus::blocking::stdintf::org_freedesktop_dbus::Properties;

use crate::SystemProfile;

const COLORD: &str = "org.freedesktop.ColorManager";
const TIMEOUT: Duration = Duration::from_millis(500);

/// colord's default profile of the display named `output` (a connector
/// such as `eDP-1`), else of its first display. `Err` when colord is not
/// reachable.
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

/// The bytes of the X11 root window's `_ICC_PROFILE` atom (the first
/// screen's profile), if an X server is reachable and has one.
fn x11_profile() -> Option<Vec<u8>> {
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
        let atom = (xlib.XInternAtom)(display, c"_ICC_PROFILE".as_ptr(), 1);
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

/// What the desktop says about the colours of the display named `output`.
pub fn profile_of_output(output: Option<&str>) -> SystemProfile {
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
    if let Some(icc) = x11_profile() {
        return SystemProfile::Assigned {
            icc,
            location: "X11 _ICC_PROFILE".into(),
        };
    }
    match colord {
        Ok(_) => SystemProfile::NoneAssigned,
        Err(why) => SystemProfile::Unavailable(format!("colord not reachable: {why}")),
    }
}
