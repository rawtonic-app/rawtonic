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

/// The parameters of a database lens for the editor. With the photo's `path`
/// they come from its EXIF exactly as a save would look them up (crop factor
/// included), so the editor's preview, the saved recipe and its export agree.
#[tauri::command]
pub async fn get_lens_distortion_params(
    maker: String,
    model: String,
    focal_length: f32,
    aperture: Option<f32>,
    distance: Option<f32>,
    path: Option<String>,
) -> Result<Option<LensDistortionParams>, String> {
    // Off the main thread: reading the photo's EXIF may open the catalog.
    tauri::async_runtime::spawn_blocking(move || {
        let db_guard = global_lens_db();
        let db = db_guard.as_deref()?;
        if let Some(path) = path {
            let (source, sidecar) = crate::file_management::parse_virtual_path(&path);
            let metadata = crate::exif_processing::load_sidecar_with_exif(&sidecar, &source);
            return crate::file_management::lens_params_for_exif(
                db,
                &maker,
                &model,
                &metadata.exif,
            );
        }
        let maker_lenses = lenses_for_maker(db, &maker);
        maker_lenses
            .iter()
            .find(|l| l.get_display_name(&maker_lenses) == model)
            .and_then(|lens| lens.get_distortion_params(focal_length, aperture, distance, None))
    })
    .await
    .map_err(|e| format!("Lens lookup failed: {e}"))
}

/// The lens fields a save would give `adjustments` on the photograph at
/// `path` (`resolve_lens_params_in_adjustments` with a fresh lookup, the
/// recipe's own camera CA version): the editor fills pending parameters in
/// with these, so its preview and the saved recipe agree.
#[tauri::command]
pub async fn resolve_lens_profile(
    path: String,
    adjustments: serde_json::Value,
) -> Result<serde_json::Value, String> {
    // Off the main thread: reading the photo's EXIF may open the catalog.
    tauri::async_runtime::spawn_blocking(move || {
        let (source, sidecar) = crate::file_management::parse_virtual_path(&path);
        let metadata = crate::exif_processing::load_sidecar_with_exif(&sidecar, &source);
        let mut resolved = adjustments;
        let db_guard = global_lens_db();
        crate::file_management::resolve_lens_params_in_adjustments(
            &mut resolved,
            crate::file_management::LensLookup::Fresh,
            &metadata.exif,
            db_guard.as_deref(),
            &source,
        );
        let field = |key: &str| {
            resolved
                .get(key)
                .cloned()
                .unwrap_or(serde_json::Value::Null)
        };
        serde_json::json!({
            "lensMaker": field("lensMaker"),
            "lensModel": field("lensModel"),
            "lensDistortionParams": field("lensDistortionParams"),
        })
    })
    .await
    .map_err(|e| format!("Lens lookup failed: {e}"))
}
