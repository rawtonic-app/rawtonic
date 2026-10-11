//! Export layer of the Rawtonic engine (ADR 0009): the full-resolution render
//! of a recipe, resize/border/pad/watermark, the image encoders and the
//! no-clobber output write. Derived from RapidRAW (commit a82251f,
//! AGPL-3.0); no Tauri dependencies.

pub mod avif_container;
pub mod exif_processing;
pub mod export_processing;
pub mod file_export;
pub mod output;
pub mod output_sharpening;
pub mod queue;
pub mod sidecars;
pub mod text_watermark;
pub mod tiff_exif;
pub mod webp_container;
pub mod xmp_embed;

use rawtonic_engine::{
    adjustment_utils, baseline_exposure, cache_utils, formats, icc_profile, image_processing, lut,
    mask_generation, output_color_space,
};
use rawtonic_gpu::gpu_processing;
use rawtonic_runtime::allocator;
#[cfg(feature = "performance-profiling")]
use rawtonic_runtime::performance;
