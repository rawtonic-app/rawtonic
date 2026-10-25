//! Desktop glue for `rawtonic_engine::cache_utils` (ADR 0009): the Tauri commands
//! and app-state access stay here, the logic lives in the engine crate.
pub use rawtonic_engine::cache_utils::*;

use crate::app_state::AppState;

#[tauri::command]
pub fn clear_image_caches(state: tauri::State<AppState>) {
    if let Ok(mut decoded_cache) = state.decoded_image_cache.lock() {
        decoded_cache.clear();
    }
    if let Ok(mut gpu_cache) = state.gpu_image_cache.lock() {
        *gpu_cache = None;
    }
    if let Ok(mut preview_cache) = state.cached_preview.lock() {
        *preview_cache = None;
    }
    if let Ok(mut reusable_previews) = state.reusable_previews.lock() {
        reusable_previews.clear();
    }
    if let Ok(mut warped_cache) = state.full_warped_cache.lock() {
        *warped_cache = None;
    }
    if let Ok(mut patched_warped_cache) = state.patched_warped_cache.lock() {
        *patched_warped_cache = None;
    }
    if let Ok(mut transformed_cache) = state.full_transformed_cache.lock() {
        *transformed_cache = None;
    }
    // The multi view's decodes held for its 1:1 previews.
    crate::multi_view_prefetch::forget_decodes();
    // The decode kept for the photograph opened next.
    crate::proxy_worker::forget_ahead();
}
