//! Camera colour matrix selection for the raw decode.
//!
//! Cameras are calibrated at two illuminants (usually Standard Light A and
//! D65). As the DNG specification (chapter 6, "Mapping Camera Color Space
//! to CIE XYZ Space") prescribes, the matrix used for a photograph is
//! interpolated between the two by inverse correlated colour temperature of
//! the white balance, found iteratively from the camera neutral.

use rawler::imgop::xyz::{FlatColorMatrix, Illuminant};
use std::collections::HashMap;

/// Correlated colour temperature of an EXIF/DNG calibration illuminant, as
/// the DNG SDK assigns them (`dng_camera_profile.cpp`).
fn illuminant_temperature(illuminant: Illuminant) -> Option<f64> {
    Some(match illuminant {
        Illuminant::A | Illuminant::Tungsten => 2850.0,
        Illuminant::IsoStudioTungsten => 3200.0,
        Illuminant::D50 => 5000.0,
        Illuminant::D55
        | Illuminant::Daylight
        | Illuminant::FineWeather
        | Illuminant::Flash
        | Illuminant::B => 5500.0,
        Illuminant::D65 | Illuminant::CloudyWeather | Illuminant::C => 6500.0,
        Illuminant::D75 | Illuminant::Shade => 7500.0,
        Illuminant::DaylightFluorescent => 6400.0,
        Illuminant::DaylightWhiteFluorescent => 5000.0,
        Illuminant::CoolWhiteFluorescent | Illuminant::Fluorescent => 4150.0,
        Illuminant::WhiteFluorescent => 3525.0,
        Illuminant::Unknown => return None,
    })
}

pub type Matrix3 = [[f64; 3]; 3];

/// Per-unit camera calibration (DNG CameraCalibration, diagonal: red,
/// green, blue gains) for the lower- and the higher-temperature calibration
/// illuminant; see `unit_calibration.rs`.
pub type CameraCalibration = [[f64; 3]; 2];

pub fn matrix3(flat: &FlatColorMatrix) -> Option<Matrix3> {
    (flat.len() == 9)
        .then(|| std::array::from_fn(|r| std::array::from_fn(|c| flat[r * 3 + c] as f64)))
}

pub fn invert(m: &Matrix3) -> Option<Matrix3> {
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    if det.abs() < 1e-12 {
        return None;
    }
    let cof =
        |r0: usize, r1: usize, c0: usize, c1: usize| m[r0][c0] * m[r1][c1] - m[r0][c1] * m[r1][c0];
    Some([
        [
            cof(1, 2, 1, 2) / det,
            -cof(0, 2, 1, 2) / det,
            cof(0, 1, 1, 2) / det,
        ],
        [
            -cof(1, 2, 0, 2) / det,
            cof(0, 2, 0, 2) / det,
            -cof(0, 1, 0, 2) / det,
        ],
        [
            cof(1, 2, 0, 1) / det,
            -cof(0, 2, 0, 1) / det,
            cof(0, 1, 0, 1) / det,
        ],
    ])
}

/// The DNG SDK's NormalizeColorMatrix: scales a colour matrix so that its
/// largest response to D50 white is 1 (left alone within 1 %). Matrices of
/// different scale would otherwise not blend by the interpolation weight:
/// the Olympus SP570UZ's D65 matrix is stored ×250 (350 −63 −30 …).
fn normalize_color_matrix(m: Matrix3) -> Matrix3 {
    const D50: [f64; 3] = [0.9642, 1.0, 0.8249];
    let max = m
        .iter()
        .map(|row| row.iter().zip(D50).map(|(v, w)| v * w).sum::<f64>())
        .fold(f64::NEG_INFINITY, f64::max);
    if max.is_finite() && max > 0.0 && !(0.99..=1.01).contains(&max) {
        m.map(|row| row.map(|v| v / max))
    } else {
        m
    }
}

pub fn blend(a: &Matrix3, b: &Matrix3, weight_a: f64) -> Matrix3 {
    std::array::from_fn(|r| {
        std::array::from_fn(|c| a[r][c] * weight_a + b[r][c] * (1.0 - weight_a))
    })
}

/// McCamy's approximation, within a few kelvin of Robertson's method along
/// the daylight/Planckian locus between 2000 K and 10 000 K. The weight is
/// linear in inverse temperature, so that error is negligible here.
fn xy_to_temperature(x: f64, y: f64) -> f64 {
    let n = (x - 0.3320) / (0.1858 - y);
    (449.0 * n.powi(3) + 3525.0 * n.powi(2) + 6823.3 * n + 5520.33).clamp(1000.0, 50_000.0)
}

/// Matrix weight of the lower-temperature calibration, as in the DNG SDK.
fn low_temperature_weight(temperature: f64, low: f64, high: f64) -> f64 {
    if temperature <= low {
        1.0
    } else if temperature >= high {
        0.0
    } else {
        (1.0 / temperature - 1.0 / high) / (1.0 / low - 1.0 / high)
    }
}

/// Matrix, temperature and — when the search did not converge — the xy of
/// the white it settled on.
type WhiteSolution = (Matrix3, f64, Option<(f64, f64)>);

/// Interpolates the XYZ→camera matrix for the white balance whose camera
/// neutral is `neutral` (camera RGB of a grey, normalised to green).
/// Returns the matrix and the white-balance temperature it was solved for,
/// plus the white's xy when the iteration did not converge.
fn interpolate_xyz_to_camera(
    low: (f64, &Matrix3),
    high: (f64, &Matrix3),
    neutral: [f64; 3],
) -> Option<WhiteSolution> {
    interpolate_calibrated(low, high, None, neutral)
}

/// `blend` of the two matrices, then (DNG SDK `FindXYZtoCamera`) the
/// camera calibration blended by the same weight in front of them.
fn blend_calibrated(
    low: &Matrix3,
    high: &Matrix3,
    weight_low: f64,
    calibration: Option<&CameraCalibration>,
) -> Matrix3 {
    let mut matrix = blend(low, high, weight_low);
    if let Some([cal_low, cal_high]) = calibration {
        for (r, row) in matrix.iter_mut().enumerate() {
            let gain = cal_low[r] * weight_low + cal_high[r] * (1.0 - weight_low);
            row.iter_mut().for_each(|v| *v *= gain);
        }
    }
    matrix
}

fn interpolate_calibrated(
    low: (f64, &Matrix3),
    high: (f64, &Matrix3),
    calibration: Option<&CameraCalibration>,
    neutral: [f64; 3],
) -> Option<WhiteSolution> {
    let (low_temperature, low_matrix) = low;
    let (high_temperature, high_matrix) = high;
    // D50 start, as the DNG SDK.
    let mut temperature = 5000.0;
    let mut matrix = blend_calibrated(
        low_matrix,
        high_matrix,
        low_temperature_weight(temperature, low_temperature, high_temperature),
        calibration,
    );
    let mut xy = (0.3457, 0.3585);
    for pass in 0..30 {
        let camera_to_xyz = invert(&matrix)?;
        let xyz: [f64; 3] =
            std::array::from_fn(|r| (0..3).map(|c| camera_to_xyz[r][c] * neutral[c]).sum());
        let sum = xyz[0] + xyz[1] + xyz[2];
        if !(sum.is_finite() && sum > 0.0 && xyz[1] > 0.0) {
            return None;
        }
        let mut next_xy = (xyz[0] / sum, xyz[1] / sum);
        // Still not converged after 30 passes: most likely a two-value
        // oscillation, which the DNG SDK ends with the mean of the last two
        // estimates (Realme RMX1851: 4904 K / 1944 K).
        if pass == 29 {
            next_xy = ((xy.0 + next_xy.0) * 0.5, (xy.1 + next_xy.1) * 0.5);
        }
        xy = next_xy;
        let next = xy_to_temperature(next_xy.0, next_xy.1);
        matrix = blend_calibrated(
            low_matrix,
            high_matrix,
            low_temperature_weight(next, low_temperature, high_temperature),
            calibration,
        );
        let converged = (next - temperature).abs() < 0.5;
        temperature = next;
        if converged {
            return Some((matrix, temperature, None));
        }
    }
    Some((matrix, temperature, Some(xy)))
}

/// Weight of the lower-temperature calibration for a white balance whose
/// camera neutral is `neutral`, found as for the camera's matrices (DNG SDK
/// search); a camera profile's matrices and tables blend by it.
pub fn calibration_weight(
    low: (f64, &Matrix3),
    high: (f64, &Matrix3),
    neutral: [f64; 3],
) -> Option<f64> {
    let (low_matrix, high_matrix) = (
        normalize_color_matrix(*low.1),
        normalize_color_matrix(*high.1),
    );
    let (_, temperature, _) =
        interpolate_xyz_to_camera((low.0, &low_matrix), (high.0, &high_matrix), neutral)?;
    Some(low_temperature_weight(temperature, low.0, high.0))
}

/// Replaces a camera's calibration matrices by the single matrix for its
/// as-shot white balance. Profiles with one usable matrix, four-colour
/// matrices or no valid white balance keep the matrix closest to daylight.
pub fn select_color_matrix(
    color_matrix: &mut HashMap<Illuminant, FlatColorMatrix>,
    wb_coeffs: [f32; 4],
) {
    if let Some((matrix, temperature)) = interpolated_matrix(color_matrix, wb_coeffs) {
        log::info!("Colour matrix interpolated for {temperature:.0} K");
        color_matrix.clear();
        color_matrix.insert(
            Illuminant::D65,
            matrix.iter().flatten().map(|v| *v as f32).collect(),
        );
        return;
    }
    if color_matrix.len() < 2 {
        return;
    }
    if let Some(best) = daylight_illuminant(color_matrix.keys().copied()) {
        color_matrix.retain(|illuminant, _| *illuminant == best);
    }
}

/// How the two calibrations of a profile mix for a white balance: the
/// lower- and higher-temperature illuminants and the weight of the lower.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CalibrationBlend {
    pub low: Illuminant,
    pub high: Illuminant,
    pub low_weight: f64,
}

/// The blend the DNG SDK uses for every per-illuminant profile part
/// (forward matrices, camera calibrations) at this white balance.
pub fn calibration_blend(
    color_matrix: &HashMap<Illuminant, FlatColorMatrix>,
    wb_coeffs: [f32; 4],
) -> Option<CalibrationBlend> {
    let (calibrations, neutral) = calibrations_and_neutral(color_matrix, wb_coeffs)?;
    let (low, high) = (calibrations.first()?, calibrations.last()?);
    let (_, temperature, _) =
        interpolate_xyz_to_camera((low.0, &low.2), (high.0, &high.2), neutral)?;
    Some(CalibrationBlend {
        low: low.1,
        high: high.1,
        low_weight: low_temperature_weight(temperature, low.0, high.0),
    })
}

fn interpolated_matrix(
    color_matrix: &HashMap<Illuminant, FlatColorMatrix>,
    wb_coeffs: [f32; 4],
) -> Option<(Matrix3, f64)> {
    let (calibrations, neutral) = calibrations_and_neutral(color_matrix, wb_coeffs)?;
    let (low, high) = (calibrations.first()?, calibrations.last()?);
    interpolate_xyz_to_camera((low.0, &low.2), (high.0, &high.2), neutral)
        .map(|(matrix, temperature, _)| (matrix, temperature))
}

/// The white balance Camera Raw renders when the DNG SDK's search for the
/// as-shot white does not converge: it keeps the mean of the last two
/// estimates and derives the camera neutral back from that white, which
/// differs from the as-shot neutral (Realme RMX1851). `None` when the search
/// converges (the as-shot balance stands).
pub fn unconverged_white_balance(
    color_matrix: &HashMap<Illuminant, FlatColorMatrix>,
    wb_coeffs: [f32; 4],
) -> Option<[f32; 4]> {
    let (calibrations, neutral) = calibrations_and_neutral(color_matrix, wb_coeffs)?;
    let (low, high) = (calibrations.first()?, calibrations.last()?);
    let (matrix, _, xy) = interpolate_xyz_to_camera((low.0, &low.2), (high.0, &high.2), neutral)?;
    let (x, y) = xy?;
    let white = [x / y, 1.0, (1.0 - x - y) / y];
    let camera: [f64; 3] = std::array::from_fn(|r| (0..3).map(|c| matrix[r][c] * white[c]).sum());
    if !camera.iter().all(|v| v.is_finite() && *v > 0.0) {
        return None;
    }
    let green = wb_coeffs[1];
    Some([
        (camera[1] / camera[0]) as f32 * green,
        green,
        (camera[1] / camera[2]) as f32 * green,
        wb_coeffs[3],
    ])
}

/// Calibrations with known temperatures sorted by temperature, and the
/// camera neutral of the white balance; `None` for single-illuminant
/// profiles or unusable balances.
#[allow(clippy::type_complexity)]
fn calibrations_and_neutral(
    color_matrix: &HashMap<Illuminant, FlatColorMatrix>,
    wb_coeffs: [f32; 4],
) -> Option<(Vec<(f64, Illuminant, Matrix3)>, [f64; 3])> {
    let mut calibrations: Vec<(f64, Illuminant, Matrix3)> = color_matrix
        .iter()
        .filter_map(|(illuminant, flat)| {
            Some((
                illuminant_temperature(*illuminant)?,
                *illuminant,
                normalize_color_matrix(matrix3(flat)?),
            ))
        })
        .collect();
    // Deterministic order: rawler stores the matrices in a HashMap.
    calibrations.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    let (low, high) = (calibrations.first()?, calibrations.last()?);
    if high.0 - low.0 < 1.0 {
        return None;
    }
    let wb: [f64; 3] = std::array::from_fn(|i| wb_coeffs[i] as f64);
    if wb.iter().any(|v| !v.is_finite() || *v <= 0.0) {
        return None;
    }
    let neutral = [wb[1] / wb[0], 1.0, wb[1] / wb[2]];
    Some((calibrations, neutral))
}

/// The illuminant closest to daylight (ties broken by enum order).
fn daylight_illuminant(illuminants: impl Iterator<Item = Illuminant>) -> Option<Illuminant> {
    let rank = |illuminant: &Illuminant| match illuminant {
        Illuminant::D65 => 0,
        Illuminant::D55 => 1,
        Illuminant::D75 => 2,
        Illuminant::D50 => 3,
        Illuminant::Daylight | Illuminant::FineWeather => 4,
        Illuminant::CloudyWeather | Illuminant::Shade | Illuminant::Flash => 5,
        Illuminant::C => 6,
        Illuminant::B => 7,
        Illuminant::Unknown => 8,
        Illuminant::DaylightFluorescent
        | Illuminant::DaylightWhiteFluorescent
        | Illuminant::CoolWhiteFluorescent
        | Illuminant::WhiteFluorescent
        | Illuminant::Fluorescent => 9,
        Illuminant::A | Illuminant::Tungsten | Illuminant::IsoStudioTungsten => 10,
    };
    illuminants.min_by_key(|illuminant| (rank(illuminant), *illuminant))
}

// ---------------------------------------------------------------------
// White balance in Kelvin and tint, as the DNG SDK (`dng_temperature`):
// Robertson's isotemperature lines in CIE 1960 uv, tint = offset along the
// line's normal × −3000.

/// Robertson's table: reciprocal megakelvin, u, v, isotherm slope.
const ROBERTSON: [[f64; 4]; 31] = [
    [0.0, 0.18006, 0.26352, -0.24341],
    [10.0, 0.18066, 0.26589, -0.25479],
    [20.0, 0.18133, 0.26846, -0.26876],
    [30.0, 0.18208, 0.27119, -0.28539],
    [40.0, 0.18293, 0.27407, -0.30470],
    [50.0, 0.18388, 0.27709, -0.32675],
    [60.0, 0.18494, 0.28021, -0.35156],
    [70.0, 0.18611, 0.28342, -0.37915],
    [80.0, 0.18740, 0.28668, -0.40955],
    [90.0, 0.18880, 0.28997, -0.44278],
    [100.0, 0.19032, 0.29326, -0.47888],
    [125.0, 0.19462, 0.30141, -0.58204],
    [150.0, 0.19962, 0.30921, -0.70471],
    [175.0, 0.20525, 0.31647, -0.84901],
    [200.0, 0.21142, 0.32312, -1.0182],
    [225.0, 0.21807, 0.32909, -1.2168],
    [250.0, 0.22511, 0.33439, -1.4512],
    [275.0, 0.23247, 0.33904, -1.7298],
    [300.0, 0.24010, 0.34308, -2.0637],
    [325.0, 0.24792, 0.34655, -2.4681],
    [350.0, 0.25591, 0.34951, -2.9641],
    [375.0, 0.26400, 0.35200, -3.5814],
    [400.0, 0.27218, 0.35407, -4.3633],
    [425.0, 0.28039, 0.35577, -5.3762],
    [450.0, 0.28863, 0.35714, -6.7262],
    [475.0, 0.29685, 0.35823, -8.5955],
    [500.0, 0.30505, 0.35907, -11.324],
    [525.0, 0.31320, 0.35968, -15.628],
    [550.0, 0.32129, 0.36011, -23.325],
    [575.0, 0.32931, 0.36038, -40.770],
    [600.0, 0.33724, 0.36051, -116.45],
];
const TINT_SCALE: f64 = -3000.0;

/// Temperature (K) and tint of a white given as CIE xy.
pub fn temperature_tint_from_xy(x: f64, y: f64) -> (f64, f64) {
    let denominator = 1.5 - x + 6.0 * y;
    let (u, v) = (2.0 * x / denominator, 3.0 * y / denominator);
    let (mut last_dt, mut last_du, mut last_dv) = (0.0, 0.0, 0.0);
    for index in 1..ROBERTSON.len() {
        let mut du: f64 = 1.0;
        let mut dv = ROBERTSON[index][3];
        let len = du.hypot(dv);
        du /= len;
        dv /= len;
        let uu = u - ROBERTSON[index][1];
        let vv = v - ROBERTSON[index][2];
        let mut dt = -uu * dv + vv * du;
        if dt <= 0.0 || index == ROBERTSON.len() - 1 {
            if dt > 0.0 {
                dt = 0.0;
            }
            dt = -dt;
            let f = if index == 1 { 0.0 } else { dt / (last_dt + dt) };
            let temperature =
                1.0e6 / (ROBERTSON[index - 1][0] * f + ROBERTSON[index][0] * (1.0 - f)).max(1e-6);
            let uu = u - (ROBERTSON[index - 1][1] * f + ROBERTSON[index][1] * (1.0 - f));
            let vv = v - (ROBERTSON[index - 1][2] * f + ROBERTSON[index][2] * (1.0 - f));
            let mut du = du * (1.0 - f) + last_du * f;
            let mut dv = dv * (1.0 - f) + last_dv * f;
            let len = du.hypot(dv);
            du /= len;
            dv /= len;
            return (temperature, (uu * du + vv * dv) * TINT_SCALE);
        }
        last_dt = dt;
        last_du = du;
        last_dv = dv;
    }
    (50_000.0, 0.0)
}

/// CIE xy of a white given as temperature (K) and tint.
pub fn xy_from_temperature_tint(temperature: f64, tint: f64) -> (f64, f64) {
    let r = 1.0e6 / temperature.clamp(1000.0, 1.0e6);
    let offset = tint / TINT_SCALE;
    for index in 0..ROBERTSON.len() - 1 {
        if r < ROBERTSON[index + 1][0] || index == ROBERTSON.len() - 2 {
            let f = (ROBERTSON[index + 1][0] - r) / (ROBERTSON[index + 1][0] - ROBERTSON[index][0]);
            let mut u = ROBERTSON[index][1] * f + ROBERTSON[index + 1][1] * (1.0 - f);
            let mut v = ROBERTSON[index][2] * f + ROBERTSON[index + 1][2] * (1.0 - f);
            let normal = |slope: f64| {
                let len = 1.0f64.hypot(slope);
                (1.0 / len, slope / len)
            };
            let (u1, v1) = normal(ROBERTSON[index][3]);
            let (u2, v2) = normal(ROBERTSON[index + 1][3]);
            let (mut u3, mut v3) = (u1 * f + u2 * (1.0 - f), v1 * f + v2 * (1.0 - f));
            let len = u3.hypot(v3);
            u3 /= len;
            v3 /= len;
            u += u3 * offset;
            v += v3 * offset;
            let d = u - 4.0 * v + 2.0;
            return (1.5 * u / d, v / d);
        }
    }
    (0.3127, 0.3290)
}

/// Temperature and tint of a camera's white balance (its multipliers), as
/// Camera Raw shows "As Shot".
pub fn white_balance_temperature(
    color_matrix: &HashMap<Illuminant, FlatColorMatrix>,
    calibration: Option<&CameraCalibration>,
    wb_coeffs: [f32; 4],
) -> Option<(f64, f64)> {
    let (calibrations, neutral) = calibrations_and_neutral(color_matrix, wb_coeffs)?;
    let (low, high) = (calibrations.first()?, calibrations.last()?);
    let (matrix, _, _) =
        interpolate_calibrated((low.0, &low.2), (high.0, &high.2), calibration, neutral)?;
    let camera_to_xyz = invert(&matrix)?;
    let xyz: [f64; 3] =
        std::array::from_fn(|r| (0..3).map(|c| camera_to_xyz[r][c] * neutral[c]).sum());
    let sum = xyz[0] + xyz[1] + xyz[2];
    (sum > 0.0).then(|| temperature_tint_from_xy(xyz[0] / sum, xyz[1] / sum))
}

/// White-balance multipliers (green = `green`) for a temperature and tint:
/// the white's XYZ through the camera matrix interpolated at that
/// temperature gives the camera neutral (DNG SDK `NeutralToXY` inverse).
pub fn white_balance_multipliers(
    color_matrix: &HashMap<Illuminant, FlatColorMatrix>,
    calibration: Option<&CameraCalibration>,
    temperature: f64,
    tint: f64,
    green: f32,
) -> Option<[f32; 4]> {
    let (calibrations, _) = calibrations_and_neutral(color_matrix, [1.0, 1.0, 1.0, 1.0])?;
    let (low, high) = (calibrations.first()?, calibrations.last()?);
    let (x, y) = xy_from_temperature_tint(temperature, tint);
    let white = [x / y, 1.0, (1.0 - x - y) / y];
    let (t, _) = temperature_tint_from_xy(x, y);
    let matrix = blend_calibrated(
        &low.2,
        &high.2,
        low_temperature_weight(t, low.0, high.0),
        calibration,
    );
    let camera: [f64; 3] = std::array::from_fn(|r| (0..3).map(|c| matrix[r][c] * white[c]).sum());
    if !camera.iter().all(|v| v.is_finite() && *v > 0.0) {
        return None;
    }
    let g = green as f64;
    Some([
        (camera[1] / camera[0] * g) as f32,
        green,
        (camera[1] / camera[2] * g) as f32,
        f32::NAN,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    // Sony ILCE-7M3 calibrations (Adobe DNG Converter).
    const A: [f32; 9] = [
        0.8664, -0.4056, 0.0403, -0.4565, 1.2378, 0.2453, -0.0301, 0.0889, 0.7157,
    ];
    const D65: [f32; 9] = [
        0.7374, -0.2389, -0.0551, -0.5435, 1.3162, 0.2519, -0.1006, 0.1795, 0.6552,
    ];

    fn sony() -> HashMap<Illuminant, FlatColorMatrix> {
        HashMap::from([(Illuminant::A, A.to_vec()), (Illuminant::D65, D65.to_vec())])
    }

    /// White-balance multipliers that neutralise the given XYZ white.
    fn wb_for_white(matrix: &[f32; 9], xyz: [f64; 3]) -> [f32; 4] {
        let m = matrix3(&matrix.to_vec()).unwrap();
        let camera: [f64; 3] = std::array::from_fn(|r| (0..3).map(|c| m[r][c] * xyz[c]).sum());
        [
            (camera[1] / camera[0]) as f32,
            1.0,
            (camera[1] / camera[2]) as f32,
            f32::NAN,
        ]
    }

    #[test]
    fn unit_calibration_gives_camera_raws_kelvin_readout() {
        // A public ILCE-7M3 frame: camera multipliers from SR2 0x7313
        // (2340 / 1024 / 1664) and Adobe DNG Converter's CameraCalibration
        // (red 1.0476, blue 0.9106). Camera Raw shows 5450 K / +6; the
        // converter's DNG computes to 5439 K / +6.
        let wb = [2340.0 / 1024.0, 1.0, 1664.0 / 1024.0, f32::NAN];
        let (t, tint) = white_balance_temperature(&sony(), None, wb).unwrap();
        assert!(
            (t - 4909.0).abs() < 15.0 && (tint - 12.0).abs() < 1.0,
            "{t} {tint}"
        );
        let calibration = [[1.0476, 1.0, 0.9106]; 2];
        let (t, tint) = white_balance_temperature(&sony(), Some(&calibration), wb).unwrap();
        assert!(
            (t - 5439.0).abs() < 15.0 && (tint - 6.0).abs() < 1.0,
            "{t} {tint}"
        );
        // The inverse maps back to the camera's multipliers.
        let m = white_balance_multipliers(&sony(), Some(&calibration), t, tint, 1.0).unwrap();
        assert!(
            (m[0] - wb[0]).abs() < 2e-3 && (m[2] - wb[2]).abs() < 2e-3,
            "{m:?}"
        );
    }

    #[test]
    fn calibration_whites_select_their_own_matrix() {
        // White points of illuminant A and D65 (Y = 1).
        let a_white = [1.09850, 1.0, 0.35585];
        let d65_white = [0.95047, 1.0, 1.08883];

        let mut tungsten = sony();
        select_color_matrix(&mut tungsten, wb_for_white(&A, a_white));
        let picked = &tungsten[&Illuminant::D65];
        for (got, want) in picked.iter().zip(A) {
            assert!((got - want).abs() < 2e-3, "{picked:?}");
        }

        let mut daylight = sony();
        select_color_matrix(&mut daylight, wb_for_white(&D65, d65_white));
        let picked = &daylight[&Illuminant::D65];
        for (got, want) in picked.iter().zip(D65) {
            assert!((got - want).abs() < 2e-3, "{picked:?}");
        }
    }

    #[test]
    fn daylight_white_balance_blends_mostly_d65() {
        let d55_white = [0.95682, 1.0, 0.92149];
        let mut matrices = sony();
        let wb = wb_for_white(&D65, d55_white);
        let (_, temperature) = interpolated_matrix(&matrices, wb).unwrap();
        assert!((5200.0..5900.0).contains(&temperature), "{temperature}");
        select_color_matrix(&mut matrices, wb);
        assert_eq!(matrices.len(), 1);
        let weight = low_temperature_weight(temperature, 2850.0, 6500.0);
        assert!((0.05..0.25).contains(&weight), "{weight}");
    }

    #[test]
    fn oscillating_white_search_ends_on_the_mean_like_the_dng_sdk() {
        // Realme RMX1851: D55 and A matrices whose white search alternates
        // between about 4900 K and 1950 K.
        let d55 = [
            0.3515625, -0.09375, -0.0625, -0.328125, 1.3125, 0.0, -0.140625, 0.4765625, 0.5546875,
        ];
        let a = [
            0.890625, -0.2265625, -0.1640625, -0.328125, 1.3125, 0.0, -0.1015625, 0.328125,
            0.3828125,
        ];
        let matrices =
            HashMap::from([(Illuminant::D55, d55.to_vec()), (Illuminant::A, a.to_vec())]);
        // As-shot neutral 0.4990 1 0.5342.
        let wb = [1.0 / 0.4990234, 1.0, 1.0 / 0.5341797, f32::NAN];
        let (_, temperature) = interpolated_matrix(&matrices, wb).unwrap();
        assert!((2750.0..3050.0).contains(&temperature), "{temperature}");
        let balanced = unconverged_white_balance(&matrices, wb).unwrap();
        // The derived neutral is bluer than the as-shot one.
        assert!(balanced[2] / balanced[0] > wb[2] / wb[0], "{balanced:?}");
        // A converging search keeps the as-shot balance.
        let sony_wb = wb_for_white(&D65, [0.95682, 1.0, 0.92149]);
        assert!(unconverged_white_balance(&sony(), sony_wb).is_none());
    }

    #[test]
    fn matrices_are_normalised_before_blending() {
        let scaled: Matrix3 = matrix3(&D65.to_vec())
            .unwrap()
            .map(|row| row.map(|v| v * 250.0));
        let normal = normalize_color_matrix(matrix3(&D65.to_vec()).unwrap());
        let rescaled = normalize_color_matrix(scaled);
        // Within the SDK's 1 % band the unscaled matrix is kept as it is.
        for (a, b) in normal.iter().flatten().zip(rescaled.iter().flatten()) {
            assert!((a - b).abs() <= 0.011 * a.abs().max(0.01), "{a} {b}");
        }
        // A ×250 D65 matrix and the A matrix blend like the unscaled pair.
        let flat_scaled: Vec<f32> = D65.iter().map(|v| v * 250.0).collect();
        let wb = wb_for_white(&D65, [0.95682, 1.0, 0.92149]);
        let plain = interpolated_matrix(&sony(), wb).unwrap().1;
        let big = HashMap::from([(Illuminant::A, A.to_vec()), (Illuminant::D65, flat_scaled)]);
        let scaled_t = interpolated_matrix(&big, wb).unwrap().1;
        assert!((plain - scaled_t).abs() < 1.0, "{plain} {scaled_t}");
    }

    #[test]
    fn missing_white_balance_keeps_daylight_matrix() {
        let mut matrices = sony();
        select_color_matrix(&mut matrices, [f32::NAN; 4]);
        assert_eq!(matrices.len(), 1);
        assert_eq!(matrices[&Illuminant::D65], D65.to_vec());
    }

    #[test]
    fn daylight_illuminant_prefers_d65_then_d50() {
        use Illuminant::{A, D50, D65, Tungsten};
        assert_eq!(daylight_illuminant([A, D65].into_iter()), Some(D65));
        assert_eq!(daylight_illuminant([A, D50].into_iter()), Some(D50));
        assert_eq!(daylight_illuminant([D50, A].into_iter()), Some(D50));
        // Equal rank: the enum order decides, whatever the map order.
        assert_eq!(
            daylight_illuminant([A, Tungsten].into_iter()),
            Some(Tungsten)
        );
        assert_eq!(
            daylight_illuminant([Tungsten, A].into_iter()),
            Some(Tungsten)
        );
        assert_eq!(daylight_illuminant(std::iter::empty()), None);
    }

    #[test]
    fn kelvin_and_tint_round_trip_like_the_dng_sdk() {
        for (t, tint) in [
            (2850.0, 0.0),
            (5500.0, 10.0),
            (6500.0, -20.0),
            (7500.0, 35.0),
            (3800.0, 21.0),
        ] {
            let (x, y) = xy_from_temperature_tint(t, tint);
            let (t2, tint2) = temperature_tint_from_xy(x, y);
            assert!((t2 - t).abs() / t < 0.002, "{t} -> {t2}");
            assert!((tint2 - tint).abs() < 0.5, "{tint} -> {tint2}");
        }
        // D65 lies near 6500 K with a small green-magenta offset.
        let (t, tint) = temperature_tint_from_xy(0.3127, 0.3290);
        assert!((6400.0..6600.0).contains(&t), "{t}");
        assert!(tint.abs() < 15.0, "{tint}");
    }

    #[test]
    fn multipliers_follow_the_temperature() {
        let matrices = sony();
        let warm = white_balance_multipliers(&matrices, None, 3000.0, 0.0, 1.0).unwrap();
        let cool = white_balance_multipliers(&matrices, None, 7500.0, 0.0, 1.0).unwrap();
        // A warm light needs less red gain and more blue gain.
        assert!(warm[0] < cool[0] && warm[2] > cool[2]);
        // As-shot readout round-trips.
        let (t, tint) = white_balance_temperature(&matrices, None, cool).unwrap();
        assert!((t - 7500.0).abs() < 60.0, "{t}");
        assert!(tint.abs() < 2.0, "{tint}");
    }
}
