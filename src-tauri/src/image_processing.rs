//! Desktop glue for `rawtonic_engine::image_processing` (ADR 0009): the Tauri commands
//! and app-state access stay here, the logic lives in the engine crate.
pub use rawtonic_engine::image_processing::*;

use crate::app_state::AppState;
pub use crate::gpu_processing::{
    GpuContext, RenderRequest, get_or_init_gpu_context, process_and_get_dynamic_image,
    process_and_get_dynamic_image_with_analytics, process_and_get_dynamic_image_with_precision,
};

pub fn resolve_tonemapper_override(
    settings: &crate::app_settings::AppSettings,
    is_raw: bool,
) -> Option<u32> {
    if !settings.tonemapper_override_enabled.unwrap_or(false) {
        return None;
    }
    let tm = if is_raw {
        settings.default_raw_tonemapper.as_deref().unwrap_or("agx")
    } else {
        settings
            .default_non_raw_tonemapper
            .as_deref()
            .unwrap_or("basic")
    };
    Some(tonemapper_mode_of(tm))
}

pub fn resolve_tonemapper_override_from_handle(
    app_handle: &tauri::AppHandle,
    is_raw: bool,
) -> Option<u32> {
    let settings = crate::app_settings::load_settings(app_handle.clone()).unwrap_or_default();
    resolve_tonemapper_override(&settings, is_raw)
}

/// Off the main thread (the decode wait and the analysis would freeze the
/// window).
#[tauri::command]
pub async fn calculate_auto_adjustments(
    state: tauri::State<'_, AppState>,
    tone_mapper: Option<String>,
    base_curve: Option<String>,
    js_adjustments: Option<serde_json::Value>,
) -> Result<serde_json::Value, String> {
    let (original_image, path, is_raw) = match crate::image_loader::original_image(&state).await {
        Ok(loaded) => (loaded.image, loaded.path, loaded.is_raw),
        // An offline photograph on its Standard preview: Auto reads the
        // tones of the whole photograph, which the preview holds as well.
        Err(error) => {
            let proxy = state
                .proxy_image
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
                .filter(|proxy| {
                    state
                        .offline_source
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .as_deref()
                        == Some(proxy.path.as_str())
                })
                .ok_or(error)?;
            (proxy.image, proxy.path, proxy.is_raw)
        }
    };
    let source_path = crate::file_management::parse_virtual_path(&path)
        .0
        .to_string_lossy()
        .to_string();
    tauri::async_runtime::spawn_blocking(move || {
        // A film negative's Auto reads the positive being edited.
        let original_image = rawtonic_engine::negative::apply_for_recipe(
            std::borrow::Cow::Borrowed(original_image.as_ref()),
            js_adjustments.as_ref().unwrap_or(&serde_json::Value::Null),
            is_raw,
        );
        auto_adjustments_json(
            &original_image,
            &source_path,
            is_raw,
            tone_mapper.as_deref(),
            base_curve.as_deref(),
        )
    })
    .await
    .map_err(|e| format!("Auto adjustments failed: {e}"))
}
