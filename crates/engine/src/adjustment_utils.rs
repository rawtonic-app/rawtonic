use image::DynamicImage;
use std::borrow::Cow;
use std::collections::HashMap;

use crate::image_processing::{
    Crop, IntoCowImage, apply_coarse_rotation, apply_crop, apply_flip, apply_geometry_warp,
    apply_rotation, fused_spatial_transform,
};

pub fn hydrate_sub_masks(
    sub_masks: &mut Vec<serde_json::Value>,
    cache: &mut HashMap<String, serde_json::Value>,
) {
    for sub_mask in sub_masks {
        let id = sub_mask
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        if id.is_empty() {
            continue;
        }

        if let Some(params) = sub_mask
            .get_mut("parameters")
            .and_then(|p| p.as_object_mut())
        {
            // Pasted from another photograph and waiting for its own
            // detection: never filled with a bitmap cached under its id
            // (the source's, which shares the id).
            if params
                .get(crate::masks::DETECT_PENDING)
                .and_then(serde_json::Value::as_bool)
                == Some(true)
            {
                continue;
            }
            let keys_to_check = ["mask_data_base64", "maskDataBase64"];
            for key in keys_to_check {
                if params.contains_key(key) {
                    let val = params.get(key).unwrap();
                    if !val.is_null() {
                        cache.insert(id.clone(), val.clone());
                    } else {
                        if let Some(cached_data) = cache.get(&id) {
                            params.insert(key.to_string(), cached_data.clone());
                        }
                    }
                }
            }
        }
    }
}

/// Restores AI patch and mask bitmaps a recipe refers to by id from `cache`,
/// and remembers the ones it carries (the editor sends each bitmap once).
pub fn hydrate_adjustments_from_cache(
    cache: &mut HashMap<String, serde_json::Value>,
    adjustments: &mut serde_json::Value,
) {
    if let Some(patches) = adjustments
        .get_mut("aiPatches")
        .and_then(|v| v.as_array_mut())
    {
        for patch in patches {
            let id = patch
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();

            if !id.is_empty() {
                let has_data = patch.get("patchData").is_some_and(|v| !v.is_null());

                if has_data {
                    if let Some(data) = patch.get("patchData") {
                        cache.insert(id.clone(), data.clone());
                    }
                } else {
                    if let Some(cached_data) = cache.get(&id) {
                        patch["patchData"] = cached_data.clone();
                    }
                }
            }

            if let Some(sub_masks) = patch.get_mut("subMasks").and_then(|v| v.as_array_mut()) {
                hydrate_sub_masks(sub_masks, cache);
            }
        }
    }

    if let Some(masks) = adjustments.get_mut("masks").and_then(|v| v.as_array_mut()) {
        for mask_container in masks {
            if let Some(sub_masks) = mask_container
                .get_mut("subMasks")
                .and_then(|v| v.as_array_mut())
            {
                hydrate_sub_masks(sub_masks, cache);
            }
        }
    }

    // Relight depth and imported patterns, named by the editor
    // (src/utils/relight.ts `stripSentRelightData`).
    if let Some(depth) = adjustments
        .get_mut("relightDepth")
        .and_then(|v| v.as_object_mut())
        && let Some(key) = depth
            .get("cacheKey")
            .and_then(|v| v.as_str())
            .map(|k| format!("relight-depth:{k}"))
    {
        hydrate_fields(cache, key, depth, &["data", "faces", "subject"]);
    }
    for light in adjustments
        .get_mut("relightLights")
        .and_then(|v| v.as_array_mut())
        .into_iter()
        .flatten()
    {
        if let Some(pattern) = light
            .get_mut("customPattern")
            .and_then(|v| v.as_object_mut())
            && let Some(key) = pattern
                .get("id")
                .and_then(|v| v.as_str())
                .map(|id| format!("relight-pattern:{id}"))
        {
            hydrate_fields(cache, key, pattern, &["data"]);
        }
    }
}

/// Remembers `fields` when the object carries its bitmap (`data`), else
/// restores them from an earlier request.
fn hydrate_fields(
    cache: &mut HashMap<String, serde_json::Value>,
    key: String,
    object: &mut serde_json::Map<String, serde_json::Value>,
    fields: &[&str],
) {
    if object.get("data").is_some_and(|v| !v.is_null()) {
        let saved = fields
            .iter()
            .filter_map(|f| object.get(*f).map(|v| (f.to_string(), v.clone())))
            .collect();
        cache.insert(key, serde_json::Value::Object(saved));
    } else if let Some(saved) = cache.get(&key).and_then(|v| v.as_object()) {
        // Only the fields the bitmap's request carried (a version-five
        // `subject` is absent from older analyses).
        for (field, value) in saved {
            object.insert(field.clone(), value.clone());
        }
    }
}

pub fn apply_spatial_transformations<'a, I: IntoCowImage<'a>>(
    image: I,
    adjustments: &serde_json::Value,
) -> (Cow<'a, DynamicImage>, (f32, f32)) {
    let orientation_steps = adjustments["orientationSteps"].as_u64().unwrap_or(0) as u8;
    let rotation_degrees = adjustments["rotation"].as_f64().unwrap_or(0.0) as f32;
    let flip_horizontal = adjustments["flipHorizontal"].as_bool().unwrap_or(false);
    let flip_vertical = adjustments["flipVertical"].as_bool().unwrap_or(false);
    let keep_negative = crate::image_processing::render_version(adjustments) >= 2;

    let crop_data: Option<Crop> = serde_json::from_value(adjustments["crop"].clone()).ok();
    let crop_json = serde_json::to_value(crop_data).unwrap_or(serde_json::Value::Null);
    let unscaled_crop_offset = crop_data.map_or((0.0, 0.0), |c| (c.x as f32, c.y as f32));

    let image = image.into_cow();
    let reorients = matches!(orientation_steps, 1..=3) || flip_horizontal || flip_vertical;
    if (reorients || rotation_degrees % 360.0 != 0.0)
        && let Some(transformed) = fused_spatial_transform(
            &image,
            orientation_steps,
            flip_horizontal,
            flip_vertical,
            rotation_degrees,
            &crop_json,
            keep_negative,
        )
    {
        return (Cow::Owned(transformed), unscaled_crop_offset);
    }

    let coarse_rotated_image = apply_coarse_rotation(image, orientation_steps);
    let flipped_image = apply_flip(coarse_rotated_image, flip_horizontal, flip_vertical);
    let rotated_image = apply_rotation(flipped_image, rotation_degrees, keep_negative);
    let cropped_image = apply_crop(rotated_image, &crop_json);

    (cropped_image, unscaled_crop_offset)
}

pub fn apply_all_transformations<'a, I: IntoCowImage<'a>>(
    image: I,
    adjustments: &serde_json::Value,
) -> (Cow<'a, DynamicImage>, (f32, f32)) {
    let start_time = std::time::Instant::now();
    let image = image.into_cow();

    let warped_image = apply_geometry_warp(image, adjustments);
    let warped_image = crate::edge_fringe::apply_for_recipe(warped_image, adjustments, 1.0);
    let blurred_image = crate::lens_blur::apply_lens_blur(warped_image, adjustments);

    let (cropped_image, unscaled_crop_offset) =
        apply_spatial_transformations(blurred_image, adjustments);

    let total_duration = start_time.elapsed();
    log::info!("apply_all_transformations took {:.2?}", total_duration);

    (cropped_image, unscaled_crop_offset)
}

#[cfg(test)]
mod relight_hydration_tests {
    #[test]
    fn a_pasted_selection_waiting_for_its_detection_takes_no_cached_bitmap() {
        let mut cache = HashMap::new();
        cache.insert("sky-c".to_string(), serde_json::json!("data:source"));
        let mut subs = vec![
            serde_json::json!({"id": "sky-c", "parameters": {"maskDataBase64": null, "detectPending": true}}),
            serde_json::json!({"id": "sky-c", "parameters": {"maskDataBase64": null}}),
        ];
        hydrate_sub_masks(&mut subs, &mut cache);
        assert_eq!(
            subs[0]["parameters"]["maskDataBase64"],
            serde_json::Value::Null
        );
        assert_eq!(subs[1]["parameters"]["maskDataBase64"], "data:source");
    }

    use super::*;
    use serde_json::json;

    #[test]
    fn relight_bitmaps_are_sent_once_and_restored_by_name() {
        let mut cache = HashMap::new();
        let mut first = json!({
            "relightDepth": {"cacheKey": "d1", "data": "DEPTH", "faces": [[1]], "version": 2},
            "relightLights": [{"customPattern": {"id": "p1", "data": "PATTERN"}}, {}],
        });
        hydrate_adjustments_from_cache(&mut cache, &mut first);
        let mut later = json!({
            "relightDepth": {"cacheKey": "d1", "data": null, "faces": null, "version": 2},
            "relightLights": [{"customPattern": {"id": "p1", "data": null}}, {}],
        });
        hydrate_adjustments_from_cache(&mut cache, &mut later);
        assert_eq!(later, first);
        // Lights without patterns stay untouched (no inserted nulls).
        assert_eq!(later["relightLights"][1], json!({}));
        // A version-five subject travels with its depth.
        let mut first = json!({
            "relightDepth": {"cacheKey": "d5", "data": "DEPTH", "faces": [], "subject": "SUBJECT", "version": 5},
        });
        hydrate_adjustments_from_cache(&mut cache, &mut first);
        let mut later = json!({
            "relightDepth": {"cacheKey": "d5", "data": null, "faces": null, "subject": null, "version": 5},
        });
        hydrate_adjustments_from_cache(&mut cache, &mut later);
        assert_eq!(later, first);
    }
}
