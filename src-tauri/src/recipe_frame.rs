//! Desktop glue for `rawtonic_engine::recipe_frame`: the commands the editor
//! asks about a camera TIFF's recipe that cannot tell whether it was made
//! on the embedded thumbnail or the full frame, and about the models its
//! migrated AI selections are computed again with. The rules live in the
//! engine crate.

use serde_json::Value;

/// `adjustments` (the open photo's recipe at `path`) moved to the full
/// frame, when the recipe cannot tell on which frame it was made
/// (`recipe_frame::undecided`): the editor offers the move and applies it as
/// one History step when the user chooses it. None for every other recipe
/// (moved when it opened, made on the full frame, or not a camera TIFF's).
#[tauri::command]
pub async fn undecided_frame_move(
    path: String,
    adjustments: Value,
) -> Result<Option<Value>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let (source, _) = crate::file_management::parse_virtual_path(&path);
        let mut moved = adjustments;
        rawtonic_engine::recipe_frame::move_to_full_frame(&mut moved, &source).then_some(moved)
    })
    .await
    .map_err(|e| e.to_string())
}

/// The AI selection types (`ai-sky`, `ai-foreground`, `ai-people`,
/// `ai-depth`) whose models are installed: the editor computes a camera
/// TIFF's migrated selections again on the full frame only with those, so
/// opening such an edit downloads nothing (PRIVACY.md); the others keep
/// their stretched thumbnail bitmap.
#[tauri::command]
pub async fn installed_selection_models(
    app_handle: tauri::AppHandle,
) -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        [
            (
                "ai-sky",
                crate::ai_processing::mask_model_installed(&app_handle, true),
            ),
            (
                "ai-foreground",
                crate::ai_processing::mask_model_installed(&app_handle, false),
            ),
            (
                "ai-people",
                crate::ai_faces::face_models_installed(&app_handle),
            ),
            (
                "ai-depth",
                crate::ai_processing::depth_model_installed(&app_handle),
            ),
        ]
        .into_iter()
        .filter(|(_, installed)| *installed)
        .map(|(kind, _)| kind.to_string())
        .collect()
    })
    .await
    .map_err(|e| e.to_string())
}
