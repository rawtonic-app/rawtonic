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

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(windows)]
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
    let monitor =
        ColorProfile::new_from_slice(icc).map_err(|e| ProfileError::Unreadable(e.to_string()))?;
    if monitor.color_space != DataColorSpace::Rgb {
        return Err(ProfileError::NotRgb);
    }
    let frames = match source {
        Encoding::Srgb => ColorProfile::new_srgb(),
        Encoding::DisplayP3 => ColorProfile::new_display_p3(),
    };
    let options = TransformOptions {
        rendering_intent: RenderingIntent::RelativeColorimetric,
        allow_use_cicp_transfer: false,
        prefer_fixed_point: false,
        ..TransformOptions::default()
    };
    let transform = frames
        .create_transform_f32(Layout::Rgb, &monitor, Layout::Rgb, options)
        .map_err(|e| ProfileError::NotConvertible(e.to_string()))?;

    let convert = |input: &[f32]| -> Result<Vec<[f32; 3]>, ProfileError> {
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
    };

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
