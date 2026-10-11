//! DNG colour calibration beyond the plain colour matrices.
//!
//! The DNG specification (chapter 6, "Mapping Camera Color Space to CIE
//! XYZ Space") maps XYZ to camera space through `AnalogBalance ×
//! CameraCalibration × ColorMatrix` per calibration illuminant, and — when
//! a profile has forward matrices — maps white-balanced camera values to
//! D50 XYZ through the interpolated `ForwardMatrix` instead of inverting
//! the colour matrix. rawler only reads the colour matrices, which left
//! phone DNGs (Pixel, Galaxy: calibrations up to 0.45/1/0.73) visibly off
//! Camera Raw. This module rewrites `RawImage::color_matrix` so the rest
//! of the pipeline renders the full DNG transform.

use crate::color_calibration::{
    CalibrationBlend, Matrix3, blend, calibration_blend, invert, matrix3,
};
use rawler::decoders::{Decoder, WellKnownIFD};
use rawler::formats::tiff::IFD;
use rawler::imgop::xyz::{FlatColorMatrix, Illuminant, SRGB_TO_XYZ_D65, XYZ_TO_SRGB_D50};
use rawler::tags::DngTag;
use std::collections::HashMap;

/// D50 white, the PCS white forward matrices map camera neutral to.
pub const D50: [f64; 3] = [0.9642, 1.0, 0.8249];

const IDENTITY: Matrix3 = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

pub fn multiply(a: &Matrix3, b: &Matrix3) -> Matrix3 {
    std::array::from_fn(|r| std::array::from_fn(|c| (0..3).map(|k| a[r][k] * b[k][c]).sum()))
}

pub fn apply(m: &Matrix3, v: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|r| (0..3).map(|c| m[r][c] * v[c]).sum())
}

pub fn diagonal(v: [f64; 3]) -> Matrix3 {
    std::array::from_fn(|r| std::array::from_fn(|c| if r == c { v[r] } else { 0.0 }))
}

fn from_f32(m: &[[f32; 3]; 3]) -> Matrix3 {
    std::array::from_fn(|r| std::array::from_fn(|c| m[r][c] as f64))
}

fn flat(m: &Matrix3) -> FlatColorMatrix {
    m.iter().flatten().map(|v| *v as f32).collect()
}

fn read_values(ifd: &IFD, tag: DngTag) -> Option<Vec<f64>> {
    let entry = ifd.get_entry(tag)?;
    Some(
        (0..entry.value.count())
            .map(|i| entry.value.force_f32(i) as f64)
            .collect(),
    )
}

fn read_matrix(ifd: &IFD, tag: DngTag) -> Option<Matrix3> {
    let values = read_values(ifd, tag)?;
    let flat: FlatColorMatrix = values.iter().map(|v| *v as f32).collect();
    matrix3(&flat).filter(|m| m.iter().flatten().all(|v| v.is_finite()))
}

/// Raw bytes of a string/byte tag, for comparing calibration signatures.
fn signature(ifd: &IFD, tag: DngTag) -> Vec<u8> {
    let Some(entry) = ifd.get_entry(tag) else {
        return Vec::new();
    };
    let mut bytes = match &entry.value {
        rawler::formats::tiff::Value::Ascii(text) => text.strings().concat().into_bytes(),
        rawler::formats::tiff::Value::Byte(bytes)
        | rawler::formats::tiff::Value::Undefined(bytes) => bytes.clone(),
        _ => Vec::new(),
    };
    while bytes.last() == Some(&0) {
        bytes.pop();
    }
    bytes
}

fn illuminant(ifd: &IFD, tag: DngTag) -> Illuminant {
    ifd.get_entry(tag)
        .map(|entry| entry.value.force_u16(0))
        .unwrap_or(21)
        .try_into()
        .unwrap_or(Illuminant::D65)
}

/// One calibration of the profile (illuminant 1 or 2).
struct Calibration {
    illuminant: Illuminant,
    color: Matrix3,
    camera_calibration: Matrix3,
    forward: Option<Matrix3>,
}

/// Rewrites the DNG's colour matrices to the full DNG transform: one
/// matrix per illuminant including analog balance and camera calibration,
/// or — for profiles with forward matrices — a single matrix whose inverse
/// is the forward-matrix transform at the as-shot white balance.
///
/// Returns whether the forward-matrix transform was used (its matrix needs
/// no further white adaptation).
pub fn apply_dng_color(decoder: &dyn Decoder, raw_image: &mut rawler::RawImage) -> bool {
    let Ok(Some(root)) = decoder.ifd(WellKnownIFD::Root) else {
        return false;
    };
    match dng_color_matrix(&root, raw_image.wb_coeffs) {
        Some((color_matrix, forward)) => {
            raw_image.color_matrix = color_matrix;
            forward
        }
        None => false,
    }
}

fn dng_color_matrix(
    root: &IFD,
    wb_coeffs: [f32; 4],
) -> Option<(HashMap<Illuminant, FlatColorMatrix>, bool)> {
    // Only three-colour profiles; four-colour cameras keep rawler's path.
    let analog_balance = match read_values(root, DngTag::AnalogBalance) {
        Some(values) if values.len() == 3 && values.iter().all(|v| v.is_finite() && *v > 0.0) => {
            diagonal([values[0], values[1], values[2]])
        }
        Some(_) => return None,
        None => IDENTITY,
    };
    // Camera calibrations belong to the profile whose signature they carry
    // (both absent counts as a match, as in the DNG SDK).
    let use_calibration = signature(root, DngTag::CameraCalibrationSignature)
        == signature(root, DngTag::ProfileCalibrationSignature);
    let tags = [
        (
            DngTag::CalibrationIlluminant1,
            DngTag::ColorMatrix1,
            DngTag::CameraCalibration1,
            DngTag::ForwardMatrix1,
        ),
        (
            DngTag::CalibrationIlluminant2,
            DngTag::ColorMatrix2,
            DngTag::CameraCalibration2,
            DngTag::ForwardMatrix2,
        ),
    ];
    let calibrations: Vec<Calibration> = tags
        .iter()
        .filter_map(
            |(illuminant_tag, color_tag, calibration_tag, forward_tag)| {
                Some(Calibration {
                    illuminant: illuminant(root, *illuminant_tag),
                    color: read_matrix(root, *color_tag)?,
                    camera_calibration: use_calibration
                        .then(|| read_matrix(root, *calibration_tag))
                        .flatten()
                        .unwrap_or(IDENTITY),
                    forward: read_matrix(root, *forward_tag),
                })
            },
        )
        .collect();
    if calibrations.is_empty() {
        return None;
    }
    let adjusted =
        |c: &Calibration| multiply(&multiply(&analog_balance, &c.camera_calibration), &c.color);
    let color_matrix: HashMap<Illuminant, FlatColorMatrix> = calibrations
        .iter()
        .map(|c| (c.illuminant, flat(&adjusted(c))))
        .collect();
    // Forward matrices of profiles with hue/saturation tables (Adobe
    // Standard and other DCP-style profiles) are fitted non-negative and
    // render far too pale without those tables: Canon EOS 7D Mark II and
    // Pixel 4a files re-saved by Lightroom went from chroma 0.95/0.89 to
    // 0.44/0.51 of Camera Raw's. Until the tables are applied, those keep
    // the colour-matrix path.
    let has_tables = [
        DngTag::ProfileHueSatMapData1,
        DngTag::ProfileHueSatMapData2,
        DngTag::ProfileLookTableData,
    ]
    .iter()
    .any(|tag| root.get_entry(*tag).is_some());
    if has_tables || !calibrations.iter().all(|c| c.forward.is_some()) {
        return Some((color_matrix, false));
    }

    // Forward matrices: the weight comes from the adjusted colour
    // matrices, the same blend mixes forward matrices and calibrations.
    let pick = |illuminant: Illuminant| calibrations.iter().find(|c| c.illuminant == illuminant);
    let (forward, camera_calibration) = match calibration_blend(&color_matrix, wb_coeffs) {
        Some(CalibrationBlend {
            low,
            high,
            low_weight,
        }) => {
            let (low, high) = (pick(low)?, pick(high)?);
            (
                blend(low.forward.as_ref()?, high.forward.as_ref()?, low_weight),
                blend(
                    &low.camera_calibration,
                    &high.camera_calibration,
                    low_weight,
                ),
            )
        }
        None => {
            // Single calibration (or no usable balance): the one the
            // colour-matrix path would keep.
            let mut single = color_matrix.clone();
            crate::color_calibration::select_color_matrix(&mut single, wb_coeffs);
            let kept = single.values().next()?;
            let c = calibrations
                .iter()
                .find(|c| flat(&adjusted(c)) == *kept)
                .unwrap_or(&calibrations[0]);
            (c.forward?, c.camera_calibration)
        }
    };
    let camera_to_rgb =
        forward_camera_to_srgb(&forward, &analog_balance, &camera_calibration, wb_coeffs)?;
    // The pipeline builds camera→sRGB as inverse(xyz2cam × sRGB→XYZ), so
    // hand it xyz2cam = inverse(camera→sRGB) × XYZ→sRGB.
    let srgb_to_xyz = from_f32(&SRGB_TO_XYZ_D65);
    let xyz_to_camera = multiply(&invert(&camera_to_rgb)?, &invert(&srgb_to_xyz)?);
    if !xyz_to_camera.iter().flatten().all(|v| v.is_finite()) {
        return None;
    }
    log::info!("DNG forward-matrix transform applied");
    Some((
        HashMap::from([(Illuminant::D65, flat(&xyz_to_camera))]),
        true,
    ))
}

/// Bradford adaptation from white `from` to white `to` (XYZ).
pub fn bradford(from: [f64; 3], to: [f64; 3]) -> Option<Matrix3> {
    const B: Matrix3 = [
        [0.8951, 0.2664, -0.1614],
        [-0.7502, 1.7135, 0.0367],
        [0.0389, -0.0685, 1.0296],
    ];
    let (f, t) = (apply(&B, from), apply(&B, to));
    if f.iter().chain(&t).any(|v| !v.is_finite() || v.abs() < 1e-9) {
        return None;
    }
    let scale = diagonal(std::array::from_fn(|i| t[i] / f[i]));
    Some(multiply(&multiply(&invert(&B)?, &scale), &B))
}

/// White adaptation like the DNG SDK's colour-matrix path, blended in away
/// from daylight. The pipeline maps white-balanced camera RGB to sRGB with
/// the matrix rows normalised (a von Kries balance in camera space, which
/// also cancels diagonal camera calibrations); the SDK takes the scene
/// white as `inverse(CM) × camera neutral` and adapts it to D50 with
/// Bradford. Over 917 models against Camera Raw the SDK way rendered warm
/// light far closer (below 4000 K: 34 of 40 models better, median −0.44
/// dE) and cool light slightly closer, but daylight slightly worse, where
/// the neutral look's per-hue table was fitted on the normalised rows. So
/// the weight is 0 from 4500 to 7000 K (renders unchanged there) and rises
/// linearly in mired to 1 at 3500 K and at 9000 K. `color_matrix` holds the
/// single matrix `select_color_matrix` left.
pub fn adapted_color_matrix(
    color_matrix: &HashMap<Illuminant, FlatColorMatrix>,
    wb_coeffs: [f32; 4],
) -> Option<HashMap<Illuminant, FlatColorMatrix>> {
    if color_matrix.len() != 1 {
        return None;
    }
    let cm = matrix3(color_matrix.values().next()?)?;
    let wb: [f64; 3] = std::array::from_fn(|i| wb_coeffs[i] as f64);
    if wb.iter().any(|v| !v.is_finite() || *v <= 0.0) {
        return None;
    }
    let neutral = [wb[1] / wb[0], 1.0, wb[1] / wb[2]];
    let white = apply(&invert(&cm)?, neutral);
    let sum = white[0] + white[1] + white[2];
    if !(sum.is_finite() && sum > 0.0) || white.iter().any(|v| *v <= 0.0) {
        return None;
    }
    let mired = 1e6 / mccamy_temperature(white[0] / sum, white[1] / sum);
    let warm = (mired - 1e6 / 4500.0) / (1e6 / 3500.0 - 1e6 / 4500.0);
    let cool = (1e6 / 7000.0 - mired) / (1e6 / 7000.0 - 1e6 / 9000.0);
    let weight = warm.max(cool).clamp(0.0, 1.0);
    if weight.is_nan() || weight <= 0.0 {
        return None;
    }
    let srgb_to_xyz = from_f32(&SRGB_TO_XYZ_D65);
    // The pipeline's matrix: normalised rows of CM × sRGB→XYZ, inverted.
    let mut rgb_to_camera = multiply(&cm, &srgb_to_xyz);
    for row in &mut rgb_to_camera {
        let sum: f64 = row.iter().sum();
        if sum.is_nan() || sum.abs() <= 1e-9 {
            return None;
        }
        row.iter_mut().for_each(|v| *v /= sum);
    }
    let normalised = invert(&rgb_to_camera)?;
    // The SDK's: scene white adapted to D50, D50 XYZ to sRGB.
    let white = white.map(|v| v / white[1]);
    let pcs_to_camera = multiply(&cm, &bradford(D50, white)?);
    let camera_to_rgb = multiply(&from_f32(&XYZ_TO_SRGB_D50), &invert(&pcs_to_camera)?);
    let sdk = multiply(&camera_to_rgb, &diagonal(neutral));
    let sdk_white = apply(&sdk, [1.0; 3]);
    let scale = (sdk_white[0] + sdk_white[1] + sdk_white[2]) / 3.0;
    if !(scale.is_finite() && scale > 0.0) {
        return None;
    }
    let blended: Matrix3 = std::array::from_fn(|r| {
        std::array::from_fn(|c| (1.0 - weight) * normalised[r][c] + weight * sdk[r][c] / scale)
    });
    let xyz_to_camera = multiply(&invert(&blended)?, &invert(&srgb_to_xyz)?);
    log::info!(
        "Colour adaptation for {:.0} K: {:.0} % DNG SDK white adaptation",
        1e6 / mired,
        weight * 100.0
    );
    xyz_to_camera
        .iter()
        .flatten()
        .all(|v| v.is_finite())
        .then(|| HashMap::from([(Illuminant::D65, flat(&xyz_to_camera))]))
}

/// McCamy's correlated colour temperature of a chromaticity.
fn mccamy_temperature(x: f64, y: f64) -> f64 {
    let n = (x - 0.3320) / (0.1858 - y);
    (449.0 * n.powi(3) + 3525.0 * n.powi(2) + 6823.3 * n + 5520.33).clamp(1000.0, 50_000.0)
}

/// White-balanced camera RGB (raw multiplied by `wb_coeffs`) to linear
/// sRGB D65 through the forward matrix, as the DNG SDK's
/// `CameraToPCS = FM × D × inverse(AB × CC)` followed by D50 XYZ → sRGB.
fn forward_camera_to_srgb(
    forward: &Matrix3,
    analog_balance: &Matrix3,
    camera_calibration: &Matrix3,
    wb_coeffs: [f32; 4],
) -> Option<Matrix3> {
    let wb: [f64; 3] = std::array::from_fn(|i| wb_coeffs[i] as f64);
    if wb.iter().any(|v| !v.is_finite() || *v <= 0.0) {
        return None;
    }
    // Camera neutral of the balance, normalised to green.
    let camera_neutral = [wb[1] / wb[0], 1.0, wb[1] / wb[2]];
    let ab_cc_inverse = invert(&multiply(analog_balance, camera_calibration))?;
    let reference_neutral = apply(&ab_cc_inverse, camera_neutral);
    if reference_neutral
        .iter()
        .any(|v| !v.is_finite() || *v <= 0.0)
    {
        return None;
    }
    let forward = normalize_forward_matrix(forward)?;
    let d = diagonal(reference_neutral.map(|v| 1.0 / v));
    let camera_to_pcs = multiply(&multiply(&forward, &d), &ab_cc_inverse);
    // Raw = diag(camera neutral × green scale) × white-balanced value.
    let raw_from_balanced = diagonal(std::array::from_fn(|i| wb[1] / wb[i]));
    let balanced_to_pcs = multiply(&camera_to_pcs, &raw_from_balanced);
    Some(multiply(&from_f32(&XYZ_TO_SRGB_D50), &balanced_to_pcs))
}

/// Scales the rows so camera white maps exactly to D50, as the DNG SDK's
/// `NormalizeForwardMatrix`.
pub fn normalize_forward_matrix(forward: &Matrix3) -> Option<Matrix3> {
    let white = apply(forward, [1.0; 3]);
    if white.iter().any(|v| !v.is_finite() || v.abs() < 1e-9) {
        return None;
    }
    Some(std::array::from_fn(|r| {
        std::array::from_fn(|c| forward[r][c] * D50[r] / white[r])
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Google Pixel-style forward matrix (rows sum to D50).
    const FM: Matrix3 = [
        [0.6484, 0.2148, 0.1016],
        [0.2578, 0.8008, -0.0586],
        [0.0234, -0.2617, 1.0625],
    ];

    #[test]
    fn neutral_maps_to_white_through_forward_matrix() {
        let ab = IDENTITY;
        let cc = diagonal([0.6572, 1.0, 0.4072]);
        let wb = [2.1, 1.0, 1.6, f32::NAN];
        let m = forward_camera_to_srgb(&FM, &ab, &cc, wb).unwrap();
        // White-balanced neutral (1, 1, 1) must render as sRGB white.
        let white = apply(&m, [1.0; 3]);
        for v in white {
            assert!((v - 1.0).abs() < 2e-3, "{white:?}");
        }
    }

    #[test]
    fn diagonal_calibration_cancels_with_forward_matrix() {
        // With diagonal AB × CC, the white-balanced transform is FM alone.
        let wb = [2.1, 1.0, 1.6, f32::NAN];
        let plain = forward_camera_to_srgb(&FM, &IDENTITY, &IDENTITY, wb).unwrap();
        let calibrated = forward_camera_to_srgb(
            &FM,
            &diagonal([1.1, 1.0, 0.9]),
            &diagonal([0.66, 1.0, 0.41]),
            wb,
        )
        .unwrap();
        for (a, b) in plain.iter().flatten().zip(calibrated.iter().flatten()) {
            assert!((a - b).abs() < 1e-9);
        }
    }

    // Sony ILCE-7M3 D65 colour matrix (Adobe).
    const SONY_D65: [f32; 9] = [
        0.7374, -0.2389, -0.0551, -0.5435, 1.3162, 0.2519, -0.1006, 0.1795, 0.6552,
    ];

    fn wb_for_white(xyz: [f64; 3]) -> [f32; 4] {
        let m = matrix3(&SONY_D65.to_vec()).unwrap();
        let camera = apply(&m, xyz);
        [
            (camera[1] / camera[0]) as f32,
            1.0,
            (camera[1] / camera[2]) as f32,
            f32::NAN,
        ]
    }

    #[test]
    fn daylight_keeps_the_normalised_matrix() {
        let single = HashMap::from([(Illuminant::D65, SONY_D65.to_vec())]);
        // D55 and D65 whites: weight 0, no new matrix.
        for white in [[0.95682, 1.0, 0.92149], [0.95047, 1.0, 1.08883]] {
            assert!(adapted_color_matrix(&single, wb_for_white(white)).is_none());
        }
    }

    #[test]
    fn tungsten_light_is_adapted_and_white_stays_white() {
        let single = HashMap::from([(Illuminant::D65, SONY_D65.to_vec())]);
        let adapted = adapted_color_matrix(&single, wb_for_white([1.09850, 1.0, 0.35585])).unwrap();
        let m = matrix3(&adapted[&Illuminant::D65]).unwrap();
        // The pipeline: inverse of the normalised rows of xyz2cam × sRGB→XYZ.
        let mut rgb_to_camera = multiply(&m, &from_f32(&SRGB_TO_XYZ_D65));
        for row in &mut rgb_to_camera {
            let sum: f64 = row.iter().sum();
            row.iter_mut().for_each(|v| *v /= sum);
        }
        let camera_to_rgb = invert(&rgb_to_camera).unwrap();
        let white = apply(&camera_to_rgb, [1.0; 3]);
        for v in white {
            assert!((v - 1.0).abs() < 1e-6, "{white:?}");
        }
        assert_ne!(adapted[&Illuminant::D65], SONY_D65.to_vec());
    }

    #[test]
    fn rows_of_equivalent_matrix_are_normalised() {
        // The pipeline normalises xyz2cam × sRGB→XYZ rows to one; the
        // equivalent matrix must already satisfy that so it is a no-op.
        let wb = [2.1, 1.0, 1.6, f32::NAN];
        let m = forward_camera_to_srgb(&FM, &IDENTITY, &IDENTITY, wb).unwrap();
        let rgb_to_camera = invert(&m).unwrap();
        for row in rgb_to_camera {
            assert!((row.iter().sum::<f64>() - 1.0).abs() < 2e-3, "{row:?}");
        }
    }
}
