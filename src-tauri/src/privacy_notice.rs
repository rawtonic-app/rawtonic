// SPDX-License-Identifier: AGPL-3.0-or-later
//! The first-start privacy notice for Windows copies no installer introduced
//! (the portable zip, or a silent install). The installers show the same
//! text on their first page and then set `PrivacyNoticeShown`
//! (`windows/installer-hooks.nsh`, `windows/msi-fragment.wxs`). Until the notice
//! was shown, one way or the other, the startup update check does not run
//! (`app_settings::reserve_update_check`). Other platforms have no such
//! installer step and are not affected.
use tauri::AppHandle;

/// Registry marker of the installers (HKCU\Software\Rawtonic Project\Rawtonic).
#[cfg(windows)]
fn installer_showed_notice() -> bool {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
    use windows::core::w;
    let mut value = 0u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: `value` and `size` outlive the call and `size` is its byte length.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Rawtonic Project\\Rawtonic"),
            w!("PrivacyNoticeShown"),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut value as *mut u32).cast()),
            Some(&mut size),
        )
    };
    status.is_ok() && value == 1
}

#[cfg(not(windows))]
fn installer_showed_notice() -> bool {
    true
}

/// Whether the notice was shown, here or by an installer.
pub(crate) fn shown(seen_in_app: Option<bool>) -> bool {
    seen_in_app == Some(true) || installer_showed_notice()
}

/// True when the frontend must show the notice before anything goes online.
#[tauri::command]
pub async fn privacy_notice_pending(app: AppHandle) -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let settings = crate::state::settings::try_current(&app)?;
        Ok(!shown(settings.privacy_notice_seen))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The photographer read the notice (or closed it): the startup check may run.
#[tauri::command]
pub async fn privacy_notice_acknowledge(app: AppHandle) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        crate::state::settings::update(&app, |settings| {
            settings.privacy_notice_seen = Some(true);
            Ok(())
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::shown;

    #[test]
    fn the_in_app_flag_counts_as_shown() {
        assert!(shown(Some(true)));
    }

    #[cfg(not(windows))]
    #[test]
    fn other_platforms_have_no_notice_to_wait_for() {
        assert!(shown(None));
    }
}
