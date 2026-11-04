//! Desktop glue for `rawtonic_engine::cache_utils` (ADR 0009): the Tauri commands
//! and app-state access stay here, the logic lives in the engine crate.
pub use rawtonic_engine::cache_utils::*;

use crate::state::develop::{DevelopSession, Scope};

#[tauri::command]
pub fn clear_image_caches(develop: tauri::State<DevelopSession>) {
    develop.invalidate(Scope::Decoder);
}
