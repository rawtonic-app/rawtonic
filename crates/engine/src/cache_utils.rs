use image::DynamicImage;
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

pub const GEOMETRY_KEYS: &[&str] = &[
    "transformDistortion",
    "transformVertical",
    "transformHorizontal",
    "transformRotate",
    "transformAspect",
    "transformScale",
    "transformXOffset",
    "transformYOffset",
    "lensDistortionAmount",
    "lensVignetteAmount",
    "lensTcaAmount",
    "lensDistortionParams",
    "lensCorrectionMode",
    "lensMaker",
    "lensModel",
    "lensDistortionEnabled",
    "lensTcaEnabled",
    "autoChromaticAberration",
    "autoCaVersion",
    "fringeVersion",
    "lensVignetteEnabled",
    "guidedPerspective",
];

pub fn calculate_geometry_hash(adjustments: &serde_json::Value) -> u64 {
    let mut hasher = DefaultHasher::new();

    if let Some(patches) = adjustments.get("aiPatches") {
        patches.to_string().hash(&mut hasher);
    }
    if let Some(eyes) = adjustments.get("redEyes") {
        eyes.to_string().hash(&mut hasher);
    }
    if let Some(spots) = adjustments.get("spots") {
        spots.to_string().hash(&mut hasher);
    }
    if let Some(retouch) = adjustments.get("faceRetouch") {
        retouch.to_string().hash(&mut hasher);
    }

    for key in GEOMETRY_KEYS {
        if let Some(val) = adjustments.get(key) {
            key.hash(&mut hasher);
            val.to_string().hash(&mut hasher);
        }
    }
    hash_resampling(adjustments, &mut hasher);

    hasher.finish()
}

/// The warps' and straightening's resampling differs by render version
/// (`GeometryParams::keep_negative`); version 1 leaves the hash as it was.
fn hash_resampling(adjustments: &serde_json::Value, hasher: &mut DefaultHasher) {
    let version = crate::image_processing::render_version(adjustments);
    if version >= 2 {
        ("renderVersion", version).hash(hasher);
    }
}

pub fn calculate_patched_warped_hash(adjustments: &serde_json::Value) -> u64 {
    let mut hasher = DefaultHasher::new();

    calculate_geometry_hash(adjustments).hash(&mut hasher);

    let effects_visible = adjustments
        .get("sectionVisibility")
        .and_then(|v| v.get("effects"))
        .and_then(|s| s.as_bool())
        .unwrap_or(true);

    let blur_enabled = effects_visible && adjustments["lensBlurEnabled"].as_bool().unwrap_or(false);
    blur_enabled.hash(&mut hasher);

    if blur_enabled {
        let blur_keys = [
            "lensBlurAmount",
            "lensBlurDiffusion",
            "lensBlurShape",
            "lensBlurMinDepth",
            "lensBlurMaxDepth",
            "lensBlurMinFade",
            "lensBlurMaxFade",
            "lensBlurDepthMap",
        ];

        for key in blur_keys {
            if let Some(val) = adjustments.get(key) {
                key.hash(&mut hasher);
                val.to_string().hash(&mut hasher);
            }
        }
    }

    hasher.finish()
}

pub fn calculate_thumbnail_base_hash(adjustments: &serde_json::Value) -> u64 {
    let mut hasher = DefaultHasher::new();

    calculate_patched_warped_hash(adjustments).hash(&mut hasher);

    adjustments["orientationSteps"]
        .as_u64()
        .unwrap_or(0)
        .hash(&mut hasher);

    hasher.finish()
}

/// Key of the uncropped preview's base (the photo before its geometry), so
/// crop-mode geometry changes reuse it. Leaves the crop and the geometry out
/// unless the base carries retouching placed in the mask frame (patches,
/// spots, face retouch), which moves with the geometry.
pub fn calculate_visual_hash(path: &str, adjustments: &serde_json::Value) -> u64 {
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    let placed = crate::image_loader::has_base_retouch(adjustments);

    if let Some(obj) = adjustments.as_object() {
        for (key, value) in obj {
            let geometry = GEOMETRY_KEYS.contains(&key.as_str())
                || matches!(
                    key.as_str(),
                    "rotation" | "orientationSteps" | "flipHorizontal" | "flipVertical"
                );
            if key == "crop" || (geometry && !placed) {
                continue;
            }
            key.hash(&mut hasher);
            value.to_string().hash(&mut hasher);
        }
    }

    hasher.finish()
}

pub fn calculate_transform_hash(adjustments: &serde_json::Value) -> u64 {
    let mut hasher = DefaultHasher::new();

    let orientation_steps = adjustments["orientationSteps"].as_u64().unwrap_or(0);
    orientation_steps.hash(&mut hasher);

    let rotation = adjustments["rotation"].as_f64().unwrap_or(0.0);
    (rotation.to_bits()).hash(&mut hasher);

    let flip_h = adjustments["flipHorizontal"].as_bool().unwrap_or(false);
    flip_h.hash(&mut hasher);

    let flip_v = adjustments["flipVertical"].as_bool().unwrap_or(false);
    flip_v.hash(&mut hasher);

    let effects_visible = adjustments
        .get("sectionVisibility")
        .and_then(|v| v.get("effects"))
        .and_then(|s| s.as_bool())
        .unwrap_or(true);

    let blur_enabled = effects_visible && adjustments["lensBlurEnabled"].as_bool().unwrap_or(false);
    blur_enabled.hash(&mut hasher);
    if blur_enabled {
        if let Some(val) = adjustments.get("lensBlurAmount") {
            val.to_string().hash(&mut hasher);
        }
        if let Some(val) = adjustments.get("lensBlurDiffusion") {
            val.to_string().hash(&mut hasher);
        }
        if let Some(val) = adjustments.get("lensBlurShape") {
            val.as_str().unwrap_or("").hash(&mut hasher);
        }
        if let Some(val) = adjustments.get("lensBlurMinDepth") {
            val.to_string().hash(&mut hasher);
        }
        if let Some(val) = adjustments.get("lensBlurMaxDepth") {
            val.to_string().hash(&mut hasher);
        }
        if let Some(val) = adjustments.get("lensBlurMinFade") {
            val.to_string().hash(&mut hasher);
        }
        if let Some(val) = adjustments.get("lensBlurMaxFade") {
            val.to_string().hash(&mut hasher);
        }
        if let Some(val) = adjustments.get("lensBlurDepthMap") {
            val.as_str().unwrap_or("").len().hash(&mut hasher);
        }
    }

    if let Some(crop_val) = adjustments.get("crop")
        && !crop_val.is_null()
    {
        crop_val.to_string().hash(&mut hasher);
    }

    for key in GEOMETRY_KEYS {
        if let Some(val) = adjustments.get(key) {
            key.hash(&mut hasher);
            val.to_string().hash(&mut hasher);
        }
    }
    hash_resampling(adjustments, &mut hasher);

    if let Some(eyes) = adjustments.get("redEyes") {
        eyes.to_string().hash(&mut hasher);
    }
    if let Some(spots) = adjustments.get("spots") {
        spots.to_string().hash(&mut hasher);
    }
    if let Some(retouch) = adjustments.get("faceRetouch") {
        retouch.to_string().hash(&mut hasher);
    }

    if let Some(patches_val) = adjustments.get("aiPatches")
        && let Some(patches_arr) = patches_val.as_array()
    {
        patches_arr.len().hash(&mut hasher);

        for patch in patches_arr {
            if let Some(id) = patch.get("id").and_then(|v| v.as_str()) {
                id.hash(&mut hasher);
            }

            let is_visible = patch
                .get("visible")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            is_visible.hash(&mut hasher);

            if let Some(patch_data) = patch.get("patchData") {
                let color_len = patch_data
                    .get("color")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .len();
                color_len.hash(&mut hasher);

                let mask_len = patch_data
                    .get("mask")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .len();
                mask_len.hash(&mut hasher);
            } else {
                let data_len = patch
                    .get("patchDataBase64")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .len();
                data_len.hash(&mut hasher);
            }

            if let Some(sub_masks_val) = patch.get("subMasks") {
                sub_masks_val.to_string().hash(&mut hasher);
            }

            let invert = patch
                .get("invert")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            invert.hash(&mut hasher);
        }
    }

    hasher.finish()
}

pub fn calculate_full_job_hash(path: &str, adjustments: &serde_json::Value) -> u64 {
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    adjustments.to_string().hash(&mut hasher);
    hasher.finish()
}

/// Decoded RAWs are the largest retained buffers: 32-bit RGB, 12 bytes per
/// pixel (about 290 MB at 24 MP, 600 MB at 50 MP). The budget includes the open
/// photograph, which the editor holds anyway, so the one just left normally
/// stays cached for quick back-and-forth while older ones are decoded again.
const DECODED_CACHE_BYTES: usize = 768 * 1024 * 1024;

/// A kept decode with its EXIF and its measures.
pub type MeasuredDecode = (
    Arc<DynamicImage>,
    HashMap<String, String>,
    Option<crate::image_loader::DecodeMeasures>,
);

pub struct DecodedImageCache {
    capacity: usize,
    byte_budget: usize,
    items: Vec<(String, Arc<DynamicImage>, HashMap<String, String>)>,
    /// The measures of each kept decode (`image_loader::DecodeMeasures`),
    /// by its key.
    measures: HashMap<String, crate::image_loader::DecodeMeasures>,
}

impl DecodedImageCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            // Count alone permits several gigabytes of retained 32-bit RAW pixels.
            byte_budget: DECODED_CACHE_BYTES,
            items: Vec::with_capacity(capacity),
            measures: HashMap::new(),
        }
    }

    pub fn set_capacity(&mut self, capacity: usize) {
        self.capacity = capacity;
        while self.items.len() > self.capacity {
            self.items.remove(0);
        }
        self.forget_dropped_measures();
    }

    pub fn get(&mut self, path: &str) -> Option<(Arc<DynamicImage>, HashMap<String, String>)> {
        if let Some(pos) = self.items.iter().position(|(p, _, _)| p == path) {
            let item = self.items.remove(pos);
            let result = (item.1.clone(), item.2.clone());
            self.items.push(item);
            Some(result)
        } else {
            None
        }
    }

    /// `get` with the measures of the decode kept (None when none were
    /// measured: a fast development, a non-RAW).
    pub fn get_measured(&mut self, path: &str) -> Option<MeasuredDecode> {
        let (image, exif) = self.get(path)?;
        Some((image, exif, self.measures.get(path).copied()))
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.measures.clear();
    }

    fn forget_dropped_measures(&mut self) {
        let items = &self.items;
        self.measures
            .retain(|key, _| items.iter().any(|(path, _, _)| path == key));
    }

    /// Before decoding an uncached image, evict entries that could not stay
    /// beside a new image the size of the most recent one, so their memory is
    /// free during the decode peak rather than only after it. Photographs from
    /// one camera are similar in size; a smaller one simply keeps less here.
    pub fn make_room_for_next(&mut self) {
        let Some(expected) = self.items.last().map(|(_, img, _)| img.as_bytes().len()) else {
            return;
        };
        let mut retained: usize = self
            .items
            .iter()
            .map(|(_, img, _)| img.as_bytes().len())
            .sum();
        while !self.items.is_empty() && retained + expected > self.byte_budget {
            retained -= self.items.remove(0).1.as_bytes().len();
        }
        self.forget_dropped_measures();
    }

    pub fn insert(
        &mut self,
        path: String,
        image: Arc<DynamicImage>,
        exif: HashMap<String, String>,
        measures: Option<crate::image_loader::DecodeMeasures>,
    ) {
        if let Some(pos) = self.items.iter().position(|(p, _, _)| *p == path) {
            self.items.remove(pos);
        }
        self.measures.remove(&path);
        let bytes = image.as_bytes().len();
        if self.capacity == 0 || bytes > self.byte_budget {
            // The new image stays open regardless, so nothing else fits beside it.
            self.clear();
            return;
        }
        let mut retained_bytes: usize = self
            .items
            .iter()
            .map(|(_, img, _)| img.as_bytes().len())
            .sum();
        while self.items.len() >= self.capacity || retained_bytes + bytes > self.byte_budget {
            retained_bytes -= self.items.remove(0).1.as_bytes().len();
        }
        if let Some(measures) = measures {
            self.measures.insert(path.clone(), measures);
        }
        self.items.push((path, image, exif));
        self.forget_dropped_measures();
    }

    /// A decode made ahead for a photograph not open yet (the next one while
    /// stepping through photographs): kept only when it fits beside the
    /// decode used last (the open photograph's) within `budget` (at least
    /// the cache's own), so it never pushes that one out; older ones make
    /// room. Whether it is kept. Two 45 MP decodes take 1.1 GB, more than
    /// the cache keeps otherwise: the caller's budget follows the machine's
    /// memory.
    pub fn offer_ahead(
        &mut self,
        path: String,
        image: Arc<DynamicImage>,
        exif: HashMap<String, String>,
        measures: Option<crate::image_loader::DecodeMeasures>,
        budget: usize,
    ) -> bool {
        let budget = budget.max(self.byte_budget);
        if let Some(pos) = self.items.iter().position(|(p, _, _)| *p == path) {
            self.items.remove(pos);
        }
        let bytes = image.as_bytes().len();
        let last = self
            .items
            .last()
            .map_or(0, |(_, img, _)| img.as_bytes().len());
        if self.capacity < 2 || last + bytes > budget {
            self.forget_dropped_measures();
            return false;
        }
        let mut retained: usize = self
            .items
            .iter()
            .map(|(_, img, _)| img.as_bytes().len())
            .sum();
        while self.items.len() > 1
            && (self.items.len() >= self.capacity || retained + bytes > budget)
        {
            retained -= self.items.remove(0).1.as_bytes().len();
        }
        self.measures.remove(&path);
        if let Some(measures) = measures {
            self.measures.insert(path.clone(), measures);
        }
        self.items.push((path, image, exif));
        self.forget_dropped_measures();
        true
    }
}

#[cfg(test)]
mod resampling_hash_tests {
    use super::*;

    /// Warped and transformed pixels resample by render version: caches of
    /// one are not the other's; version 1 (or no key) hashes as before.
    #[test]
    fn geometry_and_transform_hashes_tell_render_versions_apart() {
        let recipe = |version: Option<u64>| {
            let mut recipe = serde_json::json!({"rotation": 2.0,
                "lensDistortionParams": {"k1": 0.01}, "lensCorrectionMode": "manual"});
            if let Some(version) = version {
                recipe["renderVersion"] = serde_json::json!(version);
            }
            recipe
        };
        for hash in [calculate_geometry_hash, calculate_transform_hash] {
            assert_eq!(hash(&recipe(None)), hash(&recipe(Some(1))));
            assert_ne!(hash(&recipe(Some(1))), hash(&recipe(Some(2))));
        }
    }
}

#[cfg(test)]
mod decoded_cache_tests {
    use super::*;

    #[test]
    fn cache_evicts_by_bytes_in_lru_order_and_handles_disabled_or_oversized_entries() {
        let mut cache = DecodedImageCache::new(5);
        cache.byte_budget = 24;
        let image = Arc::new(DynamicImage::new_rgb8(2, 2)); // 12 bytes
        for key in ["a", "b"] {
            cache.insert(key.into(), image.clone(), HashMap::new(), None);
        }
        assert!(cache.get("a").is_some());
        cache.insert("c".into(), image.clone(), HashMap::new(), None);
        assert!(cache.get("b").is_none());
        assert!(cache.get("a").is_some());
        assert!(cache.get("c").is_some());
        // An image larger than the budget is not cached, and since it is the
        // open photograph nothing else may be kept beside it.
        cache.insert(
            "large".into(),
            Arc::new(DynamicImage::new_rgb8(3, 3)),
            HashMap::new(),
            None,
        );
        assert!(cache.get("large").is_none());
        assert!(cache.items.is_empty());
        cache.insert("a".into(), image.clone(), HashMap::new(), None);
        cache.set_capacity(0);
        cache.insert("disabled".into(), image, HashMap::new(), None);
        assert!(cache.items.is_empty());
    }

    #[test]
    fn each_kept_decode_keeps_its_own_measures() {
        use crate::image_loader::DecodeMeasures;
        let mut cache = DecodedImageCache::new(2);
        let image = Arc::new(DynamicImage::new_rgb8(2, 2));
        let measures = |level| DecodeMeasures {
            noise_level: Some(level),
            image_key: Some([0.1, level, 0.05]),
        };
        // Two decodes of one file for two recipes.
        cache.insert(
            "A.RAF".into(),
            image.clone(),
            HashMap::new(),
            Some(measures(0.01)),
        );
        cache.insert(
            "A.RAF\u{1}v".into(),
            image.clone(),
            HashMap::new(),
            Some(measures(0.03)),
        );
        assert_eq!(cache.get_measured("A.RAF").unwrap().2, Some(measures(0.01)));
        assert_eq!(
            cache.get_measured("A.RAF\u{1}v").unwrap().2,
            Some(measures(0.03))
        );
        // Replaced without measures (a non-RAW), evicted, cleared: none left.
        cache.insert("A.RAF".into(), image.clone(), HashMap::new(), None);
        assert_eq!(cache.get_measured("A.RAF").unwrap().2, None);
        cache.insert(
            "B.RAF".into(),
            image.clone(),
            HashMap::new(),
            Some(measures(0.02)),
        );
        cache.insert("C.RAF".into(), image, HashMap::new(), Some(measures(0.04)));
        assert!(cache.get_measured("A.RAF\u{1}v").is_none());
        assert_eq!(cache.measures.len(), 2);
        cache.clear();
        assert!(cache.measures.is_empty());
    }

    #[test]
    fn budget_counts_the_open_photograph_and_keeps_the_previous_one_when_it_fits() {
        let mut cache = DecodedImageCache::new(5);
        // 24 MP and 33 MP RAWs at 12 bytes per pixel against the real budget.
        let megapixels = |mp: u32| Arc::new(DynamicImage::new_rgb32f(mp * 1000, 1000));
        cache.insert("a7iii-1".into(), megapixels(24), HashMap::new(), None);
        cache.insert("a7iii-2".into(), megapixels(24), HashMap::new(), None);
        cache.insert("a7iv".into(), megapixels(33), HashMap::new(), None);
        assert!(cache.get("a7iii-1").is_none(), "older RAW evicted");
        assert!(cache.get("a7iii-2").is_some(), "previous RAW kept");
        assert!(cache.get("a7iv").is_some(), "open RAW cached");
        // A 50 MP RAW alone fills the budget.
        cache.insert("fc9313".into(), megapixels(50), HashMap::new(), None);
        assert!(cache.get("fc9313").is_some());
        assert_eq!(cache.items.len(), 1);
        cache.insert("fc9313-large".into(), megapixels(70), HashMap::new(), None);
        assert!(cache.items.is_empty());
    }

    #[test]
    fn a_decode_made_ahead_never_pushes_out_the_open_photograph() {
        let mut cache = DecodedImageCache::new(5);
        let megapixels = |mp: u32| Arc::new(DynamicImage::new_rgb32f(mp * 1000, 1000));
        cache.insert("previous".into(), megapixels(24), HashMap::new(), None);
        cache.insert("open".into(), megapixels(24), HashMap::new(), None);
        // The next 24 MP photo fits beside the open one; the older one goes.
        assert!(cache.offer_ahead("next".into(), megapixels(24), HashMap::new(), None, 0));
        assert!(cache.get("previous").is_none());
        assert!(cache.get("open").is_some());
        assert!(cache.get("next").is_some());
        // Beside an open 45 MP photo a 45 MP one does not fit the cache's
        // own budget: not kept; within a larger budget it is.
        let mut large = DecodedImageCache::new(5);
        large.insert("open".into(), megapixels(45), HashMap::new(), None);
        assert!(!large.offer_ahead("next".into(), megapixels(45), HashMap::new(), None, 0));
        assert!(large.get("open").is_some());
        assert!(large.get("next").is_none());
        assert!(large.offer_ahead("next".into(), megapixels(45), HashMap::new(), None, 3 << 30));
        assert!(large.get("open").is_some());
        assert!(large.get("next").is_some());
        // The next decode kept ahead lets the one before go, never the open one.
        large.get("open");
        assert!(large.offer_ahead(
            "after".into(),
            megapixels(45),
            HashMap::new(),
            None,
            1200 << 20
        ));
        assert!(large.get("open").is_some() && large.get("after").is_some());
        assert!(large.get("next").is_none());
        // Nothing is kept ahead in a cache of one decode or none.
        let mut one = DecodedImageCache::new(1);
        assert!(!one.offer_ahead("next".into(), megapixels(12), HashMap::new(), None, 3 << 30));
    }

    #[test]
    fn room_is_made_before_decoding_when_a_similar_image_would_not_fit() {
        let mut cache = DecodedImageCache::new(5);
        let megapixels = |mp: u32| Arc::new(DynamicImage::new_rgb32f(mp * 1000, 1000));
        cache.make_room_for_next();
        assert!(cache.items.is_empty());
        // A 24 MP photo can stay beside another 24 MP decode.
        cache.insert("a7iii-1".into(), megapixels(24), HashMap::new(), None);
        cache.make_room_for_next();
        assert!(cache.get("a7iii-1").is_some());
        // Three 12 MP photos and one more fit together.
        let mut small = DecodedImageCache::new(5);
        for key in ["a", "b", "c"] {
            small.insert(key.into(), megapixels(12), HashMap::new(), None);
        }
        small.make_room_for_next();
        assert_eq!(small.items.len(), 3);
        // A 50 MP photo cannot stay beside another 50 MP decode: free it first.
        cache.insert("fc9313-1".into(), megapixels(50), HashMap::new(), None);
        assert_eq!(cache.items.len(), 1);
        cache.make_room_for_next();
        assert!(cache.items.is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_uncropped_base_follows_the_geometry_only_with_placed_retouching() {
        let turned = |recipe: &serde_json::Value| {
            let mut turned = recipe.clone();
            turned["rotation"] = json!(2.5);
            turned["orientationSteps"] = json!(1);
            turned["lensDistortionAmount"] = json!(40);
            turned["crop"] = json!({"x": 10.0, "y": 10.0, "width": 100.0, "height": 80.0});
            turned
        };
        // Without retouching the base is the same whatever the geometry.
        let plain = json!({"exposure": 0.5});
        assert_eq!(
            calculate_visual_hash("/a.CR3", &plain),
            calculate_visual_hash("/a.CR3", &turned(&plain))
        );
        // A spot sits in the mask frame: turning the photo moves it on the base.
        let spotted = json!({"exposure": 0.5, "spots": [{"x": 40.0, "y": 30.0, "radius": 8.0,
            "sourceX": 60.0, "sourceY": 30.0}]});
        assert_ne!(
            calculate_visual_hash("/a.CR3", &spotted),
            calculate_visual_hash("/a.CR3", &turned(&spotted))
        );
        // The crop alone still reuses it.
        let mut cropped = spotted.clone();
        cropped["crop"] = json!({"x": 1.0, "y": 2.0, "width": 50.0, "height": 40.0});
        assert_eq!(
            calculate_visual_hash("/a.CR3", &spotted),
            calculate_visual_hash("/a.CR3", &cropped)
        );
    }
}
