// SPDX-License-Identifier: AGPL-3.0-or-later
pub(crate) mod core;
use core::UpdateInfo;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::AppHandle;

#[tauri::command]
pub async fn check_updates(app_handle: AppHandle, manual: bool) -> Result<Option<UpdateInfo>, ()> {
    match check(app_handle, manual).await {
        Ok(update) => Ok(update),
        Err(error) => {
            log::debug!("Update check failed: {error}");
            Err(())
        }
    }
}

async fn check(app: AppHandle, manual: bool) -> Result<Option<UpdateInfo>, String> {
    let version = app.package_info().version.to_string();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();
    let settings = tauri::async_runtime::spawn_blocking(move || {
        crate::app_settings::reserve_update_check(&app, manual, now)
    })
    .await
    .map_err(|e| e.to_string())??;
    let Some(settings) = settings else {
        return Ok(None);
    };
    let prereleases = core::include_prereleases(&version, settings.update_prereleases)?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(format!("Rawtonic/{version}"))
        .build()
        .map_err(|e| e.to_string())?;
    // User decision 2026-10-24: exactly one request for the ten most recent releases,
    // including pre-releases. Do not paginate or add a /latest request.
    let response = client
        .get("https://api.github.com/repos/rawtonic-app/rawtonic/releases?per_page=10")
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?;
    let bytes = response.bytes().await.map_err(|e| e.to_string())?;
    let app_image = cfg!(target_os = "linux") && std::env::var_os("APPIMAGE").is_some();
    let update = core::select_release(
        &bytes,
        &version,
        prereleases,
        this_platform(app_image),
        app_image,
    )?;
    Ok(update
        .filter(|u| manual || settings.dismissed_update_version.as_deref() != Some(&u.version)))
}

/// Read locally only: the build's target, Rosetta on an Intel build and, on
/// Linux, `/etc/os-release`.
fn this_platform(app_image: bool) -> core::Platform {
    let os = if cfg!(target_os = "macos") {
        core::Os::Mac
    } else if cfg!(target_os = "windows") {
        core::Os::Windows
    } else {
        core::Os::Linux
    };
    let arch = if cfg!(target_arch = "aarch64") || runs_under_rosetta() {
        core::Arch::Arm64
    } else {
        core::Arch::X64
    };
    let os_release = cfg!(target_os = "linux")
        .then(|| std::fs::read_to_string("/etc/os-release").ok())
        .flatten();
    core::Platform {
        os,
        arch,
        linux: core::linux_package(app_image, os_release.as_deref()),
    }
}

/// An Intel build on an Apple Silicon Mac is offered the Apple Silicon DMG.
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
fn runs_under_rosetta() -> bool {
    let mut translated: libc::c_int = 0;
    let mut size = std::mem::size_of::<libc::c_int>();
    // SAFETY: a NUL-terminated name and an int-sized output buffer with its size.
    let result = unsafe {
        libc::sysctlbyname(
            c"sysctl.proc_translated".as_ptr(),
            (&raw mut translated).cast(),
            &raw mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    result == 0 && translated == 1
}

#[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
fn runs_under_rosetta() -> bool {
    false
}
