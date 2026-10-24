// SPDX-License-Identifier: AGPL-3.0-or-later
pub(crate) mod core;
use core::UpdateInfo;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::AppHandle;

#[tauri::command]
pub async fn check_updates(app_handle: AppHandle, manual: bool) -> Option<UpdateInfo> {
    match check(app_handle, manual).await {
        Ok(update) => update,
        Err(error) => {
            log::debug!("Update check failed: {error}");
            None
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
    let response = client
        .get("https://api.github.com/repos/rawtonic-app/rawtonic/releases?per_page=10")
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?;
    let bytes = response.bytes().await.map_err(|e| e.to_string())?;
    let update = core::select_release(
        &bytes,
        &version,
        prereleases,
        cfg!(target_os = "linux") && std::env::var_os("APPIMAGE").is_some(),
    )?;
    Ok(update
        .filter(|u| manual || settings.dismissed_update_version.as_deref() != Some(&u.version)))
}
