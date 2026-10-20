//! Lens corrections the camera stores in its RAW files (Fujifilm RAF, Sony
//! ARW, Panasonic RW2, Olympus / OM System ORF, Nikon Z NEF, newer Canon CR3), fitted to the profile model
//! the geometry warp already applies. Camera Raw and Capture One apply these
//! built-in corrections to every photo of such a body.
//!
//! Interpretation, calibrated against the cameras' own corrected JPEGs
//! (notes: BENCHMARKS.md): radii are normalized by half the image diagonal; a
//! corrected pixel at radius r samples the uncorrected image at
//! r · k(r) / k(1), so the corners stay corners. Fujifilm: nine knots, k =
//! 1 + value / 100; Sony: n knots at (i + ½) / n, k = 1 + value / 16384.
//! Fujifilm vignetting: brightness in percent of the centre at the same knots
//! (not yet checked against a flat field). Lateral chromatic aberration:
//! Fujifilm, Sony and (since `cameraCaVersion` 1) Panasonic.
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use rawler::formats::tiff::IFD;
use rawler::rawsource::RawSource;

use crate::lens_correction::LensDistortionParams;

/// Correction curves read from a RAW file.
#[derive(Debug, Clone, PartialEq)]
pub struct EmbeddedLensData {
    /// Who wrote the data ("Fujifilm", "Sony").
    pub maker: &'static str,
    /// (normalized radius, k) pairs, radius ascending.
    pub distortion: Vec<(f64, f64)>,
    /// (normalized radius, relative brightness) pairs, radius ascending.
    pub vignetting: Option<Vec<(f64, f64)>>,
    /// Minimum zoom that reproduces the camera's framing (1 = corners of the
    /// corrected frame sample the sensor corners).
    pub zoom: f64,
    /// Lateral chromatic aberration: (normalized radius, red scale − 1, blue
    /// scale − 1) relative to green, radius ascending.
    pub chromatic: Option<Vec<(f64, f64, f64)>>,
}

const FUJI_DISTORTION: u16 = 0xf00b;
const FUJI_CHROMATIC: u16 = 0xf00f;
const FUJI_VIGNETTING: u16 = 0xf010;
const FUJI_IFD: u16 = 0xf000;
const SONY_DISTORTION: u16 = 0x7037;
const SONY_VIGNETTING: u16 = 0x7032;
const SR2_VIGNETTING: u16 = 0x797d;
const SR2_DISTORTION: u16 = 0x7982;
const SR2_CHROMATIC: u16 = 0x7980;
const NIKON_NEF_INFO: u16 = 0xc7d5;
const SUB_IFDS: u16 = 0x014a;
const EXIF_IFD: u16 = 0x8769;

fn values(ifd: &IFD, tag: u16) -> Option<Vec<f64>> {
    let value = &ifd.get_entry_recursive(tag)?.value;
    (0..value.count())
        .map(|i| value.get_f32(i).ok().flatten().map(f64::from))
        .collect()
}

/// Fujifilm's curves: [scale, n knots, n values, …]. Current X-series bodies
/// store nine knots (19 values), the GFX 50R eight (17), older X-Trans bodies
/// eleven from the centre (23); vignetting uses the same knots, and so does
/// the lateral CA (ChromaticAberrationParams, 0xf00f): [scale, n knots,
/// n red, n blue], each the channel's scale relative to green minus one
/// (X-T3 with XF 35mm F2: red −0.00125, blue +0.00155 — what
/// `lateral_ca::estimate` measures on the same photo).
pub fn fuji_curves(distortion: &[f64], vignetting: Option<&[f64]>) -> Option<EmbeddedLensData> {
    fuji_curves_with_ca(distortion, vignetting, None)
}

pub fn fuji_curves_with_ca(
    distortion: &[f64],
    vignetting: Option<&[f64]>,
    chromatic: Option<&[f64]>,
) -> Option<EmbeddedLensData> {
    let n = if distortion.len() % 2 == 1 {
        (distortion.len() - 1) / 2
    } else if distortion.len() >= 19 {
        9
    } else {
        return None;
    };
    if n < 2 {
        return None;
    }
    let knots = &distortion[1..=n];
    let distortion_curve = curve_values(knots, &distortion[n + 1..=2 * n], |v| 1.0 + v / 100.0);
    let vignetting_curve = vignetting
        .filter(|v| v.len() > 2 * n && v[1..=n] == *knots)
        .map(|v| {
            // Older bodies (eleven knots) start at the centre themselves.
            let mut curve = if knots[0] > 0.0 {
                vec![(0.0, 1.0)]
            } else {
                Vec::new()
            };
            curve.extend(curve_values(knots, &v[n + 1..=2 * n], |p| p / 100.0));
            curve
        });
    plausible(&distortion_curve).then_some(EmbeddedLensData {
        maker: "Fujifilm",
        distortion: distortion_curve,
        vignetting: vignetting_curve.filter(|c| c.iter().all(|&(_, b)| b > 0.05 && b <= 1.5)),
        zoom: 1.0,
        chromatic: chromatic
            .filter(|v| v.len() > 3 * n && v[1..=n] == *knots)
            .map(|v| {
                (0..n)
                    .map(|i| (knots[i], v[n + 1 + i], v[2 * n + 1 + i]))
                    .collect::<Vec<_>>()
            })
            .filter(|c| {
                c.iter().all(|&(_, r, b)| {
                    r.abs() < 0.01 && b.abs() < 0.01 && r.is_finite() && b.is_finite()
                })
            }),
    })
}

fn curve_values(knots: &[f64], values: &[f64], f: impl Fn(f64) -> f64) -> Vec<(f64, f64)> {
    knots.iter().zip(values).map(|(&r, &v)| (r, f(v))).collect()
}

/// Sony's curves: [count, values…], knots at (i + ½) / count. Distortion
/// k = 1 + value / 2¹⁴; vignetting (VignettingCorrParams, 0x7032) as the
/// correction gain 1 + value / 2¹⁴, i.e. brightness 1 / gain relative to
/// the centre: fitted to Camera Raw on 44 ARW files whose camera had shading
/// compensation on (residual 0.05 EV median; darktable's 2^(½ − 2^(v/2¹³ −
/// 1)) left 0.06 EV and a radius-dependent bias).
pub fn sony_curve(distortion: &[f64], vignetting: Option<&[f64]>) -> Option<EmbeddedLensData> {
    sony_curves_with_ca(distortion, vignetting, None)
}

/// Sony's curves plus the lateral CA (ChromaticAberrationCorrParams, SR2
/// 0x7980): [2n, n red, n blue] at the knots (i + ½) / n, each the channel's
/// scale relative to green as value / 2²¹ (darktable's scale; ILCE-9 with
/// FE 55: red −0.0004…−0.0007, blue +0.0002…+0.0006 against −0.0006…−0.0008
/// and +0.0005…+0.0007 measured by `lateral_ca::estimate`). Full-frame
/// bodies store 16 knots, APS-C and RX bodies 11 (zero padded).
pub fn sony_curves_with_ca(
    distortion: &[f64],
    vignetting: Option<&[f64]>,
    chromatic: Option<&[f64]>,
) -> Option<EmbeddedLensData> {
    let knots = |values: &[f64]| -> Option<Vec<(f64, f64)>> {
        let count = *values.first()? as usize;
        if !(2..=64).contains(&count) || values.len() < count + 1 {
            return None;
        }
        Some(
            values[1..=count]
                .iter()
                .enumerate()
                .map(|(i, &v)| ((i as f64 + 0.5) / count as f64, v))
                .collect(),
        )
    };
    let curve: Vec<(f64, f64)> = knots(distortion)?
        .into_iter()
        .map(|(r, v)| (r, 1.0 + v / 16384.0))
        .collect();
    let vignetting = vignetting.and_then(knots).map(|values| {
        values
            .into_iter()
            .map(|(r, v)| (r, 1.0 / (1.0 + v / 16384.0)))
            .collect::<Vec<_>>()
    });
    plausible(&curve).then_some(EmbeddedLensData {
        maker: "Sony",
        distortion: curve,
        vignetting: vignetting.filter(|c| c.iter().all(|&(_, b)| b > 0.05 && b <= 1.5)),
        zoom: 1.0,
        chromatic: chromatic.and_then(sony_chromatic),
    })
}

fn sony_chromatic(values: &[f64]) -> Option<Vec<(f64, f64, f64)>> {
    let count = *values.first()? as usize;
    let n = count / 2;
    if !count.is_multiple_of(2) || !(2..=32).contains(&n) || values.len() < count + 1 {
        return None;
    }
    let scale = 1.0 / f64::from(1u32 << 21);
    let curve: Vec<(f64, f64, f64)> = (0..n)
        .map(|i| {
            (
                (i as f64 + 0.5) / n as f64,
                values[1 + i] * scale,
                values[1 + n + i] * scale,
            )
        })
        .collect();
    curve
        .iter()
        .all(|&(_, r, b)| r.abs() < 0.01 && b.abs() < 0.01)
        .then_some(curve)
}

/// Knots for the models given as formulas.
const FORMULA_KNOTS: usize = 32;

/// A formula model `k(r)` (output radius r samples the camera's image at
/// `r k(r)`, radii relative to half the diagonal) as the curve the warp
/// takes, which maps corners to corners, plus the zoom that restores the
/// camera's framing: `zoom` is the output radius that samples the sensor
/// corner (`zoom k(zoom) = 1`), so the corrected corners crop inside the
/// sensor's (vignetted) corners as in the camera and Camera Raw.
fn framed_curve(k: impl Fn(f64) -> f64) -> Option<(Vec<(f64, f64)>, f64)> {
    let mut zoom = 1.0f64;
    for _ in 0..40 {
        let f = zoom * k(zoom) - 1.0;
        let h = 1e-4;
        let derivative = ((zoom + h) * k(zoom + h) - (zoom - h) * k(zoom - h)) / (2.0 * h);
        if !f.is_finite() || derivative.abs() < 1e-9 {
            return None;
        }
        zoom -= f / derivative;
        if !(0.5..=2.0).contains(&zoom) {
            return None;
        }
    }
    let curve = (1..=FORMULA_KNOTS)
        .map(|i| {
            let u = i as f64 / FORMULA_KNOTS as f64;
            (u, zoom * k(zoom * u))
        })
        .collect();
    Some((curve, zoom))
}

/// Panasonic's DistortionInfo (the model in ExifTool's PanasonicRaw notes
/// and Rigo's RW2 notes): the corrected radius is
/// `Ru = Rd + s (a Rd³ + b Rd⁵ + c Rd⁷)` with `s = 1 / (1 + v5 / 32768)`,
/// `a = v8`, `b = v4`, `c = v11` (each / 32768). Radii are in units of
/// DistortionN (v12) pixels: the half-diagonal of the camera's full frame
/// (TZ71 4000 × 3000: 2500, LX15 5472 × 3648: 3288), so frames stored in a
/// cropped aspect ratio (TZ71 1:1: 2992 × 2992) scale their radii by their
/// own half-diagonal over N. Off unless the flag nibble (v7) is 1.
pub fn panasonic_curve(v: &[i16; 16], half_diagonal: Option<f64>) -> Option<EmbeddedLensData> {
    if v[7] & 0x0f != 1 {
        return None;
    }
    let reach = match half_diagonal {
        Some(hd) if v[12] > 0 && (0.4..=1.6).contains(&(hd / v[12] as f64)) => hd / v[12] as f64,
        _ => 1.0,
    };
    let scale = 1.0 / (1.0 + v[5] as f64 / 32768.0);
    let (a, b, c) = (
        v[8] as f64 / 32768.0,
        v[4] as f64 / 32768.0,
        v[11] as f64 / 32768.0,
    );
    let forward = |rd: f64| {
        let rd2 = rd * rd;
        rd + scale * rd * rd2 * (a + rd2 * (b + rd2 * c))
    };
    // The frame's corner lies at rd = reach. Strong superzoom corrections
    // (TZ80, TZ96: +17 % at the corner) turn over just beyond it, where
    // Newton's method on the full curve diverged and the data were dropped;
    // the polynomial only has to be monotonic up to the corner.
    let corner = reach;
    let steps = 64;
    if (1..=steps).any(|i| {
        let (r0, r1) = (
            corner * (i - 1) as f64 / steps as f64,
            corner * i as f64 / steps as f64,
        );
        forward(r1) <= forward(r0)
    }) {
        return None;
    }
    let ru_corner = forward(corner);
    // rd for ru in [0, ru_corner], by bisection.
    let inverse = |ru: f64| {
        let (mut lo, mut hi) = (0.0, corner);
        for _ in 0..60 {
            let mid = 0.5 * (lo + hi);
            if forward(mid) < ru {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        0.5 * (lo + hi)
    };
    // Output radius u (frame corner = 1) at the zoom that maps the corner to
    // the sensor corner; the camera's framing is ru = reach.
    let zoom = ru_corner / reach;
    let curve: Vec<(f64, f64)> = (1..=FORMULA_KNOTS)
        .map(|i| {
            let u = i as f64 / FORMULA_KNOTS as f64;
            let ru = (zoom * u * reach).min(ru_corner).max(1e-6);
            (u, zoom * inverse(ru) / ru)
        })
        .collect();
    plausible(&curve).then_some(EmbeddedLensData {
        maker: "Panasonic",
        distortion: curve,
        vignetting: None,
        zoom,
        chromatic: None,
    })
}

/// Units of Panasonic's CA shifts per pixel (fitted, see `panasonic_chromatic`).
const PANASONIC_CA_UNITS_PER_PIXEL: f64 = 1200.0;

/// Panasonic's lateral CA (RW2 IFD0 tag 0x011b, 32 values; undocumented,
/// layout worked out by fitting `lateral_ca::estimate` on 333 raw.pixls.us
/// RW2 files, notes: reference/lateral-ca.md): four knot radii in sensor
/// pixels from the optical centre (values 17, 16, 4, 11; the last about the
/// sensor's half-diagonal), and at those radii the red (values 12, 23, 8,
/// 26) and blue (27, 10, 20, 29) image's radial shift against green, in
/// 1/1200 pixel, negative outward. The shift is linear between the knots
/// from 0 at the centre and continues along the last segment beyond the
/// fourth knot. Values 0, 1, 30 and 31 change with every frame (checksums).
/// The rest (frame counters, flags) is not used. Returns (radius relative
/// to the frame's half-diagonal, red scale − 1, blue scale − 1) as
/// `EmbeddedLensData::chromatic`.
///
/// Fit: one unit for all bodies; on 333 files the measured CA (mean |band
/// median| over the radius bands) fell from 0.36 ‰ to 0.16 ‰, lower on 299,
/// higher by more than 0.03 ‰ on 11 (LX7, LX100, FZ72, GF10 frames).
pub fn panasonic_chromatic(v: &[i16; 32], half_diagonal: f64) -> Option<Vec<(f64, f64, f64)>> {
    let knots = [v[17], v[16], v[4], v[11]].map(f64::from);
    if knots[0] <= 0.0 || knots.windows(2).any(|w| w[1] <= w[0]) {
        return None;
    }
    // Binned or oddly cropped frames (LX7 at half size) cover a different
    // part of the sensor than the radii describe.
    if !(0.7..=1.3).contains(&(half_diagonal / knots[3])) {
        return None;
    }
    let red = [v[12], v[23], v[8], v[26]].map(f64::from);
    let blue = [v[27], v[10], v[20], v[29]].map(f64::from);
    let shift = |values: &[f64; 4], r: f64| -> f64 {
        if r >= knots[3] {
            let slope = (values[3] - values[2]) / (knots[3] - knots[2]);
            return values[3] + slope * (r - knots[3]);
        }
        let (mut r0, mut s0) = (0.0, 0.0);
        for (&r1, &s1) in knots.iter().zip(values) {
            if r <= r1 {
                return s0 + (s1 - s0) * (r - r0) / (r1 - r0);
            }
            (r0, s0) = (r1, s1);
        }
        s0
    };
    let curve: Vec<(f64, f64, f64)> = (1..=FORMULA_KNOTS)
        .map(|i| {
            let u = i as f64 / FORMULA_KNOTS as f64;
            let r = u * half_diagonal;
            let scale = |values: &[f64; 4]| -shift(values, r) / (PANASONIC_CA_UNITS_PER_PIXEL * r);
            (u, scale(&red), scale(&blue))
        })
        .collect();
    curve
        .iter()
        .all(|&(_, r, b)| r.abs() < 0.01 && b.abs() < 0.01)
        .then_some(curve)
}

/// Olympus / OM System ImageProcessing distortion [k2, k4, k6, s] (ExifTool
/// tag 0x150a): an output radius `r` (corner = 1) samples the camera's image
/// at `r s (1 + k2 (r s)² + k4 (r s)⁴ + k6 (r s)⁶)`; `0 0 0 1` means none.
pub fn olympus_curve(d: &[f64], reach: f64) -> Option<EmbeddedLensData> {
    let [k2, k4, k6, s] = *d else {
        return None;
    };
    if (k2 == 0.0 && k4 == 0.0 && k6 == 0.0) || !(0.5..=1.5).contains(&s) {
        return None;
    }
    let k = |r: f64| {
        let rs2 = (r * reach * s) * (r * reach * s);
        s * (1.0 + rs2 * (k2 + rs2 * (k4 + rs2 * k6)))
    };
    let (curve, zoom) = framed_curve(k)?;
    plausible(&curve).then_some(EmbeddedLensData {
        maker: "Olympus",
        distortion: curve,
        vignetting: None,
        zoom,
        chromatic: None,
    })
}

/// Nikon's lens data (NikonNEFInfo, tag 0xC7D5 in a SubIFD of Z-mount NEFs).
#[derive(Debug, Clone, PartialEq)]
pub struct NikonLensInfo {
    /// DistortionInfo flag: Z-mount lenses 3 (correction required by the
    /// design) or 1 (optional); the D780 writes 2, the D6 0.
    pub distortion_flag: u8,
    /// DistortionInfo coefficients as stored (offsets 0x14, 0x1c, 0x24, 0x2c).
    pub distortion: [f64; 4],
    /// VignetteInfo coefficients as stored (offsets 0x14 … 0x44, every
    /// second one) and the strength factor at offset 0x60.
    pub vignetting: [f64; 4],
    pub vignetting_strength: f64,
}

/// Parses the NikonNEFInfo blob: "Nikon\0" + version, then a small TIFF whose
/// tag 5 is DistortionInfo and tag 6 VignetteInfo (signed rationals).
pub fn parse_nikon_nef_info(blob: &[u8]) -> Option<NikonLensInfo> {
    let tiff = blob.strip_prefix(b"Nikon\0")?.get(4..)?;
    let little = match tiff.get(0..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let u16_at = |b: &[u8], at: usize| -> Option<u16> {
        let v: [u8; 2] = b.get(at..at + 2)?.try_into().ok()?;
        Some(if little {
            u16::from_le_bytes(v)
        } else {
            u16::from_be_bytes(v)
        })
    };
    let u32_at = |b: &[u8], at: usize| -> Option<u32> {
        let v: [u8; 4] = b.get(at..at + 4)?.try_into().ok()?;
        Some(if little {
            u32::from_le_bytes(v)
        } else {
            u32::from_be_bytes(v)
        })
    };
    let srational = |b: &[u8], at: usize| -> Option<f64> {
        let den = u32_at(b, at + 4)?;
        (den != 0).then(|| u32_at(b, at).map(|num| num as i32 as f64 / den as f64))?
    };
    let ifd = u32_at(tiff, 4)? as usize;
    let entries = u16_at(tiff, ifd)? as usize;
    let mut distortion_info = None;
    let mut vignette_info = None;
    for i in 0..entries.min(32) {
        let at = ifd + 2 + 12 * i;
        let (tag, count, offset) = (
            u16_at(tiff, at)?,
            u32_at(tiff, at + 4)? as usize,
            u32_at(tiff, at + 8)? as usize,
        );
        let data = tiff.get(offset..offset.checked_add(count)?);
        match tag {
            5 => distortion_info = data,
            6 => vignette_info = data,
            _ => {}
        }
    }
    let distortion_info = distortion_info.filter(|d| d.len() >= 0x34 && d.starts_with(b"0100"))?;
    let vignette_info = vignette_info.filter(|v| v.len() >= 0x68 && v.starts_with(b"0100"));
    let distortion = [0x14, 0x1c, 0x24, 0x2c].map(|at| srational(distortion_info, at));
    let vignetting = vignette_info.map(|v| [0x14, 0x24, 0x34, 0x44].map(|at| srational(v, at)));
    let strength = vignette_info.and_then(|v| {
        let den = u32_at(v, 0x64)?;
        (den != 0).then(|| u32_at(v, 0x60).map(|num| num as f64 / den as f64))?
    });
    Some(NikonLensInfo {
        distortion_flag: distortion_info[4],
        distortion: [
            distortion[0]?,
            distortion[1]?,
            distortion[2]?,
            distortion[3]?,
        ],
        vignetting: match vignetting {
            Some([Some(a), Some(b), Some(c), Some(d)]) => [a, b, c, d],
            _ => [0.0; 4],
        },
        vignetting_strength: strength.unwrap_or(0.0),
    })
}

/// The distortion table of Nikon's compacts (COOLPIX P950, P1000, P1100):
/// NikonNEFInfo tag 2, header "0100", flag 3, layout 3, 17 knots, then 17
/// signed rationals: the distortion in percent at radii 0, 1/16 … 1 (corner
/// = 1). An output pixel at `r` samples the sensor at `r (1 + v(r) / 100)`
/// (Adobe DNG Converter's WarpRectilinear for a P1000 file within 0.02 %).
/// Layout 2 (A1000, B700: 15 valid knots) spans a little beyond the corner
/// and is resampled.
pub fn parse_nikon_compact_distortion(blob: &[u8]) -> Option<Vec<f64>> {
    let tiff = blob.strip_prefix(b"Nikon\0")?.get(4..)?;
    let little = match tiff.get(0..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let u16_at = |b: &[u8], at: usize| -> Option<u16> {
        let v: [u8; 2] = b.get(at..at + 2)?.try_into().ok()?;
        Some(if little {
            u16::from_le_bytes(v)
        } else {
            u16::from_be_bytes(v)
        })
    };
    let u32_at = |b: &[u8], at: usize| -> Option<u32> {
        let v: [u8; 4] = b.get(at..at + 4)?.try_into().ok()?;
        Some(if little {
            u32::from_le_bytes(v)
        } else {
            u32::from_be_bytes(v)
        })
    };
    let ifd = u32_at(tiff, 4)? as usize;
    let entries = u16_at(tiff, ifd)? as usize;
    let table = (0..entries.min(32)).find_map(|i| {
        let at = ifd + 2 + 12 * i;
        (u16_at(tiff, at)? == 2).then(|| {
            let (count, offset) = (
                u32_at(tiff, at + 4)? as usize,
                u32_at(tiff, at + 8)? as usize,
            );
            tiff.get(offset..offset.checked_add(count)?)
        })?
    })?;
    if !table.starts_with(b"0100") || table.get(4..6)? != [3, 3] && table.get(4..6)? != [3, 2] {
        return None;
    }
    let knot = |i: usize| -> Option<f64> {
        let at = 8 + 8 * i;
        let den = u32_at(table, at + 4)?;
        (den != 0).then(|| u32_at(table, at).map(|num| num as i32 as f64 / den as f64))?
    };
    let values: Vec<f64> = if table.get(5) == Some(&3) {
        if table.get(6) != Some(&17) {
            return None;
        }
        (0..17).map(knot).collect::<Option<_>>()?
    } else {
        // Layout 2 (A1000, B700): 15 valid knots spaced 0.0738 of the
        // corner radius (fitted to Adobe DNG Converter's WarpRectilinear for
        // an A1000 file: rms 0.58 % of radius against 18 % at the corner),
        // resampled onto the 17 corner-to-corner knots of layout 3.
        const STEP: f64 = 0.0738;
        let raw: Vec<f64> = (0..15).map(knot).collect::<Option<_>>()?;
        (0..17)
            .map(|j| {
                let x = (j as f64 / 16.0 / STEP).min(14.0);
                let i = (x.floor() as usize).min(13);
                let t = x - i as f64;
                Some(raw[i] + (raw[i + 1] - raw[i]) * t)
            })
            .collect::<Option<_>>()?
    };
    (values[0] == 0.0 && values.iter().all(|v| v.abs() < 30.0)).then_some(values)
}

/// The compact table as the warp's curve (corners to corners).
pub fn nikon_compact_curve(values: &[f64]) -> Option<EmbeddedLensData> {
    let n = values.len();
    if n < 3 {
        return None;
    }
    let ratio = |r: f64| {
        let x = (r * (n - 1) as f64).clamp(0.0, (n - 1) as f64);
        let i = (x.floor() as usize).min(n - 2);
        let t = x - i as f64;
        1.0 + (values[i] + (values[i + 1] - values[i]) * t) / 100.0
    };
    let corner = ratio(1.0);
    let distortion: Vec<(f64, f64)> = (1..=FORMULA_KNOTS)
        .map(|i| {
            let u = i as f64 / FORMULA_KNOTS as f64;
            (u, ratio(u) / corner)
        })
        .collect();
    plausible(&distortion).then_some(EmbeddedLensData {
        maker: "Nikon",
        distortion,
        vignetting: None,
        zoom: 1.0,
        chromatic: None,
    })
}

/// Nikon's built-in correction, fitted to Adobe DNG Converter's opcodes for
/// 14 Z-mount files of 11 bodies. With the radius relative to the
/// half-diagonal (corner = 1), an output pixel at `r` samples the sensor at
/// `r (1 + d₃ r² + d₂ r³ + d₁ r⁴)` up to the framing zoom (0.07 % of the
/// radius from the converter's WarpRectilinear), and the vignetting gain at
/// sensor radius `r` is `1 + t (v₄ r² + v₃ r⁴ + v₂ r⁶ + v₁ r⁸)` (its
/// FixVignetteRadial exactly) — both stored in reverse order of the powers.
/// The converter bakes these in only for lenses whose correction is
/// required; Camera Raw applies them to "optional" lenses as well (Z 30 +
/// 40 mm f/2: +0.25 EV in the corners, the data predicts +0.28), so they
/// are used for both. F-mount DSLRs that write the block (D780 flag 2,
/// D6 flag 0) are left alone: Camera Raw does not correct them (with the
/// data, the D6/D780 corners came out 0.55 EV brighter than its render).
pub fn nikon_curve(info: &NikonLensInfo) -> Option<EmbeddedLensData> {
    let [d1, d2, d3, d4] = info.distortion;
    // The fourth term was zero in every file seen; its power is unknown.
    if !matches!(info.distortion_flag, 1 | 3) || d4 != 0.0 {
        return None;
    }
    let p = |r: f64| 1.0 + r * r * (d3 + r * (d2 + r * d1));
    let corner = p(1.0);
    let distortion: Vec<(f64, f64)> = (1..=FORMULA_KNOTS)
        .map(|i| {
            let u = i as f64 / FORMULA_KNOTS as f64;
            (u, p(u) / corner)
        })
        .collect();
    let [v1, v2, v3, v4] = info.vignetting;
    let t = info.vignetting_strength;
    let vignetting =
        (t > 0.0 && t <= 1.0 && [v1, v2, v3, v4].iter().any(|v| *v != 0.0)).then(|| {
            (0..=FORMULA_KNOTS)
                .map(|i| {
                    let r = i as f64 / FORMULA_KNOTS as f64;
                    let r2 = r * r;
                    let gain = 1.0 + t * r2 * (v4 + r2 * (v3 + r2 * (v2 + r2 * v1)));
                    (r, 1.0 / gain)
                })
                .collect::<Vec<_>>()
        });
    plausible(&distortion).then_some(EmbeddedLensData {
        maker: "Nikon",
        distortion,
        vignetting: vignetting.filter(|c| c.iter().all(|&(_, b)| b > 0.05 && b <= 1.5)),
        zoom: 1.0,
        chromatic: None,
    })
}

/// Rejects garbage: radii ascending within [0, 2], corrections within ±30 %.
fn plausible(curve: &[(f64, f64)]) -> bool {
    curve.len() >= 2
        && curve.windows(2).all(|w| w[0].0 < w[1].0)
        && curve
            .iter()
            .all(|&(r, k)| (0.0..=2.0).contains(&r) && (0.7..=1.3).contains(&k))
}

fn read_u32_be<R: Read + Seek>(reader: &mut R, at: u64) -> Option<u32> {
    reader.seek(SeekFrom::Start(at)).ok()?;
    let mut bytes = [0u8; 4];
    reader.read_exact(&mut bytes).ok()?;
    Some(u32::from_be_bytes(bytes))
}

/// The recipe key naming which of the cameras' CA data a camera-mode recipe
/// was made with.
pub const CA_VERSION_KEY: &str = "cameraCaVersion";
/// The newest: 1 (2026-10-17) adds Panasonic's (RW2 tag 0x011b). Recipes
/// without the key were made with Fujifilm's and Sony's only; saving them
/// again fills in the same parameters as before.
pub const CA_VERSION: u64 = 1;

/// The recipe's camera CA version: `cameraCaVersion`, 0 when missing.
pub fn ca_version_of(adjustments: &serde_json::Value) -> u64 {
    adjustments
        .get(CA_VERSION_KEY)
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0)
        .min(CA_VERSION)
}

/// The correction data stored in the RAW file, if any.
pub fn read(path: &Path) -> Option<EmbeddedLensData> {
    read_version(path, CA_VERSION)
}

/// The correction data as camera CA version `ca_version` read it.
pub fn read_version(path: &Path, ca_version: u64) -> Option<EmbeddedLensData> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    let source = RawSource::new(path).ok()?;
    let mut reader = source.reader();
    match extension.as_str() {
        "raf" => {
            // Camera Raw ignores the stored corrections of these models
            // (Adobe DNG Converter writes no opcodes for them although the
            // files carry the same tables as the X100S or HS30EXR, which it
            // corrects); applying them left corners 0.45-1.2 EV brighter.
            const IGNORED_BY_CAMERA_RAW: &[&str] =
                &["FinePix X100", "FinePix F550EXR", "FinePix HS20EXR"];
            // The F600EXR, also without opcodes, renders closer with them.
            // The model string of the RAF header (bytes 28-60).
            let mut header = [0u8; 60];
            std::fs::File::open(path)
                .ok()?
                .read_exact(&mut header)
                .ok()?;
            let model = std::str::from_utf8(&header[28..60])
                .ok()
                .map(|text| text.trim_end_matches('\0').to_string());
            if model.is_some_and(|model| {
                IGNORED_BY_CAMERA_RAW
                    .iter()
                    .any(|m| model.trim().eq_ignore_ascii_case(m))
            }) {
                return None;
            }
            // The FujiIFD's offset is stored at byte 100 of the RAF header.
            let offset = read_u32_be(&mut reader, 100)?;
            let ifd =
                IFD::new_root_with_correction(&mut reader, 0, offset, 0, 10, &[FUJI_IFD]).ok()?;
            let distortion = values(&ifd, FUJI_DISTORTION)?;
            let vignetting = values(&ifd, FUJI_VIGNETTING);
            let chromatic = values(&ifd, FUJI_CHROMATIC);
            fuji_curves_with_ca(&distortion, vignetting.as_deref(), chromatic.as_deref())
        }
        "arw" => {
            let settings = crate::capture_info::sony_lens_settings(path.to_str()?);
            if settings.is_some_and(|s| s.distortion_off) {
                return None;
            }
            let vignetting_auto = settings.is_some_and(|s| s.vignetting_auto);
            let ifd =
                IFD::new_root_with_correction(&mut reader, 0, 0, 0, 10, &[SUB_IFDS, EXIF_IFD])
                    .ok()?;
            // A 16:9, 4:3 or 1:1 frame reaches a smaller part of the 3:2
            // sensor's radius the data refer to.
            let mut head = Vec::new();
            std::fs::File::open(path)
                .ok()?
                .take(1 << 20)
                .read_to_end(&mut head)
                .ok()?;
            let reach = match crate::capture_info::aspect_crop(&head) {
                Some(crate::capture_info::AspectCrop::Centered(w, h)) => {
                    let (w, h) = (w as f64, h as f64);
                    let full_w = w.max(h * 1.5);
                    w.hypot(h) / full_w.hypot(full_w / 1.5)
                }
                _ => 1.0,
            };
            // The lateral CA is only in the encrypted SR2 directory.
            let sr2 = rawler::decoders::arw::ArwDecoder::sr2_subifd(&source).ok();
            let chromatic = sr2.as_ref().and_then(|sr2| values(sr2, SR2_CHROMATIC));
            if let Some(distortion) = values(&ifd, SONY_DISTORTION) {
                let vignetting = values(&ifd, SONY_VIGNETTING).filter(|_| vignetting_auto);
                return sony_curves_with_ca(
                    &distortion,
                    vignetting.as_deref(),
                    chromatic.as_deref(),
                )
                .map(|d| reframed(d, reach));
            }
            // Older bodies (NEX, early ILCE) keep the parameters only in the
            // encrypted SR2 directory (tags 0x797d / 0x7982 there), which
            // Camera Raw reads too.
            let sr2 = sr2?;
            let vignetting = values(&sr2, SR2_VIGNETTING).filter(|_| vignetting_auto);
            sony_curves_with_ca(
                &values(&sr2, SR2_DISTORTION)?,
                vignetting.as_deref(),
                chromatic.as_deref(),
            )
            .map(|d| reframed(d, reach))
        }
        "dng" => sony_dng_lens(&source).or_else(|| adobe_profile(&source)),
        "cr3" => canon_ctmd_lens(path),
        "rw2" | "rwl" => {
            let path = path.to_str()?;
            let chromatic = crate::capture_info::panasonic_ca_info(path)
                .filter(|_| ca_version >= 1)
                .and_then(|(values, half_diagonal)| panasonic_chromatic(&values, half_diagonal));
            let distortion = crate::capture_info::panasonic_distortion_info(path)
                .and_then(|(values, half_diagonal)| panasonic_curve(&values, half_diagonal));
            match distortion {
                Some(data) => Some(EmbeddedLensData { chromatic, ..data }),
                // Some bodies store CA data without distortion data (TZ81).
                None => chromatic.map(|chromatic| EmbeddedLensData {
                    maker: "Panasonic",
                    distortion: vec![(0.5, 1.0), (1.0, 1.0)],
                    vignetting: None,
                    zoom: 1.0,
                    chromatic: Some(chromatic),
                }),
            }
        }
        "nef" | "nrw" => {
            let ifd =
                IFD::new_root_with_correction(&mut reader, 0, 0, 0, 10, &[SUB_IFDS, EXIF_IFD])
                    .ok()?;
            let blob = match &ifd.get_entry_recursive(NIKON_NEF_INFO)?.value {
                rawler::formats::tiff::Value::Undefined(bytes)
                | rawler::formats::tiff::Value::Byte(bytes) => bytes.clone(),
                _ => return None,
            };
            match parse_nikon_nef_info(&blob) {
                Some(info) => nikon_curve(&info),
                None => nikon_compact_curve(&parse_nikon_compact_distortion(&blob)?),
            }
        }
        "orf" | "ori" => {
            let (distortion, _ca) = crate::capture_info::olympus_lens_correction(path.to_str()?)?;
            // The model's corner is the 4:3 sensor's; an aspect-ratio frame
            // (16:9, 3:2, 1:1) reaches a smaller part of its radius.
            let mut head = Vec::new();
            std::fs::File::open(path)
                .ok()?
                .take(8 << 20)
                .read_to_end(&mut head)
                .ok()?;
            let reach = crate::capture_info::aspect_crop(&head)
                .map(|crop| match crop {
                    crate::capture_info::AspectCrop::Relative(_, _, w, h)
                    | crate::capture_info::AspectCrop::Absolute(_, _, w, h)
                    | crate::capture_info::AspectCrop::Centered(w, h) => {
                        let (w, h) = (w as f64, h as f64);
                        let full_w = w.max(h * 4.0 / 3.0);
                        let full_h = full_w * 3.0 / 4.0;
                        (w * w + h * h).sqrt() / (full_w * full_w + full_h * full_h).sqrt()
                    }
                })
                .filter(|reach| (0.5..=1.0).contains(reach))
                .unwrap_or(1.0);
            olympus_curve(&distortion, reach)
        }
        _ => None,
    }
}

/// Linear interpolation; constant below the first knot, continued along the
/// last segment beyond the last (Sony's last knot lies just inside the corner).
fn interpolate(curve: &[(f64, f64)], r: f64) -> f64 {
    let (first, last) = (curve[0], curve[curve.len() - 1]);
    if r <= first.0 {
        return first.1;
    }
    if r >= last.0 {
        let before = curve[curve.len() - 2];
        if last.0 > before.0 && last.1 != before.1 {
            return last.1 + (last.1 - before.1) * (r - last.0) / (last.0 - before.0);
        }
        return last.1;
    }
    let i = curve.partition_point(|&(x, _)| x <= r);
    let (a, b) = (curve[i - 1], curve[i]);
    a.1 + (b.1 - a.1) * (r - a.0) / (b.0 - a.0)
}

/// Least squares for three unknowns via the normal equations.
fn solve3(rows: &[[f64; 4]]) -> Option<[f64; 3]> {
    let mut m = [[0.0f64; 4]; 3];
    for row in rows {
        for i in 0..3 {
            for j in 0..3 {
                m[i][j] += row[i] * row[j];
            }
            m[i][3] += row[i] * row[3];
        }
    }
    for col in 0..3 {
        let pivot = (col..3).max_by(|&a, &b| m[a][col].abs().total_cmp(&m[b][col].abs()))?;
        m.swap(col, pivot);
        if m[col][col].abs() < 1e-12 {
            return None;
        }
        let pivot_row = m[col];
        for (row, values) in m.iter_mut().enumerate() {
            if row != col {
                let f = values[col] / pivot_row[col];
                for (value, pivot_value) in values.iter_mut().zip(pivot_row).skip(col) {
                    *value -= f * pivot_value;
                }
            }
        }
    }
    Some([m[0][3] / m[0][0], m[1][3] / m[1][1], m[2][3] / m[2][2]])
}

const FIT_SAMPLES: usize = 64;

/// PTLens coefficients (a, b, c) with r_src = r (a r³ + b r² + c r + 1 − a − b − c)
/// for the curve k(r) / k(1) on [0, 1], minimizing the radial displacement.
pub fn fit_distortion(curve: &[(f64, f64)]) -> Option<[f64; 3]> {
    let corner = interpolate(curve, 1.0);
    let rows: Vec<[f64; 4]> = (1..=FIT_SAMPLES)
        .map(|i| {
            let r = i as f64 / FIT_SAMPLES as f64;
            // Weighted by r: the error that matters is the displacement in pixels.
            let target = interpolate(curve, r) / corner - 1.0;
            [
                r * (r * r * r - 1.0),
                r * (r * r - 1.0),
                r * (r - 1.0),
                r * target,
            ]
        })
        .collect();
    solve3(&rows)
}

/// Vignetting coefficients with brightness 1 + k1 r² + k2 r⁴ + k3 r⁶ on [0, 1].
pub fn fit_vignetting(curve: &[(f64, f64)]) -> Option<[f64; 3]> {
    let rows: Vec<[f64; 4]> = (0..=FIT_SAMPLES)
        .map(|i| {
            let r2 = (i as f64 / FIT_SAMPLES as f64).powi(2);
            [
                r2,
                r2 * r2,
                r2 * r2 * r2,
                interpolate(curve, r2.sqrt()) - 1.0,
            ]
        })
        .collect();
    solve3(&rows)
}

/// Lateral CA (a, b) with scale − 1 = a + b r² on [0, 1], minimizing the
/// displacement r (scale − 1).
pub fn fit_chromatic(curve: &[(f64, f64)]) -> Option<(f64, f64)> {
    let (mut s00, mut s01, mut s11, mut t0, mut t1) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for i in 1..=FIT_SAMPLES {
        let r = i as f64 / FIT_SAMPLES as f64;
        let (x0, x1, y) = (r, r * r * r, r * interpolate(curve, r));
        s00 += x0 * x0;
        s01 += x0 * x1;
        s11 += x1 * x1;
        t0 += x0 * y;
        t1 += x1 * y;
    }
    let det = s00 * s11 - s01 * s01;
    (det.abs() > 1e-12).then(|| ((t0 * s11 - t1 * s01) / det, (s00 * t1 - s01 * t0) / det))
}

/// The data as parameters of the geometry warp's PTLens model.
pub fn to_params(data: &EmbeddedLensData) -> Option<LensDistortionParams> {
    let [a, b, c] = fit_distortion(&data.distortion)?;
    let vignetting = data
        .vignetting
        .as_deref()
        .and_then(fit_vignetting)
        .unwrap_or([0.0; 3]);
    let mut params = LensDistortionParams::ptlens(a, b, c, vignetting).with_zoom(data.zoom);
    if let Some(chromatic) = data.chromatic.as_deref().filter(|c| !c.is_empty()) {
        let channel = |pick: fn(&(f64, f64, f64)) -> f64| {
            let curve: Vec<(f64, f64)> = chromatic.iter().map(|p| (p.0, pick(p))).collect();
            fit_chromatic(&curve)
        };
        if let (Some(red), Some(blue)) = (channel(|p| p.1), channel(|p| p.2)) {
            params = params.with_tca(red, blue);
        }
    }
    Some(params)
}

/// The recipe value of the lens mode that uses the camera's data.
pub const CAMERA_MODE: &str = "camera";

/// Parameters from the camera's data for `source`, if it carries any.
pub fn params_for(source: &Path) -> Option<LensDistortionParams> {
    params_for_version(source, CA_VERSION)
}

/// Parameters from the camera's data for a recipe: as its camera CA
/// version reads them (`cameraCaVersion`), so a recipe saved again keeps
/// rendering as it was made.
pub fn params_for_recipe(
    source: &Path,
    adjustments: &serde_json::Value,
) -> Option<LensDistortionParams> {
    params_for_version(source, ca_version_of(adjustments))
}

/// The `source_facts` version from which `camera_lens` carries Panasonic
/// lateral CA data (camera CA version 1).
const PANASONIC_CA_FACTS: u32 = 3;

/// Whether `params_for_recipe` gives the camera data the recipe reads. Not
/// offline when the registered facts predate what its camera CA version
/// reads (a Panasonic preview made before its CA data was read): saving then
/// keeps the recipe's parameters instead of dropping the camera's CA
/// correction.
pub fn recipe_params_known(source: &Path, adjustments: &serde_json::Value) -> bool {
    crate::source_facts::registered(&source.to_string_lossy()).is_none_or(|facts| {
        facts.version >= PANASONIC_CA_FACTS
            || ca_version_of(adjustments) == 0
            || !is_panasonic(source)
    })
}

fn is_panasonic(source: &Path) -> bool {
    source
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("rw2") || e.eq_ignore_ascii_case("rwl"))
}

fn params_for_version(source: &Path, ca_version: u64) -> Option<LensDistortionParams> {
    // Read once per file (and modification time): opening a photograph,
    // its thumbnail and its export all ask, and reading the RAW header costs
    // several milliseconds.
    type Cache = std::sync::Mutex<
        std::collections::HashMap<
            (std::path::PathBuf, std::time::SystemTime, u64),
            Option<LensDistortionParams>,
        >,
    >;
    // Offline: the data the file had (source_facts), read at the current
    // version; version 0 had no Panasonic CA and no CA-only data.
    if let Some(facts) = crate::source_facts::registered(&source.to_string_lossy()) {
        let params = facts.camera_lens.clone();
        if ca_version == 0 && is_panasonic(source) {
            return params
                .map(LensDistortionParams::without_tca)
                .filter(|p| !p.is_identity());
        }
        return params;
    }
    static CACHE: std::sync::OnceLock<Cache> = std::sync::OnceLock::new();
    let modified = std::fs::metadata(source).and_then(|m| m.modified()).ok();
    let key = modified.map(|time| (source.to_path_buf(), time, ca_version));
    let cache = CACHE.get_or_init(Default::default);
    if let Some(key) = &key
        && let Some(params) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(key)
    {
        return params.clone();
    }
    let params = read_version(source, ca_version)
        .as_ref()
        .and_then(to_params);
    if let Some(key) = key {
        let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
        if cache.len() > 4096 {
            cache.clear();
        }
        cache.insert(key, params.clone());
    }
    params
}

/// Profile corrections are on by default (Capture One): a photo that was never
/// edited, whose recipe has no lens mode at all, uses the camera's correction
/// data. Canon EOS R files, whose CR3s carry no readable correction data,
/// use the lens's lensfun profile when one matches the EXIF lens name
/// exactly: Camera Raw applies its own profile to them by default (R8 +
/// EF 24-105: corners +0.47 EV, R50 V + RF 28: +0.76 EV), not to DSLRs.
/// Saved recipes always carry a mode and are left as they are. Never-edited
/// RAWs also get the current `fringeVersion` (`edge_fringe::apply_default`)
/// and `autoCaVersion` (`lateral_ca::apply_default`), a photo without any
/// recipe the current `renderVersion`, and with the camera's data its
/// current `cameraCaVersion`.
pub fn apply_default(adjustments: &mut serde_json::Value, source: &Path) {
    if !(adjustments.is_null() || adjustments.is_object())
        || adjustments.get("lensCorrectionMode").is_some()
    {
        return;
    }
    // A never-edited RAW also starts with the current edge-fringe pass, as
    // the editor's new edits do.
    let is_raw = crate::formats::is_raw_file(source);
    // A photo without any recipe renders as a new edit (`renderVersion`);
    // a saved partial recipe without a lens mode (batch presets, batch Auto)
    // keeps the version it was made with.
    if adjustments.is_null() || adjustments.as_object().is_some_and(|map| map.is_empty()) {
        crate::image_processing::apply_render_default(adjustments);
    }
    crate::edge_fringe::apply_default(adjustments, true, is_raw);
    // And with the current automatic lateral CA, which decides itself
    // against the camera's CA data (`lateral_ca::applies`, `competes`).
    crate::lateral_ca::apply_default(adjustments, true, is_raw);
    if let Some(params) = params_for(source) {
        if adjustments.is_null() {
            *adjustments = serde_json::json!({});
        }
        adjustments["lensCorrectionMode"] = serde_json::json!(CAMERA_MODE);
        adjustments["lensDistortionParams"] = serde_json::to_value(params).unwrap_or_default();
        adjustments[CA_VERSION_KEY] = serde_json::json!(CA_VERSION);
        return;
    }
    let Some((maker, model, params)) = lensfun_default(source) else {
        return;
    };
    if adjustments.is_null() {
        *adjustments = serde_json::json!({});
    }
    adjustments["lensCorrectionMode"] = serde_json::json!("auto");
    adjustments["lensMaker"] = serde_json::json!(maker);
    adjustments["lensModel"] = serde_json::json!(model);
    adjustments["lensDistortionParams"] = serde_json::to_value(params).unwrap_or_default();
}

/// One 4-byte box of an ISO base media file (CR3): (type, body start, end).
fn bmff_boxes(file: &mut std::fs::File, start: u64, end: u64) -> Vec<([u8; 4], u64, u64)> {
    let mut boxes = Vec::new();
    let mut pos = start;
    while pos + 8 <= end {
        let mut header = [0u8; 16];
        if file.seek(SeekFrom::Start(pos)).is_err() || file.read_exact(&mut header[..8]).is_err() {
            break;
        }
        let mut size = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as u64;
        let kind = [header[4], header[5], header[6], header[7]];
        let mut body = pos + 8;
        if size == 1 {
            if file.read_exact(&mut header[8..]).is_err() {
                break;
            }
            size = u64::from_be_bytes(header[8..16].try_into().unwrap_or_default());
            body += 8;
        } else if size == 0 {
            size = end - pos;
        }
        if size < body - pos || pos + size > end {
            break;
        }
        boxes.push((kind, body, pos + size));
        pos += size;
    }
    boxes
}

/// The first sample of the CR3's timed-metadata (CTMD) track.
fn canon_ctmd_sample(path: &Path) -> Option<Vec<u8>> {
    let mut file = std::fs::File::open(path).ok()?;
    let length = file.metadata().ok()?.len();
    let find = |file: &mut std::fs::File, start: u64, end: u64, kind: &[u8; 4]| {
        bmff_boxes(file, start, end)
            .into_iter()
            .find(|(k, ..)| k == kind)
            .map(|(_, body, end)| (body, end))
    };
    let (moov, moov_end) = find(&mut file, 0, length, b"moov")?;
    for (kind, trak, trak_end) in bmff_boxes(&mut file, moov, moov_end) {
        if &kind != b"trak" {
            continue;
        }
        let Some((stbl, stbl_end)) = find(&mut file, trak, trak_end, b"mdia")
            .and_then(|(b, e)| find(&mut file, b, e, b"minf"))
            .and_then(|(b, e)| find(&mut file, b, e, b"stbl"))
        else {
            continue;
        };
        let Some((stsd, stsd_end)) = find(&mut file, stbl, stbl_end, b"stsd") else {
            continue;
        };
        // stsd: version/flags and entry count, then the sample entries.
        if find(&mut file, stsd + 8, stsd_end, b"CTMD").is_none() {
            continue;
        }
        let boxes = bmff_boxes(&mut file, stbl, stbl_end);
        let mut read_at = |offset: u64, buf: &mut [u8]| {
            file.seek(SeekFrom::Start(offset)).ok()?;
            file.read_exact(buf).ok()
        };
        let offset = if let Some((_, body, _)) = boxes.iter().find(|(k, ..)| k == b"co64") {
            let mut b = [0u8; 8];
            read_at(body + 8, &mut b)?;
            u64::from_be_bytes(b)
        } else {
            let (_, body, _) = boxes.iter().find(|(k, ..)| k == b"stco")?;
            let mut b = [0u8; 4];
            read_at(body + 8, &mut b)?;
            u32::from_be_bytes(b) as u64
        };
        let (_, stsz, _) = boxes.iter().find(|(k, ..)| k == b"stsz")?;
        let mut b = [0u8; 12];
        read_at(*stsz + 4, &mut b)?;
        let mut size = u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as u64;
        if size == 0 {
            size = u32::from_be_bytes([b[8], b[9], b[10], b[11]]) as u64;
        }
        if size == 0 || size > 1 << 24 || offset + size > length {
            return None;
        }
        let mut sample = vec![0u8; size as usize];
        read_at(offset, &mut sample)?;
        return Some(sample);
    }
    None
}

/// Radii and brightness of CTMD record 14 in a CTMD sample
/// (`canon_ctmd_lens`).
fn canon_vignetting_table(sample: &[u8]) -> Option<(Vec<u16>, Vec<u16>)> {
    const SLOTS: usize = 23;
    let mut rest = sample;
    let record = loop {
        if rest.len() < 12 {
            return None;
        }
        let size = u32::from_le_bytes(rest[..4].try_into().ok()?) as usize;
        let kind = u16::from_le_bytes([rest[4], rest[5]]);
        if size < 12 || size > rest.len() {
            return None;
        }
        if kind == 14 {
            break &rest[12..size];
        }
        rest = &rest[size..];
    };
    let words: Vec<u16> = record
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect();
    let count = *words.first()? as usize;
    if !(2..=SLOTS).contains(&count) || words.len() < 3 + 2 * SLOTS {
        return None;
    }
    let brightness = &words[3..3 + count];
    let radii = &words[3 + SLOTS..3 + SLOTS + count];
    if brightness[0] == 0 || radii.windows(2).any(|w| w[1] <= w[0]) {
        return None;
    }
    Some((radii.to_vec(), brightness.to_vec()))
}

/// Canon's lens correction data in CTMD record 14 of newer CR3s (EOS R5
/// Mark II, R6 Mark III; Camera Raw's "Camera Settings" profile for them).
/// After three counts (17) come 23-slot blocks of little-endian u16:
/// brightness (8191 = centre), radii, two tables and radii, one table and
/// radii (the tables around 16384 are not used yet). Radii are in 5 µm on
/// the sensor: 3596 is half the width of a 36 mm sensor, the last knot (4543)
/// lies just beyond a full-frame corner (21.6 mm). Normalized here by half
/// the recorded image's diagonal (FocalPlaneXResolution), so crop modes
/// read the same table.
fn canon_ctmd_lens(path: &Path) -> Option<EmbeddedLensData> {
    const RADIUS_MM: f64 = 0.005;
    let sample = canon_ctmd_sample(path)?;
    let source = RawSource::new(path).ok()?;
    let decoder = rawler::get_decoder(&source).ok()?;
    let metadata = decoder
        .raw_metadata(&source, &rawler::decoders::RawDecodeParams::default())
        .ok()?;
    // Camera Raw uses this data on the EOS R5 Mark II and R6 Mark III; on
    // other bodies that store it (R50 V) it uses its own lens profile, which
    // the lensfun profile, where there is one, matches better (R50 V + RF 28:
    // dE 0.79 with lensfun, 1.01 with the camera's data).
    let model = metadata.model.trim();
    let model = model.strip_prefix("Canon ").unwrap_or(model);
    if !matches!(
        model,
        "EOS R5m2" | "EOS R5 Mark II" | "EOS R6 Mark III" | "EOS R6m3" | "EOS R1"
    ) && lensfun_default(path).is_some()
    {
        return None;
    }
    let (radii, brightness) = canon_vignetting_table(&sample)?;
    let centre = f64::from(*brightness.first()?);
    let resolution = metadata
        .exif
        .focal_plane_x_resolution
        .filter(|r| r.d != 0 && r.n != 0)
        .map(|r| r.n as f64 / r.d as f64)?;
    let per_mm = match metadata.exif.focal_plane_resolution_unit {
        Some(3) => resolution / 10.0,
        _ => resolution / 25.4,
    };
    let width = f64::from(metadata.exif.exif_image_width.filter(|w| *w > 0)?);
    let height = f64::from(metadata.exif.exif_image_height.filter(|h| *h > 0)?);
    let half_diagonal_mm = width.hypot(height) / 2.0 / per_mm;
    let vignetting = radii
        .iter()
        .zip(&brightness)
        .map(|(&radius, &value)| {
            (
                f64::from(radius) * RADIUS_MM / half_diagonal_mm,
                f64::from(value) / centre,
            )
        })
        .collect();
    Some(EmbeddedLensData {
        maker: "Canon",
        distortion: vec![(0.0, 1.0), (1.0, 1.0)],
        vignetting: Some(vignetting),
        zoom: 1.0,
        chromatic: None,
    })
}

/// The blocks of Adobe DNG Converter's DNGPrivateData: "Adobe\0", then
/// four-character tag, big-endian length and data (padded to even) per block.
fn adobe_private_blocks(data: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut blocks = Vec::new();
    let Some(mut rest) = data.strip_prefix(b"Adobe\0") else {
        return blocks;
    };
    while rest.len() >= 8 {
        let tag = [rest[0], rest[1], rest[2], rest[3]];
        let length = u32::from_be_bytes([rest[4], rest[5], rest[6], rest[7]]) as usize;
        let Some(body) = rest.get(8..8 + length) else {
            break;
        };
        blocks.push((tag, body));
        // Blocks are padded to an even length (an odd MakN of the A3000,
        // A7 and A7R hid the SR2 block that follows it).
        rest = rest.get(8 + length + (length & 1)..).unwrap_or_default();
    }
    blocks
}

/// Sony's lens data for an Adobe DNG Converter file of a Sony body. The
/// DNG's WarpRectilinear only carries lateral chromatic aberration (green
/// plane identity); Camera Raw takes distortion and vignetting from the SR2
/// data kept in the DNG (A7C II DNG: corners and midtones 0.2-0.4 EV darker
/// without the vignetting), under the camera's settings, as for the ARW.
fn sony_dng_lens(source: &RawSource) -> Option<EmbeddedLensData> {
    let decoder = rawler::get_decoder(source).ok()?;
    let metadata = decoder
        .raw_metadata(source, &rawler::decoders::RawDecodeParams::default())
        .ok()?;
    if !metadata.make.eq_ignore_ascii_case("sony") {
        return None;
    }
    let root = decoder.ifd(rawler::decoders::WellKnownIFD::Root).ok()??;
    let private = match &root.get_entry(rawler::tags::DngTag::DNGPrivateData)?.value {
        rawler::formats::tiff::Value::Byte(bytes)
        | rawler::formats::tiff::Value::Undefined(bytes) => bytes.clone(),
        _ => return None,
    };
    let blocks = adobe_private_blocks(&private);
    let maker_note = blocks.iter().find(|(tag, _)| tag == b"MakN")?.1.get(6..)?;
    let settings = crate::capture_info::sony_lens_settings_from(maker_note)?;
    let sr2_block = blocks.iter().find(|(tag, _)| tag == b"SR2 ")?.1;
    let sr2 = rawler::decoders::arw::ArwDecoder::sr2_subifd_from_adobe_block(sr2_block).ok()?;
    let vignetting = values(&sr2, SR2_VIGNETTING)
        .or_else(|| values(&sr2, SONY_VIGNETTING))
        .filter(|_| settings.vignetting_auto);
    let distortion = values(&sr2, SR2_DISTORTION)
        .or_else(|| values(&sr2, SONY_DISTORTION))
        .filter(|_| !settings.distortion_off);
    let distortion = match distortion {
        Some(distortion) => distortion,
        None => {
            let count = *vignetting.as_ref()?.first()? as usize;
            std::iter::once(count as f64)
                .chain(std::iter::repeat_n(0.0, count))
                .collect()
        }
    };
    // The lateral CA is the DNG's WarpRectilinear, applied when decoding.
    let reach = user_crop_diagonal(decoder.as_ref()).unwrap_or(1.0);
    sony_curve(&distortion, vignetting.as_deref()).map(|d| reframed(d, reach))
}

/// Curves for a frame cropped from the one the data refer to, whose corner
/// lies at `reach` of the full frame's radius.
fn reframed(mut data: EmbeddedLensData, reach: f64) -> EmbeddedLensData {
    if (0.5..1.0).contains(&reach) {
        for point in data.distortion.iter_mut() {
            point.0 /= reach;
        }
        for point in data.vignetting.iter_mut().flatten() {
            point.0 /= reach;
        }
        for point in data.chromatic.iter_mut().flatten() {
            point.0 /= reach;
        }
    }
    data
}

/// The lens profile Adobe DNG Converter embeds in the XMP of DNGs from
/// bodies with built-in corrections (Fujifilm, Nikon Z, Panasonic, OM System:
/// photoshop:CameraProfiles, "Camera Settings"), which Camera Raw applies.
/// The DNG's WarpRectilinear only carries lateral chromatic aberration
/// (a radial term within 0.2 %); distortion and vignetting are here. Both
/// models work on the radius divided by the focal length; with the sensor
/// size (SensorFormatFactor: 35 mm half-diagonal 21.633 mm divided by it)
/// that becomes the frame-relative radius. PerspectiveModel: an ideal point
/// at radius r is imaged at r (1 + k1 r² + k2 r⁴ + k3 r⁶), scaled by its
/// ScaleFactor, and magnified at least enough to keep the corners filled.
/// VignetteModelPiecewiseParam: (radius, relative illumination) pairs.
fn adobe_profile(source: &RawSource) -> Option<EmbeddedLensData> {
    let decoder = rawler::get_decoder(source).ok()?;
    let root = decoder.ifd(rawler::decoders::WellKnownIFD::Root).ok()??;
    let xmp = match &root.get_entry(rawler::tags::TiffCommonTag::Xmp)?.value {
        rawler::formats::tiff::Value::Byte(bytes)
        | rawler::formats::tiff::Value::Undefined(bytes) => {
            String::from_utf8_lossy(bytes).into_owned()
        }
        rawler::formats::tiff::Value::Ascii(text) => text.strings().concat(),
        _ => return None,
    };
    let start = xmp.find("<photoshop:CameraProfiles>")?;
    let end = xmp[start..]
        .find("</photoshop:CameraProfiles>")
        .map_or(xmp.len(), |end| start + end);
    let profile = &xmp[start..end];
    let attribute = |within: &str, name: &str| -> Option<f64> {
        let key = format!("stCamera:{name}=\"");
        let start = within.find(&key)? + key.len();
        let end = within[start..].find('"')? + start;
        within[start..end].trim().parse().ok()
    };
    let focal = attribute(profile, "FocalLength")?;
    let factor = attribute(profile, "SensorFormatFactor")?;
    if !(focal > 0.0 && factor > 0.05) {
        return None;
    }
    // The frame's half-diagonal in units of the focal length; a
    // DefaultUserCrop (the in-camera aspect ratio) shortens it.
    let reach = 21.633 / factor / focal * user_crop_diagonal(decoder.as_ref()).unwrap_or(1.0);
    let scale = 1.0 / reach;

    let distortion = profile
        .find("<crlcp:PerspectiveModel")
        .and_then(|at| {
            let model = &profile[at..];
            let k = [
                attribute(model, "RadialDistortParam1").unwrap_or(0.0),
                attribute(model, "RadialDistortParam2").unwrap_or(0.0),
                attribute(model, "RadialDistortParam3").unwrap_or(0.0),
            ];
            let magnification = attribute(model, "ScaleFactor")
                .filter(|s| (0.5..=2.0).contains(s))
                .unwrap_or(1.0);
            k.iter()
                .all(|v| v.is_finite())
                .then_some((k, magnification))
        })
        .filter(|(k, _)| k.iter().any(|&v| v != 0.0))
        .and_then(|([k1, k2, k3], magnification)| {
            let (curve, zoom) = framed_curve(|u| {
                let r2 = (u * reach) * (u * reach);
                1.0 + r2 * (k1 + r2 * (k2 + r2 * k3))
            })?;
            // Camera Raw's framing is the model's own scale times
            // ScaleFactor (Fujifilm wide-angle zooms 1.01-1.04 show more of
            // the frame, in its renders of the RAF and of the DNG alike),
            // never beyond the corners (the warp keeps at least zoom 1).
            Some((curve, zoom / magnification))
        })
        .filter(|(curve, _)| plausible(curve));

    let vignetting = profile
        .find("<stCamera:VignetteModelPiecewiseParam>")
        .and_then(|at| {
            let list = &profile[at..];
            let end = list.find("</stCamera:VignetteModelPiecewiseParam>")?;
            let points: Vec<(f64, f64)> = list[..end]
                .split("<rdf:li>")
                .skip(1)
                .filter_map(|item| {
                    let item = item.split("</rdf:li>").next()?;
                    let (r, v) = item.split_once(',')?;
                    Some((
                        r.trim().parse::<f64>().ok()? * scale,
                        v.trim().parse::<f64>().ok()?,
                    ))
                })
                .filter(|&(r, v)| r > 0.0 && r <= 1.25 && v > 0.05 && v <= 1.5)
                .collect();
            (points.len() >= 3).then_some(points)
        });

    if distortion.is_none() && vignetting.is_none() {
        return None;
    }
    let (distortion, zoom) =
        distortion.unwrap_or_else(|| ((1..=8).map(|i| (i as f64 / 8.0, 1.0)).collect(), 1.0));
    Some(EmbeddedLensData {
        maker: "Adobe",
        distortion,
        vignetting,
        zoom,
        chromatic: None,
    })
}

/// The diagonal of a DNG's DefaultUserCrop relative to the default crop's.
fn user_crop_diagonal(decoder: &dyn rawler::decoders::Decoder) -> Option<f64> {
    let raw = decoder
        .ifd(rawler::decoders::WellKnownIFD::VirtualDngRawTags)
        .ok()??;
    let crop = &raw.get_entry(rawler::tags::DngTag::DefaultUserCrop)?.value;
    if crop.count() != 4 {
        return None;
    }
    let [top, left, bottom, right] = std::array::from_fn(|i| crop.force_f32(i) as f64);
    let size = &raw.get_entry(rawler::tags::DngTag::DefaultCropSize)?.value;
    let (width, height) = (size.force_f32(0) as f64, size.force_f32(1) as f64);
    let (w, h) = (right - left, bottom - top);
    (w > 0.25 && h > 0.25 && w <= 1.0 && h <= 1.0 && width > 0.0 && height > 0.0)
        .then(|| (w * width).hypot(h * height) / width.hypot(height))
}

/// EXIF LensModel (tag 0xA434) read directly from the file.
fn exif_lens_model(source: &Path) -> Option<String> {
    let file = std::fs::File::open(source).ok()?;
    let exif = exif::Reader::new()
        .read_from_container(&mut std::io::BufReader::new(file))
        .ok()?;
    let field = exif.get_field(exif::Tag::LensModel, exif::In::PRIMARY)?;
    match &field.value {
        exif::Value::Ascii(values) => values
            .first()
            .map(|bytes| String::from_utf8_lossy(bytes).trim().to_string())
            .filter(|name| !name.is_empty()),
        _ => None,
    }
}

/// Fixed-lens compacts whose lensfun profile brings them closer to Camera
/// Raw's default rendering (one corpus file each, 2026-10-06: S120 dE 3.62
/// → 1.13, G9 X Mark II 1.82 → 1.03, G1 X Mark III 1.09 → 0.61, G1 X Mark II
/// 2.10 → 1.84, G16 0.91 → 0.83). Enabling every PowerShot/Coolpix made 13
/// others worse (G11 0.95 → 13.4, S110 1.05 → 5.6: lensfun data differs or
/// Camera Raw does not correct them), so the list stays explicit.
const LENSFUN_DEFAULT_COMPACTS: &[&str] = &[
    "PowerShot S120",
    "PowerShot G9 X Mark II",
    "PowerShot G1 X Mark III",
    "PowerShot G1 X Mark II",
    "PowerShot G16",
];

fn is_fixed_lens_compact(make: &str, model: &str) -> bool {
    make.to_ascii_lowercase().contains("canon")
        && LENSFUN_DEFAULT_COMPACTS
            .iter()
            .any(|m| model.trim_start_matches("Canon ").eq_ignore_ascii_case(m))
}

/// Canon's mirrorless bodies: "EOS R", "EOS RP", "EOS R5", "EOS R6m2",
/// "EOS R50 V" … but not the "EOS Rebel" DSLRs.
fn is_canon_eos_r(make: &str, model: &str) -> bool {
    if !make.to_ascii_lowercase().contains("canon") {
        return false;
    }
    let model = model.trim();
    let model = model.strip_prefix("Canon ").unwrap_or(model);
    let Some(rest) = model.strip_prefix("EOS R") else {
        return false;
    };
    rest.is_empty() || rest.starts_with('P') || rest.starts_with(|c: char| c.is_ascii_digit())
}

/// The lensfun profile for a Canon EOS R file: (maker, model, parameters).
pub fn lensfun_default(source: &Path) -> Option<(String, String, LensDistortionParams)> {
    type Cache = std::sync::Mutex<
        std::collections::HashMap<
            (std::path::PathBuf, std::time::SystemTime),
            Option<(String, String, LensDistortionParams)>,
        >,
    >;
    if let Some(facts) = crate::source_facts::registered(&source.to_string_lossy()) {
        return facts.lensfun_default.clone();
    }
    static CACHE: std::sync::OnceLock<Cache> = std::sync::OnceLock::new();
    let key = std::fs::metadata(source)
        .and_then(|m| m.modified())
        .ok()
        .map(|time| (source.to_path_buf(), time));
    let cache = CACHE.get_or_init(Default::default);
    if let Some(key) = &key
        && let Some(hit) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(key)
    {
        return hit.clone();
    }
    let result = std::panic::catch_unwind(|| lensfun_default_uncached(source))
        .ok()
        .flatten();
    if let Some(key) = key {
        let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
        if cache.len() > 4096 {
            cache.clear();
        }
        cache.insert(key, result.clone());
    }
    result
}

fn lensfun_default_uncached(source: &Path) -> Option<(String, String, LensDistortionParams)> {
    let extension = source.extension()?.to_str()?.to_ascii_lowercase();
    if !matches!(extension.as_str(), "cr3" | "cr2" | "dng") {
        return None;
    }
    let db = crate::lens_correction::global_lens_db()?;
    let raw = RawSource::new(source).ok()?;
    let decoder = rawler::get_decoder(&raw).ok()?;
    let metadata = decoder
        .raw_metadata(&raw, &rawler::decoders::RawDecodeParams::default())
        .ok()?;
    let rational = |r: rawler::formats::tiff::Rational| (r.d != 0).then(|| r.n as f32 / r.d as f32);
    let exif = &metadata.exif;
    let focal = exif.focal_length.and_then(rational)?;
    let aperture = exif.fnumber.and_then(rational);
    let distance = exif.subject_distance.and_then(rational);
    // DNG conversions of EOS R files carry no lens data; Camera Raw renders
    // them like the CR3s.
    let lens = if matches!(extension.as_str(), "cr3" | "dng")
        && is_canon_eos_r(&metadata.make, &metadata.model)
    {
        // rawler leaves the lens name empty for DNGs; read it from EXIF.
        let lens_name = exif
            .lens_model
            .clone()
            .or_else(|| exif_lens_model(source))?;
        crate::lens_correction::find_lens_exact(&db, "Canon", &lens_name)?
    } else if extension != "dng" && is_fixed_lens_compact(&metadata.make, &metadata.model) {
        // Camera Raw corrects these compacts by default with Adobe's own
        // profiles; lensfun's fixed lens for the camera's mount stands in.
        crate::lens_correction::find_lens_by_camera_mount(&db, &metadata.make, &metadata.model)?
    } else {
        return None;
    };
    // Crop-mode captures (an EOS R5's APS-C frame) are smaller than the
    // body's sensor: their width from FocalPlaneXResolution decides.
    let frame_crop = (|| {
        let resolution = exif.focal_plane_x_resolution.and_then(rational)?;
        let unit_mm = match exif.focal_plane_resolution_unit? {
            2 => 25.4,
            3 => 10.0,
            4 => 1.0,
            _ => return None,
        };
        let width_mm = exif.exif_image_width? as f32 / resolution * unit_mm;
        Some(36.0 / width_mm)
    })();
    let crop =
        crate::lens_correction::body_crop_factor(&db, &metadata.make, &metadata.model, frame_crop);
    let params = lens.get_distortion_params(focal, aperture, distance, crop)?;
    let (maker, model) = crate::lens_correction::lens_result(&db, lens);
    Some((maker, model, params))
}

/// What the Lens tool shows for a photo's camera data.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CameraLensProfile {
    pub maker: &'static str,
    pub params: LensDistortionParams,
    pub vignetting: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    // Panasonic DMC-FZ330 DistortionInfo (barrel; N = 2500 = frame half-diagonal).
    const FZ330: [i16; 16] = [
        28721, -18280, 274, 0, -2282, 0, 241, 1, 8820, 356, 598, -663, 2500, 856, 19541, -8954,
    ];

    /// A NikonNEFInfo blob with a Z 6 + NIKKOR Z 24-70mm f/4 S file's values.
    fn nikon_blob(distortion_flag: u8) -> Vec<u8> {
        let rational = |v: f64| {
            let mut b = ((v * 1048576.0).round() as i32).to_le_bytes().to_vec();
            b.extend_from_slice(&1048576u32.to_le_bytes());
            b
        };
        let mut dist = b"0100".to_vec();
        dist.extend_from_slice(&[distortion_flag, 1, 0, 0]);
        dist.extend(rational(21.6));
        dist.extend_from_slice(&4u32.to_le_bytes());
        for v in [0.018975, -0.012295, 0.041512, 0.0] {
            dist.extend(rational(v));
        }
        dist.resize(84, 0);
        let mut vig = b"0100".to_vec();
        vig.extend_from_slice(&[1, 1, 0, 0]);
        vig.extend(rational(21.6));
        vig.extend_from_slice(&8u32.to_le_bytes());
        for v in [0.793414, 0.0, -0.637074, 0.0, 0.117527, 0.0, 0.431501, 0.0] {
            vig.extend(rational(v));
        }
        vig.resize(0x60, 0);
        vig.extend_from_slice(&541u32.to_le_bytes());
        vig.extend_from_slice(&1024u32.to_le_bytes());
        vig.resize(116, 0);
        let mut tiff = b"II*\0".to_vec();
        tiff.extend_from_slice(&8u32.to_le_bytes());
        tiff.extend_from_slice(&2u16.to_le_bytes());
        let data_start = 8 + 2 + 2 * 12 + 4;
        for (tag, data, offset) in [
            (5u16, &dist, data_start),
            (6u16, &vig, data_start + dist.len()),
        ] {
            tiff.extend_from_slice(&tag.to_le_bytes());
            tiff.extend_from_slice(&7u16.to_le_bytes());
            tiff.extend_from_slice(&(data.len() as u32).to_le_bytes());
            tiff.extend_from_slice(&(offset as u32).to_le_bytes());
        }
        tiff.extend_from_slice(&0u32.to_le_bytes());
        tiff.extend(&dist);
        tiff.extend(&vig);
        let mut blob = b"Nikon\0\x01\x01\0\0".to_vec();
        blob.extend(tiff);
        blob
    }

    #[test]
    fn nikon_data_matches_the_dng_converter_opcodes() {
        let info = parse_nikon_nef_info(&nikon_blob(3)).unwrap();
        assert_eq!(info.distortion_flag, 3);
        assert!((info.vignetting_strength - 541.0 / 1024.0).abs() < 1e-12);
        let data = nikon_curve(&info).unwrap();
        // Adobe DNG Converter wrote FixVignetteRadial k = 0.227971,
        // 0.062092, −0.336579, 0.419177 for this file.
        let k = [0.227971, 0.062092, -0.336579, 0.419177];
        for &(r, brightness) in data.vignetting.as_ref().unwrap() {
            let r2 = r * r;
            let gain = 1.0 + r2 * (k[0] + r2 * (k[1] + r2 * (k[2] + r2 * k[3])));
            assert!((brightness * gain - 1.0).abs() < 1e-4, "r {r}");
        }
        // Its WarpRectilinear (green): src / r = kr0 + kr1 r² + kr2 r⁴ + kr3 r⁶,
        // equal to the stored model up to the framing scale.
        let kr = [0.939447, 0.035228, 0.007943, 0.002331];
        let adobe = |r: f64| kr[0] + r * r * (kr[1] + r * r * (kr[2] + r * r * kr[3]));
        for &(r, k) in &data.distortion {
            assert!((k - adobe(r) / adobe(1.0)).abs() < 1e-3, "r {r}");
        }
        // Corrections marked optional by the lens apply as well.
        let optional = parse_nikon_nef_info(&nikon_blob(1)).unwrap();
        assert_eq!(nikon_curve(&optional), Some(data));
        // DSLR blocks (flag 0 or 2) are not used.
        for flag in [0, 2] {
            assert!(nikon_curve(&parse_nikon_nef_info(&nikon_blob(flag)).unwrap()).is_none());
        }
    }

    #[test]
    fn nikon_compact_tables_follow_the_dng_converter() {
        // A COOLPIX P1000 file's table (percent at radii 0, 1/16 … 1).
        let values = [
            0.0, 0.00788, 0.03152, 0.07085, 0.12583, 0.19632, 0.28222, 0.38334, 0.4995, 0.63046,
            0.77598, 0.93576, 1.10948, 1.29723, 1.49656, 1.71093, 1.93576,
        ];
        let mut table = b"0100".to_vec();
        table.extend_from_slice(&[3, 3, 17, 1]);
        for v in values {
            table.extend_from_slice(&((v * 100000.0f64).round() as i32).to_le_bytes());
            table.extend_from_slice(&100000u32.to_le_bytes());
        }
        let mut tiff = b"II*\0".to_vec();
        tiff.extend_from_slice(&8u32.to_le_bytes());
        tiff.extend_from_slice(&1u16.to_le_bytes());
        tiff.extend_from_slice(&2u16.to_le_bytes());
        tiff.extend_from_slice(&7u16.to_le_bytes());
        tiff.extend_from_slice(&(table.len() as u32).to_le_bytes());
        tiff.extend_from_slice(&26u32.to_le_bytes());
        tiff.extend_from_slice(&0u32.to_le_bytes());
        tiff.extend(&table);
        let mut blob = b"Nikon\0\x01\x01\0\0".to_vec();
        blob.extend(tiff);
        assert!(parse_nikon_nef_info(&blob).is_none());
        let parsed = parse_nikon_compact_distortion(&blob).unwrap();
        let data = nikon_compact_curve(&parsed).unwrap();
        // Adobe's WarpRectilinear (green) for that file.
        let kr = [0.980988, 0.019191, 1.5e-05, -7.4e-05];
        let adobe = |r: f64| kr[0] + r * r * (kr[1] + r * r * (kr[2] + r * r * kr[3]));
        for &(r, k) in &data.distortion {
            assert!((k - adobe(r) / adobe(1.0)).abs() < 5e-4, "r {r}");
        }
    }

    #[test]
    fn canon_mirrorless_bodies_are_recognised() {
        for model in [
            "Canon EOS R",
            "EOS RP",
            "Canon EOS R5",
            "Canon EOS R6m2",
            "EOS R50 V",
            "Canon EOS R100",
        ] {
            assert!(is_canon_eos_r("Canon", model), "{model}");
        }
        for model in ["Canon EOS Rebel T6", "Canon EOS 5D Mark IV", "Canon EOS M6"] {
            assert!(!is_canon_eos_r("Canon", model), "{model}");
        }
        assert!(!is_canon_eos_r("Nikon", "EOS R5"));
    }

    #[test]
    fn panasonic_framing_samples_the_sensor_corner_at_the_zoomed_corner() {
        let data = panasonic_curve(&FZ330, Some(2500.0)).unwrap();
        // Ru(Rd = 1) = 1 + a + b + c with s = 1.
        let expected = 1.0 + (8820.0 - 2282.0 - 663.0) / 32768.0;
        assert!((data.zoom - expected).abs() < 1e-6, "zoom {}", data.zoom);
        // The curve the warp takes maps corner to corner.
        let last = data.distortion.last().unwrap();
        assert!((last.0 - 1.0).abs() < 1e-12 && (last.1 - 1.0).abs() < 1e-9);
        // Off when the flag nibble is not 1.
        let mut off = FZ330;
        off[7] = 0;
        assert!(panasonic_curve(&off, Some(2500.0)).is_none());
    }

    #[test]
    fn panasonic_radius_follows_distortion_n_for_cropped_aspect_frames() {
        // A 1:1 frame of a 4000 × 3000 sensor reaches only 2115.6 / 2500 of
        // the model's radius: less correction and less zoom.
        let full = panasonic_curve(&FZ330, Some(2500.0)).unwrap();
        let square = panasonic_curve(&FZ330, Some(2115.6)).unwrap();
        assert!(square.zoom < full.zoom && square.zoom > 1.0);
    }

    // Panasonic DMC-GX80 lateral CA data (RW2 tag 0x011b) of a raw.pixls.us
    // frame: knots 963, 1926, 2407, 2888 px; red −838 … −1077, blue −1011 …
    // −1307 (both images larger than green's).
    const GX80_CA: [i16; 32] = [
        -2952, -28394, 48, -16128, 2407, 832, 7168, 26559, -838, 2, -1011, 2888, -213, -513, 256,
        24608, 1926, 963, 40, 1792, -847, 320, 22656, -600, -16442, -600, -1077, -501, -16384,
        -1307, -10503, -6481,
    ];

    #[test]
    fn panasonic_ca_shifts_are_read_at_the_knots() {
        // 4592 × 3448 frame.
        let hd = 0.5 * 4592f64.hypot(3448.0);
        let curve = panasonic_chromatic(&GX80_CA, hd).unwrap();
        assert_eq!(curve.len(), FORMULA_KNOTS);
        // Sample 21 (r = 21/32 of the half-diagonal, 1884 px) lies between
        // the first two knots: red shifted by −213 + (−600 + 213) · (1884 −
        // 963) / 963 in 1/1200 px; its scale relative to green is sampled
        // that much further out. Blue at sample 26 (2333 px) between −1011
        // at 1926 and −847 at 2407.
        let check = |i: usize, pick: fn(&(f64, f64, f64)) -> f64, knots: [f64; 4]| {
            let r = i as f64 / 32.0 * hd;
            let shift = knots[2] + (knots[3] - knots[2]) * (r - knots[0]) / (knots[1] - knots[0]);
            assert!((pick(&curve[i - 1]) + shift / 1200.0 / r).abs() < 1e-12);
        };
        check(21, |p| p.1, [963.0, 1926.0, -213.0, -600.0]);
        check(26, |p| p.2, [1926.0, 2407.0, -1011.0, -847.0]);
        // Inside the first knot the shift grows linearly from the centre: a
        // constant scale.
        assert!((curve[0].1 - 213.0 / 1200.0 / 963.0).abs() < 1e-9);
        let params = to_params(&EmbeddedLensData {
            maker: "Panasonic",
            distortion: vec![(0.5, 1.0), (1.0, 1.0)],
            vignetting: None,
            zoom: 1.0,
            chromatic: Some(curve),
        })
        .unwrap();
        assert!(!params.is_identity());
        assert!(params.clone().without_tca().is_identity());
    }

    #[test]
    fn implausible_panasonic_ca_data_is_ignored() {
        let hd = 0.5 * 4592f64.hypot(3448.0);
        // A frame binned to half size (LX7) reaches half the knots' radius.
        assert!(panasonic_chromatic(&GX80_CA, hd / 2.0).is_none());
        // Bodies without the data write zeros.
        assert!(panasonic_chromatic(&[0; 32], hd).is_none());
        let mut wild = GX80_CA;
        wild[12] = i16::MIN;
        assert!(panasonic_chromatic(&wild, hd).is_none());
    }

    /// A minimal RW2 header: IFD0 with the sensor borders, DistortionInfo
    /// and the CA data.
    fn rw2_header(distortion: &[i16; 16], ca: &[i16; 32]) -> Vec<u8> {
        let mut entries: Vec<(u16, u16, u32, Vec<u8>)> = vec![
            (0x0004, 3, 1, 8u32.to_le_bytes().to_vec()),
            (0x0005, 3, 1, 8u32.to_le_bytes().to_vec()),
            (0x0006, 3, 1, 3456u32.to_le_bytes().to_vec()),
            (0x0007, 3, 1, 4600u32.to_le_bytes().to_vec()),
        ];
        let words = |v: &[i16]| v.iter().flat_map(|w| w.to_le_bytes()).collect::<Vec<u8>>();
        entries.push((0x0119, 7, 32, words(distortion)));
        entries.push((0x011b, 7, 64, words(ca)));
        let mut out = b"IIU\0".to_vec();
        out.extend(8u32.to_le_bytes());
        out.extend((entries.len() as u16).to_le_bytes());
        let mut data_at = 8 + 2 + 12 * entries.len() + 4;
        let mut data: Vec<u8> = Vec::new();
        for (tag, typ, count, value) in &entries {
            out.extend(tag.to_le_bytes());
            out.extend(typ.to_le_bytes());
            out.extend(count.to_le_bytes());
            if value.len() <= 4 {
                let mut inline = value.clone();
                inline.resize(4, 0);
                out.extend(inline);
            } else {
                out.extend((data_at as u32).to_le_bytes());
                data.extend(value);
                data_at += value.len();
            }
        }
        out.extend(0u32.to_le_bytes());
        out.extend(data);
        out
    }

    #[test]
    fn offline_facts_from_before_panasonic_ca_do_not_answer_for_newer_recipes() {
        use crate::source_facts::{SourceFacts, VERSION, forget, register};
        let v1 = serde_json::json!({CA_VERSION_KEY: 1});
        let v0 = serde_json::json!({});
        let source = Path::new("/offline/volume/Job/Capture/P1000123.RW2");
        let path = source.to_string_lossy();
        assert!(
            recipe_params_known(source, &v1),
            "online: read from the file"
        );
        register(
            &path,
            SourceFacts {
                version: 2,
                ..Default::default()
            },
        );
        assert!(!recipe_params_known(source, &v1));
        assert!(recipe_params_known(source, &v0));
        register(
            &path,
            SourceFacts {
                version: VERSION,
                ..Default::default()
            },
        );
        assert!(recipe_params_known(source, &v1));
        forget(&path);
        // Other makers' facts had their data before.
        let other = Path::new("/offline/volume/Job/Capture/DSCF0001.RAF");
        register(&other.to_string_lossy(), SourceFacts::default());
        assert!(recipe_params_known(other, &v1));
        forget(&other.to_string_lossy());
    }

    #[test]
    fn recipes_without_the_camera_ca_version_keep_their_parameters() {
        let temp = tempfile::tempdir().unwrap();
        let with_distortion = temp.path().join("a.RW2");
        std::fs::write(&with_distortion, rw2_header(&FZ330, &GX80_CA)).unwrap();
        let old = params_for_recipe(&with_distortion, &serde_json::json!({})).unwrap();
        let new = params_for_recipe(
            &with_distortion,
            &serde_json::json!({CA_VERSION_KEY: CA_VERSION}),
        )
        .unwrap();
        assert_eq!(old, new.clone().without_tca());
        assert_ne!(old, new);
        assert_eq!(params_for(&with_distortion), Some(new));
        // CA data without distortion data (TZ81): camera mode only with
        // version 1.
        let ca_only = temp.path().join("b.RW2");
        std::fs::write(&ca_only, rw2_header(&[0; 16], &GX80_CA)).unwrap();
        assert!(params_for_recipe(&ca_only, &serde_json::json!({})).is_none());
        let params = params_for(&ca_only).unwrap();
        assert!(params.clone().without_tca().is_identity());
        // A never-edited recipe gets camera mode with the current version.
        let mut never = serde_json::Value::Null;
        apply_default(&mut never, &ca_only);
        assert_eq!(never["lensCorrectionMode"], CAMERA_MODE);
        assert_eq!(never[CA_VERSION_KEY], CA_VERSION);
        assert_eq!(ca_version_of(&never), CA_VERSION);
    }

    #[test]
    fn olympus_none_is_no_correction_and_zoom_solves_the_corner() {
        assert!(olympus_curve(&[0.0, 0.0, 0.0, 1.0], 1.0).is_none());
        let data = olympus_curve(&[-0.12, 0.05, -0.01, 0.95], 1.0).unwrap();
        let k = |r: f64| {
            let rs2 = (r * 0.95f64).powi(2);
            0.95 * (1.0 + rs2 * (-0.12 + rs2 * (0.05 - 0.01 * rs2)))
        };
        assert!((data.zoom * k(data.zoom) - 1.0).abs() < 1e-6);
    }

    // Fujifilm X-H2S with XF8-16mmF2.8 R LM WR at 15.2 mm (values as stored).
    const FUJI_DISTORTION_VALUES: [f64; 19] = [
        416.5555556,
        0.3535211268,
        0.5,
        0.6126760563,
        0.7070422535,
        0.7908450704,
        0.8661971831,
        0.9352112676,
        1.0,
        1.06056338,
        0.7516937256,
        1.209457397,
        1.580474854,
        1.912963867,
        2.255081177,
        2.635742188,
        3.066207886,
        3.583404541,
        3.583404541,
    ];
    const FUJI_VIGNETTING_VALUES: [f64; 19] = [
        416.5555556,
        0.3535211268,
        0.5,
        0.6126760563,
        0.7070422535,
        0.7908450704,
        0.8661971831,
        0.9352112676,
        1.0,
        1.06056338,
        87.07470703,
        78.82958984,
        72.16308594,
        66.66650391,
        62.36279297,
        58.44726562,
        55.30615234,
        52.55322266,
        52.55322266,
    ];
    // Sony ILCE-7M4 with FE 24-70mm F2.8 GM II at 58 mm.
    const SONY_DISTORTION_VALUES: [f64; 17] = [
        16.0, 2.0, 2.0, 6.0, 14.0, 27.0, 43.0, 64.0, 88.0, 117.0, 150.0, 188.0, 231.0, 279.0,
        331.0, 387.0, 447.0,
    ];

    fn ptlens(r: f64, [a, b, c]: [f64; 3]) -> f64 {
        a * r * r * r + b * r * r + c * r + 1.0 - a - b - c
    }

    #[test]
    fn camera_lateral_ca_becomes_the_warp_tca() {
        // X-T3 with XF 35mm F2 (ChromaticAberrationParams, nine knots).
        let knots = &FUJI_DISTORTION_VALUES[1..10];
        let red = [
            -0.00125, -0.00125, -0.00128, -0.00128, -0.00122, -0.00122, -0.00116, -0.00116,
            -0.00116,
        ];
        let blue = [
            0.00156, 0.00156, 0.00156, 0.00156, 0.00153, 0.0015, 0.0015, 0.00153, 0.00153,
        ];
        let chromatic: Vec<f64> = std::iter::once(416.0)
            .chain(knots.iter().copied())
            .chain(red)
            .chain(blue)
            .collect();
        let data = fuji_curves_with_ca(&FUJI_DISTORTION_VALUES, None, Some(&chromatic)).unwrap();
        let params = serde_json::to_value(to_params(&data).unwrap()).unwrap();
        let at = |key: &str, r2: f64| {
            params[key].as_f64().unwrap() + params[&format!("{key}2")].as_f64().unwrap() * r2
        };
        for r in [0.5f64, 0.8, 1.0] {
            assert!(
                (at("tca_vr", r * r) - (1.0 - 0.0012)).abs() < 1e-4,
                "red at {r}"
            );
            assert!(
                (at("tca_vb", r * r) - (1.0 + 0.00153)).abs() < 1e-4,
                "blue at {r}"
            );
        }
        // Other knots, or implausible values, carry no CA.
        let mut other = chromatic.clone();
        other[2] += 0.01;
        assert!(
            fuji_curves_with_ca(&FUJI_DISTORTION_VALUES, None, Some(&other))
                .unwrap()
                .chromatic
                .is_none()
        );
        let mut wild = chromatic;
        wild[12] = 0.5;
        assert!(
            fuji_curves_with_ca(&FUJI_DISTORTION_VALUES, None, Some(&wild))
                .unwrap()
                .chromatic
                .is_none()
        );
        // Without CA data the parameters stay as before (no TCA terms).
        let plain = serde_json::to_value(
            to_params(&fuji_curves(&FUJI_DISTORTION_VALUES, None).unwrap()).unwrap(),
        )
        .unwrap();
        assert_eq!(plain["tca_vr"], 1.0);
        assert!(plain.get("tca_vr2").is_none());

        // Sony ILCE-9 with FE 55mm (SR2 0x7980): [2n, n red, n blue] / 2^21.
        let sony: Vec<f64> = [
            32.0, -768.0, -640.0, -512.0, -384.0, -384.0, -384.0, -384.0, -512.0, -640.0, -768.0,
            -768.0, -768.0, -768.0, -896.0, -1152.0, -1408.0, 384.0, 384.0, 384.0, 512.0, 512.0,
            512.0, 640.0, 640.0, 640.0, 640.0, 768.0, 896.0, 1024.0, 1152.0, 1152.0, 1280.0,
        ]
        .to_vec();
        let curve = sony_chromatic(&sony).unwrap();
        assert_eq!(curve.len(), 16);
        assert!((curve[0].0 - 1.0 / 32.0).abs() < 1e-12);
        assert!((curve[15].1 + 1408.0 / 2097152.0).abs() < 1e-12);
        assert!((curve[15].2 - 1280.0 / 2097152.0).abs() < 1e-12);
        // APS-C bodies: eleven knots, zero padded.
        let mut aps_c = vec![22.0];
        aps_c.extend(std::iter::repeat_n(-128.0, 22));
        aps_c.extend(std::iter::repeat_n(0.0, 10));
        assert_eq!(sony_chromatic(&aps_c).unwrap().len(), 11);
        assert!(sony_chromatic(&[3.0, 1.0, 2.0, 3.0]).is_none());
    }

    #[test]
    fn fujifilm_curves_fit_the_profile_model_closely() {
        let data = fuji_curves(&FUJI_DISTORTION_VALUES, Some(&FUJI_VIGNETTING_VALUES)).unwrap();
        let coefficients = fit_distortion(&data.distortion).unwrap();
        let corner = interpolate(&data.distortion, 1.0);
        for i in 0..=20 {
            let r = i as f64 / 20.0;
            let expected = interpolate(&data.distortion, r) / corner;
            // Displacement within 0.05 % of the half diagonal: under 2 px at 3,750 px.
            let error = r * (ptlens(r, coefficients) - expected).abs();
            assert!(error < 5e-4, "r = {r}: {error}");
        }
        // Corners stay corners; the middle is pulled in (barrel correction).
        assert!((ptlens(1.0, coefficients) - 1.0).abs() < 1e-12);
        assert!(ptlens(0.5, coefficients) < 0.99);
        let [k1, k2, k3] = fit_vignetting(data.vignetting.as_deref().unwrap()).unwrap();
        let brightness = |r: f64| 1.0 + k1 * r * r + k2 * r.powi(4) + k3 * r.powi(6);
        assert!((brightness(1.0) - 0.5255).abs() < 0.02);
        assert!((brightness(0.5) - 0.788).abs() < 0.03);
        assert!((brightness(0.0) - 1.0).abs() < 0.03);
    }

    #[test]
    fn sony_curve_uses_half_step_knots_and_sixteen_thousandths() {
        let data = sony_curve(&SONY_DISTORTION_VALUES, None).unwrap();
        assert_eq!(data.distortion[0].0, 0.5 / 16.0);
        assert!((data.distortion[15].1 - (1.0 + 447.0 / 16384.0)).abs() < 1e-12);
        assert!(data.vignetting.is_none());
        let coefficients = fit_distortion(&data.distortion).unwrap();
        let corner = interpolate(&data.distortion, 1.0);
        for i in 0..=20 {
            let r = i as f64 / 20.0;
            let expected = interpolate(&data.distortion, r) / corner;
            assert!(
                r * (ptlens(r, coefficients) - expected).abs() < 5e-4,
                "r = {r}"
            );
        }
    }

    // Fujifilm GFX 50R with GF63mmF2.8 R WR: eight knots (values as stored).
    const GFX_KNOTS: [f64; 8] = [
        0.35356488823890686,
        0.5001828074455261,
        0.6124314665794373,
        0.7071297764778137,
        0.7904936075210571,
        0.8661791682243347,
        0.935283362865448,
        1.0,
    ];

    #[test]
    fn gfx_curves_have_eight_knots() {
        let distortion: Vec<f64> = [645.0]
            .into_iter()
            .chain(GFX_KNOTS)
            .chain([
                -0.374, -0.747, -1.137, -1.519, -1.911, -2.303, -2.695, -3.098,
            ])
            .collect();
        let vignetting: Vec<f64> = [645.0]
            .into_iter()
            .chain(GFX_KNOTS)
            .chain([97.34, 93.22, 88.73, 84.14, 79.56, 75.02, 70.72, 66.21])
            .collect();
        let data = fuji_curves(&distortion, Some(&vignetting)).expect("GFX curves");
        assert_eq!(data.distortion.len(), 8);
        let (r, k) = data.distortion[7];
        assert!(r == 1.0 && (k - 0.96902).abs() < 1e-9);
        let vignetting_curve = data.vignetting.expect("vignetting");
        assert_eq!(vignetting_curve.len(), 9);
        assert_eq!(vignetting_curve[0], (0.0, 1.0));
        assert!((vignetting_curve[8].1 - 0.6621).abs() < 1e-9);
        assert!(to_params(&fuji_curves(&distortion, None).unwrap()).is_some());
        // Vignetting stored for other knots is not paired with this distortion.
        let mut other = vignetting.clone();
        other[1] = 0.3;
        assert!(
            fuji_curves(&distortion, Some(&other))
                .unwrap()
                .vignetting
                .is_none()
        );
    }

    #[test]
    fn implausible_data_is_ignored() {
        // Vignetting gain 1 + v / 2¹⁴: brightness 1 at the centre, ½ where
        // the camera stores 16384.
        let vignetting = [3.0, 0.0, 8192.0, 16384.0];
        let data = sony_curve(&SONY_DISTORTION_VALUES, Some(&vignetting)).unwrap();
        let curve = data.vignetting.unwrap();
        assert_eq!(curve.len(), 3);
        assert!((curve[0].1 - 1.0).abs() < 1e-9 && (curve[2].1 - 0.5).abs() < 1e-9);
        assert!((curve[1].0 - 0.5).abs() < 1e-9 && (curve[1].1 - 2.0 / 3.0).abs() < 1e-9);
        assert!(sony_curve(&[0.0], None).is_none());
        assert!(sony_curve(&[4.0, 1.0, 2.0], None).is_none());
        let mut broken = FUJI_DISTORTION_VALUES;
        broken[12] = 400.0;
        assert!(fuji_curves(&broken, None).is_none());
        assert!(fuji_curves(&FUJI_DISTORTION_VALUES[..10], None).is_none());
    }

    #[test]
    fn the_default_only_touches_never_edited_recipes_of_files_with_camera_data() {
        let temp = tempfile::tempdir().unwrap();
        let jpeg = temp.path().join("photo.jpg");
        std::fs::write(&jpeg, b"not a raw").unwrap();
        let mut never = serde_json::Value::Null;
        apply_default(&mut never, &jpeg);
        // No camera data: only the current render version, as a new edit.
        assert_eq!(
            never,
            serde_json::json!({"renderVersion": crate::image_processing::RENDER_VERSION})
        );
        let mut saved = serde_json::json!({"lensCorrectionMode": "manual", "exposure": 1});
        let before = saved.clone();
        apply_default(&mut saved, &temp.path().join("missing.raf"));
        assert_eq!(saved, before);
        // A never-edited RAW starts with the edge-fringe pass, a saved one not.
        let mut never_raw = serde_json::Value::Null;
        apply_default(&mut never_raw, &temp.path().join("missing.raf"));
        assert_eq!(
            never_raw[crate::edge_fringe::VERSION_KEY],
            crate::edge_fringe::VERSION
        );
        // And with the automatic lateral CA default.
        assert_eq!(
            never_raw[crate::lateral_ca::VERSION_KEY],
            crate::lateral_ca::VERSION
        );
        assert_eq!(
            never_raw[crate::image_processing::RENDER_VERSION_KEY],
            crate::image_processing::RENDER_VERSION
        );
        // A saved partial recipe without a lens mode (a batch preset) keeps
        // its render version.
        let mut partial = serde_json::json!({"clarity": 30});
        apply_default(&mut partial, &temp.path().join("missing.raf"));
        assert!(
            partial
                .get(crate::image_processing::RENDER_VERSION_KEY)
                .is_none()
        );
        assert!(read(&temp.path().join("missing.raf")).is_none());
        let fake = temp.path().join("fake.raf");
        std::fs::write(&fake, vec![0u8; 256]).unwrap();
        assert!(read(&fake).is_none(), "garbage RAF headers are ignored");
    }

    #[test]
    fn adobe_private_blocks_skip_the_padding_of_odd_blocks() {
        let mut data = b"Adobe\0".to_vec();
        data.extend_from_slice(b"MakN");
        data.extend_from_slice(&3u32.to_be_bytes());
        data.extend_from_slice(&[1, 2, 3, 0]);
        data.extend_from_slice(b"SR2 ");
        data.extend_from_slice(&2u32.to_be_bytes());
        data.extend_from_slice(&[4, 5]);
        let blocks = adobe_private_blocks(&data);
        assert_eq!(
            blocks,
            vec![(*b"MakN", &[1u8, 2, 3][..]), (*b"SR2 ", &[4u8, 5][..])]
        );
    }

    #[test]
    fn lensfun_defaults_only_for_measured_compacts() {
        assert!(super::is_fixed_lens_compact("Canon", "PowerShot S120"));
        assert!(super::is_fixed_lens_compact(
            "Canon",
            "Canon PowerShot G9 X Mark II"
        ));
        assert!(!super::is_fixed_lens_compact("Canon", "PowerShot G11"));
        assert!(!super::is_fixed_lens_compact("Nikon", "Coolpix A"));
    }
}

#[cfg(test)]
mod canon_ctmd_tests {
    #[test]
    fn reads_the_brightness_table_after_other_records() {
        // EOS R6 Mark III + RF 50mm F1.4 L VCM at f/7.1, CTMD record 14.
        let brightness: [u16; 17] = [
            8191, 8079, 7767, 7550, 7302, 7028, 6734, 6428, 6275, 6124, 5975, 5889, 5834, 5766,
            5692, 5505, 5095,
        ];
        let radii: [u16; 17] = [
            0, 757, 1514, 1893, 2271, 2650, 3028, 3407, 3596, 3786, 3975, 4088, 4164, 4259, 4353,
            4448, 4543,
        ];
        let mut words = vec![17u16, 17, 17];
        for block in [&brightness[..], &radii[..]] {
            words.extend_from_slice(block);
            words.extend(std::iter::repeat_n(0, 23 - block.len()));
        }
        words.extend(std::iter::repeat_n(16384, 23 * 5));
        let payload: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        let mut sample = Vec::new();
        // A preceding record (type 1, 24 bytes) is skipped.
        sample.extend_from_slice(&24u32.to_le_bytes());
        sample.extend_from_slice(&1u16.to_le_bytes());
        sample.extend_from_slice(&[0; 18]);
        sample.extend_from_slice(&((12 + payload.len()) as u32).to_le_bytes());
        sample.extend_from_slice(&14u16.to_le_bytes());
        sample.extend_from_slice(&[0; 6]);
        sample.extend_from_slice(&payload);
        let (r, b) = super::canon_vignetting_table(&sample).expect("table");
        assert_eq!(r, radii);
        assert_eq!(b, brightness);
        // Without record 14: nothing.
        assert!(super::canon_vignetting_table(&sample[..24]).is_none());
    }
}
