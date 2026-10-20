use std::collections::{VecDeque, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Weak};

use image::DynamicImage;

/// Cross-selection reuse needs every input that changes the prepared pixels,
/// including patch payloads, offsets/encoding and depth-map contents. Compute
/// this only at a reuse boundary, not for each ordinary tone-slider frame.
pub(crate) fn preparation_key(adjustments: &serde_json::Value) -> u64 {
    let mut hash = DefaultHasher::new();
    crate::cache_utils::calculate_transform_hash(adjustments).hash(&mut hash);
    crate::cache_utils::calculate_patched_warped_hash(adjustments).hash(&mut hash);
    hash.finish()
}

/// The key of a base built from a reduced source shown only until the
/// decode is in (no lateral CA measured on it, `editor_preview::
/// build_reduced_preview_base`): never the key of the same source's
/// measured base.
pub(crate) fn interim_key(preparation_key: u64) -> u64 {
    let mut hash = DefaultHasher::new();
    preparation_key.hash(&mut hash);
    "interim".hash(&mut hash);
    hash.finish()
}

#[derive(Clone)]
pub(crate) struct PreviewBase {
    pub image: Arc<DynamicImage>,
    pub scale: f32,
    pub unscaled_crop_offset: (f32, f32),
}

struct Entry {
    source: Weak<DynamicImage>,
    transform_hash: u64,
    preview_dim: u32,
    preview: PreviewBase,
}

/// Reuse the exact resampled pixels when returning to a decoded photograph.
/// Weak source identities neither pin decoded RAWs nor confuse a reloaded file
/// with an old version at the same path. Full-size source buffers stay in the
/// existing decoded/active caches instead of gaining another strong owner here.
pub(crate) struct PreviewBaseCache {
    entries: VecDeque<Entry>,
    byte_budget: usize,
    entry_limit: usize,
    max_entry_bytes: usize,
}

impl Default for PreviewBaseCache {
    fn default() -> Self {
        Self {
            entries: VecDeque::new(),
            byte_budget: 128 * 1024 * 1024,
            entry_limit: 4,
            max_entry_bytes: 64 * 1024 * 1024,
        }
    }
}

impl PreviewBaseCache {
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn get(
        &mut self,
        source: &Arc<DynamicImage>,
        transform_hash: u64,
        preview_dim: u32,
    ) -> Option<PreviewBase> {
        self.entries.retain(|entry| entry.source.strong_count() > 0);
        let index = self.entries.iter().position(|entry| {
            entry.source.ptr_eq(&Arc::downgrade(source))
                && entry.transform_hash == transform_hash
                && entry.preview_dim == preview_dim
        })?;
        let entry = self.entries.remove(index)?;
        let preview = entry.preview.clone();
        self.entries.push_back(entry);
        Some(preview)
    }

    pub fn insert(
        &mut self,
        source: &Arc<DynamicImage>,
        transform_hash: u64,
        preview_dim: u32,
        preview: PreviewBase,
    ) {
        let bytes = preview.image.as_bytes().len();
        if Arc::ptr_eq(source, &preview.image)
            || bytes > self.max_entry_bytes
            || bytes > self.byte_budget
            || self.entry_limit == 0
        {
            return;
        }
        let source = Arc::downgrade(source);
        self.entries.retain(|entry| {
            entry.source.strong_count() > 0
                && !(entry.source.ptr_eq(&source)
                    && entry.transform_hash == transform_hash
                    && entry.preview_dim == preview_dim)
        });
        let mut retained: usize = self
            .entries
            .iter()
            .map(|entry| entry.preview.image.as_bytes().len())
            .sum();
        while self.entries.len() >= self.entry_limit || retained + bytes > self.byte_budget {
            let Some(entry) = self.entries.pop_front() else {
                break;
            };
            retained -= entry.preview.image.as_bytes().len();
        }
        self.entries.push_back(Entry {
            source,
            transform_hash,
            preview_dim,
            preview,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source() -> Arc<DynamicImage> {
        Arc::new(DynamicImage::new_rgb8(8, 8))
    }

    fn preview() -> PreviewBase {
        PreviewBase {
            image: Arc::new(DynamicImage::new_rgb8(2, 2)),
            scale: 0.25,
            unscaled_crop_offset: (2.0, 3.0),
        }
    }

    #[test]
    fn cached_preview_requires_source_transform_and_resolution_and_shares_pixels() {
        let mut cache = PreviewBaseCache::default();
        let original = source();
        let preview = preview();
        cache.insert(&original, 10, 2, preview.clone());
        let hit = cache.get(&original, 10, 2).unwrap();
        assert!(Arc::ptr_eq(&hit.image, &preview.image));
        assert_eq!(hit.scale, 0.25);
        assert_eq!(hit.unscaled_crop_offset, (2.0, 3.0));
        assert!(cache.get(&original, 11, 2).is_none());
        assert!(cache.get(&original, 10, 4).is_none());
        assert!(cache.get(&source(), 10, 2).is_none());
        assert_eq!(Arc::strong_count(&original), 1);
        drop(original);
        assert!(cache.get(&source(), 10, 2).is_none());
        assert!(cache.entries.is_empty());
    }

    #[test]
    fn cache_evicts_by_bytes_in_lru_order_and_refuses_full_sources_or_oversized_entries() {
        let mut cache = PreviewBaseCache {
            byte_budget: 24,
            max_entry_bytes: 12,
            ..Default::default()
        };
        let a = source();
        let b = source();
        let c = source();
        cache.insert(&a, 0, 2, preview());
        cache.insert(&b, 0, 2, preview());
        assert!(cache.get(&a, 0, 2).is_some());
        cache.insert(&c, 0, 2, preview());
        assert!(cache.get(&b, 0, 2).is_none());
        assert!(cache.get(&a, 0, 2).is_some());
        assert!(cache.get(&c, 0, 2).is_some());
        let mut too_large = preview();
        too_large.image = source();
        cache.insert(&b, 0, 8, too_large);
        let mut full_source = preview();
        full_source.image = Arc::clone(&b);
        cache.insert(&b, 0, 8, full_source);
        assert!(cache.get(&b, 0, 8).is_none());
        assert_eq!(cache.entries.len(), 2);
    }

    #[test]
    fn persistent_keys_include_equal_length_patch_depth_payloads_and_patch_placement() {
        let adjustments = serde_json::json!({
            "aiPatches": [{"id":"patch", "visible":true, "patchData": {
                "color":"AAAA", "mask":"CCCC", "offsetX":1, "offsetY":2, "isSrgbEncoded":false
            }}],
            "lensBlurEnabled":true, "lensBlurDepthMap":"EEEE"
        });
        let original = source();
        let mut cache = PreviewBaseCache::default();
        let key = preparation_key(&adjustments);
        cache.insert(&original, key, 2, preview());
        for (pointer, value) in [
            ("/aiPatches/0/patchData/color", serde_json::json!("BBBB")),
            ("/aiPatches/0/patchData/mask", serde_json::json!("DDDD")),
            ("/aiPatches/0/patchData/offsetX", serde_json::json!(2)),
            ("/aiPatches/0/patchData/offsetY", serde_json::json!(3)),
            (
                "/aiPatches/0/patchData/isSrgbEncoded",
                serde_json::json!(true),
            ),
            ("/lensBlurDepthMap", serde_json::json!("FFFF")),
        ] {
            let mut changed = adjustments.clone();
            *changed.pointer_mut(pointer).unwrap() = value;
            // The active-preview shortcut alone cannot distinguish these recipes.
            assert_eq!(
                crate::cache_utils::calculate_transform_hash(&adjustments),
                crate::cache_utils::calculate_transform_hash(&changed),
            );
            let changed_key = preparation_key(&changed);
            assert_ne!(key, changed_key, "{pointer}");
            assert!(cache.get(&original, changed_key, 2).is_none());
        }
        let mut tone_change = adjustments.clone();
        tone_change["exposure"] = serde_json::json!(1.0);
        assert_eq!(key, preparation_key(&tone_change));
        assert!(
            cache
                .get(&original, preparation_key(&tone_change), 2)
                .is_some()
        );
    }

    #[test]
    fn an_interim_base_never_stands_in_for_a_measured_one() {
        // A Standard proxy rendered before the photograph was known to be
        // offline, then rendered as its lasting source.
        let proxy = source();
        let key = preparation_key(&serde_json::json!({"autoCaVersion": 3}));
        let mut cache = PreviewBaseCache::default();
        cache.insert(&proxy, interim_key(key), 2, preview());
        assert_ne!(interim_key(key), key);
        assert!(cache.get(&proxy, key, 2).is_none());
        assert!(cache.get(&proxy, interim_key(key), 2).is_some());
    }

    #[test]
    fn explicit_clear_releases_pixels_even_while_source_is_active() {
        let original = source();
        let prepared = preview();
        let weak_pixels = Arc::downgrade(&prepared.image);
        let mut cache = PreviewBaseCache::default();
        cache.insert(&original, 0, 2, prepared);
        assert!(weak_pixels.upgrade().is_some());
        cache.clear();
        assert!(weak_pixels.upgrade().is_none());
        assert!(cache.get(&original, 0, 2).is_none());
    }
}
