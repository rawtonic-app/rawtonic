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
            } else if readable(source) {
                // A file that opens without EXIF: nothing to detect, for good.
                map.remove("lensMaker");
                map.remove("lensModel");
            } else {
                // Nothing to detect from yet (offline, no cached EXIF): not
                // a lookup that found nothing; tried again later.
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
                    if let Some(params) = lens_params_for_shot(db, maker, model, exif_data.as_ref())
                    {
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

/// The lens's profile for the photo whose EXIF map is `exif` (the Lens
/// tool's choice and every save look it up so: `shot_facts`).
pub fn lens_params_for_shot(
    db: &crate::lens_correction::LensDatabase,
    maker: &str,
    model: &str,
    exif: Option<&HashMap<String, String>>,
) -> Option<crate::lens_correction::LensDistortionParams> {
    let (focal_length, aperture, distance, camera_crop) = shot_facts(exif, db);
    crate::lens_correction::resolve_lens_params(
        db,
        maker,
        model,
        focal_length,
        aperture,
        distance,
        camera_crop,
    )
}

/// The leading number of an EXIF value as the maps store it ("f/8",
/// "75 mm", "6 EV", "50").
fn exif_number(exif: &HashMap<String, String>, key: &str) -> Option<f32> {
    let text = exif.get(key)?.trim();
    let text = text.strip_prefix("f/").unwrap_or(text);
    text.split(|c: char| c.is_whitespace() || c == 'm')
        .next()?
        .parse::<f32>()
        .ok()
        .filter(|v| v.is_finite() && *v > 0.0)
}

/// What a lens profile is looked up at: focal length (50 mm without one),
/// aperture, focus distance and the frame's crop factor. The aperture is
/// the FNumber; ApertureValue is APEX (f-number = 2^(Av/2)) and stands in
/// only without it. The crop factor is the frame's from the 35 mm
/// equivalent focal length where the photo states one, else the body's
/// (`lens_correction::body_crop_factor`). An equivalent equal to the focal
/// length says nothing: EXIF maps read before 2026-10-20 (kept in caches
/// and sidecars) copied the focal length into it, and a full-frame body's
/// crop factor is the body's anyway.
fn shot_facts(
    exif: Option<&HashMap<String, String>>,
    db: &crate::lens_correction::LensDatabase,
) -> (f32, Option<f32>, Option<f32>, Option<f32>) {
    let Some(exif) = exif else {
        return (50.0, None, None, None);
    };
    let focal_length = exif_number(exif, "FocalLength")
        .or_else(|| exif_number(exif, "FocalLengthIn35mmFilm"))
        .unwrap_or(50.0);
    let aperture = exif_number(exif, "FNumber")
        .or_else(|| exif_number(exif, "ApertureValue").map(|av| 2f32.powf(av / 2.0)));
    let distance = exif
        .get("SubjectDistance")
        .and_then(|d| d.replace(" m", "").trim().parse::<f32>().ok());
    let frame = exif_number(exif, "FocalLengthIn35mmFilm")
        .zip(exif_number(exif, "FocalLength"))
        .map(|(equivalent, focal)| equivalent / focal)
        .filter(|crop| (crop - 1.0).abs() > 1e-3);
    let field = |key: &str| exif.get(key).map(String::as_str).unwrap_or("");
    let camera_crop =
        crate::lens_correction::body_crop_factor(db, field("Make"), field("Model"), frame);
    (focal_length, aperture, distance, camera_crop)
}

/// Whether the photo can be read now, so the camera facts read from it (or
/// their absence) are final: it opens and is not a cloud file waiting to be
/// downloaded (reading that starts the download; as the app's
/// `preview_proxy::source_available`).
pub fn readable(source: &Path) -> bool {
    std::fs::File::open(source).is_ok() && !is_cloud_placeholder(source)
}

#[cfg(target_os = "macos")]
fn is_cloud_placeholder(source: &Path) -> bool {
    use std::os::macos::fs::MetadataExt;
    const SF_DATALESS: u32 = 0x4000_0000;
    std::fs::symlink_metadata(source).is_ok_and(|m| m.st_flags() & SF_DATALESS != 0)
}

#[cfg(not(target_os = "macos"))]
fn is_cloud_placeholder(_source: &Path) -> bool {
    false
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

    /// A lens with vignetting at f/5.6 and f/8 on an APS-C body.
    fn apsc_db(dir: &Path) -> (crate::lens_correction::LensDatabase, String) {
        std::fs::write(
            dir.join("lenses.xml"),
            r#"<lensdatabase><camera>
                <maker>Canon</maker><model>Canon EOS M100</model><mount>Canon EF-M</mount>
                <cropfactor>1.6</cropfactor>
            </camera><lens>
                <maker>Canon</maker><model>Canon EF 50mm f/1.8 STM</model><mount>Canon EF</mount>
                <cropfactor>1.0</cropfactor>
                <calibration>
                    <distortion model="ptlens" focal="50" a="0.01" b="-0.02" c="0.01"/>
                    <vignetting model="pa" focal="50" aperture="5.6" distance="10" k1="-0.5" k2="0.1" k3="0"/>
                    <vignetting model="pa" focal="50" aperture="8" distance="10" k1="-0.2" k2="0.05" k3="0"/>
                </calibration>
            </lens></lensdatabase>"#,
        )
        .unwrap();
        let db = load_lensfun_dir(dir);
        let lenses = lenses_for_maker(&db, "Canon");
        let lens = lenses[0].get_display_name(&lenses);
        (db, lens)
    }

    fn resolved(
        db: &crate::lens_correction::LensDatabase,
        lens: &str,
        exif: &[(&str, &str)],
    ) -> serde_json::Value {
        let mut recipe = serde_json::json!({
            "lensCorrectionMode": "manual", "lensMaker": "Canon", "lensModel": lens,
        });
        let exif = exif
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<HashMap<_, _>>();
        resolve_lens_params_in_adjustments(
            &mut recipe,
            LensLookup::Fresh,
            &Some(exif),
            Some(db),
            Path::new("/photos/IMG_0001.CR2"),
        );
        recipe["lensDistortionParams"].clone()
    }

    /// The vignetting row is the FNumber's: ApertureValue is APEX (6 = f/8)
    /// and stands in, converted, only without an FNumber.
    #[test]
    fn the_lens_profile_is_looked_up_at_the_f_number() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, lens) = apsc_db(tmp.path());
        let at = |aperture| {
            let params =
                resolve_lens_params(&db, "Canon", &lens, 50.0, Some(aperture), None, Some(1.6));
            serde_json::to_value(params.unwrap()).unwrap()
        };
        let base = [
            ("Make", "Canon"),
            ("Model", "Canon EOS M100"),
            ("FocalLength", "50"),
        ];
        let both = [
            base.as_slice(),
            &[("FNumber", "f/8"), ("ApertureValue", "f/6")],
        ]
        .concat();
        assert_eq!(resolved(&db, &lens, &both), at(8.0));
        let apex = [base.as_slice(), &[("ApertureValue", "f/6")]].concat();
        assert_eq!(resolved(&db, &lens, &apex), at(8.0));
        assert_ne!(at(8.0), at(5.6));
    }

    /// The profile is scaled to the body's crop factor: from the camera list
    /// when the EXIF map's 35 mm equivalent only repeats the focal length (as
    /// maps read before 2026-10-20 do for every CR2, CR3, RAF, ORF and RW2),
    /// from the equivalent where it states another frame.
    #[test]
    fn the_lens_profile_is_scaled_to_the_frames_crop_factor() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, lens) = apsc_db(tmp.path());
        let at = |crop| {
            let params =
                resolve_lens_params(&db, "Canon", &lens, 50.0, Some(8.0), None, Some(crop));
            serde_json::to_value(params.unwrap()).unwrap()
        };
        let base = [
            ("Make", "Canon"),
            ("Model", "Canon EOS M100"),
            ("FNumber", "f/8"),
        ];
        let copied = [
            base.as_slice(),
            &[("FocalLength", "50"), ("FocalLengthIn35mmFilm", "50")],
        ]
        .concat();
        assert_eq!(resolved(&db, &lens, &copied), at(1.6));
        let stated = [
            base.as_slice(),
            &[("FocalLength", "50"), ("FocalLengthIn35mmFilm", "75 mm")],
        ]
        .concat();
        assert_eq!(resolved(&db, &lens, &stated), at(1.5));
        assert_ne!(at(1.6), at(1.0));
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
    fn auto_on_a_readable_file_without_exif_finds_no_profile_and_exports() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = zoom_db(tmp.path());
        let scan = tmp.path().join("scan.png");
        std::fs::write(&scan, b"no exif").unwrap();
        let mut recipe = serde_json::json!({
            "lensCorrectionMode": "auto", "lensMaker": "Canon", "lensModel": "old",
            "lensDistortionParams": {"k1": 0.01},
        });
        resolve_lens_params_in_adjustments(&mut recipe, LensLookup::Fresh, &None, Some(&db), &scan);
        assert!(!is_unresolved(&recipe));
        assert!(recipe.get("lensModel").is_none());
        assert!(recipe.get("lensDistortionParams").is_none());
        // A recipe left unresolved by an earlier offline save exports.
        let mut saved_offline = serde_json::json!({
            "lensCorrectionMode": "auto", "lensProfileFor": LENS_UNRESOLVED,
        });
        resolve_unresolved(&mut saved_offline, &None, Some(&db), &scan).unwrap();
        assert!(!is_unresolved(&saved_offline));
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
