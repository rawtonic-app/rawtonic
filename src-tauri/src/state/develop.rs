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
use crate::app_state::{CachedPreview, TransformedImageCache};
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
