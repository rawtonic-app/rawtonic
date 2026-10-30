//! The camera preview in Develop (Shift+\, View ▸ Camera Preview): the JPEG
//! the camera wrote into the RAW shown natively in place of the edit
//! (`gpu_processing::camera_cover`), never over it in the webview. The
//! pixels are the photograph's still (`embedded_still::still_for`: the
//! camera's largest embedded JPEG, turned, framed like the development and
//! converted to the display's encoding). A view only: the recipe, the
//! history and the file are never touched.

use crate::app_state::AppState;
use crate::file_management::parse_virtual_path;
use tauri::Manager;

/// Shows the camera preview of `path`; whether it is on screen. Nothing
/// when the photograph has no camera JPEG, is no longer the one asked for
/// (stepping on meanwhile), or the viewer shows several photographs.
#[tauri::command]
pub(crate) async fn show_camera_preview(
    path: String,
    app_handle: tauri::AppHandle,
) -> Result<bool, String> {
    let state = app_handle.state::<AppState>();
    let Some(context) = state
        .gpu_context
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
    else {
        return Ok(false);
    };
    let Some(area) = context
        .display
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|display| display.viewer_area)
    else {
        return Ok(false);
    };
    let (source, _) = parse_virtual_path(&path);
    let view = (area[2].max(1.0) as u32, area[3].max(1.0) as u32);
    let space = context.display_space;
    let still =
        tokio::task::spawn_blocking(move || crate::embedded_still::still_for(&source, view, space))
            .await
            .map_err(|e| format!("Camera preview task failed: {e}"))?;
    let Some(still) = still else {
        return Ok(false);
    };
    let mut display = context.display.lock().unwrap_or_else(|e| e.into_inner());
    let Some(display) = display.as_mut() else {
        return Ok(false);
    };
    // Checked under the display lock: a step to the next photograph
    // (`release_still_for`) either came first or takes this cover away.
    if !asked_for(
        &state
            .current_source
            .lock()
            .unwrap_or_else(|e| e.into_inner()),
        &path,
    ) {
        return Ok(false);
    }
    Ok(display.show_camera_cover(
        &context.device,
        &context.queue,
        &still.image,
        &path,
        still.shown,
    ))
}

/// Back to the edit (off the main thread: it waits for the display).
#[tauri::command]
pub(crate) async fn hide_camera_preview(app_handle: tauri::AppHandle) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        let state = app_handle.state::<AppState>();
        let Some(context) = state
            .gpu_context
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        else {
            return;
        };
        if let Some(display) = context
            .display
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
        {
            display.hide_camera_cover(&context.device, &context.queue);
        }
    })
    .await
    .map_err(|e| format!("Camera preview task failed: {e}"))
}

/// Whether `path` is still the photograph Develop shows (`current`, the
/// source asked for last).
fn asked_for(current: &str, path: &str) -> bool {
    !path.is_empty() && current == path
}

#[cfg(test)]
mod tests {
    use super::asked_for;

    #[test]
    fn a_camera_preview_of_a_photograph_stepped_away_from_is_not_shown() {
        assert!(asked_for("/a.ARW", "/a.ARW"));
        assert!(!asked_for("/b.ARW", "/a.ARW"));
        assert!(!asked_for("", ""));
    }
}
