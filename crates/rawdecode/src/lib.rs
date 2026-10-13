//! RAW decode layer of the Rawtonic engine (ADR 0009): file bytes to oriented
//! linear camera RGB, with the capture metadata and camera colour the develop
//! defaults need. Derived from RapidRAW (commit a82251f, AGPL-3.0); no Tauri,
//! catalog or develop-model dependencies.

pub mod camera_exposure;
pub mod camera_matrices;
pub mod capture_info;
pub mod color_calibration;
pub mod demosaic;
pub mod dng_color;
pub mod dng_opcodes;
pub mod fingerprint;
pub mod gamut;
pub mod gpu_demosaic;
pub mod highlight_reconstruction;
pub mod hot_pixels;
pub mod image_key;
pub mod mosaic_denoise;
pub mod multi_exposure;
pub mod noise_level;
pub mod orientation;
pub mod raw_processing;
pub mod unit_calibration;
pub mod unit_calibration_table;

use rawtonic_runtime::allocator;
#[cfg(feature = "performance-profiling")]
use rawtonic_runtime::performance;
