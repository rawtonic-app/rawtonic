//! GPU layer of the Rawtonic engine (ADR 0009): the wgpu develop pipeline
//! (`GpuProcessor`, specialised shader variants, tiled CPU readback) and the
//! native display presentation onto a surface the app creates. Derived from
//! RapidRAW (commit a82251f, AGPL-3.0); no Tauri dependencies.

pub mod device_start;
pub mod gpu_processing;
pub mod pipeline_cache;

use rawtonic_engine::{camera_profile, image_processing, lut, mask_generation};
#[cfg(feature = "performance-profiling")]
use rawtonic_runtime::performance;
