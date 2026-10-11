//! Desktop glue for `rawtonic_engine::adjustment_utils` (ADR 0009): the Tauri commands
//! and app-state access stay here, the logic lives in the engine crate.
pub use rawtonic_engine::adjustment_utils::*;

use crate::app_state::AppState;

pub fn hydrate_adjustments(state: &tauri::State<AppState>, adjustments: &mut serde_json::Value) {
    let mut cache = state.patch_cache.lock().unwrap();
    hydrate_adjustments_from_cache(&mut cache, adjustments);
}
