//! Desktop glue for `rawtonic_engine::recipe_frame`: the command the editor
//! asks about a camera TIFF's recipe that cannot tell whether it was made
//! on the embedded thumbnail or the full frame. The rule lives in the
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
