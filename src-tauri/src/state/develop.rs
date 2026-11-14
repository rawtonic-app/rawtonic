//! The develop session: the caches built from the open photograph and its
//! recipe (plan step 3.3), and the one place they are invalidated.
//!
//! Every cache here is keyed so that a recipe change cannot hit a stale
//! entry (transform, geometry, warp and mask hashes of the recipe; the
//! source's identity as a weak pointer or its path), so a recipe edit
//! invalidates nothing. What does invalidate them is an event on the
//! photograph's source or the decoder, each a [`Scope`]; see `README.md`
//! for which caches each scope drops and why the others are kept.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use image::DynamicImage;

use super::lock::lock_recover;
use crate::cache_utils::DecodedImageCache;
use crate::gpu_processing::GpuImageCache;
use crate::mask_generation::{FullWarpedMaskCache, MaskBitmapCache};

/// What happened, and so which caches no longer hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Another photograph became the open one (`image_loader::open_source`):
    /// the bitmaps the editor sent for the previous one (AI patches, AI
    /// masks, relight data) may share its ids. Not the source's caches: a
    /// stored proxy's frame carries the new photograph's bitmaps before its
    /// decode replaces the source.
    OtherPhoto,
    /// The open photograph's source bitmap was replaced (another photograph
    /// opened, a decode or AI Denoise replaced the proxy or the earlier
    /// decode): everything built from the source. The decodes and the
    /// reusable preview bases stay, keyed by their own source.
    Source,
    /// The decodes no longer hold: the decoder's settings changed
    /// (Preferences), or an external editor rewrote photographs (Edit With).
    /// Every decode and what was built from one, the multi view's held decodes and the decode kept
    /// for the next photograph. The masks, geometry bases and patches stay:
    /// the open source is not replaced until its photograph is opened again,
    /// which is a [`Scope::Source`].
    Decoder,
    /// The user cleared the decoded-RAW cache (Storage): the decodes only.
    DecodedCache,
    /// The preview base is built again (`editor_preview`): its GPU upload.
    /// Takes only the upload's lock, so it may be called while the preview
    /// lock is held.
    PreviewUpload,
}

pub type TransformedImageCache = (u64, Arc<DynamicImage>, (f32, f32));

#[derive(Clone)]
pub struct CachedPreview {
    pub source: std::sync::Weak<DynamicImage>,
    pub image: Arc<DynamicImage>,
    pub small_image: Option<Arc<DynamicImage>>,
    pub transform_hash: u64,
    pub scale: f32,
    pub unscaled_crop_offset: (f32, f32),
    pub preview_dim: u32,
    pub interactive_divisor: f32,
    /// Built from a reduced source shown only until the decode is in
    /// (`editor_preview::interim_source`).
    pub interim: bool,
    /// Built while a control was dragged, without the edge-fringe pass's
    /// chromatic-aberration stage (`edge_fringe::DRAFT_KEY`).
    pub fringe_draft: bool,
    /// The full preparation key of the recipe it was built for
    /// (`preview_cache::preparation_key`, draft keys included): checked where
    /// another recipe would take it (`serves_recipe`).
    pub preparation_key: u64,
}

impl CachedPreview {
    /// Whether this base serves a frame of `source` with `transform_hash`.
    /// An interim base never serves a lasting source: the same Standard
    /// proxy may turn out to be an offline photograph's only source. A
    /// fringe draft serves only frames while a control is dragged.
    pub fn serves(
        &self,
        source: &Arc<DynamicImage>,
        transform_hash: u64,
        interim: bool,
        interactive: bool,
    ) -> bool {
        self.source.ptr_eq(&Arc::downgrade(source))
            && self.transform_hash == transform_hash
            && self.interim == interim
            && (interactive || !self.fringe_draft)
    }

    /// Whether this base serves a preview frame with `transform_hash`: a
    /// drag's frames by the transform hash (`serves`), lasting frames also by
    /// the whole `preparation_key` (computed only then), since the transform
    /// hash takes some inputs by size only (depth maps, patch payloads).
    pub fn serves_frame(
        &self,
        source: &Arc<DynamicImage>,
        transform_hash: u64,
        interim: bool,
        interactive: bool,
        preparation_key: impl FnOnce() -> u64,
    ) -> bool {
        self.serves(source, transform_hash, interim, interactive)
            && (interactive || self.preparation_key == preparation_key())
    }

    /// Whether this base serves a lasting frame of another recipe with
    /// `(transform_hash, preparation_key)` (the split's before): the
    /// transform hash alone takes some inputs by size only (depth maps,
    /// patch payloads), so the whole preparation key must match too.
    pub fn serves_recipe(
        &self,
        source: &Arc<DynamicImage>,
        (transform_hash, preparation_key): (u64, u64),
        interim: bool,
    ) -> bool {
        self.serves(source, transform_hash, interim, false)
            && self.preparation_key == preparation_key
    }
}

/// The develop caches of the open photograph. Locks: `README.md` here.
pub struct DevelopSession {
    /// The decodes of recently opened photographs, by decode key.
    pub decoded_image_cache: Mutex<DecodedImageCache>,
    /// The open photograph's preview base for its recipe's transform.
    pub cached_preview: Mutex<Option<CachedPreview>>,
    /// Preview bases of recently shown photographs (weak source identity).
    pub(crate) reusable_previews: Mutex<crate::preview_cache::PreviewBaseCache>,
    /// The preview base's GPU upload.
    pub gpu_image_cache: Mutex<Option<GpuImageCache>>,
    /// Mask bitmaps for the preview.
    pub mask_cache: Mutex<MaskBitmapCache>,
    /// Masks drawn for the layer list's thumbnails, apart from the
    /// preview's so they never evict its full-size masks.
    pub mask_thumbnail_cache: Mutex<MaskBitmapCache>,
    /// The bitmaps the editor sent for the open photograph (AI patches, AI
    /// masks, relight data), by id.
    pub patch_cache: Mutex<HashMap<String, serde_json::Value>>,
    /// Pre-geometry bases by visual hash (tool results, negative preview).
    pub geometry_cache: Mutex<HashMap<u64, DynamicImage>>,
    /// The full-resolution warped mask source.
    pub full_warped_cache: Mutex<Option<FullWarpedMaskCache>>,
    /// The editor photo's `full_warped_cache` builds in progress, so a stroke
    /// arriving during a prewarm of the same warp waits for it instead of
    /// building twice.
    pub full_warped_build: crate::editor_preview::BuildSlot<crate::editor_preview::WarpBuildKey>,
    /// The photo whose full-resolution mask source the editor wants built
    /// ahead of the first Auto Mask stroke (`prewarm_mask_source`).
    pub mask_source_wanted: Mutex<Option<String>>,
    /// The last final preview's photo and hydrated recipe, which the mask
    /// source prewarm builds from.
    pub mask_source_recipe: Mutex<Option<(String, Arc<serde_json::Value>)>>,
    /// The retouched (patched) and warped full-resolution source.
    pub patched_warped_cache: Mutex<Option<(u64, Arc<DynamicImage>)>>,
    /// The transformed full-resolution source.
    pub full_transformed_cache: Mutex<Option<TransformedImageCache>>,
}

impl Default for DevelopSession {
    fn default() -> Self {
        Self {
            decoded_image_cache: Mutex::new(DecodedImageCache::new(5)),
            cached_preview: Mutex::new(None),
            reusable_previews: Mutex::new(Default::default()),
            gpu_image_cache: Mutex::new(None),
            mask_cache: Mutex::new(Default::default()),
            mask_thumbnail_cache: Mutex::new(Default::default()),
            patch_cache: Mutex::new(HashMap::new()),
            geometry_cache: Mutex::new(HashMap::new()),
            full_warped_cache: Mutex::new(None),
            full_warped_build: Default::default(),
            mask_source_wanted: Mutex::new(None),
            mask_source_recipe: Mutex::new(None),
            patched_warped_cache: Mutex::new(None),
            full_transformed_cache: Mutex::new(None),
        }
    }
}

impl DevelopSession {
    /// Drops what rebuilds on demand without changing what the open
    /// photograph shows: other photographs' decodes and preview bases and
    /// the open one's full-resolution derived sources (exports, masks and
    /// retouching build them again). Before an idle shader compile under a
    /// compile limit, where one compile commits 4–5 GB and the app's own
    /// memory decides whether the total stays within 6 GB on a 15 GB Radeon
    /// 680M laptop (G76/WCC11, coordinator decision D6; WCC8 measured
    /// about 0.38 GB).
    pub fn release_rebuildable(&self) {
        lock_recover(&self.decoded_image_cache).clear();
        lock_recover(&self.reusable_previews).clear();
        lock_recover(&self.patch_cache).clear();
        *lock_recover(&self.full_warped_cache) = None;
        *lock_recover(&self.patched_warped_cache) = None;
        *lock_recover(&self.full_transformed_cache) = None;
    }

    /// Drops what `scope` makes stale. Takes each cache's lock alone, one
    /// after the other; the caller holds none of this session's locks
    /// (except `cached_preview` for [`Scope::PreviewUpload`]).
    pub fn invalidate(&self, scope: Scope) {
        match scope {
            Scope::OtherPhoto => lock_recover(&self.patch_cache).clear(),
            Scope::Source => {
                *lock_recover(&self.cached_preview) = None;
                *lock_recover(&self.gpu_image_cache) = None;
                *lock_recover(&self.full_warped_cache) = None;
                *lock_recover(&self.full_transformed_cache) = None;
                *lock_recover(&self.patched_warped_cache) = None;
                lock_recover(&self.mask_cache).clear();
                lock_recover(&self.mask_thumbnail_cache).clear();
                lock_recover(&self.geometry_cache).clear();
            }
            Scope::Decoder => {
                lock_recover(&self.decoded_image_cache).clear();
                *lock_recover(&self.gpu_image_cache) = None;
                *lock_recover(&self.cached_preview) = None;
                lock_recover(&self.reusable_previews).clear();
                *lock_recover(&self.full_warped_cache) = None;
                *lock_recover(&self.patched_warped_cache) = None;
                *lock_recover(&self.full_transformed_cache) = None;
                // The multi view's decodes held for its 1:1 previews.
                crate::multi_view_prefetch::forget_decodes();
                // The decode kept for the photograph opened next.
                crate::proxy_worker::forget_ahead();
            }
            Scope::DecodedCache => lock_recover(&self.decoded_image_cache).clear(),
            Scope::PreviewUpload => *lock_recover(&self.gpu_image_cache) = None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fills the caches each scope is checked on; returns which still hold.
    fn kept_after(scope: Scope) -> [bool; 4] {
        let _statics = crate::multi_view_prefetch::tests::STATICS
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let develop = DevelopSession::default();
        let image = Arc::new(DynamicImage::new_rgb32f(2, 2));
        develop.decoded_image_cache.lock().unwrap().insert(
            "a".into(),
            Arc::clone(&image),
            HashMap::new(),
            None,
        );
        develop
            .patch_cache
            .lock()
            .unwrap()
            .insert("subject".into(), serde_json::Value::Null);
        develop
            .geometry_cache
            .lock()
            .unwrap()
            .insert(1, (*image).clone());
        *develop.patched_warped_cache.lock().unwrap() = Some((1, image));
        develop.invalidate(scope);
        [
            develop
                .decoded_image_cache
                .lock()
                .unwrap()
                .get("a")
                .is_some(),
            !develop.patch_cache.lock().unwrap().is_empty(),
            !develop.geometry_cache.lock().unwrap().is_empty(),
            develop.patched_warped_cache.lock().unwrap().is_some(),
        ]
    }

    /// [decodes, patches, geometry bases, retouched source] each scope keeps
    /// (the table in `README.md` and the cache map of plan step 3.3).
    #[test]
    fn each_scope_drops_only_what_it_makes_stale() {
        assert_eq!(kept_after(Scope::OtherPhoto), [true, false, true, true]);
        assert_eq!(kept_after(Scope::Source), [true, true, false, false]);
        assert_eq!(kept_after(Scope::Decoder), [false, true, true, false]);
        assert_eq!(kept_after(Scope::DecodedCache), [false, true, true, true]);
        assert_eq!(kept_after(Scope::PreviewUpload), [true, true, true, true]);
    }
}

#[cfg(test)]
mod cached_preview_tests {
    use super::CachedPreview;
    use image::DynamicImage;
    use std::sync::Arc;

    #[test]
    fn an_interim_base_does_not_serve_the_same_source_once_it_lasts() {
        // A Standard proxy's base built while it stood in for a decode, then
        // the photograph turned out offline: the same proxy is its source.
        let proxy = Arc::new(DynamicImage::new_rgb32f(4, 4));
        let cached = CachedPreview {
            source: Arc::downgrade(&proxy),
            image: Arc::new(DynamicImage::new_rgb32f(2, 2)),
            small_image: None,
            transform_hash: 7,
            scale: 0.5,
            unscaled_crop_offset: (0.0, 0.0),
            preview_dim: 2,
            interactive_divisor: 1.0,
            interim: true,
            fringe_draft: false,
            preparation_key: 0,
        };
        assert!(cached.serves(&proxy, 7, true, false));
        assert!(!cached.serves(&proxy, 7, false, false));
        assert!(!cached.serves(&proxy, 8, true, false));
        assert!(!cached.serves(&Arc::new(DynamicImage::new_rgb32f(4, 4)), 7, true, false));
    }

    #[test]
    fn a_fringe_draft_serves_only_frames_of_a_drag() {
        let source = Arc::new(DynamicImage::new_rgb32f(4, 4));
        let cached = CachedPreview {
            source: Arc::downgrade(&source),
            image: Arc::new(DynamicImage::new_rgb32f(2, 2)),
            small_image: None,
            transform_hash: 7,
            scale: 0.5,
            unscaled_crop_offset: (0.0, 0.0),
            preview_dim: 2,
            interactive_divisor: 1.0,
            interim: false,
            fringe_draft: true,
            preparation_key: 0,
        };
        assert!(cached.serves(&source, 7, false, true));
        assert!(!cached.serves(&source, 7, false, false));
        let finished = CachedPreview {
            fringe_draft: false,
            ..cached
        };
        assert!(finished.serves(&source, 7, false, true));
        assert!(finished.serves(&source, 7, false, false));
    }
}

#[cfg(test)]
mod serves_recipe_tests {
    use super::CachedPreview;
    use image::DynamicImage;
    use serde_json::json;
    use std::sync::Arc;

    /// A snapshot compared with whose lens blur depth map differs from the
    /// edit's by its pixels only (the same length): the transform hash takes
    /// it by size, so the edit's blurred base must not serve the before.
    #[test]
    fn an_edit_base_serves_another_recipe_only_with_its_whole_preparation() {
        let depth = |pixels: &str| json!({"lensBlurEnabled": true, "lensBlurAmount": 40, "lensBlurDepthMap": format!("data:image/png;base64,{pixels}")});
        let (edit, snapshot) = (depth("AAAA"), depth("BBBB"));
        let transform = rawtonic_engine::cache_utils::calculate_transform_hash;
        assert_eq!(transform(&edit), transform(&snapshot));
        let key = crate::preview_cache::preparation_key;
        assert_ne!(key(&edit), key(&snapshot));
        let source = Arc::new(DynamicImage::new_rgb32f(4, 4));
        let cached = CachedPreview {
            source: Arc::downgrade(&source),
            image: Arc::new(DynamicImage::new_rgb32f(2, 2)),
            small_image: None,
            transform_hash: transform(&edit),
            scale: 0.5,
            unscaled_crop_offset: (0.0, 0.0),
            preview_dim: 2,
            interactive_divisor: 1.0,
            interim: false,
            fringe_draft: false,
            preparation_key: key(&edit),
        };
        assert!(cached.serves_recipe(&source, (transform(&edit), key(&edit)), false));
        assert!(!cached.serves_recipe(&source, (transform(&snapshot), key(&snapshot)), false));
        // The held peek shows the snapshot as a preview frame of its own: a
        // lasting frame takes the edit's base only with its preparation; a
        // drag's frames keep the cheap check.
        let hash = transform(&snapshot);
        assert!(!cached.serves_frame(&source, hash, false, false, || key(&snapshot)));
        assert!(cached.serves_frame(&source, hash, false, false, || key(&edit)));
        assert!(cached.serves_frame(&source, hash, false, true, || unreachable!()));
    }
}
