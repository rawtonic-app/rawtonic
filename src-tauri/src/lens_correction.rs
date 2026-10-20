//! Desktop glue for `rawtonic_engine::lens_correction` (ADR 0009): the Tauri commands
//! and app-state access stay here, the logic lives in the engine crate.
pub use rawtonic_engine::lens_correction::*;

pub fn load_lensfun_db(app_handle: &tauri::AppHandle) -> LensDatabase {
    let combined_db = {
        use tauri::Manager;
        let resource_path = app_handle
            .path()
            .resolve("lensfun_db", tauri::path::BaseDirectory::Resource)
            .expect("failed to resolve lensfun_db directory");
        load_lensfun_dir(&resource_path)
    };

    log::info!(
        "Loaded {} lenses and {} cameras from Lensfun database.",
        combined_db.lenses.len(),
        combined_db.cameras.len()
    );
    combined_db
}

#[tauri::command]
pub fn get_lensfun_makers() -> Result<Vec<String>, String> {
    let db_guard = global_lens_db();
    if let Some(db) = db_guard.as_deref() {
        let mut makers: Vec<String> = db.lenses.iter().map(|lens| lens.get_maker()).collect();
        makers.sort_unstable();
        makers.dedup();
        Ok(makers)
    } else {
        Err("Lens database not loaded".to_string())
    }
}

#[tauri::command]
pub fn get_lensfun_lenses_for_maker(maker: String) -> Result<Vec<String>, String> {
    let db_guard = global_lens_db();
    if let Some(db) = db_guard.as_deref() {
        let maker_lenses = lenses_for_maker(db, &maker);

        let mut models: Vec<String> = maker_lenses
            .iter()
            .map(|lens| lens.get_display_name(&maker_lenses))
            .collect();
        models.sort_unstable();
        models.dedup();
        Ok(models)
    } else {
        Err("Lens database not loaded".to_string())
    }
}

#[tauri::command]
pub fn autodetect_lens(
    maker: String,
    model: String,
    camera_model: Option<String>,
) -> Result<Option<(String, String)>, String> {
    let db_guard = global_lens_db();
    if let Some(db) = db_guard.as_deref() {
        Ok(find_best_lens_match(
            db,
            &maker,
            &model,
            camera_model.as_deref().unwrap_or(""),
        ))
    } else {
        Ok(None)
    }
}

/// The lens's profile for the photo whose EXIF map the editor holds, as a
/// save resolves it (`lens_recipe::lens_params_for_shot`).
#[tauri::command]
pub fn get_lens_distortion_params(
    maker: String,
    model: String,
    exif: Option<std::collections::HashMap<String, String>>,
) -> Result<Option<LensDistortionParams>, String> {
    let db_guard = global_lens_db();
    Ok(db_guard.as_deref().and_then(|db| {
        rawtonic_engine::lens_recipe::lens_params_for_shot(db, &maker, &model, exif.as_ref())
    }))
}
