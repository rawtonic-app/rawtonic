//! Diffraction correction (Capture One's Lens tool; Lightroom has none):
//! stopped-down lenses blur by diffraction, an Airy disk whose size grows
//! with the f-number and matters once it reaches the pixel pitch. The
//! correction sharpens with a radius and strength derived from both,
//! fitted to Capture One's response (notes: reference/diffraction.md).
//!
//! The Airy disk is approximated by a Gaussian of σ = 0.42 λ N (λ = 0.55
//! µm). Capture One's gain matched an unsharp mask of radius 1.85 σ and
//! amount 0.34 / σ (σ in pixels) on four cameras from f/11 to f/27, and it
//! left an f/5.6 frame (σ 0.29 px) alone; the shader's kernel reaches
//! 1.6 px.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use rawler::decoders::RawDecodeParams;
use rawler::rawsource::RawSource;

use crate::image_processing::GlobalAdjustments;

const WAVELENGTH_UM: f32 = 0.55;
const AIRY_SIGMA: f32 = 0.42;
const RADIUS_PER_SIGMA: f32 = 1.85;
const MAX_RADIUS: f32 = 1.6;
const AMOUNT_TIMES_SIGMA: f32 = 0.6;
const MAX_AMOUNT: f32 = 1.2;
/// Airy σ (px) below which the correction fades out.
const ONSET: (f32, f32) = (0.35, 0.55);

/// `adjustments.diffractionVersion`, written when the correction is switched
/// on. `2`: a body without FocalPlaneXResolution takes its crop factor from
/// the whole bundled database. Missing (`1`, recipes from before
/// 2026-10-18): without the camera entries that update added
/// (`lens_correction::CAMERAS_ADDED_2026_10_18`), so those renders stay as
/// they were.
pub const VERSION: u64 = 2;
pub const VERSION_KEY: &str = "diffractionVersion";

/// The shader flag for a recipe that asks for the correction: its version
/// (`apply` reads it back).
pub fn requested_flag(adjustments: &serde_json::Value) -> f32 {
    if adjustments["diffractionCorrection"].as_bool() != Some(true) {
        return 0.0;
    }
    if adjustments[VERSION_KEY].as_u64().unwrap_or(1) >= 2 {
        2.0
    } else {
        1.0
    }
}

fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Unsharp-mask radius (px) and amount for an Airy σ in pixels.
pub fn correction(airy_sigma_px: f32) -> (f32, f32) {
    if !(airy_sigma_px.is_finite() && airy_sigma_px > 0.0) {
        return (0.0, 0.0);
    }
    let radius = (RADIUS_PER_SIGMA * airy_sigma_px).min(MAX_RADIUS);
    let amount = (AMOUNT_TIMES_SIGMA / airy_sigma_px).min(MAX_AMOUNT)
        * smoothstep(ONSET.0, ONSET.1, airy_sigma_px);
    (radius, amount)
}

/// Airy σ in pixels for an f-number and a pixel pitch in µm.
pub fn airy_sigma(f_number: f32, pitch_um: f32) -> Option<f32> {
    (f_number > 0.0 && pitch_um > 0.0).then(|| AIRY_SIGMA * WAVELENGTH_UM * f_number / pitch_um)
}

/// Airy σ for the current version and for version 1 (`VERSION`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Sigmas {
    pub current: Option<f32>,
    pub v1: Option<f32>,
}

fn cache() -> &'static Mutex<HashMap<String, Sigmas>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Sigmas>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// Airy σ in pixels of the developed frame from the file's f-number and the
/// frame's width (FocalPlaneXResolution, else the camera's crop factor).
pub(crate) fn sigmas_for_path(path: &str) -> Sigmas {
    // Offline: the values the file gave (source_facts).
    if let Some(facts) = crate::source_facts::registered(path) {
        return Sigmas {
            current: facts.diffraction_sigma,
            v1: if facts.diffraction_v1_differs {
                facts.diffraction_sigma_v1
            } else {
                facts.diffraction_sigma
            },
        };
    }
    if let Some(found) = cache().lock().unwrap_or_else(|e| e.into_inner()).get(path) {
        return *found;
    }
    let none = Sigmas {
        current: None,
        v1: None,
    };
    let sigmas = (|| {
        let source = RawSource::new(std::path::Path::new(path)).ok()?;
        let decoder = rawler::get_decoder(&source).ok()?;
        let metadata = decoder
            .raw_metadata(&source, &RawDecodeParams::default())
            .ok()?;
        let rational =
            |r: rawler::formats::tiff::Rational| (r.d != 0).then(|| r.n as f32 / r.d as f32);
        let exif = &metadata.exif;
        let f_number = exif.fnumber.and_then(rational)?;
        let raw = decoder
            .raw_image(&source, &RawDecodeParams::default(), true)
            .ok()?;
        let width_px = raw.crop_area.map(|c| c.d.w).unwrap_or(raw.width).max(1) as f32;
        // FocalPlaneXResolution: pixels per unit on the sensor.
        let from_focal_plane = (|| {
            let resolution = exif.focal_plane_x_resolution.and_then(rational)?;
            let unit_mm = match exif.focal_plane_resolution_unit? {
                2 => 25.4,
                3 => 10.0,
                4 => 1.0,
                _ => return None,
            };
            Some(unit_mm * 1000.0 / resolution)
        })()
        .filter(|p| (1.0..20.0).contains(p));
        let from_database = |excluded: &[(&str, &str)]| {
            let db = crate::lens_correction::global_lens_db()?;
            let crop = crate::lens_correction::camera_crop_factor_without(
                &db,
                &metadata.make,
                &metadata.model,
                excluded,
            )
            .or_else(|| {
                crate::lens_correction::camera_crop_factor_loose_without(
                    &db,
                    &metadata.model,
                    excluded,
                )
            })?;
            Some(36.0 / crop * 1000.0 / width_px)
        };
        let sigma = |pitch_um: Option<f32>| airy_sigma(f_number, pitch_um?);
        Some(Sigmas {
            current: sigma(from_focal_plane.or_else(|| from_database(&[]))),
            v1: sigma(
                from_focal_plane
                    .or_else(|| from_database(crate::lens_correction::CAMERAS_ADDED_2026_10_18)),
            ),
        })
    })()
    .unwrap_or(none);
    cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(path.to_string(), sigmas);
    sigmas
}

/// Fills the correction for the photo at `path` when the recipe asked for it
/// (`sharpen_falloff[2]` = its version, `requested_flag`): `[1]` radius in
/// original pixels, `[2]` amount.
pub fn apply(global: &mut GlobalAdjustments, path: &str) {
    let requested = global.sharpen_falloff[2];
    global.sharpen_falloff[1] = 0.0;
    global.sharpen_falloff[2] = 0.0;
    if requested < 0.5 || global.is_raw_image == 0 {
        return;
    }
    let sigmas = sigmas_for_path(path);
    let sigma = if requested > 1.5 {
        sigmas.current
    } else {
        sigmas.v1
    };
    if let Some(sigma) = sigma {
        let (radius, amount) = correction(sigma);
        global.sharpen_falloff[1] = radius;
        global.sharpen_falloff[2] = amount;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shader_flag_carries_the_recipes_version() {
        let flag = |recipe: serde_json::Value| requested_flag(&recipe);
        assert_eq!(flag(serde_json::json!({})), 0.0);
        assert_eq!(flag(serde_json::json!({"diffractionVersion": 2})), 0.0);
        assert_eq!(
            flag(serde_json::json!({"diffractionCorrection": true})),
            1.0
        );
        assert_eq!(
            flag(serde_json::json!({"diffractionCorrection": true, "diffractionVersion": 2})),
            2.0
        );
    }

    /// Prints the Airy σ found for `$DIFFRACTION_FILES` (`;`-separated).
    #[test]
    #[ignore = "tool: needs DIFFRACTION_FILES"]
    fn print_airy_sigma() {
        crate::lens_correction::set_global_lens_db(std::sync::Arc::new(
            crate::lens_correction::embedded_lens_db(),
        ));
        for path in std::env::var("DIFFRACTION_FILES").unwrap().split(';') {
            let source = RawSource::new(std::path::Path::new(path)).unwrap();
            let decoder = rawler::get_decoder(&source).unwrap();
            let m = decoder
                .raw_metadata(&source, &RawDecodeParams::default())
                .unwrap();
            println!("{} | {} | {:?}", m.make, m.model, m.exif.fnumber);
            println!("{path}: {:?}", sigmas_for_path(path));
        }
    }

    #[test]
    fn correction_follows_the_airy_disk() {
        // Nikon Z 7II at f/11 (4.35 µm) against a D800E at f/27 (4.88 µm).
        let z7 = airy_sigma(11.0, 4.35).unwrap();
        let d800e = airy_sigma(27.0, 4.88).unwrap();
        assert!((z7 - 0.58).abs() < 0.01 && (d800e - 1.28).abs() < 0.01);
        let (r_z7, a_z7) = correction(z7);
        let (r_d8, a_d8) = correction(d800e);
        // Wider blur, wider radius, gentler amount (noise).
        assert!(r_d8 > r_z7 && a_d8 < a_z7);
        assert!(r_d8 <= MAX_RADIUS);
        // Open apertures are left alone (an EOS R5 at f/5.6).
        let r5 = airy_sigma(5.6, 4.39).unwrap();
        assert_eq!(correction(r5).1, 0.0);
        assert_eq!(correction(f32::NAN), (0.0, 0.0));
    }
}
