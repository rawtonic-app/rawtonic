// SPDX-License-Identifier: AGPL-3.0-or-later
pub(crate) mod core;
pub(crate) mod install;
#[cfg(any(windows, test))]
pub(crate) mod staging;
use core::UpdateInfo;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Manager};

/// QA builds only: a local release feed (`RAWTONIC_QA_UPDATE_FEED=http://host:port`)
/// replaces GitHub for the check and the in-app update, so two QA builds can
/// update one to the other (docs/testing/in-app-update.md). A release build
/// (`cc.rawtonic.editor`) never reads it.
const QA_FEED_ENV: &str = "RAWTONIC_QA_UPDATE_FEED";

pub(crate) fn source(app: &AppHandle) -> core::Source {
    if app.config().identifier.starts_with("cc.rawtonic.qa")
        && let Some(feed) = std::env::var(QA_FEED_ENV)
            .ok()
            .and_then(|base| core::Source::qa_feed(&base))
    {
        log::info!("Update check: QA feed {}", feed.api_url);
        return feed;
    }
    core::Source::github()
}

fn running_in_flatpak() -> bool {
    core::in_flatpak(
        std::env::var("FLATPAK_ID").ok().as_deref(),
        cfg!(target_os = "linux") && std::path::Path::new("/.flatpak-info").exists(),
    )
}

/// Preferences shows where updates come from instead of the update switches
/// when Flatpak delivers them.
#[tauri::command]
pub fn updates_managed_by_flatpak() -> bool {
    running_in_flatpak()
}

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
    // Inside Flatpak the updates come from Flatpak (`flatpak update
    // cc.rawtonic.editor`): no request goes to GitHub, not even on "Check now".
    if running_in_flatpak() {
        return if manual {
            Err("Updates come from Flatpak".into())
        } else {
            Ok(None)
        };
    }
    let version = app.package_info().version.to_string();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();
    let settings_app = app.clone();
    let settings = tauri::async_runtime::spawn_blocking(move || {
        crate::app_settings::reserve_update_check(&settings_app, manual, now)
    })
    .await
    .map_err(|e| e.to_string())??;
    let Some(settings) = settings else {
        return Ok(None);
    };
    let prereleases = core::include_prereleases(&version, settings.update_prereleases)?;
    let source = source(&app);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(format!("Rawtonic/{version}"))
        .build()
        .map_err(|e| e.to_string())?;
    // User decision 2026-10-24: exactly one request for the ten most recent releases,
    // including pre-releases. Do not paginate or add a /latest request.
    let response = client
        .get(&source.api_url)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?;
    let bytes = response.bytes().await.map_err(|e| e.to_string())?;
    let update = core::select_release(&bytes, &version, prereleases, this_platform(), &source)?
        .filter(|u| manual || settings.dismissed_update_version.as_deref() != Some(&u.version));
    // What "Install update" may fetch after the click (nothing before it).
    if let Some(offer) = update.as_ref().and_then(|u| u.offer.clone()) {
        app.state::<install::UpdateInstall>().offer(offer, source);
    }
    Ok(update)
}

/// Read locally only: the build's target, Rosetta on an Intel build, how
/// this copy was installed and, on Linux, `/etc/os-release`.
fn this_platform() -> core::Platform {
    let app_image = running_appimage().is_some();
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
        install: this_install(os, app_image),
    }
}

fn this_install(os: core::Os, app_image: bool) -> core::Install {
    let exe = tauri::utils::platform::current_exe().or_else(|_| std::env::current_exe());
    match os {
        core::Os::Linux if app_image => core::Install::AppImage,
        core::Os::Linux => core::Install::LinuxOther,
        core::Os::Mac => match exe {
            Ok(exe) => core::mac_install(&exe, read_only_volume),
            Err(_) => core::Install::Unbundled,
        },
        core::Os::Windows => {
            let Some(dir) = exe
                .ok()
                .and_then(|e| e.parent().map(std::path::Path::to_path_buf))
            else {
                return core::Install::WindowsPortable;
            };
            let program_files = ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"]
                .into_iter()
                .filter_map(|name| std::env::var(name).ok())
                .collect::<Vec<_>>();
            core::windows_install(
                &dir.to_string_lossy(),
                dir.join("uninstall.exe").is_file(),
                &program_files,
            )
        }
    }
}

/// The AppImage file this process runs from (Linux), see
/// `core::running_appimage`, as it was at the first call (app start, lib.rs
/// setup). The in-app update replaces only that file.
pub(crate) fn running_appimage() -> Option<std::path::PathBuf> {
    started_appimage().map(|(path, _)| path.clone())
}

/// The running AppImage and its file identity (device, inode) at the start.
#[cfg(unix)]
fn started_appimage() -> Option<&'static (std::path::PathBuf, (u64, u64))> {
    use std::os::unix::fs::MetadataExt;
    static STARTED: std::sync::OnceLock<Option<(std::path::PathBuf, (u64, u64))>> =
        std::sync::OnceLock::new();
    STARTED
        .get_or_init(|| {
            if !cfg!(target_os = "linux") {
                return None;
            }
            let exe = std::env::current_exe().ok()?;
            let bundle = tauri::utils::platform::bundle_type()
                == Some(tauri::utils::config::BundleType::AppImage);
            let appimage = std::env::var_os("APPIMAGE").map(std::path::PathBuf::from);
            let appdir = std::env::var_os("APPDIR").map(std::path::PathBuf::from);
            let path =
                core::running_appimage(bundle, appimage.as_deref(), appdir.as_deref(), &exe)?;
            let meta = std::fs::metadata(&path).ok()?;
            Some((path, (meta.dev(), meta.ino())))
        })
        .as_ref()
}

#[cfg(not(unix))]
fn started_appimage() -> Option<&'static (std::path::PathBuf, (u64, u64))> {
    None
}

/// Whether the running AppImage's file is still the one Rawtonic started
/// from (an external update or a manual copy may have replaced it since).
#[cfg(target_os = "linux")]
pub(crate) fn appimage_unchanged() -> bool {
    use std::os::unix::fs::MetadataExt;
    started_appimage().is_some_and(|(path, id)| {
        std::fs::symlink_metadata(path)
            .is_ok_and(|m| m.file_type().is_file() && (m.dev(), m.ino()) == *id)
    })
}

/// Whether `path` lies on a read-only volume (a mounted disk image, App
/// Translocation's read-only mount).
#[cfg(unix)]
fn read_only_volume(path: &std::path::Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return true;
    };
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::zeroed();
    // SAFETY: a NUL-terminated path and a statvfs-sized output buffer.
    let result = unsafe { libc::statvfs(c_path.as_ptr(), stat.as_mut_ptr()) };
    // SAFETY: statvfs filled the buffer when it returned 0.
    result != 0 || unsafe { stat.assume_init() }.f_flag & libc::ST_RDONLY != 0
}

#[cfg(not(unix))]
fn read_only_volume(_: &std::path::Path) -> bool {
    false
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
