//! Camera profiles the photographer chooses: DCP files (DNG Camera
//! Profile, e.g. from a ColorChecker or dcamprof). Rawtonic never picks one
//! by itself; a recipe names one in `cameraProfile`, as a path inside the
//! session (`profiles/Name.dcp`) or, outside sessions, an absolute path.
//!
//! A profile replaces the look's colour: as the DNG SDK renders it, the
//! camera's white-balanced RGB goes through the profile's forward matrix
//! (or colour matrix) to XYZ D50, then the HueSatMap acts on HSV of linear
//! ProPhoto before exposure, the LookTable after exposure, and the
//! profile's tone curve (or Camera Raw's default curve) maps tones. The
//! decoded image is linear sRGB made with the camera's own matrix, so the
//! shader first undoes that matrix (`image_color`) and applies the
//! profile's in one 3×3 step; matrices and HueSatMaps blend between the
//! two calibrations by the as-shot white balance like the camera's.
//!
//! The Standard and Camera Raw looks take profiles; Classic and AgX edits
//! ignore them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use crate::color_calibration::{Matrix3, invert};
use crate::dng_color::apply as transform;
use crate::dng_color::{D50, bradford, diagonal, multiply, normalize_forward_matrix};
use crate::image_processing::GlobalAdjustments;

const TAG_UNIQUE_CAMERA_MODEL: u16 = 50708;
const TAG_COLOR_MATRIX1: u16 = 50721;
const TAG_COLOR_MATRIX2: u16 = 50722;
const TAG_CALIBRATION_ILLUMINANT1: u16 = 50778;
const TAG_CALIBRATION_ILLUMINANT2: u16 = 50779;
const TAG_PROFILE_NAME: u16 = 50936;
const TAG_HUE_SAT_MAP_DIMS: u16 = 50937;
const TAG_HUE_SAT_MAP1: u16 = 50938;
const TAG_HUE_SAT_MAP2: u16 = 50939;
const TAG_PROFILE_TONE_CURVE: u16 = 50940;
const TAG_FORWARD_MATRIX1: u16 = 50964;
const TAG_FORWARD_MATRIX2: u16 = 50965;
const TAG_LOOK_TABLE_DIMS: u16 = 50981;
const TAG_LOOK_TABLE_DATA: u16 = 50982;
const TAG_HUE_SAT_MAP_ENCODING: u16 = 51107;
const TAG_LOOK_TABLE_ENCODING: u16 = 51108;
const TAG_BASELINE_EXPOSURE_OFFSET: u16 = 51109;

/// Table entries the GPU holds for one profile (HueSatMap plus LookTable).
pub const TABLE_CAPACITY: usize = 65_536;

/// Linear sRGB (D65) to XYZ D50 (Bradford) and back, for the profile's PCS.
const XYZ_D50_TO_SRGB: Matrix3 = [
    [3.1338561, -1.6168667, -0.4906146],
    [-0.9787684, 1.9161415, 0.0334540],
    [0.0719453, -0.2289914, 1.4052427],
];

/// One HSV table: per entry hue shift (degrees), saturation scale and value
/// scale, value-major, then hue, then saturation.
#[derive(Debug, Clone, PartialEq)]
pub struct HsvTable {
    pub hue_divisions: usize,
    pub sat_divisions: usize,
    pub val_divisions: usize,
    /// Value axis indexed in sRGB gamma instead of linear.
    pub srgb_value: bool,
    pub data: Vec<[f32; 3]>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Profile {
    pub name: String,
    /// The camera the profile was made for (UniqueCameraModel).
    pub camera: String,
    pub illuminants: (u16, u16),
    pub color_matrix: [Option<Matrix3>; 2],
    pub forward_matrix: [Option<Matrix3>; 2],
    pub hue_sat_map: [Option<HsvTable>; 2],
    pub look_table: Option<HsvTable>,
    /// ProfileToneCurve as (x, y) pairs, linear 0–1.
    pub tone_curve: Option<Vec<f32>>,
    pub baseline_exposure_offset: f32,
}

/// Correlated colour temperature of an EXIF LightSource code.
fn illuminant_temperature(code: u16) -> Option<f64> {
    Some(match code {
        17 | 3 => 2856.0,
        24 => 3200.0,
        15 => 3450.0,
        14 | 2 => 4150.0,
        23 | 13 => 5003.0,
        1 | 4 | 9 | 18 | 20 => 5500.0,
        12 => 6400.0,
        10 | 19 | 21 => 6504.0,
        11 | 22 => 7500.0,
        _ => return None,
    })
}

/// Raw IFD0 entries of a TIFF/DNG or DCP ("IIRC") file.
struct Ifd<'a> {
    bytes: &'a [u8],
    little: bool,
    entries: Vec<(u16, u16, u32, usize)>,
}

impl<'a> Ifd<'a> {
    fn parse(bytes: &'a [u8]) -> Option<Self> {
        let little = match bytes.get(0..4)? {
            b"II*\0" | b"IIRC" => true,
            b"MM\0*" | b"MMCR" => false,
            _ => return None,
        };
        let mut ifd = Ifd {
            bytes,
            little,
            entries: Vec::new(),
        };
        let offset = ifd.u32_at(4)? as usize;
        let count = ifd.u16_at(offset)? as usize;
        for i in 0..count {
            let at = offset + 2 + 12 * i;
            let (tag, kind, n) = (ifd.u16_at(at)?, ifd.u16_at(at + 2)?, ifd.u32_at(at + 4)?);
            ifd.entries.push((tag, kind, n, at + 8));
        }
        Some(ifd)
    }

    fn u16_at(&self, at: usize) -> Option<u16> {
        let b: [u8; 2] = self.bytes.get(at..at + 2)?.try_into().ok()?;
        Some(if self.little {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        })
    }

    fn u32_at(&self, at: usize) -> Option<u32> {
        let b: [u8; 4] = self.bytes.get(at..at + 4)?.try_into().ok()?;
        Some(if self.little {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        })
    }

    fn data(&self, tag: u16, size: usize) -> Option<(usize, usize, u16)> {
        let &(_, kind, n, value_at) = self.entries.iter().find(|e| e.0 == tag)?;
        let n = n as usize;
        let len = size.checked_mul(n)?;
        let at = if len <= 4 {
            value_at
        } else {
            self.u32_at(value_at)? as usize
        };
        self.bytes.get(at..at.checked_add(len)?)?;
        Some((at, n, kind))
    }

    fn text(&self, tag: u16) -> Option<String> {
        let (at, n, kind) = self.data(tag, 1)?;
        if kind != 2 && kind != 1 && kind != 7 {
            return None;
        }
        let raw = &self.bytes[at..at + n];
        let end = raw.iter().position(|b| *b == 0).unwrap_or(raw.len());
        Some(String::from_utf8_lossy(&raw[..end]).trim().to_string())
    }

    /// Numeric values of a tag as f64 (SHORT, LONG, RATIONAL, SRATIONAL,
    /// FLOAT, DOUBLE).
    fn values(&self, tag: u16) -> Option<Vec<f64>> {
        let kind = self.entries.iter().find(|e| e.0 == tag)?.1;
        let size = match kind {
            3 => 2,
            4 | 9 | 11 => 4,
            5 | 10 | 12 => 8,
            _ => return None,
        };
        let (data_at, n, _) = self.data(tag, size)?;
        (0..n)
            .map(|i| {
                let at = data_at + size * i;
                Some(match kind {
                    3 => self.u16_at(at)? as f64,
                    4 => self.u32_at(at)? as f64,
                    9 => self.u32_at(at)? as i32 as f64,
                    11 => f32::from_bits(self.u32_at(at)?) as f64,
                    5 => self.u32_at(at)? as f64 / self.u32_at(at + 4)?.max(1) as f64,
                    10 => {
                        let d = self.u32_at(at + 4)? as i32;
                        if d == 0 {
                            0.0
                        } else {
                            self.u32_at(at)? as i32 as f64 / d as f64
                        }
                    }
                    12 => {
                        let (a, b) = (self.u32_at(at)? as u64, self.u32_at(at + 4)? as u64);
                        f64::from_bits(if self.little {
                            b << 32 | a
                        } else {
                            a << 32 | b
                        })
                    }
                    _ => return None,
                })
            })
            .collect()
    }

    fn matrix(&self, tag: u16) -> Option<Matrix3> {
        let v = self.values(tag)?;
        (v.len() == 9 && v.iter().all(|x| x.is_finite()))
            .then(|| std::array::from_fn(|r| std::array::from_fn(|c| v[r * 3 + c])))
    }

    fn table(&self, dims_tag: u16, data_tag: u16, encoding_tag: u16) -> Option<HsvTable> {
        let dims = self.values(dims_tag)?;
        let (h, s, v) = (
            *dims.first()? as usize,
            *dims.get(1)? as usize,
            dims.get(2).copied().unwrap_or(1.0).max(1.0) as usize,
        );
        let data = self.values(data_tag)?;
        if h == 0 || s < 2 || data.len() != h * s * v * 3 {
            return None;
        }
        Some(HsvTable {
            hue_divisions: h,
            sat_divisions: s,
            val_divisions: v,
            srgb_value: self.values(encoding_tag).and_then(|e| e.first().copied()) == Some(1.0),
            data: data
                .as_chunks::<3>()
                .0
                .iter()
                .map(|c| [c[0] as f32, c[1] as f32, c[2] as f32])
                .collect(),
        })
    }
}

/// Reads a DCP file (or the profile in a DNG's IFD0).
pub fn parse(bytes: &[u8]) -> Result<Profile, String> {
    let ifd = Ifd::parse(bytes).ok_or("Not a DCP camera profile")?;
    let illuminant = |tag| {
        ifd.values(tag)
            .and_then(|v| v.first().copied())
            .unwrap_or(0.0) as u16
    };
    let profile = Profile {
        name: ifd.text(TAG_PROFILE_NAME).unwrap_or_default(),
        camera: ifd.text(TAG_UNIQUE_CAMERA_MODEL).unwrap_or_default(),
        illuminants: (
            illuminant(TAG_CALIBRATION_ILLUMINANT1),
            illuminant(TAG_CALIBRATION_ILLUMINANT2),
        ),
        color_matrix: [ifd.matrix(TAG_COLOR_MATRIX1), ifd.matrix(TAG_COLOR_MATRIX2)],
        forward_matrix: [
            ifd.matrix(TAG_FORWARD_MATRIX1),
            ifd.matrix(TAG_FORWARD_MATRIX2),
        ],
        hue_sat_map: [
            ifd.table(
                TAG_HUE_SAT_MAP_DIMS,
                TAG_HUE_SAT_MAP1,
                TAG_HUE_SAT_MAP_ENCODING,
            ),
            ifd.table(
                TAG_HUE_SAT_MAP_DIMS,
                TAG_HUE_SAT_MAP2,
                TAG_HUE_SAT_MAP_ENCODING,
            ),
        ],
        look_table: ifd.table(
            TAG_LOOK_TABLE_DIMS,
            TAG_LOOK_TABLE_DATA,
            TAG_LOOK_TABLE_ENCODING,
        ),
        tone_curve: ifd
            .values(TAG_PROFILE_TONE_CURVE)
            .filter(|v| v.len() >= 4 && v.len().is_multiple_of(2))
            .map(|v| v.iter().map(|x| *x as f32).collect()),
        baseline_exposure_offset: ifd
            .values(TAG_BASELINE_EXPOSURE_OFFSET)
            .and_then(|v| v.first().copied())
            .unwrap_or(0.0) as f32,
    };
    if profile.color_matrix[0].is_none() {
        return Err("The camera profile has no colour matrix".into());
    }
    let entries = profile.hue_sat_map[0].as_ref().map_or(0, |t| t.data.len())
        + profile.look_table.as_ref().map_or(0, |t| t.data.len());
    if entries > TABLE_CAPACITY {
        return Err(format!(
            "The camera profile's colour tables are too large ({entries} entries, at most {TABLE_CAPACITY})"
        ));
    }
    Ok(profile)
}

/// A profile prepared for one photo: the 3×3 step from the decoded linear
/// sRGB to the profile's linear sRGB, the tables (HueSatMap blended for
/// the white balance, then LookTable) and the tone curve.
#[derive(Debug, Clone, PartialEq)]
pub struct Rendering {
    pub matrix: [[f32; 3]; 3],
    pub hue_sat_map: Option<HsvTable>,
    pub look_table: Option<HsvTable>,
    pub tone_curve: Option<Vec<f32>>,
    pub baseline_exposure_offset: f32,
}

/// Weight of calibration 1 for this white balance (1 for one-matrix
/// profiles).
fn weight_of_first(profile: &Profile, neutral: [f64; 3]) -> f64 {
    let (Some(m1), Some(m2)) = (profile.color_matrix[0], profile.color_matrix[1]) else {
        return 1.0;
    };
    let (Some(t1), Some(t2)) = (
        illuminant_temperature(profile.illuminants.0),
        illuminant_temperature(profile.illuminants.1),
    ) else {
        return 1.0;
    };
    if (t1 - t2).abs() < 1.0 {
        return 1.0;
    }
    let (low, high, first_is_low) = if t1 < t2 {
        ((t1, &m1), (t2, &m2), true)
    } else {
        ((t2, &m2), (t1, &m1), false)
    };
    let low_weight =
        crate::color_calibration::calibration_weight(low, high, neutral).unwrap_or(0.5);
    if first_is_low {
        low_weight
    } else {
        1.0 - low_weight
    }
}

fn blend(a: Option<Matrix3>, b: Option<Matrix3>, w: f64) -> Option<Matrix3> {
    match (a, b) {
        (Some(a), Some(b)) => Some(crate::color_calibration::blend(&a, &b, w)),
        (a, b) => a.or(b),
    }
}

fn blend_tables(a: &Option<HsvTable>, b: &Option<HsvTable>, w: f32) -> Option<HsvTable> {
    match (a, b) {
        (Some(a), Some(b)) if a.data.len() == b.data.len() => {
            let mut table = a.clone();
            for (out, other) in table.data.iter_mut().zip(&b.data) {
                for k in 0..3 {
                    out[k] = out[k] * w + other[k] * (1.0 - w);
                }
            }
            Some(table)
        }
        (a, b) => a.clone().or_else(|| b.clone()),
    }
}

/// Prepares `profile` for a photo whose decode used `image`.
pub fn rendering(
    profile: &Profile,
    image: &crate::raw_processing::ImageColor,
) -> Result<Rendering, String> {
    let wb: [f64; 3] = std::array::from_fn(|i| image.wb[i] as f64);
    if wb.iter().any(|v| !v.is_finite() || *v <= 0.0) {
        return Err("The photo has no usable white balance".into());
    }
    // Camera neutral of the balance, normalised to green.
    let neutral = [wb[1] / wb[0], 1.0, wb[1] / wb[2]];
    let w = weight_of_first(profile, neutral);
    let decoded: Matrix3 =
        std::array::from_fn(|r| std::array::from_fn(|c| image.camera_to_srgb[r][c] as f64));
    let balanced_from_decoded = invert(&decoded).ok_or("Singular camera matrix")?;
    let to_srgb =
        if let Some(forward) = blend(profile.forward_matrix[0], profile.forward_matrix[1], w) {
            // White-balanced camera RGB → XYZ D50 → linear sRGB.
            let forward = normalize_forward_matrix(&forward).ok_or("Invalid forward matrix")?;
            multiply(
                &multiply(&XYZ_D50_TO_SRGB, &forward),
                &balanced_from_decoded,
            )
        } else {
            // Colour matrix only: camera → XYZ, the scene white adapted to D50.
            let color = blend(profile.color_matrix[0], profile.color_matrix[1], w)
                .ok_or("No colour matrix")?;
            let camera_to_xyz = invert(&color).ok_or("Singular colour matrix")?;
            let white = transform(&camera_to_xyz, neutral);
            let adapt = bradford(white, D50).ok_or("Invalid white")?;
            // Raw = diag(camera neutral) × white-balanced value (up to scale).
            let raw_from_balanced = diagonal(neutral);
            multiply(
                &multiply(
                    &multiply(&XYZ_D50_TO_SRGB, &multiply(&adapt, &camera_to_xyz)),
                    &raw_from_balanced,
                ),
                &balanced_from_decoded,
            )
        };
    // Keep the decode's exposure: decoded white (1, 1, 1) stays white.
    let white = transform(&to_srgb, [1.0; 3]);
    if !(white[1].is_finite() && white[1] > 1e-6) {
        return Err("Invalid profile matrix".into());
    }
    let matrix =
        std::array::from_fn(|r| std::array::from_fn(|c| (to_srgb[r][c] / white[1]) as f32));
    Ok(Rendering {
        matrix,
        hue_sat_map: blend_tables(&profile.hue_sat_map[0], &profile.hue_sat_map[1], w as f32),
        look_table: profile.look_table.clone(),
        tone_curve: profile.tone_curve.clone(),
        baseline_exposure_offset: profile.baseline_exposure_offset,
    })
}

#[cfg(any(test, feature = "test-support"))]
pub fn rgb_to_hsv(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let v = r.max(g).max(b);
    let gap = v - r.min(g).min(b);
    if gap <= 0.0 || v <= 0.0 {
        return (0.0, 0.0, v);
    }
    let h = if r == v {
        let h = (g - b) / gap;
        if h < 0.0 { h + 6.0 } else { h }
    } else if g == v {
        2.0 + (b - r) / gap
    } else {
        4.0 + (r - g) / gap
    };
    (h, gap / v, v)
}

#[cfg(any(test, feature = "test-support"))]
pub fn hsv_to_rgb(h: f32, s: f32, v: f32) -> [f32; 3] {
    if s <= 0.0 {
        return [v; 3];
    }
    let h = h.rem_euclid(6.0);
    let i = (h as usize).min(5);
    let f = h - i as f32;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    match i {
        0 => [v, t, p],
        1 => [q, v, p],
        2 => [p, v, t],
        3 => [p, q, v],
        4 => [t, p, v],
        _ => [v, p, q],
    }
}

#[cfg(any(test, feature = "test-support"))]
fn srgb_encode(v: f32) -> f32 {
    if v <= 0.0031308 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

#[cfg(any(test, feature = "test-support"))]
fn srgb_decode(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

#[cfg(any(test, feature = "test-support"))]
impl HsvTable {
    fn entry(&self, v: usize, h: usize, s: usize) -> [f32; 3] {
        self.data[(v * self.hue_divisions + h) * self.sat_divisions + s]
    }

    /// One pixel of linear ProPhoto RGB through the table, as the DNG
    /// SDK's `RefBaselineHueSatMap` (the shader's `dcp_table` mirrors it).
    pub fn apply(&self, rgb: [f32; 3]) -> [f32; 3] {
        let (h, s, v) = rgb_to_hsv(rgb[0].max(0.0), rgb[1].max(0.0), rgb[2].max(0.0));
        let h_scaled = if self.hue_divisions > 1 {
            h * self.hue_divisions as f32 / 6.0
        } else {
            0.0
        };
        let s_scaled = s * (self.sat_divisions - 1) as f32;
        let mut h0 = h_scaled as usize;
        let s0 = (s_scaled as usize).min(self.sat_divisions - 2);
        let h_fract = h_scaled - h0 as f32;
        let s_fract = s_scaled - s0 as f32;
        let mut h1 = h0 + 1;
        if h0 >= self.hue_divisions - 1 {
            h0 = self.hue_divisions - 1;
            h1 = 0;
        }
        let v_encoded = if self.srgb_value {
            srgb_encode(v.min(1.0))
        } else {
            v.min(1.0)
        };
        let (v0, v1, v_fract) = if self.val_divisions > 1 {
            let v_scaled = v_encoded * (self.val_divisions - 1) as f32;
            let v0 = (v_scaled as usize).min(self.val_divisions - 2);
            (v0, v0 + 1, v_scaled - v0 as f32)
        } else {
            (0, 0, 0.0)
        };
        let bilinear = |vi: usize| {
            let e00 = self.entry(vi, h0, s0);
            let e01 = self.entry(vi, h1, s0);
            let e10 = self.entry(vi, h0, s0 + 1);
            let e11 = self.entry(vi, h1, s0 + 1);
            std::array::from_fn::<f32, 3, _>(|k| {
                let a = e00[k] * (1.0 - h_fract) + e01[k] * h_fract;
                let b = e10[k] * (1.0 - h_fract) + e11[k] * h_fract;
                a * (1.0 - s_fract) + b * s_fract
            })
        };
        let lower = bilinear(v0);
        let shift = if self.val_divisions > 1 {
            let upper = bilinear(v1);
            std::array::from_fn::<f32, 3, _>(|k| lower[k] * (1.0 - v_fract) + upper[k] * v_fract)
        } else {
            lower
        };
        let h = h + shift[0] * (6.0 / 360.0);
        let s = (s * shift[1]).min(1.0);
        // The SDK pins the value to 1 because its data never exceeds white;
        // Rawtonic keeps highlight headroom above 1, so only scale it.
        let v = if self.srgb_value && v <= 1.0 {
            srgb_decode((v_encoded * shift[2]).clamp(0.0, 1.0))
        } else {
            v * shift[2]
        };
        hsv_to_rgb(h, s, v)
    }
}

/// GPU form of a rendering: the tables as vec4 entries (hue shift,
/// saturation scale, value scale, 0), HueSatMap first.
pub fn table_entries(rendering: &Rendering) -> Vec<[f32; 4]> {
    rendering
        .hue_sat_map
        .iter()
        .chain(rendering.look_table.iter())
        .flat_map(|t| t.data.iter().map(|e| [e[0], e[1], e[2], 0.0]))
        .collect()
}

// ---------------------------------------------------------------------
// Recipes name profiles by path; GlobalAdjustments carries a key.

fn names() -> &'static Mutex<HashMap<u32, String>> {
    static NAMES: OnceLock<Mutex<HashMap<u32, String>>> = OnceLock::new();
    NAMES.get_or_init(Default::default)
}

/// Key of a recipe's `cameraProfile` (0 = none); `apply` resolves it.
pub fn register(name: &str) -> u32 {
    let name = name.trim();
    if name.is_empty() {
        return 0;
    }
    // FNV-1a; 0 is reserved for "no profile".
    let mut hash: u32 = 0x811c_9dc5;
    for byte in name.bytes() {
        hash = (hash ^ byte as u32).wrapping_mul(0x0100_0193);
    }
    let key = hash.max(1);
    names()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key, name.to_string());
    key
}

/// Where a recipe's profile name points for the photo at `image_path`.
pub fn resolve(name: &str, image_path: &Path) -> Option<PathBuf> {
    let path = Path::new(name);
    if path.is_absolute() {
        return Some(path.to_path_buf());
    }
    // Inside the session only: no parent components.
    if path
        .components()
        .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return None;
    }
    Some(crate::session_layout::session_root(image_path)?.join(path))
}

type Prepared = Arc<(Rendering, Vec<[f32; 4]>)>;
type RenderingCache = Mutex<HashMap<String, Option<Prepared>>>;
type TableCache = Mutex<HashMap<u32, Arc<Vec<[f32; 4]>>>>;
type ColourCache =
    Mutex<HashMap<(String, Option<[u32; 3]>), Option<crate::raw_processing::ImageColor>>>;

fn renderings() -> &'static RenderingCache {
    static CACHE: OnceLock<RenderingCache> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

fn tables_by_key() -> &'static TableCache {
    static TABLES: OnceLock<TableCache> = OnceLock::new();
    TABLES.get_or_init(Default::default)
}

/// Publishes tables under `key` without a profile file (GPU tests).
#[cfg(any(test, feature = "test-support"))]
pub fn publish_tables(key: u32, entries: Vec<[f32; 4]>) {
    tables_by_key()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key, Arc::new(entries));
}

/// The GPU tables published under `key` by `apply`.
pub fn tables(key: u32) -> Option<Arc<Vec<[f32; 4]>>> {
    tables_by_key()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&key)
        .cloned()
}

/// The decode's colour for a photo, at its own or at other white-balance
/// multipliers (cached).
pub fn image_colour(
    image_path: &str,
    wb: Option<[f32; 4]>,
) -> Option<crate::raw_processing::ImageColor> {
    // Offline: the colour the file had (source_facts).
    if let Some(facts) = crate::source_facts::registered(image_path)
        && let Some(colour) = &facts.colour
    {
        return colour.colour(wb);
    }
    static CACHE: OnceLock<ColourCache> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let key = (
        image_path.to_string(),
        wb.map(|w| [w[0].to_bits(), w[1].to_bits(), w[2].to_bits()]),
    );
    if let Some(found) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return found.clone();
    }
    let colour = std::fs::read(image_path)
        .ok()
        .and_then(|bytes| crate::raw_processing::image_color_with(&bytes, wb))
        .map(|mut colour| {
            colour.unit_calibration = crate::baseline_exposure::read_camera(image_path).and_then(
                |(make, model, _, _)| {
                    crate::unit_calibration::camera_calibration(image_path, &make, &model)
                },
            );
            colour
        });
    cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key, colour.clone());
    colour
}

/// The decode's colour at the As Shot balance read from the file, and
/// whether it comes from DNG forward matrices (source_facts).
pub fn image_colour_details(image_path: &str) -> Option<(crate::raw_processing::ImageColor, bool)> {
    let bytes = std::fs::read(image_path).ok()?;
    let (mut colour, forward) = crate::raw_processing::image_color_details(&bytes, None)?;
    colour.unit_calibration =
        crate::baseline_exposure::read_camera(image_path).and_then(|(make, model, _, _)| {
            crate::unit_calibration::camera_calibration(image_path, &make, &model)
        });
    Some((colour, forward))
}

/// Camera Raw's "As Shot" temperature (K) and tint of a photo.
pub fn as_shot_white_balance(image_path: &str) -> Option<(f64, f64)> {
    let image = image_colour(image_path, None)?;
    crate::color_calibration::white_balance_temperature(
        &image.calibrations,
        image.unit_calibration.as_ref(),
        image.wb,
    )
}

/// The photo's colour at a chosen temperature and tint: the decode's
/// matrices for that balance, and the 3×3 step from the decoded image
/// (made at the camera's balance) to it.
struct Balanced {
    image: crate::raw_processing::ImageColor,
    matrix: Matrix3,
}

fn balanced(image_path: &str, temperature: f64, tint: f64) -> Option<Balanced> {
    let shot = image_colour(image_path, None)?;
    let wb = crate::color_calibration::white_balance_multipliers(
        &shot.calibrations,
        shot.unit_calibration.as_ref(),
        temperature,
        tint,
        shot.wb[1],
    )?;
    let new = image_colour(image_path, Some(wb))?;
    let f64m = |m: [[f32; 3]; 3]| -> Matrix3 {
        std::array::from_fn(|r| std::array::from_fn(|c| m[r][c] as f64))
    };
    let shot_from_decoded = invert(&f64m(shot.camera_to_srgb))?;
    let gain = diagonal(std::array::from_fn(|i| (new.wb[i] / shot.wb[i]) as f64));
    let matrix = multiply(
        &multiply(&f64m(new.camera_to_srgb), &gain),
        &shot_from_decoded,
    );
    // For a profile: the decode as if made at the new balance.
    let back = diagonal(std::array::from_fn(|i| (shot.wb[i] / new.wb[i]) as f64));
    let virtual_decode = multiply(&f64m(shot.camera_to_srgb), &back);
    Some(Balanced {
        image: crate::raw_processing::ImageColor {
            camera_to_srgb: std::array::from_fn(|r| {
                std::array::from_fn(|c| virtual_decode[r][c] as f32)
            }),
            wb: new.wb,
            calibrations: new.calibrations,
            unit_calibration: new.unit_calibration,
        },
        matrix,
    })
}

fn prepare(
    profile_path: &Path,
    image_path: &str,
    image: &crate::raw_processing::ImageColor,
    key: &str,
) -> Option<Prepared> {
    if let Some(found) = renderings()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(key)
    {
        return found.clone();
    }
    let result = (|| {
        let profile = parse(&std::fs::read(profile_path).map_err(|e| e.to_string())?)?;
        let rendering = rendering(&profile, image)?;
        let entries = table_entries(&rendering);
        Ok::<_, String>(Arc::new((rendering, entries)))
    })();
    let prepared = match result {
        Ok(prepared) => Some(prepared),
        Err(error) => {
            log::warn!(
                "Camera profile {} not applied to {image_path}: {error}",
                profile_path.display()
            );
            None
        }
    };
    renderings()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key.to_string(), prepared.clone());
    prepared
}

/// Base look flags (`profile_curve[15].w`) for a profile with its own curve
/// and for one that uses Camera Raw's default curve.
pub const FLAG_OWN_CURVE: f32 = 4.0;
pub const FLAG_DEFAULT_CURVE: f32 = 5.0;

/// Applies the recipe's camera profile (`global.camera_profile[0]`, from
/// `register`) for the photo at `image_path`, after the base look.
pub fn apply(global: &mut GlobalAdjustments, image_path: &str) {
    let key = global.camera_profile[0];
    global.camera_profile = [0; 4];
    let custom_wb = (global.white_balance[0] > 0.5).then_some((
        global.white_balance[1] as f64,
        global.white_balance[2] as f64,
    ));
    global.white_balance = [0.0; 4];
    if global.tonemapper_mode != crate::baseline_exposure::CAMERA_RAW_MODE
        || global.is_raw_image == 0
        || (key == 0 && custom_wb.is_none())
    {
        return;
    }
    let balanced = custom_wb.and_then(|(t, tint)| balanced(image_path, t, tint));
    let profile_path = (key != 0)
        .then(|| {
            names()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&key)
                .cloned()
        })
        .flatten()
        .and_then(|name| resolve(&name, Path::new(image_path)));
    let Some(profile_path) = profile_path else {
        // Kelvin white balance without a profile: the camera's own matrices.
        if let Some(b) = balanced {
            let m: [[f32; 3]; 3] =
                std::array::from_fn(|r| std::array::from_fn(|c| b.matrix[r][c] as f32));
            global.camera_profile_matrix = crate::image_processing::gpu_mat3_from_rows(m);
            global.camera_profile[3] = 1;
        }
        return;
    };
    let image = match &balanced {
        Some(b) => b.image.clone(),
        None => match image_colour(image_path, None) {
            Some(image) => image,
            None => return,
        },
    };
    let cache_key = format!(
        "{}\u{1}{image_path}\u{1}{:?}",
        profile_path.display(),
        custom_wb.map(|(t, tint)| ((t * 10.0).round() as i64, (tint * 10.0).round() as i64))
    );
    let Some(prepared) = prepare(&profile_path, image_path, &image, &cache_key) else {
        return;
    };
    let (rendering, entries) = (&prepared.0, &prepared.1);
    global.camera_profile_matrix = crate::image_processing::gpu_mat3_from_rows(rendering.matrix);
    let hue_sat = rendering.hue_sat_map.as_ref();
    let look = rendering.look_table.as_ref();
    let dims = |t: Option<&HsvTable>| {
        t.map_or([0; 4], |t| {
            [
                t.hue_divisions as u32,
                t.sat_divisions as u32,
                t.val_divisions as u32,
                u32::from(t.srgb_value),
            ]
        })
    };
    global.camera_profile_hue_sat = dims(hue_sat);
    global.camera_profile_look = dims(look);
    // Tables key: profile, photo and balance (the HueSatMap blends by it).
    let table_key = register(&cache_key);
    tables_by_key()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(table_key, Arc::new(entries.clone()));
    global.camera_profile = [1, table_key, hue_sat.map_or(0, |t| t.data.len() as u32), 1];
    global.baseline_exposure += rendering.baseline_exposure_offset;
    match rendering
        .tone_curve
        .as_deref()
        .and_then(crate::baseline_exposure::sample_profile_curve)
    {
        Some(curve) => {
            for (i, value) in curve.iter().enumerate() {
                global.profile_curve[i / 4][i % 4] = *value;
            }
            global.profile_curve[15][3] = FLAG_OWN_CURVE;
        }
        None => global.profile_curve[15][3] = FLAG_DEFAULT_CURVE,
    }
    // The make's residual look stands in for Adobe's tables; a profile
    // brings its own.
    global.profile_curve[15][1] = 0.0;
}

// ---------------------------------------------------------------------
// Choosing profiles: copies live with the session (`profiles/`), so a
// moved session keeps them; photos outside sessions use the app's folder.

#[derive(serde::Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProfileEntry {
    /// What a recipe's `cameraProfile` holds.
    pub value: String,
    pub name: String,
    /// The camera the profile was made for.
    pub camera: String,
}

fn entry(value: String, bytes: &[u8], file_name: &str) -> Option<ProfileEntry> {
    let profile = parse(bytes).ok()?;
    let name = if profile.name.is_empty() {
        file_name
            .trim_end_matches(".dcp")
            .trim_end_matches(".DCP")
            .to_string()
    } else {
        profile.name
    };
    Some(ProfileEntry {
        value,
        name,
        camera: profile.camera,
    })
}

fn profile_folder(image_path: &Path, app_data: Option<&Path>) -> Option<(PathBuf, bool)> {
    match crate::session_layout::session_root(image_path) {
        Some(root) => Some((root.join("profiles"), true)),
        None => Some((app_data?.join("camera_profiles"), false)),
    }
}

/// Copies a DCP next to the photo's session (no-clobber: an identical copy
/// is reused, a different file of the same name gets a numbered name) and
/// returns what the recipe stores.
pub fn import(
    source: &Path,
    image_path: &Path,
    app_data: Option<&Path>,
) -> Result<ProfileEntry, String> {
    let bytes = std::fs::read(source).map_err(|e| format!("Cannot read the profile: {e}"))?;
    parse(&bytes)?;
    let (folder, in_session) =
        profile_folder(image_path, app_data).ok_or("No folder for camera profiles")?;
    std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
    let file_name = source
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("Invalid profile file name")?;
    let (stem, extension) = match file_name.rsplit_once('.') {
        Some((stem, extension)) => (stem.to_string(), extension.to_string()),
        None => (file_name.to_string(), "dcp".to_string()),
    };
    let mut target = folder.join(file_name);
    let mut counter = 2;
    loop {
        match std::fs::read(&target) {
            Ok(existing) if existing == bytes => break,
            Ok(_) => {
                target = folder.join(format!("{stem} {counter}.{extension}"));
                counter += 1;
            }
            Err(_) => {
                rawtonic_metadata::atomic_write(&target, &bytes).map_err(|e| e.to_string())?;
                break;
            }
        }
    }
    let written = target
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(file_name)
        .to_string();
    let value = if in_session {
        format!("profiles/{written}")
    } else {
        target.to_string_lossy().into_owned()
    };
    entry(value, &bytes, &written).ok_or_else(|| "Invalid camera profile".into())
}

/// Profiles available to the photo: those in its session's `profiles/` (or
/// the app's folder outside sessions).
pub fn list(image_path: &Path, app_data: Option<&Path>) -> Vec<ProfileEntry> {
    let Some((folder, in_session)) = profile_folder(image_path, app_data) else {
        return Vec::new();
    };
    let Ok(read) = std::fs::read_dir(&folder) else {
        return Vec::new();
    };
    let mut entries: Vec<ProfileEntry> = read
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("dcp"))
        })
        .filter_map(|p| {
            let file_name = p.file_name()?.to_str()?.to_string();
            let value = if in_session {
                format!("profiles/{file_name}")
            } else {
                p.to_string_lossy().into_owned()
            };
            entry(value, &std::fs::read(&p).ok()?, &file_name)
        })
        .collect();
    entries.sort_by_key(|entry| entry.name.to_lowercase());
    entries
}

#[derive(serde::Serialize, Debug, Clone, Copy, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WhiteBalanceInfo {
    pub temperature: f64,
    pub tint: f64,
}

/// Temperature and tint that make a sampled colour of the decoded image
/// (linear, at the camera's balance) neutral.
pub fn white_balance_for_sample(image_path: &str, rgb: [f32; 3]) -> Option<(f64, f64)> {
    let image = image_colour(image_path, None)?;
    let m: Matrix3 =
        std::array::from_fn(|r| std::array::from_fn(|c| image.camera_to_srgb[r][c] as f64));
    let balanced = crate::dng_color::apply(&invert(&m)?, rgb.map(f64::from));
    // Back to raw camera values, then the multipliers that balance them.
    let raw: [f64; 3] = std::array::from_fn(|i| balanced[i] / image.wb[i] as f64);
    if !raw.iter().all(|v| v.is_finite() && *v > 1e-9) {
        return None;
    }
    let wb = [
        (raw[1] / raw[0]) as f32,
        1.0,
        (raw[1] / raw[2]) as f32,
        f32::NAN,
    ];
    crate::color_calibration::white_balance_temperature(
        &image.calibrations,
        image.unit_calibration.as_ref(),
        wb,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(h: usize, s: usize, v: usize) -> HsvTable {
        HsvTable {
            hue_divisions: h,
            sat_divisions: s,
            val_divisions: v,
            srgb_value: false,
            data: vec![[0.0, 1.0, 1.0]; h * s * v],
        }
    }

    /// Prints "As Shot" temperature/tint for `$WB_FILES` (`;`-separated).
    #[test]
    #[ignore = "tool: needs WB_FILES"]
    fn print_as_shot_white_balance() {
        for item in std::env::var("WB_FILES").unwrap().split(';') {
            let (path, target) = item
                .split_once('@')
                .map_or((item, None), |(p, t)| (p, Some(t)));
            println!("{path}\t{:?}", as_shot_white_balance(path));
            let image = image_colour(path, None).unwrap();
            println!("  as-shot multipliers {:?}", image.wb);
            if let Some(t) = target {
                let (k, tint) = t.split_once(',').unwrap();
                let m = crate::color_calibration::white_balance_multipliers(
                    &image.calibrations,
                    image.unit_calibration.as_ref(),
                    k.parse().unwrap(),
                    tint.parse().unwrap(),
                    image.wb[1],
                )
                .unwrap();
                println!("  multipliers for {k} K / {tint}: {m:?}");
                // A surface that is neutral under that balance, as the
                // eyedropper samples it from the decode.
                let balanced: [f32; 3] = std::array::from_fn(|i| image.wb[i] / m[i]);
                let rgb: [f32; 3] = std::array::from_fn(|r| {
                    (0..3)
                        .map(|c| image.camera_to_srgb[r][c] * balanced[c])
                        .sum()
                });
                println!(
                    "  eyedropper on it: {:?}",
                    white_balance_for_sample(path, rgb)
                );
            }
        }
    }

    /// Kelvin and tint of given camera multipliers: `$WB_MULT` = "raw@r,b"
    /// entries separated by `;` (green 1).
    #[test]
    #[ignore = "tool: needs WB_MULT"]
    fn kelvin_of_multipliers() {
        for item in std::env::var("WB_MULT").unwrap().split(';') {
            let (raw, m) = item.split_once('@').unwrap();
            let (r, b) = m.split_once(',').unwrap();
            let image = image_colour(raw, None).unwrap();
            let wb = [r.parse().unwrap(), 1.0, b.parse().unwrap(), f32::NAN];
            println!(
                "{m}: {:?}",
                crate::color_calibration::white_balance_temperature(
                    &image.calibrations,
                    image.unit_calibration.as_ref(),
                    wb,
                )
            );
        }
    }

    /// Compares the As Shot Kelvin from the decoder's matrices with the one
    /// from a DCP's colour matrices: `$WB_DCP` = "raw;dcp" pairs separated by
    /// `|`.
    #[test]
    #[ignore = "tool: needs WB_DCP"]
    fn as_shot_kelvin_with_dcp_matrices() {
        use rawler::imgop::xyz::Illuminant;
        for pair in std::env::var("WB_DCP").unwrap().split('|') {
            let (raw, dcp) = pair.split_once(';').unwrap();
            let image = image_colour(raw, None).unwrap();
            let profile = parse(&std::fs::read(dcp).unwrap()).unwrap();
            let illuminant = |code: u16| match code {
                17 => Illuminant::A,
                21 => Illuminant::D65,
                23 => Illuminant::D50,
                20 => Illuminant::D55,
                _ => Illuminant::Unknown,
            };
            let mut matrices = std::collections::HashMap::new();
            for (i, code) in [profile.illuminants.0, profile.illuminants.1]
                .into_iter()
                .enumerate()
            {
                if let Some(m) = profile.color_matrix[i] {
                    matrices.insert(
                        illuminant(code),
                        m.iter().flatten().map(|v| *v as f32).collect(),
                    );
                }
            }
            println!("{raw}");
            println!(
                "  decoder matrices: {:?}",
                crate::color_calibration::white_balance_temperature(
                    &image.calibrations,
                    image.unit_calibration.as_ref(),
                    image.wb,
                )
            );
            println!(
                "  DCP matrices:     {:?}",
                crate::color_calibration::white_balance_temperature(&matrices, None, image.wb)
            );
            for (k, v) in &image.calibrations {
                println!("  decoder {k:?}: {:?}", &v[..3]);
            }
            for (k, v) in &matrices {
                println!("  dcp     {k:?}: {:?}", &v[..3]);
            }
        }
    }

    /// Traces one white-balanced camera colour through a DCP for a RAW:
    /// `$DCP_TRACE` = "dcp;raw;r,g,b".
    #[test]
    #[ignore = "tool: needs DCP_TRACE"]
    fn trace_colour_through_profile() {
        let spec = std::env::var("DCP_TRACE").unwrap();
        let parts: Vec<&str> = spec.split(';').collect();
        let profile = parse(&std::fs::read(parts[0]).unwrap()).unwrap();
        let image = crate::raw_processing::image_color(&std::fs::read(parts[1]).unwrap()).unwrap();
        let cam: Vec<f32> = parts[2].split(',').map(|v| v.parse().unwrap()).collect();
        let decoded: [f32; 3] =
            std::array::from_fn(|r| (0..3).map(|c| image.camera_to_srgb[r][c] * cam[c]).sum());
        let r = rendering(&profile, &image).unwrap();
        let srgb: [f32; 3] =
            std::array::from_fn(|i| (0..3).map(|c| r.matrix[i][c] * decoded[c]).sum());
        const S2P: [[f32; 3]; 3] = [
            [0.529_345_8, 0.330_072_8, 0.140_581_2],
            [0.098_374_4, 0.873_461_1, 0.028_164_7],
            [0.016_883_2, 0.117_672_5, 0.865_444_3],
        ];
        let pro: [f32; 3] = std::array::from_fn(|i| (0..3).map(|c| S2P[i][c] * srgb[c]).sum());
        println!(
            "wb {:?} decoded {decoded:?} profile sRGB {srgb:?} prophoto {pro:?} hsv {:?}",
            image.wb,
            rgb_to_hsv(pro[0], pro[1], pro[2])
        );
        for (name, t) in [("HueSatMap", &r.hue_sat_map), ("LookTable", &r.look_table)] {
            if let Some(t) = t {
                let (mut hmin, mut hmax, mut smin, mut smax, mut vmin, mut vmax) =
                    (f32::MAX, f32::MIN, f32::MAX, f32::MIN, f32::MAX, f32::MIN);
                for e in &t.data {
                    hmin = hmin.min(e[0]);
                    hmax = hmax.max(e[0]);
                    smin = smin.min(e[1]);
                    smax = smax.max(e[1]);
                    vmin = vmin.min(e[2]);
                    vmax = vmax.max(e[2]);
                }
                println!(
                    "{name} dims {}x{}x{} srgb {} hue [{hmin},{hmax}] sat [{smin},{smax}] val [{vmin},{vmax}]",
                    t.hue_divisions, t.sat_divisions, t.val_divisions, t.srgb_value
                );
            } else {
                println!("{name} absent");
            }
        }
        if let Some(t) = &r.hue_sat_map {
            let o = t.apply(pro);
            println!(
                "after HueSatMap {o:?} hsv {:?}",
                rgb_to_hsv(o[0], o[1], o[2])
            );
            if let Some(l) = &r.look_table {
                let o2 = l.apply(o);
                println!(
                    "after LookTable {o2:?} hsv {:?}",
                    rgb_to_hsv(o2[0], o2[1], o2[2])
                );
            }
        }
    }

    #[test]
    fn hsv_round_trip() {
        for rgb in [
            [0.2, 0.5, 0.9],
            [0.9, 0.1, 0.4],
            [0.3, 0.3, 0.3],
            [1.5, 0.2, 0.0],
        ] {
            let (h, s, v) = rgb_to_hsv(rgb[0], rgb[1], rgb[2]);
            let back = hsv_to_rgb(h, s, v);
            for k in 0..3 {
                assert!((back[k] - rgb[k]).abs() < 1e-5, "{rgb:?} -> {back:?}");
            }
        }
    }

    #[test]
    fn identity_tables_leave_pixels_unchanged() {
        let mut srgb = identity(36, 8, 16);
        srgb.srgb_value = true;
        for p in [[0.2, 0.5, 0.9], [0.9, 0.1, 0.4], [2.0, 1.5, 0.3], [0.0; 3]] {
            for table in [&identity(90, 30, 1), &srgb] {
                let out = table.apply(p);
                for k in 0..3 {
                    assert!((out[k] - p[k]).abs() < 1e-4, "{p:?} vs {out:?}");
                }
            }
        }
    }

    #[test]
    fn saturation_scale_and_hue_shift_act_on_prophoto_hsv() {
        let mut table = identity(6, 2, 1);
        table.data.fill([60.0, 0.5, 1.0]);
        // Pure ProPhoto red: hue 0 -> 1 sextant (yellow), saturation halved.
        let out = table.apply([1.0, 0.0, 0.0]);
        let (h, s, v) = rgb_to_hsv(out[0], out[1], out[2]);
        assert!((h - 1.0).abs() < 1e-4 && (s - 0.5).abs() < 1e-4 && (v - 1.0).abs() < 1e-4);
    }

    /// A camera whose white-balanced RGB is linear sRGB; its DCP forward
    /// matrix is sRGB → XYZ D50.
    fn srgb_camera() -> (crate::raw_processing::ImageColor, Profile) {
        let srgb_to_d50 = invert(&XYZ_D50_TO_SRGB).unwrap();
        let profile = Profile {
            illuminants: (21, 0),
            color_matrix: [Some(invert(&srgb_to_d50).unwrap()), None],
            forward_matrix: [Some(srgb_to_d50), None],
            ..Default::default()
        };
        let image = crate::raw_processing::ImageColor {
            camera_to_srgb: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            wb: [2.0, 1.0, 1.5, f32::NAN],
            calibrations: Default::default(),
            unit_calibration: None,
        };
        (image, profile)
    }

    #[test]
    fn matching_forward_matrix_renders_the_decode_unchanged() {
        let (image, profile) = srgb_camera();
        let r = rendering(&profile, &image).unwrap();
        for (i, row) in r.matrix.iter().enumerate() {
            for (j, v) in row.iter().enumerate() {
                let expected = if i == j { 1.0 } else { 0.0 };
                assert!((v - expected).abs() < 1e-3, "{:?}", r.matrix);
            }
        }
    }

    #[test]
    fn profile_matrix_keeps_white() {
        let (image, mut profile) = srgb_camera();
        // A profile that saturates: white must still map to white.
        let fm = profile.forward_matrix[0].unwrap();
        let boost: Matrix3 = [[1.2, -0.1, -0.1], [-0.1, 1.2, -0.1], [-0.1, -0.1, 1.2]];
        profile.forward_matrix[0] = Some(multiply(&fm, &boost));
        for forward in [true, false] {
            if !forward {
                profile.forward_matrix = [None, None];
            }
            let r = rendering(&profile, &image).unwrap();
            for row in r.matrix {
                assert!(
                    (row.iter().sum::<f32>() - 1.0).abs() < 1e-3,
                    "{:?}",
                    r.matrix
                );
            }
        }
    }

    #[test]
    fn hue_sat_maps_blend_by_the_white_balance() {
        let mut a = identity(6, 2, 1);
        a.data.iter_mut().for_each(|e| e[1] = 2.0);
        let blended = blend_tables(&Some(a), &Some(identity(6, 2, 1)), 0.25).unwrap();
        assert!((blended.data[0][1] - 1.25).abs() < 1e-6);
    }

    #[test]
    fn profile_names_resolve_inside_the_session_only() {
        let dir = std::env::temp_dir().join(format!("rt-profile-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Capture")).unwrap();
        std::fs::write(dir.join("session.json"), "{}").unwrap();
        std::fs::write(dir.join("session.sqlite"), "").unwrap();
        let image = dir.join("Capture/a.CR3");
        assert_eq!(
            resolve("profiles/x.dcp", &image),
            Some(dir.join("profiles/x.dcp"))
        );
        assert_eq!(resolve("../x.dcp", &image), None);
        assert_eq!(register(""), 0);
        let key = register("profiles/x.dcp");
        assert!(key != 0 && key == register("profiles/x.dcp"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn tiny_profile() -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(b"IIRC");
        b.extend_from_slice(&8u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        let matrix_at = 8 + 2 + 12 + 4;
        b.extend_from_slice(&TAG_COLOR_MATRIX1.to_le_bytes());
        b.extend_from_slice(&10u16.to_le_bytes());
        b.extend_from_slice(&9u32.to_le_bytes());
        b.extend_from_slice(&(matrix_at as u32).to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        for i in 0..9 {
            let v: i32 = if i % 4 == 0 { 1 } else { 0 };
            b.extend_from_slice(&v.to_le_bytes());
            b.extend_from_slice(&1i32.to_le_bytes());
        }
        b
    }

    #[test]
    fn imported_profiles_live_in_the_session_without_clobbering() {
        let dir = std::env::temp_dir().join(format!("rt-profile-import-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Capture")).unwrap();
        std::fs::write(dir.join("session.json"), "{}").unwrap();
        std::fs::write(dir.join("session.sqlite"), "").unwrap();
        let image = dir.join("Capture/a.CR3");
        let source = dir.join("My Camera.dcp");
        std::fs::write(&source, tiny_profile()).unwrap();
        let first = import(&source, &image, None).unwrap();
        assert_eq!(first.value, "profiles/My Camera.dcp");
        // The same file again is reused; a different one gets a new name.
        assert_eq!(import(&source, &image, None).unwrap().value, first.value);
        let mut other = tiny_profile();
        other.extend_from_slice(b"x");
        std::fs::write(&source, other).unwrap();
        assert_eq!(
            import(&source, &image, None).unwrap().value,
            "profiles/My Camera 2.dcp"
        );
        assert_eq!(list(&image, None).len(), 2);
        // Not a profile: refused, nothing written.
        std::fs::write(&source, b"nope").unwrap();
        assert!(import(&source, &image, None).is_err());
        assert_eq!(list(&image, None).len(), 2);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn parses_little_endian_profile() {
        let mut b = Vec::new();
        b.extend_from_slice(b"IIRC");
        b.extend_from_slice(&8u32.to_le_bytes());
        let entries = 4u16;
        b.extend_from_slice(&entries.to_le_bytes());
        let data_at = 8 + 2 + 12 * entries as u32 + 4;
        let matrix_at = data_at;
        let dims_at = matrix_at + 9 * 8;
        let map_at = dims_at + 12;
        let mut entry = |tag: u16, kind: u16, n: u32, value: u32| {
            b.extend_from_slice(&tag.to_le_bytes());
            b.extend_from_slice(&kind.to_le_bytes());
            b.extend_from_slice(&n.to_le_bytes());
            b.extend_from_slice(&value.to_le_bytes());
        };
        entry(TAG_COLOR_MATRIX1, 10, 9, matrix_at);
        entry(TAG_CALIBRATION_ILLUMINANT1, 3, 1, 17);
        entry(TAG_HUE_SAT_MAP_DIMS, 4, 3, dims_at);
        entry(TAG_HUE_SAT_MAP1, 11, 36, map_at);
        b.extend_from_slice(&0u32.to_le_bytes());
        for i in 0..9 {
            let v: i32 = if i % 4 == 0 { 10_000 } else { 0 };
            b.extend_from_slice(&v.to_le_bytes());
            b.extend_from_slice(&10_000i32.to_le_bytes());
        }
        for d in [6u32, 2, 1] {
            b.extend_from_slice(&d.to_le_bytes());
        }
        for _ in 0..12 {
            for x in [5.0f32, 1.0, 1.0] {
                b.extend_from_slice(&x.to_le_bytes());
            }
        }
        let profile = parse(&b).unwrap();
        assert_eq!(profile.illuminants.0, 17);
        assert_eq!(profile.color_matrix[0].unwrap()[1][1], 1.0);
        let map = profile.hue_sat_map[0].as_ref().unwrap();
        assert_eq!(
            (map.hue_divisions, map.sat_divisions, map.val_divisions),
            (6, 2, 1)
        );
        assert_eq!(map.data[11], [5.0, 1.0, 1.0]);
        assert!(profile.hue_sat_map[1].is_none() && profile.look_table.is_none());
        assert!(parse(b"not a profile").is_err());
    }

    /// Tool: Sony SR2 sub-IFD entries of `$SR2_FILES` (`;`-separated), for
    /// probing which maker-note values Adobe reads.
    #[test]
    #[ignore = "tool: needs SR2_FILES"]
    fn dump_sr2_entries() {
        for path in std::env::var("SR2_FILES").unwrap().split(';') {
            let bytes = std::fs::read(path).unwrap();
            let source = rawler::rawsource::RawSource::new_from_slice(&bytes);
            let Ok(sr2) = rawler::decoders::arw::ArwDecoder::sr2_subifd(&source) else {
                println!("{path}\tno sr2");
                continue;
            };
            let mut line = String::new();
            for (tag, entry) in sr2.entries() {
                let n = entry.value.count();
                if n == 0 || n > 16 {
                    continue;
                }
                let v: Vec<String> = (0..n)
                    .map(|i| format!("{}", entry.value.force_f32(i)))
                    .collect();
                line.push_str(&format!(" {tag:#06x}={}", v.join(",")));
            }
            println!("{path}\t{line}");
        }
    }
}
