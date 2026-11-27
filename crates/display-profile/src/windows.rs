//! The profile Windows assigns to the monitor a window is on.
//!
//! `GetICMProfileW` on a device context of the monitor gives the default
//! profile from Colour Management (per user when "Use my settings for this
//! device" is on), the same lookup Chromium, and so the webview, uses for
//! the interface. With advanced colour on (HDR, or Auto colour management
//! on Windows 11) the compositor already matches sRGB windows to the
//! display, so the preview is left to it.

use std::ffi::c_void;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};

use windows_sys::Win32::Devices::Display::{
    DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO, DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
    DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO,
    DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SOURCE_DEVICE_NAME,
    DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QDC_ONLY_ACTIVE_PATHS,
    QueryDisplayConfig,
};
use windows_sys::Win32::Graphics::Gdi::{
    CreateDCW, DeleteDC, GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MONITORINFOEXW,
    MonitorFromWindow,
};
use windows_sys::Win32::UI::ColorSystem::GetICMProfileW;

use crate::SystemProfile;

/// The monitor (`HMONITOR`) that holds most of the window `hwnd`; it
/// identifies the display for as long as it stays connected.
pub fn monitor_of(hwnd: isize) -> isize {
    // SAFETY: any window handle (even a stale one) is accepted; the
    // nearest monitor is returned.
    unsafe { MonitorFromWindow(hwnd as *mut c_void, MONITOR_DEFAULTTONEAREST) as isize }
}

/// The GDI device name (`\\.\DISPLAY1`) of `monitor`, NUL-terminated.
fn device_name(monitor: isize) -> Option<[u16; 32]> {
    // SAFETY: MONITORINFOEXW is plain data; cbSize says which form it is.
    unsafe {
        let mut info: MONITORINFOEXW = zeroed();
        info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        let ok = GetMonitorInfoW(
            monitor as *mut c_void,
            &mut info as *mut MONITORINFOEXW as *mut MONITORINFO,
        );
        (ok != 0).then_some(info.szDevice)
    }
}

fn wide_str(name: &[u16]) -> &[u16] {
    let end = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    &name[..end]
}

/// Whether advanced colour (HDR or Auto colour management) is on for the
/// display behind GDI device `device`; `None` if it cannot be told.
fn advanced_color_enabled(device: &[u16]) -> Option<bool> {
    // SAFETY: the buffers are sized from GetDisplayConfigBufferSizes and
    // every request packet carries its own size.
    unsafe {
        let (mut paths, mut modes) = (0u32, 0u32);
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut paths, &mut modes) != 0 {
            return None;
        }
        let mut path_info: Vec<DISPLAYCONFIG_PATH_INFO> = vec![zeroed(); paths as usize];
        let mut mode_info: Vec<DISPLAYCONFIG_MODE_INFO> = vec![zeroed(); modes as usize];
        if QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut paths,
            path_info.as_mut_ptr(),
            &mut modes,
            mode_info.as_mut_ptr(),
            null_mut(),
        ) != 0
        {
            return None;
        }
        path_info.truncate(paths as usize);
        for path in &path_info {
            let mut source: DISPLAYCONFIG_SOURCE_DEVICE_NAME = zeroed();
            source.header = DISPLAYCONFIG_DEVICE_INFO_HEADER {
                r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                size: size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
                adapterId: path.sourceInfo.adapterId,
                id: path.sourceInfo.id,
            };
            if DisplayConfigGetDeviceInfo(&mut source.header) != 0
                || wide_str(&source.viewGdiDeviceName) != device
            {
                continue;
            }
            let mut color: DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO = zeroed();
            color.header = DISPLAYCONFIG_DEVICE_INFO_HEADER {
                r#type: DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
                size: size_of::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>() as u32,
                adapterId: path.targetInfo.adapterId,
                id: path.targetInfo.id,
            };
            if DisplayConfigGetDeviceInfo(&mut color.header) != 0 {
                return None;
            }
            // Bit 1: advancedColorEnabled.
            return Some(color.Anonymous.value & 0b10 != 0);
        }
        None
    }
}

/// The path of the default ICC profile of GDI device `device`.
fn profile_path(device: &[u16]) -> Option<String> {
    let mut name = device.to_vec();
    name.push(0);
    // SAFETY: `name` is NUL-terminated; the DC is deleted before returning
    // and the buffer length is passed in and out.
    unsafe {
        let dc = CreateDCW(name.as_ptr(), null(), null(), null());
        if dc.is_null() {
            return None;
        }
        let mut buffer = vec![0u16; 1024];
        let mut len = buffer.len() as u32;
        let ok = GetICMProfileW(dc, &mut len, buffer.as_mut_ptr());
        DeleteDC(dc);
        if ok == 0 {
            return None;
        }
        let path = String::from_utf16_lossy(wide_str(&buffer));
        (!path.is_empty()).then_some(path)
    }
}

/// What Windows says about the colours of `monitor` (from [`monitor_of`]).
pub fn profile_of_monitor(monitor: isize) -> SystemProfile {
    let Some(device) = device_name(monitor) else {
        return SystemProfile::Unavailable("the window's monitor is not known".into());
    };
    let device = wide_str(&device);
    if advanced_color_enabled(device) == Some(true) {
        return SystemProfile::ManagedBySystem;
    }
    let Some(path) = profile_path(device) else {
        return SystemProfile::NoneAssigned;
    };
    match std::fs::read(&path) {
        Ok(icc) => SystemProfile::Assigned {
            icc,
            location: path,
        },
        Err(error) => SystemProfile::Unavailable(format!("{path}: {error}")),
    }
}
