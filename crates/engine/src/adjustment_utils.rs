use image::DynamicImage;
use rawtonic_recipe::masks;
use std::borrow::Cow;
use std::collections::HashMap;

use crate::image_processing::{
    IntoCowImage, apply_coarse_rotation, apply_crop, apply_flip, apply_geometry_warp,
    apply_rotation, fused_spatial_transform,
};

/// The start of what the editor sends in a mask component's bitmap field
/// for a bitmap it sent whole before: `cached:<key>`, the key naming the
/// bitmap's content ([`bitmap_key`]). The native side fills it in from its
/// cache. A null field means the component has no bitmap (its inputs select
/// nothing, or its detection is still running) and renders nothing.
pub const CACHED_BITMAP: &str = "cached";

/// How many bitmaps the cache keeps under one component id: the one the
/// preview shows and those queued exports carried under the same id (an AI
/// selection computed again, an Object prompt) since, the oldest dropped.
const BITMAPS_PER_COMPONENT: usize = 8;

/// The key of a mask bitmap's content, as the editor computes it
/// (`bitmapKey` in src/utils/bitmapIds.ts): cyrb53 (public domain, by
/// bryc) over the string's UTF-16 code units, in decimal.
pub fn bitmap_key(bitmap: &str) -> String {
    let (mut h1, mut h2): (u32, u32) = (0xdeadbeef, 0x41c6ce57);
    for unit in bitmap.encode_utf16() {
        let unit = u32::from(unit);
        h1 = (h1 ^ unit).wrapping_mul(2_654_435_761);
        h2 = (h2 ^ unit).wrapping_mul(1_597_334_677);
    }
    h1 = (h1 ^ (h1 >> 16)).wrapping_mul(2_246_822_507);
    h1 ^= (h2 ^ (h2 >> 13)).wrapping_mul(3_266_489_909);
    h2 = (h2 ^ (h2 >> 16)).wrapping_mul(2_246_822_507);
    h2 ^= (h1 ^ (h1 >> 13)).wrapping_mul(3_266_489_909);
    ((u64::from(h2 & 0x1f_ffff) << 32) | u64::from(h1)).to_string()
}

/// Where `cache` keeps the bitmap of `key` under component `id`.
fn bitmap_entry(id: &str, key: &str) -> String {
    format!("mask-bitmap:{id}:{key}")
}

/// Where `cache` lists the keys of the bitmaps it holds under `id`, the
/// most recently used last.
fn bitmap_versions(id: &str) -> String {
    format!("mask-bitmaps:{id}")
}

/// Who a recipe being hydrated comes from: what it carries is kept as the
/// most recently used bitmap (the editor, which refers to its newest one
/// later), or as the least (a queued export, which carries its bitmaps
/// whole and never refers to them later): exports carrying other versions
/// of a mask push out each other, never the one the editor refers to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sender {
    Editor,
    QueuedExport,
}

/// Marks `key` as the most recently used bitmap under `id` (as the least
/// for a [`Sender::QueuedExport`]), dropping the oldest beyond
/// [`BITMAPS_PER_COMPONENT`] other than `key`.
fn touch_bitmap(
    cache: &mut HashMap<String, serde_json::Value>,
    id: &str,
    key: &str,
    sender: Sender,
) {
    let versions_key = bitmap_versions(id);
    let mut versions: Vec<String> = cache
        .get(&versions_key)
        .and_then(|v| v.as_array())
        .map(|keys| {
            keys.iter()
                .filter_map(|k| k.as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let known = versions.iter().position(|k| k == key);
    if let Some(held) = known {
        versions.remove(held);
    }
    match sender {
        Sender::Editor => versions.push(key.to_string()),
        // A bitmap the cache holds keeps its place (the editor may be
        // showing it); a new one comes in as the least recently used.
        Sender::QueuedExport => versions.insert(known.unwrap_or(0), key.to_string()),
    }
    while versions.len() > BITMAPS_PER_COMPONENT {
        let oldest = versions.iter().position(|k| k != key).unwrap_or(0);
        let oldest = versions.remove(oldest);
        cache.remove(&bitmap_entry(id, &oldest));
    }
    cache.insert(versions_key, serde_json::json!(versions));
}

/// Fills in the mask bitmaps `sub_masks` refer to as `cached:<key>` from
/// `cache` and remembers the ones they carry, by component id and content.
/// Requests run independently (the preview, queued exports carrying an
/// older or newer bitmap under the same id, overlays): each one's
/// reference names the bitmap it means, so none takes another state's
/// bitmap. A component whose bitmap is null renders none and evicts
/// nothing. A reference the cache cannot fill becomes null.
pub fn hydrate_sub_masks(
    sub_masks: &mut Vec<serde_json::Value>,
    cache: &mut HashMap<String, serde_json::Value>,
) {
    hydrate_sub_masks_from(sub_masks, cache, Sender::Editor);
}

/// [`hydrate_sub_masks`] for a recipe from `sender`.
fn hydrate_sub_masks_from(
    sub_masks: &mut Vec<serde_json::Value>,
    cache: &mut HashMap<String, serde_json::Value>,
    sender: Sender,
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
            for key in ["mask_data_base64", masks::MASK_DATA_BASE64.into_name()] {
                let Some(val) = params.get_mut(key) else {
                    continue;
                };
                let Some(text) = val.as_str() else {
                    continue;
                };
                if let Some(content) = text
                    .strip_prefix(CACHED_BITMAP)
                    .and_then(|rest| rest.strip_prefix(':'))
                {
                    let content = content.to_string();
                    match cache.get(&bitmap_entry(&id, &content)).cloned() {
                        Some(bitmap) => {
                            *val = bitmap;
                            if sender == Sender::Editor {
                                touch_bitmap(cache, &id, &content, sender);
                            }
                        }
                        None => *val = serde_json::Value::Null,
                    }
                } else if text == CACHED_BITMAP {
                    // A reference that names no content cannot tell which
                    // bitmap it means.
                    *val = serde_json::Value::Null;
                } else {
                    let content = bitmap_key(text);
                    cache.insert(bitmap_entry(&id, &content), val.clone());
                    touch_bitmap(cache, &id, &content, sender);
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
    hydrate_adjustments_from(cache, adjustments, Sender::Editor);
}

/// [`hydrate_adjustments_from_cache`] for a recipe from `sender`.
pub fn hydrate_adjustments_from(
    cache: &mut HashMap<String, serde_json::Value>,
    adjustments: &mut serde_json::Value,
    sender: Sender,
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

            if let Some(sub_masks) = patch
                .get_mut(masks::SUB_MASKS.name())
                .and_then(|v| v.as_array_mut())
            {
                hydrate_sub_masks_from(sub_masks, cache, sender);
            }
        }
    }

    if let Some(masks) = adjustments
        .get_mut(masks::MASKS.name())
        .and_then(|v| v.as_array_mut())
    {
        for mask_container in masks {
            if let Some(sub_masks) = mask_container
                .get_mut(masks::SUB_MASKS.name())
                .and_then(|v| v.as_array_mut())
            {
                hydrate_sub_masks_from(sub_masks, cache, sender);
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

/// The spatial steps of a recipe, as [`apply_spatial_transformations`]
/// reads them.
struct SpatialSteps {
    orientation_steps: u8,
    rotation_degrees: f32,
    flip_horizontal: bool,
    flip_vertical: bool,
    crop_json: serde_json::Value,
    unscaled_crop_offset: (f32, f32),
}

impl SpatialSteps {
    fn of(adjustments: &serde_json::Value) -> Self {
        let geometry =
            rawtonic_recipe::geometry::resolve(rawtonic_recipe::StoredRef::new(adjustments));
        let crop_data = geometry.crop;
        Self {
            orientation_steps: geometry.orientation_steps.get() as u8,
            rotation_degrees: geometry.rotation.get() as f32,
            flip_horizontal: geometry.flip_horizontal,
            flip_vertical: geometry.flip_vertical,
            crop_json: serde_json::to_value(crop_data).unwrap_or(serde_json::Value::Null),
            unscaled_crop_offset: crop_data.map_or((0.0, 0.0), |c| (c.x as f32, c.y as f32)),
        }
    }

    fn reorients(&self) -> bool {
        matches!(self.orientation_steps, 1..=3) || self.flip_horizontal || self.flip_vertical
    }
}

/// When all that follows the edge-fringe pass in
/// [`apply_all_transformations`] on a `width × height` warped frame is a
/// crop (no lens blur, orientation, flip or rotation): the window it keeps
/// (x, y, width, height; the whole frame without a crop) and the crop
/// offset that function returns.
pub fn crop_after_fringe(
    adjustments: &serde_json::Value,
    width: u32,
    height: u32,
) -> Option<([u32; 4], (f32, f32))> {
    let steps = SpatialSteps::of(adjustments);
    if crate::lens_blur::may_apply(adjustments)
        || steps.reorients()
        || steps.rotation_degrees % 360.0 != 0.0
    {
        return None;
    }
    let (x, y, w, h) = crate::image_processing::crop_rect(&steps.crop_json, width, height)
        .unwrap_or((0, 0, width, height));
    Some(([x, y, w, h], steps.unscaled_crop_offset))
}

pub fn apply_spatial_transformations<'a, I: IntoCowImage<'a>>(
    image: I,
    adjustments: &serde_json::Value,
) -> (Cow<'a, DynamicImage>, (f32, f32)) {
    let keep_negative = crate::recipe_versions::RenderVersion::of(adjustments).keeps_negative();
    let steps = SpatialSteps::of(adjustments);
    let reorients = steps.reorients();
    let SpatialSteps {
        orientation_steps,
        rotation_degrees,
        flip_horizontal,
        flip_vertical,
        crop_json,
        unscaled_crop_offset,
    } = steps;

    let image = image.into_cow();
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

/// The geometry stage of a render: warp, edge-fringe pass, lens blur, then
/// orientation, rotation and crop. Fails where the edge-fringe pass's or
/// the warp's GPU version cannot run. `is_raw`: `image` is a RAW's decode,
/// not a photograph's encoded values (`apply_geometry_warp`).
pub fn apply_all_transformations<'a, I: IntoCowImage<'a>>(
    image: I,
    adjustments: &serde_json::Value,
    is_raw: bool,
) -> Result<(Cow<'a, DynamicImage>, (f32, f32)), String> {
    let start_time = std::time::Instant::now();
    let warped_image = apply_warp(image, adjustments, is_raw)?;
    let result = apply_transformations_after_warp(warped_image, adjustments);
    log::info!(
        "apply_all_transformations took {:.2?}",
        start_time.elapsed()
    );
    result
}

/// The warp of [`apply_all_transformations`] (fails only where it runs on
/// the GPU and the GPU fails).
pub fn apply_warp<'a, I: IntoCowImage<'a>>(
    image: I,
    adjustments: &serde_json::Value,
    is_raw: bool,
) -> Result<Cow<'a, DynamicImage>, String> {
    #[cfg(feature = "performance-profiling")]
    let _stage = crate::performance::PerformanceStage::new("transform_warp");
    apply_geometry_warp(image.into_cow(), adjustments, is_raw)
}

/// What follows the warp in [`apply_all_transformations`]: the edge-fringe
/// pass, lens blur, then orientation, rotation and crop.
pub fn apply_transformations_after_warp<'a>(
    warped_image: Cow<'a, DynamicImage>,
    adjustments: &serde_json::Value,
) -> Result<(Cow<'a, DynamicImage>, (f32, f32)), String> {
    #[cfg(feature = "performance-profiling")]
    let fringe_stage = crate::performance::PerformanceStage::new("transform_fringe");
    let warped_image = crate::edge_fringe::apply_for_recipe(warped_image, adjustments, 1.0)?;
    #[cfg(feature = "performance-profiling")]
    drop(fringe_stage);
    let blurred_image = crate::lens_blur::apply_lens_blur(warped_image, adjustments);
    Ok(apply_spatial_transformations(blurred_image, adjustments))
}

#[cfg(test)]
mod hydration_tests {
    #[test]
    fn a_pasted_selection_waiting_for_its_detection_takes_no_cached_bitmap() {
        let mut cache = HashMap::new();
        let mut source = vec![
            serde_json::json!({"id": "sky-c", "parameters": {"maskDataBase64": "data:source"}}),
        ];
        hydrate_sub_masks(&mut source, &mut cache);
        let mut subs = vec![
            serde_json::json!({"id": "sky-c", "parameters": {"maskDataBase64": null, "detectPending": true}}),
            serde_json::json!({"id": "sky-c", "parameters": {"maskDataBase64": refer("data:source")}}),
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
    fn a_component_without_a_bitmap_renders_none() {
        let mut cache = HashMap::new();
        let mut first = vec![json!({"id": "p", "parameters": {"maskDataBase64": "data:lips"}})];
        hydrate_sub_masks(&mut first, &mut cache);

        // Its last part unticked: no bitmap, not the cached one.
        let mut cleared =
            vec![json!({"id": "p", "parameters": {"maskDataBase64": null, "parts": []}})];
        hydrate_sub_masks(&mut cleared, &mut cache);
        assert_eq!(cleared[0]["parameters"]["maskDataBase64"], json!(null));
    }

    #[test]
    fn an_older_request_without_the_bitmap_keeps_a_newer_one_cached() {
        // An overlay asked while the selection was computed, hydrated after
        // the preview that brought the result.
        let mut cache = HashMap::new();
        let mut preview = vec![json!({"id": "p", "parameters": {"maskDataBase64": "data:new"}})];
        hydrate_sub_masks(&mut preview, &mut cache);
        let mut overlay = vec![json!({"id": "p", "parameters": {"maskDataBase64": null}})];
        hydrate_sub_masks(&mut overlay, &mut cache);
        let mut later =
            vec![json!({"id": "p", "parameters": {"maskDataBase64": refer("data:new")}})];
        hydrate_sub_masks(&mut later, &mut cache);
        assert_eq!(later[0]["parameters"]["maskDataBase64"], "data:new");
        let mut unknown =
            vec![json!({"id": "q", "parameters": {"maskDataBase64": refer("data:new")}})];
        hydrate_sub_masks(&mut unknown, &mut cache);
        assert_eq!(unknown[0]["parameters"]["maskDataBase64"], json!(null));
    }

    #[test]
    fn a_sent_bitmap_is_referred_to_by_its_component_id() {
        let mut cache = HashMap::new();
        let mut first = vec![json!({"id": "p", "parameters": {"maskDataBase64": "data:lips"}})];
        hydrate_sub_masks(&mut first, &mut cache);
        let mut later =
            vec![json!({"id": "p", "parameters": {"maskDataBase64": refer("data:lips")}})];
        hydrate_sub_masks(&mut later, &mut cache);
        assert_eq!(later, first);
    }

    /// What the editor sends for a bitmap it sent whole before.
    fn refer(bitmap: &str) -> String {
        format!("{CACHED_BITMAP}:{}", bitmap_key(bitmap))
    }

    /// One request's bitmap of component `id` after hydration.
    fn hydrated(
        cache: &mut HashMap<String, serde_json::Value>,
        id: &str,
        sent: &str,
    ) -> serde_json::Value {
        let mut subs = vec![json!({"id": id, "parameters": {"maskDataBase64": sent}})];
        hydrate_sub_masks(&mut subs, cache);
        subs[0]["parameters"]["maskDataBase64"].clone()
    }

    #[test]
    fn the_editor_and_the_native_side_key_a_bitmap_alike() {
        // The same values as `bitmapKey` in src/utils/bitmapIds.test.ts.
        assert_eq!(bitmap_key(""), "3338908027751811");
        assert_eq!(bitmap_key("data:thumb"), "7596472580574772");
        assert_eq!(bitmap_key("data:full"), "6409932804508894");
        assert_eq!(bitmap_key("é✓😀"), "2813155200082369");
    }

    #[test]
    fn an_export_queued_before_an_undo_leaves_the_preview_its_bitmap() {
        // A migrated Sky selection computed again on the full frame, an
        // export of it queued, then Undo back to the stretched bitmap: the
        // preview sends that whole, the queued export (pinned with the
        // full-frame one) runs after it, and the next preview refers to
        // the stretched one.
        let mut cache = HashMap::new();
        hydrated(&mut cache, "sky", "data:thumb");
        hydrated(&mut cache, "sky", "data:full");
        hydrated(&mut cache, "sky", "data:thumb");
        assert_eq!(hydrated(&mut cache, "sky", "data:full"), "data:full");
        assert_eq!(
            hydrated(&mut cache, "sky", &refer("data:thumb")),
            "data:thumb"
        );
        // Redo refers to the full-frame one, which the export left there.
        assert_eq!(
            hydrated(&mut cache, "sky", &refer("data:full")),
            "data:full"
        );
    }

    #[test]
    fn an_object_prompt_undone_with_an_export_queued_keeps_each_bitmap() {
        let mut cache = HashMap::new();
        hydrated(&mut cache, "object", "data:first-prompt");
        hydrated(&mut cache, "object", "data:second-prompt");
        // Undo of the second prompt, then the queued export of it.
        hydrated(&mut cache, "object", "data:first-prompt");
        hydrated(&mut cache, "object", "data:second-prompt");
        assert_eq!(
            hydrated(&mut cache, "object", &refer("data:first-prompt")),
            "data:first-prompt"
        );
        // A reference naming no content (an older editor) takes no bitmap.
        assert_eq!(hydrated(&mut cache, "object", CACHED_BITMAP), json!(null));
    }

    #[test]
    fn queued_exports_never_push_out_the_bitmap_the_idle_preview_refers_to() {
        let mut cache = HashMap::new();
        hydrated(&mut cache, "o", "data:shown");
        // More queued exports of other versions than the cache keeps, while
        // the preview sends nothing.
        for prompt in 0..2 * BITMAPS_PER_COMPONENT {
            let mut subs = vec![
                json!({"id": "o", "parameters": {"maskDataBase64": format!("data:{prompt}")}}),
            ];
            hydrate_sub_masks_from(&mut subs, &mut cache, Sender::QueuedExport);
            assert_eq!(
                subs[0]["parameters"]["maskDataBase64"],
                format!("data:{prompt}")
            );
        }
        assert_eq!(
            hydrated(&mut cache, "o", &refer("data:shown")),
            "data:shown"
        );
        let held = cache
            .keys()
            .filter(|k| k.starts_with("mask-bitmap:o:"))
            .count();
        assert_eq!(held, BITMAPS_PER_COMPONENT);
    }

    #[test]
    fn a_queued_export_of_the_shown_bitmap_leaves_it_where_the_editor_put_it() {
        let mut cache = HashMap::new();
        let export = |cache: &mut HashMap<String, serde_json::Value>, bitmap: &str| {
            let mut subs = vec![json!({"id": "o", "parameters": {"maskDataBase64": bitmap}})];
            hydrate_sub_masks_from(&mut subs, cache, Sender::QueuedExport);
        };
        // Exports of several revisions queued; Develop shows one of them.
        for prompt in 0..BITMAPS_PER_COMPONENT {
            hydrated(&mut cache, "o", &format!("data:{prompt}"));
        }
        hydrated(&mut cache, "o", "data:shown");
        // The queue drains: the export of the shown revision, then others.
        export(&mut cache, "data:shown");
        for prompt in 0..2 * BITMAPS_PER_COMPONENT {
            export(&mut cache, &format!("data:new-{prompt}"));
        }
        assert_eq!(
            hydrated(&mut cache, "o", &refer("data:shown")),
            "data:shown"
        );
    }

    #[test]
    fn the_cache_keeps_a_few_bitmaps_per_component_the_used_one_last() {
        let mut cache = HashMap::new();
        hydrated(&mut cache, "o", "data:shown");
        for prompt in 0..BITMAPS_PER_COMPONENT {
            hydrated(&mut cache, "o", &format!("data:{prompt}"));
            // The preview refers to its bitmap between the exports.
            assert_eq!(
                hydrated(&mut cache, "o", &refer("data:shown")),
                "data:shown"
            );
        }
        assert_eq!(hydrated(&mut cache, "o", &refer("data:0")), json!(null));
        let held = cache
            .keys()
            .filter(|k| k.starts_with("mask-bitmap:o:"))
            .count();
        assert_eq!(held, BITMAPS_PER_COMPONENT);
    }

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
