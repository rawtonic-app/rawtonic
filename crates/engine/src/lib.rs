//! Develop layer of the Rawtonic engine (ADR 0009): the adjustment model and
//! its translation to GPU uniforms, geometry, CPU detail stages, scopes, auto
//! analysis, camera defaults and looks, colour tools, lens correction maths.
//! Derived from RapidRAW (commit a82251f, AGPL-3.0); no Tauri dependencies.

pub mod adjustment_utils;
pub mod auto_adjust;
pub mod auto_straighten;
pub mod avif;
pub mod baseline_exposure;
pub mod cache_utils;
pub mod calibration;
pub mod camera_profile;
pub mod color_grading;
pub mod color_noise;
pub mod diffraction;
pub mod edge_fringe;
pub mod embedded_lens;
pub mod face_retouch;
pub mod file_io;
pub mod formats;
pub mod guided_perspective;
pub mod icc_profile;
pub mod image_loader;
pub mod image_processing;
pub mod input_profile;
pub mod lateral_ca;
pub mod lens_blur;
pub mod lens_correction;
pub mod lens_recipe;
pub mod luma_levels;
pub mod lut;
pub mod mask_generation;
pub mod mask_parameters;
pub mod mask_thumbnail;
pub mod masks;
pub mod output_color_space;
pub mod patch_fill;
pub mod point_curve;
pub mod proxy_codec;
pub mod region_curve;
pub mod relight;
pub mod session_layout;
pub mod source_facts;
pub mod spot_removal;
pub mod upright;
pub mod white_balance_estimate;

use rawtonic_rawdecode::{
    camera_exposure, capture_info, color_calibration, dng_color, dng_opcodes, image_key,
    noise_level, raw_processing, unit_calibration,
};
use rawtonic_runtime::allocator;
#[cfg(feature = "performance-profiling")]
use rawtonic_runtime::performance;
