//! The monitor's colour profile and the preview's display transform
//! (notes: parity-gaps/G23, G62; benchmark-results/mcm1-2026-11-27).
//!
//! Lightroom and Capture One show the photograph through the profile the
//! operating system assigns to the monitor. Rawtonic does the same:
//!
//! - **macOS:** frames are tagged Display P3 and the window server
//!   (ColorSync) matches them, and the webview's thumbnails, to the
//!   display's profile. Nothing to do here.
//! - **Windows:** the desktop compositor shows a window's values as they
//!   are on a display without advanced colour, so the native present pass
//!   converts the frame (encoded sRGB) into the monitor's profile with the
//!   [`DisplayLut`] built here from the profile Windows assigns to the
//!   window's monitor ([`windows`]). The webview (Chromium) reads the same
//!   profile for the interface and the Library's thumbnails.
//! - **Linux:** the preview is shown by the webview, not by a native pass;
//!   [`linux`] only finds the profile (colord, then the X11 `_ICC_PROFILE`
//!   atom) to say which one the system holds.
//!
//! The transform is relative colorimetric (white maps to white) and clips
//! to the monitor's gamut. A profile that converts within
//! [`IDENTITY_TOLERANCE`] of the frame's own encoding (an sRGB monitor) is
//! no transform at all, so such a display shows the frame byte for byte as
//! before. Exports never pass through it.

use std::fmt;

use moxcms::{ColorProfile, DataColorSpace, Layout, RenderingIntent, TransformOptions};

pub mod kwin;
#[cfg(all(target_os = "linux", feature = "system-lookup"))]
pub mod linux;
#[cfg(all(windows, feature = "system-lookup"))]
pub mod windows;

/// Grid points per axis of the display transform's table. 65³ with the
/// grey response split off keeps a converted value within 0.4 of an 8-bit
/// code of the exact transform for wide-gamut matrix profiles
/// (`lut_matches_the_exact_transform`).
pub const LUT_SIZE: u32 = 65;

/// A profile whose transform moves no grid point by more than this (one
/// 8-bit code) is treated as the frame's own encoding.
pub const IDENTITY_TOLERANCE: f32 = 1.0 / 255.0;

/// The encoding of the frames the display shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Srgb,
    DisplayP3,
}

/// Why a profile cannot be used; the display then shows the frame as sRGB.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileError {
    /// Not an ICC profile, or a damaged one.
    Unreadable(String),
    /// Not an RGB profile (grey, CMYK, …).
    NotRgb,
    /// Read, but no transform into it could be built or it gives no usable
    /// colours (white not white, values not numbers).
    NotConvertible(String),
}

impl fmt::Display for ProfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreadable(why) => write!(f, "not a readable ICC profile ({why})"),
            Self::NotRgb => write!(f, "not an RGB display profile"),
            Self::NotConvertible(why) => write!(f, "no usable transform into it ({why})"),
        }
    }
}

impl std::error::Error for ProfileError {}

/// The display transform in two steps, as the present shader samples it:
///
/// 1. a 3D table over the frame's encoded values (entry `r + g·n + b·n²`
///    for the grid point (r, g, b)/(n − 1)), sampled trilinearly, gives for
///    each channel the grey level whose monitor value that channel takes;
/// 2. per channel, the monitor's grey response ([`CURVE_SIZE`] points,
///    linear between them) turns that level into the monitor's value.
///
/// Splitting off the grey response keeps the table smooth: a monitor curve
/// that rises steeply from black (a pure gamma 2.2) put 1.6 codes of
/// interpolation error into the darkest colours of a plain 65³ table, 0.4
/// with the split (`lut_matches_the_exact_transform`).
#[derive(Debug, Clone, PartialEq)]
pub struct DisplayLut {
    size: u32,
    levels: Vec<[f32; 3]>,
    curve: Vec<[f32; 3]>,
}

/// Points of each channel's grey response.
pub const CURVE_SIZE: u32 = 4096;

fn linear_at(curve: &[[f32; 3]], channel: usize, at: f32) -> f32 {
    let max = (curve.len() - 1) as f32;
    let at = at.clamp(0.0, 1.0) * max;
    let i = (at.floor() as usize).min(curve.len() - 2);
    let f = at - i as f32;
    curve[i][channel] * (1.0 - f) + curve[i + 1][channel] * f
}

/// The grey level whose value on `channel` is `value` (the curve rises
/// monotonically); the lowest such level where the curve is flat.
fn level_of(curve: &[[f32; 3]], channel: usize, value: f32) -> f32 {
    let max = (curve.len() - 1) as f32;
    if value <= curve[0][channel] {
        return 0.0;
    }
    if value >= curve[curve.len() - 1][channel] {
        return 1.0;
    }
    // The first point at or above `value`.
    let upper = curve.partition_point(|point| point[channel] < value);
    let (lo, hi) = (curve[upper - 1][channel], curve[upper][channel]);
    let f = if hi > lo {
        (value - lo) / (hi - lo)
    } else {
        0.0
    };
    (upper as f32 - 1.0 + f) / max
}

impl DisplayLut {
    /// Grid points per axis of the table.
    pub fn size(&self) -> u32 {
        self.size
    }

    /// The table of grey levels, red fastest.
    pub fn levels(&self) -> &[[f32; 3]] {
        &self.levels
    }

    /// The grey response per channel, [`CURVE_SIZE`] points from 0 to 1.
    pub fn curve(&self) -> &[[f32; 3]] {
        &self.curve
    }

    /// The monitor's encoded values for encoded `rgb` (clamped to 0…1).
    pub fn apply(&self, rgb: [f32; 3]) -> [f32; 3] {
        let n = self.size as usize;
        let max = (n - 1) as f32;
        let at = rgb.map(|v| v.clamp(0.0, 1.0) * max);
        let base = at.map(|v| (v.floor() as usize).min(n - 2));
        let frac = [0, 1, 2].map(|c| at[c] - base[c] as f32);
        let entry = |r: usize, g: usize, b: usize| self.levels[r + g * n + b * n * n];
        let mut level = [0.0f32; 3];
        for corner in 0..8 {
            let (dr, dg, db) = (corner & 1, (corner >> 1) & 1, (corner >> 2) & 1);
            let weight = [dr, dg, db]
                .iter()
                .zip(frac)
                .map(|(&d, f)| if d == 1 { f } else { 1.0 - f })
                .product::<f32>();
            let value = entry(base[0] + dr, base[1] + dg, base[2] + db);
            for c in 0..3 {
                level[c] += weight * value[c];
            }
        }
        [0, 1, 2].map(|c| linear_at(&self.curve, c, level[c]))
    }

    /// The conversion of 8-bit values, built once per transform
    /// ([`ByteConverter`]).
    pub fn byte_converter(&self) -> ByteConverter {
        let n = self.size as usize;
        let max = (n - 1) as f32;
        // Per input code: the grid cell below it (as an offset per axis) and
        // the weight of the one above.
        let cell = |stride: usize| -> Vec<(usize, f32)> {
            (0..256)
                .map(|code| {
                    let at = code as f32 / 255.0 * max;
                    let base = (at.floor() as usize).min(n - 2);
                    (base * stride, at - base as f32)
                })
                .collect()
        };
        // Per channel: the monitor's code for each of OUT_STEPS grey levels.
        let out = (0..=OUT_STEPS)
            .map(|i| {
                let level = i as f32 / OUT_STEPS as f32;
                [0, 1, 2].map(|c| (linear_at(&self.curve, c, level) * 255.0).round() as u8)
            })
            .collect();
        ByteConverter {
            n,
            cells: [cell(1), cell(n), cell(n * n)],
            levels: self.levels.clone(),
            out,
        }
    }
}

/// Grey levels per channel of [`ByteConverter`]'s output table.
const OUT_STEPS: usize = 16384;

/// [`DisplayLut`] for 8-bit RGBA pixels: each value as
/// [`DisplayLut::apply`] gives it, to the nearest code (±1). For frames and
/// thumbnails shown by a webview, where no present pass converts them.
#[derive(Debug, Clone)]
pub struct ByteConverter {
    n: usize,
    cells: [Vec<(usize, f32)>; 3],
    levels: Vec<[f32; 3]>,
    out: Vec<[u8; 3]>,
}

impl ByteConverter {
    /// Converts 8-bit RGBA pixels in place (alpha untouched).
    // Each channel's own level picks its code: no slice copy.
    #[allow(clippy::manual_memcpy)]
    pub fn convert_rgba8(&self, pixels: &mut [u8]) {
        let [rs, gs, bs] = &self.cells;
        let out = &self.out;
        let n = self.n;
        let levels = &self.levels;
        let (dg, db) = (n, n * n);
        for pixel in pixels.as_chunks_mut::<4>().0 {
            let (r, fr) = rs[pixel[0] as usize];
            let (g, fg) = gs[pixel[1] as usize];
            let (b, fb) = bs[pixel[2] as usize];
            let o = r + g + b;
            let (c000, c100, c010, c110) =
                (levels[o], levels[o + 1], levels[o + dg], levels[o + dg + 1]);
            let (c001, c101, c011, c111) = (
                levels[o + db],
                levels[o + db + 1],
                levels[o + db + dg],
                levels[o + db + dg + 1],
            );
            for c in 0..3 {
                let x00 = c000[c] + (c100[c] - c000[c]) * fr;
                let x10 = c010[c] + (c110[c] - c010[c]) * fr;
                let x01 = c001[c] + (c101[c] - c001[c]) * fr;
                let x11 = c011[c] + (c111[c] - c011[c]) * fr;
                let y0 = x00 + (x10 - x00) * fg;
                let y1 = x01 + (x11 - x01) * fg;
                let level = (y0 + (y1 - y0) * fb).clamp(0.0, 1.0);
                pixel[c] = out[(level * OUT_STEPS as f32 + 0.5) as usize][c];
            }
        }
    }
}

/// The profile's description (its name in the operating system's colour
/// settings), if it has one.
pub fn description(icc: &[u8]) -> Option<String> {
    let profile = ColorProfile::new_from_slice(icc).ok()?;
    let text = match profile.description? {
        moxcms::ProfileText::PlainString(text) => text,
        moxcms::ProfileText::Description(text) => {
            if text.unicode_string.trim().is_empty() {
                text.ascii_string
            } else {
                text.unicode_string
            }
        }
        moxcms::ProfileText::Localizable(texts) => {
            let english = texts.iter().find(|t| t.language.eq_ignore_ascii_case("en"));
            english.or(texts.first())?.value.clone()
        }
    };
    let text = text.trim_matches(|c: char| c == '\0' || c.is_whitespace());
    (!text.is_empty()).then(|| text.to_string())
}

/// The display transform from frames encoded in `source` into the monitor
/// profile `icc`; `None` when the monitor is the frame's own encoding
/// (within [`IDENTITY_TOLERANCE`]).
pub fn display_lut(source: Encoding, icc: &[u8]) -> Result<Option<DisplayLut>, ProfileError> {
    let monitor = rgb_profile(icc)?;
    let frames = match source {
        Encoding::Srgb => ColorProfile::new_srgb(),
        Encoding::DisplayP3 => ColorProfile::new_display_p3(),
    };
    let transform = frames
        .create_transform_f32(Layout::Rgb, &monitor, Layout::Rgb, transform_options())
        .map_err(|e| ProfileError::NotConvertible(e.to_string()))?;
    table(|input| run(transform.as_ref(), input))
}

/// The transform for images a colour-managing webview shows (Windows:
/// WebView2 converts every untagged image from sRGB into the display it
/// reads from the system's monitor profile `system`, None: no profile, i.e.
/// sRGB) when the monitor profile chosen in Preferences, `chosen`, is to be
/// shown instead (notes: benchmark-results/mcm3-2026-11-28/REPORT.md).
///
/// Chromium takes that display to be the profile's primaries with the sRGB
/// tone curve, not the profile's own curves
/// (`ui/display/win/color_profile_reader.cc`: `makeSRGBGamma`). So each
/// sRGB value goes to the monitor values `chosen` gives it (as Develop's
/// native pass shows them), then back through [`webview_display`] of
/// `system` to the sRGB value that this model of the webview turns into
/// those monitor values (to the table's ±1; measured on Windows by WMCM3,
/// not here). `None` when that is the image itself. Colours of `chosen` that
/// lie outside sRGB in the system's primaries (a wide-gamut system
/// profile) are clipped to sRGB here and cannot be shown as `chosen` gives
/// them.
pub fn managed_webview_lut(
    chosen: &[u8],
    system: Option<&[u8]>,
) -> Result<Option<DisplayLut>, ProfileError> {
    let srgb = ColorProfile::new_srgb();
    let into_chosen = srgb
        .create_transform_f32(
            Layout::Rgb,
            &rgb_profile(chosen)?,
            Layout::Rgb,
            transform_options(),
        )
        .map_err(|e| ProfileError::NotConvertible(e.to_string()))?;
    let display = match system {
        Some(icc) => webview_display(icc)?,
        None => None,
    };
    let back = match display {
        Some(display) => Some(
            display
                .create_transform_f32(Layout::Rgb, &srgb, Layout::Rgb, transform_options())
                .map_err(|e| ProfileError::NotConvertible(e.to_string()))?,
        ),
        None => None,
    };
    table(|input| {
        let monitor: Vec<f32> = run(into_chosen.as_ref(), input)?
            .into_iter()
            .flatten()
            .collect();
        match &back {
            Some(back) => run(back.as_ref(), &monitor),
            None => Ok(monitor.as_chunks::<3>().0.to_vec()),
        }
    })
}

/// The display Chromium on Windows converts images into for the monitor
/// profile `icc`: its primaries (the matrix's colorants) with the sRGB tone
/// curve; `None` for a profile without usable primaries (tables only),
/// for which Chromium takes the display to be sRGB
/// (`skia/ext/color_profile.cc`).
pub fn webview_display(icc: &[u8]) -> Result<Option<ColorProfile>, ProfileError> {
    let system = rgb_profile(icc)?;
    let colorants = [
        system.red_colorant,
        system.green_colorant,
        system.blue_colorant,
    ];
    // The colorants add up to the PCS white (D50) in a matrix profile.
    let white = colorants
        .iter()
        .fold([0.0f64; 3], |w, c| [w[0] + c.x, w[1] + c.y, w[2] + c.z]);
    let d50 = [0.9642, 1.0, 0.8249];
    if colorants.iter().any(|c| c.y <= 0.0)
        || white.iter().zip(d50).any(|(w, d)| (w - d).abs() > 0.02)
    {
        return Ok(None);
    }
    let mut display = ColorProfile::new_srgb();
    display.red_colorant = system.red_colorant;
    display.green_colorant = system.green_colorant;
    display.blue_colorant = system.blue_colorant;
    display.cicp = None;
    Ok(Some(display))
}

/// `icc` read as an RGB profile.
fn rgb_profile(icc: &[u8]) -> Result<ColorProfile, ProfileError> {
    let profile =
        ColorProfile::new_from_slice(icc).map_err(|e| ProfileError::Unreadable(e.to_string()))?;
    if profile.color_space != DataColorSpace::Rgb {
        return Err(ProfileError::NotRgb);
    }
    Ok(profile)
}

/// Relative colorimetric, in floating point, without CICP shortcuts.
fn transform_options() -> TransformOptions {
    TransformOptions {
        rendering_intent: RenderingIntent::RelativeColorimetric,
        allow_use_cicp_transfer: false,
        prefer_fixed_point: false,
        ..TransformOptions::default()
    }
}

/// RGB triples through `transform`, clamped to 0…1.
fn run(
    transform: &moxcms::TransformF32Executor,
    input: &[f32],
) -> Result<Vec<[f32; 3]>, ProfileError> {
    let mut out = vec![0.0f32; input.len()];
    transform
        .transform(input, &mut out)
        .map_err(|e| ProfileError::NotConvertible(e.to_string()))?;
    if out.iter().any(|v| !v.is_finite()) {
        return Err(ProfileError::NotConvertible(
            "values are not numbers".into(),
        ));
    }
    Ok(out
        .as_chunks::<3>()
        .0
        .iter()
        .map(|c| c.map(|v| v.clamp(0.0, 1.0)))
        .collect())
}

/// The table of the transform `convert` (RGB triples in, RGB triples out);
/// `None` when it moves no grid point by more than [`IDENTITY_TOLERANCE`].
fn table(
    convert: impl Fn(&[f32]) -> Result<Vec<[f32; 3]>, ProfileError>,
) -> Result<Option<DisplayLut>, ProfileError> {
    let n = LUT_SIZE as usize;
    let step = 1.0 / (n - 1) as f32;
    let mut grid = Vec::with_capacity(n * n * n * 3);
    for b in 0..n {
        for g in 0..n {
            for r in 0..n {
                grid.extend([r as f32 * step, g as f32 * step, b as f32 * step]);
            }
        }
    }
    let converted = convert(&grid)?;
    // Relative colorimetric: the frame's white is the monitor's white.
    let white = converted[converted.len() - 1];
    if white.iter().any(|v| (v - 1.0).abs() > 0.02) {
        return Err(ProfileError::NotConvertible(format!(
            "white becomes {white:?}"
        )));
    }
    let worst = converted
        .iter()
        .zip(grid.as_chunks::<3>().0)
        .flat_map(|(out, input)| (0..3).map(move |c| (out[c] - input[c]).abs()))
        .fold(0.0f32, f32::max);
    if worst <= IDENTITY_TOLERANCE {
        return Ok(None);
    }

    let m = CURVE_SIZE as usize;
    let greys: Vec<f32> = (0..m)
        .flat_map(|i| [i as f32 / (m - 1) as f32; 3])
        .collect();
    let mut curve = convert(&greys)?;
    // Rising (a damaged profile may wobble): each point at least the last.
    for i in 1..m {
        let previous = curve[i - 1];
        for (value, floor) in curve[i].iter_mut().zip(previous) {
            *value = value.max(floor);
        }
    }
    if (0..3).any(|c| curve[m - 1][c] - curve[0][c] < 0.5) {
        return Err(ProfileError::NotConvertible(
            "its grey scale does not rise from black to white".into(),
        ));
    }
    let levels = converted
        .iter()
        .map(|out| [0, 1, 2].map(|c| level_of(&curve, c, out[c])))
        .collect();
    Ok(Some(DisplayLut {
        size: LUT_SIZE,
        levels,
        curve,
    }))
}

/// What the operating system says about a display's colours.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemProfile {
    /// The system matches every window to the display itself (macOS;
    /// Windows with HDR or Auto colour management on): the preview stays in
    /// its own encoding.
    ManagedBySystem,
    /// The ICC profile assigned to the display, and where it was found.
    Assigned { icc: Vec<u8>, location: String },
    /// No profile assigned: the display is taken as sRGB.
    NoneAssigned,
    /// The profile could not be looked up (why).
    Unavailable(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Linear sRGB → linear `space` (both D65) and the encodings, from the
    /// published primaries, independent of moxcms.
    fn srgb_decode(v: f64) -> f64 {
        if v <= 0.040_45 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    }

    fn srgb_encode(v: f64) -> f64 {
        if v <= 0.003_130_8 {
            v * 12.92
        } else {
            1.055 * v.powf(1.0 / 2.4) - 0.055
        }
    }

    fn rgb_to_xyz(primaries: [[f64; 2]; 3], white: [f64; 2]) -> [[f64; 3]; 3] {
        let xyz = |[x, y]: [f64; 2]| [x / y, 1.0, (1.0 - x - y) / y];
        let [r, g, b] = primaries.map(xyz);
        let m = [[r[0], g[0], b[0]], [r[1], g[1], b[1]], [r[2], g[2], b[2]]];
        let s = mul_vec(&invert(&m), xyz(white));
        std::array::from_fn(|row| std::array::from_fn(|col| m[row][col] * s[col]))
    }

    fn invert(m: &[[f64; 3]; 3]) -> [[f64; 3]; 3] {
        let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
            - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
        let c = |r1: usize, c1: usize, r2: usize, c2: usize| {
            m[r1][c1] * m[r2][c2] - m[r1][c2] * m[r2][c1]
        };
        [
            [
                c(1, 1, 2, 2) / det,
                -c(0, 1, 2, 2) / det,
                c(0, 1, 1, 2) / det,
            ],
            [
                -c(1, 0, 2, 2) / det,
                c(0, 0, 2, 2) / det,
                -c(0, 0, 1, 2) / det,
            ],
            [
                c(1, 0, 2, 1) / det,
                -c(0, 0, 2, 1) / det,
                c(0, 0, 1, 1) / det,
            ],
        ]
    }

    fn mul_vec(m: &[[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
        std::array::from_fn(|r| m[r][0] * v[0] + m[r][1] * v[1] + m[r][2] * v[2])
    }

    fn mul(a: &[[f64; 3]; 3], b: &[[f64; 3]; 3]) -> [[f64; 3]; 3] {
        std::array::from_fn(|r| std::array::from_fn(|c| (0..3).map(|k| a[r][k] * b[k][c]).sum()))
    }

    const D65: [f64; 2] = [0.3127, 0.3290];
    const SRGB: [[f64; 2]; 3] = [[0.64, 0.33], [0.30, 0.60], [0.15, 0.06]];
    const P3: [[f64; 2]; 3] = [[0.680, 0.320], [0.265, 0.690], [0.150, 0.060]];
    const ADOBE: [[f64; 2]; 3] = [[0.64, 0.33], [0.21, 0.71], [0.15, 0.06]];

    /// sRGB 8-bit patch → the monitor's 8-bit code (exact maths, clipped).
    fn expected(patch: [u8; 3], primaries: [[f64; 2]; 3], encode: impl Fn(f64) -> f64) -> [u8; 3] {
        let to = mul(&invert(&rgb_to_xyz(primaries, D65)), &rgb_to_xyz(SRGB, D65));
        let linear = patch.map(|v| srgb_decode(v as f64 / 255.0));
        mul_vec(&to, linear).map(|v| (encode(v.clamp(0.0, 1.0)) * 255.0).round() as u8)
    }

    fn code(v: f32) -> u8 {
        (v * 255.0).round() as u8
    }

    const PATCHES: [[u8; 3]; 10] = [
        [255, 0, 0],
        [0, 255, 0],
        [0, 0, 255],
        [200, 100, 50],
        [50, 120, 200],
        [128, 128, 128],
        [255, 255, 255],
        [0, 0, 0],
        [240, 200, 170],
        [20, 60, 30],
    ];

    fn assert_patches(lut: &DisplayLut, primaries: [[f64; 2]; 3], encode: impl Fn(f64) -> f64) {
        for patch in PATCHES {
            let got = lut.apply(patch.map(|v| v as f32 / 255.0)).map(code);
            let want = expected(patch, primaries, &encode);
            for c in 0..3 {
                assert!(
                    (got[c] as i16 - want[c] as i16).abs() <= 1,
                    "{patch:?}: {got:?} vs {want:?}"
                );
            }
        }
    }

    #[test]
    fn eight_bit_pixels_get_the_tables_values() {
        // The webview's frames and thumbnails (Linux): every code as
        // `apply` gives it, alpha kept; the known patches within ±1.
        let icc = ColorProfile::new_adobe_rgb().encode().unwrap();
        let lut = display_lut(Encoding::Srgb, &icc).unwrap().unwrap();
        let mut pixels = Vec::new();
        for r in (0..=255u16).step_by(15) {
            for g in (0..=255u16).step_by(17) {
                for b in (0..=255u16).step_by(5) {
                    pixels.extend([r as u8, g as u8, b as u8, (r ^ b) as u8]);
                }
            }
        }
        for patch in PATCHES {
            pixels.extend([patch[0], patch[1], patch[2], 255]);
        }
        let mut converted = pixels.clone();
        lut.byte_converter().convert_rgba8(&mut converted);
        for (before, after) in pixels
            .as_chunks::<4>()
            .0
            .iter()
            .zip(converted.as_chunks::<4>().0)
        {
            let want = lut
                .apply([0, 1, 2].map(|c| before[c] as f32 / 255.0))
                .map(code);
            for c in 0..3 {
                assert!(
                    (after[c] as i16 - want[c] as i16).abs() <= 1,
                    "{before:?}: {after:?} vs {want:?}"
                );
            }
            assert_eq!(after[3], before[3]);
        }
        let gamma = |v: f64| v.powf(256.0 / 563.0);
        let start = converted.len() - PATCHES.len() * 4;
        for (patch, got) in PATCHES.iter().zip(converted[start..].as_chunks::<4>().0) {
            let want = expected(*patch, ADOBE, gamma);
            for c in 0..3 {
                assert!(
                    (got[c] as i16 - want[c] as i16).abs() <= 1,
                    "{patch:?}: {got:?} vs {want:?}"
                );
            }
        }
    }

    #[test]
    fn a_display_p3_monitor_gets_the_known_p3_values() {
        let icc = ColorProfile::new_display_p3().encode().unwrap();
        let lut = display_lut(Encoding::Srgb, &icc)
            .unwrap()
            .expect("a transform");
        assert_patches(&lut, P3, srgb_encode);
        // The published value of sRGB red in Display P3.
        assert_eq!(lut.apply([1.0, 0.0, 0.0]).map(code), [234, 51, 35]);
    }

    #[test]
    fn an_adobe_rgb_monitor_gets_gamma_2_2_values() {
        let icc = ColorProfile::new_adobe_rgb().encode().unwrap();
        let lut = display_lut(Encoding::Srgb, &icc)
            .unwrap()
            .expect("a transform");
        assert_patches(&lut, ADOBE, |v| v.powf(256.0 / 563.0));
    }

    #[test]
    fn an_srgb_monitor_is_no_transform() {
        let icc = ColorProfile::new_srgb().encode().unwrap();
        assert_eq!(display_lut(Encoding::Srgb, &icc), Ok(None));
        let p3 = ColorProfile::new_display_p3().encode().unwrap();
        assert_eq!(display_lut(Encoding::DisplayP3, &p3), Ok(None));
    }

    #[test]
    fn lut_matches_the_exact_transform() {
        // Between grid points, against moxcms converting each value itself.
        for icc in [
            ColorProfile::new_display_p3().encode().unwrap(),
            ColorProfile::new_adobe_rgb().encode().unwrap(),
        ] {
            let lut = display_lut(Encoding::Srgb, &icc).unwrap().unwrap();
            let monitor = ColorProfile::new_from_slice(&icc).unwrap();
            let exact = ColorProfile::new_srgb()
                .create_transform_f32(
                    Layout::Rgb,
                    &monitor,
                    Layout::Rgb,
                    TransformOptions {
                        rendering_intent: RenderingIntent::RelativeColorimetric,
                        allow_use_cicp_transfer: false,
                        prefer_fixed_point: false,
                        ..TransformOptions::default()
                    },
                )
                .unwrap();
            let mut input = Vec::new();
            for r in (0..=255u32).step_by(7) {
                for g in (3..=255u32).step_by(11) {
                    for b in (5..=255u32).step_by(13) {
                        input.extend([r, g, b].map(|v| v as f32 / 255.0));
                    }
                }
            }
            let mut want = vec![0.0; input.len()];
            exact.transform(&input, &mut want).unwrap();
            let worst = input
                .as_chunks::<3>()
                .0
                .iter()
                .zip(want.as_chunks::<3>().0)
                .flat_map(|(i, w)| {
                    let got = lut.apply(*i);
                    (0..3).map(move |c| (got[c] - w[c].clamp(0.0, 1.0)).abs() * 255.0)
                })
                .fold(0.0f32, f32::max);
            // Measured 2026-11-27: 0.15 (Display P3), 0.39 (Adobe RGB).
            eprintln!("worst {worst:.3} codes");
            assert!(worst < 0.5, "worst {worst} codes");
        }
    }

    /// A synthetic panel profile: sRGB primaries, a BT.709-type curve
    /// (gamma 1/0.45 with a linear toe; a pure power curve would turn the
    /// colorants' fixed-point crosstalk into codes near black in the
    /// webview model below).
    fn gamma_panel() -> Vec<u8> {
        let mut panel = ColorProfile::new_srgb();
        let curve = moxcms::ToneReprCurve::Parametric(vec![
            1.0 / 0.45,
            1.0 / 1.099,
            0.099 / 1.099,
            1.0 / 4.5,
            0.081,
        ]);
        panel.red_trc = Some(curve.clone());
        panel.green_trc = Some(curve.clone());
        panel.blue_trc = Some(curve);
        panel.cicp = None;
        panel.encode().unwrap()
    }

    /// What WebView2 shows for the served 8-bit `value` on a monitor with
    /// the profile `system`: sRGB converted into the profile's primaries
    /// with the sRGB curve (Chromium's `makeSRGBGamma`), to the nearest code.
    fn webview_shows(system: &[u8], value: [u8; 3]) -> [u8; 3] {
        let profile = ColorProfile::new_from_slice(system).unwrap();
        let mut monitor = ColorProfile::new_srgb();
        monitor.red_colorant = profile.red_colorant;
        monitor.green_colorant = profile.green_colorant;
        monitor.blue_colorant = profile.blue_colorant;
        monitor.cicp = None;
        let transform = ColorProfile::new_srgb()
            .create_transform_f32(Layout::Rgb, &monitor, Layout::Rgb, transform_options())
            .unwrap();
        let out = run(transform.as_ref(), &value.map(|v| v as f32 / 255.0)).unwrap();
        out[0].map(code)
    }

    /// The table's 8-bit value for sRGB `patch`, as `ByteConverter` serves it.
    fn served(lut: &DisplayLut, patch: [u8; 3]) -> [u8; 3] {
        let mut pixel = [patch[0], patch[1], patch[2], 255];
        lut.byte_converter().convert_rgba8(&mut pixel);
        [pixel[0], pixel[1], pixel[2]]
    }

    #[test]
    fn webview_thumbnails_are_the_image_itself_without_another_profile() {
        // No override is no table at all (the app never asks); an override
        // that converts as the webview does with the system's profile (its
        // own primaries with the sRGB curve) is none either.
        let srgb = ColorProfile::new_srgb().encode().unwrap();
        assert_eq!(managed_webview_lut(&srgb, None), Ok(None));
        assert_eq!(managed_webview_lut(&srgb, Some(&srgb)), Ok(None));
        let p3 = ColorProfile::new_display_p3().encode().unwrap();
        assert_eq!(managed_webview_lut(&p3, Some(&p3)), Ok(None));
        // A panel profile with sRGB primaries: the webview treats it as sRGB.
        assert_eq!(managed_webview_lut(&srgb, Some(&gamma_panel())), Ok(None));
    }

    #[test]
    fn webview_thumbnails_get_the_profiles_own_curve() {
        // The system's own profile chosen, with a curve other than sRGB's
        // (Adobe RGB, gamma 2.2): Develop shows its full transform, the
        // webview its primaries with the sRGB curve, so the table makes up
        // the difference, greys included.
        let adobe = ColorProfile::new_adobe_rgb().encode().unwrap();
        let gamma = |v: f64| v.powf(256.0 / 563.0);
        let lut = managed_webview_lut(&adobe, Some(&adobe))
            .unwrap()
            .expect("a table");
        for patch in PATCHES {
            let want = expected(patch, ADOBE, gamma);
            let shown = webview_shows(&adobe, served(&lut, patch));
            for c in 0..3 {
                assert!(
                    (shown[c] as i16 - want[c] as i16).abs() <= 1,
                    "{patch:?}: {shown:?} vs {want:?}"
                );
            }
        }
        // Untreated, the webview's grey would keep sRGB's code.
        assert_eq!(webview_shows(&adobe, [128, 128, 128]), [128, 128, 128]);
        assert_eq!(expected([128, 128, 128], ADOBE, gamma), [127, 127, 127]);
    }

    #[test]
    fn webview_thumbnails_show_the_chosen_profiles_values() {
        // Override Adobe RGB (wide), system a panel profile: after the
        // webview's sRGB → panel conversion each patch shows the Adobe RGB
        // value of the exact maths (±1).
        let adobe = ColorProfile::new_adobe_rgb().encode().unwrap();
        let gamma = |v: f64| v.powf(256.0 / 563.0);
        let panel = gamma_panel();
        let lut = managed_webview_lut(&adobe, Some(&panel))
            .unwrap()
            .expect("a table");
        // No profile assigned: the webview shows sRGB values as they are.
        let unprofiled = managed_webview_lut(&adobe, None).unwrap().expect("a table");
        for patch in PATCHES {
            let want = expected(patch, ADOBE, gamma);
            for (shown, how) in [
                (webview_shows(&panel, served(&lut, patch)), "panel"),
                (served(&unprofiled, patch), "no profile"),
            ] {
                for c in 0..3 {
                    assert!(
                        (shown[c] as i16 - want[c] as i16).abs() <= 1,
                        "{how} {patch:?}: {shown:?} vs {want:?}"
                    );
                }
            }
        }
        // Red really moved.
        assert!(served(&lut, [255, 0, 0])[0] < 240);
    }

    #[test]
    fn webview_thumbnails_on_a_wide_system_profile_clip_to_srgb() {
        // System Display P3, override Adobe RGB: colours inside sRGB in
        // P3's terms are shown as Adobe RGB gives them; saturated ones
        // that P3 holds outside sRGB come out as far as sRGB reaches.
        let adobe = ColorProfile::new_adobe_rgb().encode().unwrap();
        let p3 = ColorProfile::new_display_p3().encode().unwrap();
        let gamma = |v: f64| v.powf(256.0 / 563.0);
        let lut = managed_webview_lut(&adobe, Some(&p3)).unwrap().unwrap();
        for patch in [
            [128, 128, 128],
            [255, 255, 255],
            [0, 0, 0],
            [200, 100, 50],
            [240, 200, 170],
        ] {
            let want = expected(patch, ADOBE, gamma);
            let shown = webview_shows(&p3, served(&lut, patch));
            for c in 0..3 {
                assert!(
                    (shown[c] as i16 - want[c] as i16).abs() <= 1,
                    "{patch:?}: {shown:?} vs {want:?}"
                );
            }
        }
        // Adobe RGB's red (219, 0, 0) lies outside sRGB in P3's terms: the
        // webview shows the clipped colour instead (the limit noted at
        // `managed_webview_lut`).
        let red = webview_shows(&p3, served(&lut, [255, 0, 0]));
        assert!(red[1] > 30 && red[2] > 20, "{red:?}");
    }

    #[test]
    fn webview_thumbnails_refuse_unusable_profiles() {
        let adobe = ColorProfile::new_adobe_rgb().encode().unwrap();
        let gray = ColorProfile::new_gray_with_gamma(2.2).encode().unwrap();
        assert!(managed_webview_lut(b"junk", None).is_err());
        assert!(managed_webview_lut(&adobe, Some(b"junk")).is_err());
        assert_eq!(
            managed_webview_lut(&adobe, Some(&gray)),
            Err(ProfileError::NotRgb)
        );
        // A system profile without a matrix: the webview takes the display
        // to be sRGB, so the table is the one for no profile.
        let mut tables = ColorProfile::new_srgb();
        tables.red_colorant = Default::default();
        tables.green_colorant = Default::default();
        tables.blue_colorant = Default::default();
        tables.cicp = None;
        assert_eq!(
            managed_webview_lut(&adobe, Some(&tables.encode().unwrap())),
            managed_webview_lut(&adobe, None)
        );
    }

    #[test]
    fn unusable_profiles_are_errors_not_panics() {
        assert!(matches!(
            display_lut(Encoding::Srgb, b"not an icc profile"),
            Err(ProfileError::Unreadable(_))
        ));
        let icc = ColorProfile::new_display_p3().encode().unwrap();
        assert!(display_lut(Encoding::Srgb, &icc[..icc.len() / 2]).is_err());
        assert!(display_lut(Encoding::Srgb, &[]).is_err());
        let gray = ColorProfile::new_gray_with_gamma(2.2).encode().unwrap();
        assert_eq!(
            display_lut(Encoding::Srgb, &gray),
            Err(ProfileError::NotRgb)
        );
    }

    #[test]
    fn profiles_say_their_name() {
        let mut profile = ColorProfile::new_display_p3();
        profile.description = Some(moxcms::ProfileText::Localizable(vec![
            moxcms::LocalizableString::new("en".into(), "US".into(), "Studio EIZO".into()),
        ]));
        assert_eq!(
            description(&profile.encode().unwrap()).as_deref(),
            Some("Studio EIZO")
        );
        assert_eq!(description(b"nope"), None);
    }
}
