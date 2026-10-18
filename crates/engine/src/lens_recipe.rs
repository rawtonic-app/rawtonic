//! A recipe's lens parameters: what they were resolved for
//! (`lensProfileFor`) and resolving them for its lens choice — at save time
//! (`LensLookup`) and before an export of a recipe saved while its photo
//! could not be read (`resolve_unresolved`). Data in: the photo's EXIF is
//! passed by the caller.
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

/// How a save treats the lens parameters a recipe carries.
pub enum LensLookup<'a> {
    /// An editor save or a batch edit: the parameters, or their absence,
    /// stay as they are while they were resolved for the recipe's current
    /// lens choice (`lensProfileFor`, written here) — an unrelated edit, an
    /// undo. A recipe from before that key keeps them while its lens choice
    /// and parameters are those saved (`previous`). A changed choice (lens
    /// tool, paste, preset, preset Amount, the camera data's CA version) is
    /// resolved. So a Lensfun update — a renamed lens, a re-measured profile,
    /// a newly covered lens — reaches a saved edit only through a new choice.
    Keep { previous: Option<&'a Value> },
    /// Resolved again in the current database: Auto lens correction.
    Fresh,
}

/// What a recipe's lens parameters were resolved for (`lens_profile_stamp`).
pub const LENS_PROFILE_FOR: &str = "lensProfileFor";
/// `lensProfileFor` of a recipe whose lens could not be resolved yet.
pub const LENS_UNRESOLVED: &str = "unresolved";

/// What a recipe's lens parameters were resolved for, and the parameters:
/// the mode and the lens (chosen or detected), for the camera's own data its
/// CA version, then the parameters themselves (normalised; none when there
/// are none). Parameters cleared or changed by anything but a resolution no
/// longer match it.
pub fn lens_profile_stamp(recipe: &Value) -> String {
    let field = |key: &str| {
        recipe
            .get(key)
            .filter(|v| !v.is_null())
            .cloned()
            .unwrap_or(Value::Null)
    };
    let mode = recipe
        .get("lensCorrectionMode")
        .and_then(Value::as_str)
        .unwrap_or("manual");
    let params = recipe
        .get("lensDistortionParams")
        .filter(|p| !p.is_null())
        .map(|p| {
            serde_json::from_value::<crate::lens_correction::LensDistortionParams>(p.clone())
                .ok()
                .and_then(|params| serde_json::to_value(params).ok())
                .unwrap_or_else(|| p.clone())
        })
        .unwrap_or(Value::Null);
    let stamp = if mode == crate::embedded_lens::CAMERA_MODE {
        serde_json::json!([mode, field(crate::embedded_lens::CA_VERSION_KEY), params])
    } else {
        serde_json::json!([mode, field("lensMaker"), field("lensModel"), params])
    };
    stamp.to_string()
}

/// A recipe handed to the editor: one saved before `lensProfileFor` gets it
/// in memory, so undo and redo carry it too (its parameters were resolved
/// for its lens choice when it was saved).
pub fn stamp_loaded_lens_profile(adjustments: &mut Value) {
    if adjustments.is_object() && adjustments.get(LENS_PROFILE_FOR).is_none() {
        adjustments[LENS_PROFILE_FOR] = serde_json::json!(lens_profile_stamp(adjustments));
    }
}

pub fn resolve_lens_params_in_adjustments(
    adjustments: &mut Value,
    lookup: LensLookup,
    exif_data: &Option<HashMap<String, String>>,
    lens_db: Option<&crate::lens_correction::LensDatabase>,
    source: &Path,
) {
    if !adjustments.is_object() {
        return;
    }
    let stamp = lens_profile_stamp(adjustments);
    let keep = match lookup {
        LensLookup::Keep { previous } => {
            match adjustments.get(LENS_PROFILE_FOR).and_then(Value::as_str) {
                Some(resolved) => resolved == stamp,
                // Only a recipe saved before the key, or one resolved for
                // this very choice and these parameters, vouches for them;
                // never one marked unresolved.
                None => previous.is_some_and(|previous| {
                    match previous.get(LENS_PROFILE_FOR).and_then(Value::as_str) {
                        None => lens_profile_stamp(previous) == stamp,
                        Some(resolved) => resolved == stamp,
                    }
                }),
            }
        }
        LensLookup::Fresh => false,
    };
    if keep || resolve_lens_choice(adjustments, exif_data, lens_db, source) {
        let stamp = lens_profile_stamp(adjustments);
        adjustments[LENS_PROFILE_FOR] = serde_json::json!(stamp);
    } else {
        // Not resolved (no database or camera facts yet): marked, so every
        // later save tries again.
        adjustments[LENS_PROFILE_FOR] = serde_json::json!(LENS_UNRESOLVED);
    }
}

/// Resolves the recipe's lens choice; false when it could not (no camera
/// facts or no lens database yet), so it is tried again on the next save.
fn resolve_lens_choice(
    adjustments: &mut Value,
    exif_data: &Option<HashMap<String, String>>,
    lens_db: Option<&crate::lens_correction::LensDatabase>,
    source: &Path,
) -> bool {
    if let Some(map) = adjustments.as_object_mut() {
        let mode = map
            .get("lensCorrectionMode")
            .and_then(|v| v.as_str())
            .unwrap_or("manual");

        // The camera's own data: no lens database entry to resolve.
        if mode == crate::embedded_lens::CAMERA_MODE {
            let recipe = Value::Object(map.clone());
            if !crate::embedded_lens::recipe_params_known(source, &recipe) {
                return false;
            }
            // Neither facts registered for it nor the file to read them
            // from: not "no camera data", tried again later.
            if crate::source_facts::registered(&source.to_string_lossy()).is_none()
                && !readable(source)
            {
                return false;
            }
            match crate::embedded_lens::params_for_recipe(source, &recipe) {
                Some(params) => {
                    map.insert(
                        "lensDistortionParams".to_string(),
                        serde_json::to_value(params).unwrap_or_default(),
                    );
                }
                None => {
                    map.remove("lensDistortionParams");
                }
            }
            return true;
        }
        let Some(db) = lens_db else {
            return false;
        };

        if mode == "auto" {
            if let Some(exif) = exif_data {
                let exif_maker = exif.get("Make").map(|s| s.as_str()).unwrap_or("");
                let exif_model = exif.get("LensModel").map(|s| s.as_str()).unwrap_or("");
                let exif_camera_model = exif.get("Model").map(|s| s.as_str()).unwrap_or("");
                {
                    if let Some((detected_maker, detected_model)) =
                        crate::lens_correction::find_best_lens_match(
                            db,
                            exif_maker,
                            exif_model,
                            exif_camera_model,
                        )
                    {
                        map.insert(
                            "lensMaker".to_string(),
                            serde_json::to_value(&detected_maker).unwrap(),
                        );
                        map.insert(
                            "lensModel".to_string(),
                            serde_json::to_value(&detected_model).unwrap(),
                        );
                    } else {
                        map.remove("lensMaker");
                        map.remove("lensModel");
                    }
                }
            } else {
                // Nothing to detect from (offline without EXIF): not a
                // lookup that found nothing; tried again later.
                return false;
            }
        }

        {
            let has_valid_lens = match (
                map.get("lensMaker").and_then(|v| v.as_str()),
                map.get("lensModel").and_then(|v| v.as_str()),
            ) {
                (Some(maker), Some(model)) if !maker.is_empty() && !model.is_empty() => {
                    // A photo that cannot be opened (offline, no access) has
                    // camera facts still to come: not resolved at a guessed
                    // focal length, tried again later. A file that opens but
                    // has no EXIF never gets them; it takes the fallback.
                    if exif_data.is_none() && !readable(source) {
                        return false;
                    }
                    let mut focal_length = 50.0;
                    let mut aperture = None;
                    let mut distance = None;

                    if let Some(exif) = exif_data {
                        if let Some(fl_str) = exif
                            .get("FocalLength")
                            .or(exif.get("FocalLengthIn35mmFilm"))
                            && let Ok(fl) = fl_str.replace(" mm", "").trim().parse::<f32>()
                        {
                            focal_length = fl;
                        }
                        if let Some(ap_str) = exif.get("ApertureValue").or(exif.get("FNumber"))
                            && let Ok(ap) = ap_str.replace("f/", "").trim().parse::<f32>()
                        {
                            aperture = Some(ap);
                        }
                        if let Some(dist_str) = exif.get("SubjectDistance")
                            && let Ok(dist) = dist_str.replace(" m", "").trim().parse::<f32>()
                        {
                            distance = Some(dist);
                        }
                    }

                    // The body's crop factor from the 35 mm equivalent.
                    let camera_crop = exif_data.as_ref().and_then(|exif| {
                        let number = |key: &str| {
                            exif.get(key)?
                                .replace("mm", "")
                                .trim()
                                .parse::<f32>()
                                .ok()
                                .filter(|v| *v > 0.0)
                        };
                        Some(number("FocalLengthIn35mmFilm")? / number("FocalLength")?)
                    });
                    if let Some(params) = crate::lens_correction::resolve_lens_params(
                        db,
                        maker,
                        model,
                        focal_length,
                        aperture,
                        distance,
                        camera_crop.filter(|c| (0.5..=8.0).contains(c)),
                    ) {
                        map.insert(
                            "lensDistortionParams".to_string(),
                            serde_json::to_value(params).unwrap(),
                        );
                        true
                    } else {
                        false
                    }
                }
                _ => false,
            };

            if !has_valid_lens {
                map.remove("lensDistortionParams");
            }
            // A lens named as before a database rename (a saved edit, a
            // preset, copied settings) resolves to the renamed entry
            // (`find_lens_by_display_name`) and takes its current name.
            if has_valid_lens
                && let (Some(maker), Some(model)) = (
                    map.get("lensMaker").and_then(|v| v.as_str()),
                    map.get("lensModel").and_then(|v| v.as_str()),
                )
                && let Some((current, _)) =
                    crate::lens_correction::find_lens_by_display_name(db, maker, model)
                && current != model
            {
                map.insert("lensModel".to_string(), serde_json::json!(current));
            }
        }
        return true;
    }
    false
}

/// Whether the photo can be opened, so the camera facts read from it (or
/// their absence) are final.
fn readable(source: &Path) -> bool {
    std::fs::File::open(source).is_ok()
}

/// Whether the recipe was saved while its lens parameters could not be
/// resolved (`resolve_lens_params_in_adjustments`).
pub fn is_unresolved(recipe: &Value) -> bool {
    recipe.get(LENS_PROFILE_FOR).and_then(Value::as_str) == Some(LENS_UNRESOLVED)
}

/// Before rendering a recipe saved while its photo could not be read: its
/// lens parameters resolved now, from the photo's EXIF. An error when that
/// still cannot be done, rather than rendering missing or another lens's
/// parameters.
pub fn resolve_unresolved(
    recipe: &mut Value,
    exif_data: &Option<HashMap<String, String>>,
    lens_db: Option<&crate::lens_correction::LensDatabase>,
    source: &Path,
) -> Result<(), String> {
    if !is_unresolved(recipe) {
        return Ok(());
    }
    resolve_lens_params_in_adjustments(
        recipe,
        LensLookup::Keep { previous: None },
        exif_data,
        lens_db,
        source,
    );
    if is_unresolved(recipe) {
        return Err(format!(
            "{}: the lens correction could not be looked up (photo or lens database unavailable)",
            source.display()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lens_correction::{lenses_for_maker, load_lensfun_dir, resolve_lens_params};

    fn zoom_db(dir: &Path) -> (crate::lens_correction::LensDatabase, String) {
        std::fs::write(
            dir.join("lenses.xml"),
            r#"<lensdatabase><lens>
                <maker>Canon</maker><model>Canon RF 24-70mm F2.8L IS USM</model><mount>Canon RF</mount>
                <cropfactor>1.0</cropfactor>
                <calibration>
                    <distortion model="ptlens" focal="24" a="0.03" b="-0.09" c="0.06"/>
                    <distortion model="ptlens" focal="70" a="0.002" b="0.004" c="-0.003"/>
                </calibration>
            </lens></lensdatabase>"#,
        )
        .unwrap();
        let db = load_lensfun_dir(dir);
        let lenses = lenses_for_maker(&db, "Canon");
        let zoom = lenses[0].get_display_name(&lenses);
        (db, zoom)
    }

    #[test]
    fn camera_data_of_a_photo_that_cannot_be_read_stays_unresolved() {
        let mut recipe = serde_json::json!({
            "lensCorrectionMode": crate::embedded_lens::CAMERA_MODE,
            "lensDistortionParams": {"k1": 0.01},
        });
        resolve_lens_params_in_adjustments(
            &mut recipe,
            LensLookup::Fresh,
            &None,
            None,
            Path::new("/offline/volume/Job/Capture/P1000999.RW2"),
        );
        assert!(is_unresolved(&recipe));
        assert_eq!(
            recipe["lensDistortionParams"]["k1"],
            serde_json::json!(0.01)
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_manual_lens_on_a_file_without_read_access_stays_unresolved() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let (db, zoom) = zoom_db(tmp.path());
        let locked = tmp.path().join("IMG_0003.CR3");
        std::fs::write(&locked, b"raw").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::File::open(&locked).is_ok() {
            return; // running as root: every file opens
        }
        let mut recipe = serde_json::json!({
            "lensCorrectionMode": "manual", "lensMaker": "Canon", "lensModel": zoom,
        });
        resolve_lens_params_in_adjustments(
            &mut recipe,
            LensLookup::Keep { previous: None },
            &None,
            Some(&db),
            &locked,
        );
        assert!(is_unresolved(&recipe));
        assert!(recipe.get("lensDistortionParams").is_none());
    }

    #[test]
    fn an_unresolved_recipe_is_resolved_before_it_renders_or_fails_clearly() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, zoom) = zoom_db(tmp.path());
        let source = Path::new("/photos/IMG_0004.CR3");
        let mut recipe = serde_json::json!({
            "lensCorrectionMode": "manual", "lensMaker": "Canon", "lensModel": zoom,
            "lensProfileFor": LENS_UNRESOLVED,
        });
        // No lens database: an error, not a render without the correction.
        let mut no_db = recipe.clone();
        assert!(resolve_unresolved(&mut no_db, &None, None, source).is_err());

        let exif = Some(HashMap::from([(
            "FocalLength".to_string(),
            "24 mm".to_string(),
        )]));
        resolve_unresolved(&mut recipe, &exif, Some(&db), source).unwrap();
        let at_24 = resolve_lens_params(&db, "Canon", &zoom, 24.0, None, None, None).unwrap();
        assert_eq!(
            recipe["lensDistortionParams"],
            serde_json::to_value(at_24).unwrap()
        );
        assert!(!is_unresolved(&recipe));
        // A resolved recipe is left as it is.
        let before = recipe.clone();
        resolve_unresolved(&mut recipe, &None, None, source).unwrap();
        assert_eq!(recipe, before);
    }
}
