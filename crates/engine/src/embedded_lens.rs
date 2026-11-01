//! Lens corrections the camera stores in its RAW files (Fujifilm RAF, Sony
//! ARW, Panasonic RW2, Olympus / OM System ORF, Nikon Z NEF, newer Canon CR3,
//! Samsung SRW since `cameraGeometryVersion` 4), fitted to the profile model
//! the geometry warp already applies. Camera Raw and Capture One apply these
//! built-in corrections to every photo of such a body.
//!
//! Interpretation, calibrated against the cameras' own corrected JPEGs
//! (notes: BENCHMARKS.md): radii are normalized by half the image diagonal; a
//! corrected pixel at radius r samples the uncorrected image at
//! r · k(r) / k(1), so the corners stay corners. Fujifilm: nine knots, k =
//! 1 + value / 100; Sony: n knots, k = 1 + value / 16384, at (i + ½) / n
//! before `cameraGeometryVersion` 1 and as Camera Raw reads them since
//! (`sony_lens`).
//! Fujifilm vignetting: brightness in percent of the centre at the same knots
//! (not yet checked against a flat field). Lateral chromatic aberration:
//! Fujifilm, Sony and (since `cameraCaVersion` 1) Panasonic.
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use rawler::formats::tiff::IFD;
use rawler::rawsource::RawSource;

use crate::lens_correction::{LensDistortionParams, RadialTable};

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
    /// The zoom is the camera's framing itself (`LensDistortionParams::exact_zoom`).
    pub exact_zoom: bool,
    /// Lateral chromatic aberration: (normalized radius, red scale − 1, blue
    /// scale − 1) relative to green, radius ascending.
    pub chromatic: Option<Vec<(f64, f64, f64)>>,
    /// The model is the warp's polynomial itself (k1, k2, k3 of
    /// `LensDistortionParams::polynomial`), which `to_params` takes as it is
    /// instead of fitting `distortion` (Samsung's model, `samsung_curve`).
    pub polynomial: Option<[f64; 3]>,
    /// `distortion` is the warp's radial table itself, which `to_params`
    /// takes as it is (`Tabulated`).
    pub tabulated: Option<Tabulated>,
    /// The distortion's centre, offset from the developed frame's centre
    /// relative to its half diagonal (x right, y down; Nikon Z's per-shot
    /// optical centre, `nikon_centre`): `LensDistortionParams::with_centre`.
    pub centre: Option<[f64; 2]>,
}

/// The kinds of the warp's radial tables `EmbeddedLensData::distortion` can
/// be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tabulated {
    /// At uniform knots from the centre, cubic between them
    /// (`LensDistortionParams::radial_table`; Sony from
    /// `cameraGeometryVersion` 6).
    Uniform,
    /// At the knots' own radii, linear in r² between them
    /// (`LensDistortionParams::knot_table`; Fujifilm from
    /// `cameraGeometryVersion` 7).
    Knots,
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
const SAMSUNG_KEY: u16 = 0xa020;
const SAMSUNG_DISTORTION: u16 = 0xa050;
const SAMSUNG_APPLIED_DISTORTION: u16 = 0xa057;
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
        exact_zoom: false,
        polynomial: None,
        tabulated: None,
        centre: None,
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

/// Fujifilm's distortion k(r) as camera geometry version 5 reads its knots:
/// from k = 1 at the centre, linear in r² between knots (the nine-knot
/// bodies store them at √(i / 8)), along the last segment beyond the last.
/// Version 0 kept k at the first knot's value inside it, which on the
/// nine-knot bodies (first knot 0.354) bent the centre third: X-T5 + XF18
/// against its camera JPEG 1.5–2.2 px shape error, 0.50–0.54 px read this
/// way (notes: parity-gaps/G71).
fn fuji_k(curve: &[(f64, f64)], r: f64) -> f64 {
    let last = curve[curve.len() - 1];
    if r >= last.0 {
        return interpolate(curve, r);
    }
    let i = curve.partition_point(|&(x, _)| x <= r);
    let (a, b) = if i == 0 {
        ((0.0, 1.0), curve[0])
    } else {
        (curve[i - 1], curve[i])
    };
    a.1 + (b.1 - a.1) * (r * r - a.0 * a.0) / (b.0 * b.0 - a.0 * a.0)
}

/// Fujifilm's distortion framed as the cameras' JPEGs show it (camera
/// geometry version 5): an output radius u samples the sensor at A u k(u)
/// (`fuji_k`) with A = 1 / max k on the border of the frame (width over
/// height `aspect`), the largest frame that samples nothing beyond its
/// edges: corners on the corners for pincushion data, the shorter sides'
/// midpoints on the edges for barrel data. Fitted against the camera's own
/// JPEG on the raw.pixls.us RAFs, A came out within 0.0013 of that on 34
/// bodies whose data correct anything (X-E4 + XF27 1.00354 vs 1.00352,
/// X-T5 + XF18 1.0212 vs 1.0213, GFX 50S + GF63 1.01059 vs 1.01059), the
/// strongly pincushioned HS20EXR–HS33EXR and X-S1 zoom 0.0013–0.0023
/// further; version 0 framed barrel data the
/// same way plus the warp's 0.2 % margin (X-E4: the 0.18 % Camera Raw
/// showed more). Some bodies (XF1, XQ1, XQ2, F770EXR, F900EXR, HS50EXR,
/// S1, X-A3, X-A5, X-A10, X-T100) frame barrel data at A = 1 instead; their
/// data do not tell them apart (the X-A1 and X-A10 store the same table and
/// frame differently; notes: parity-gaps/G71). Vignetting and CA keep
/// version 0's reading.
pub fn fuji_framed(data: EmbeddedLensData, aspect: f64) -> Option<EmbeddedLensData> {
    let curve = data.distortion.clone();
    let peak = fuji_border_peak(&curve, aspect);
    let (distortion, zoom) = framed_curve(|u| fuji_k(&curve, u) / peak)?;
    plausible(&distortion).then_some(EmbeddedLensData {
        distortion,
        zoom,
        exact_zoom: true,
        ..data
    })
}

/// The largest k (`fuji_k`) on the border of a frame of width over height
/// `aspect`, whose radii run from the shorter side's midpoint to the
/// corner; k is monotone between knots.
fn fuji_border_peak(curve: &[(f64, f64)], aspect: f64) -> f64 {
    let short = 1.0 / aspect.max(1.0 / aspect).hypot(1.0);
    curve
        .iter()
        .map(|&(r, _)| r)
        .filter(|r| (short..1.0).contains(r))
        .chain([short, 1.0])
        .map(|r| fuji_k(curve, r))
        .fold(f64::MIN, f64::max)
}

/// Fujifilm's distortion as camera geometry version 7 applies it: framed as
/// `fuji_framed` (A = 1 / max k on the frame's border), but warped through
/// the camera's knots themselves (`Tabulated::Knots`, linear in r² as
/// `fuji_k`) instead of a three-term PTLens fit of that curve, which on
/// wide lenses errs by up to ±3.3 px (X-S20 + XF16-80 at 16 mm; X-T4 +
/// XF16-55 at 16 mm ±1.8 px; notes: parity-gaps/G71). The warp's table
/// T(q) = zoom A k(zoom q): the knots at r_i / zoom, from the centre.
pub fn fuji_tabulated(data: EmbeddedLensData, aspect: f64) -> Option<EmbeddedLensData> {
    let curve = data.distortion.clone();
    let peak = fuji_border_peak(&curve, aspect);
    let zoom = framing_zoom(|u| fuji_k(&curve, u) / peak)?;
    let centre = (curve[0].0 > 0.0).then_some((0.0, 1.0));
    let distortion: Vec<(f64, f64)> = centre
        .into_iter()
        .chain(curve.iter().copied())
        .map(|(r, k)| (r / zoom, zoom * k / peak))
        .collect();
    plausible(&distortion).then_some(EmbeddedLensData {
        distortion,
        zoom,
        exact_zoom: true,
        tabulated: Some(Tabulated::Knots),
        centre: None,
        ..data
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
        exact_zoom: false,
        polynomial: None,
        tabulated: None,
        centre: None,
        chromatic: chromatic.and_then(sony_chromatic),
    })
}

/// The frame a Sony RAW's data are applied to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SonyFrame {
    /// Where its corner lies on the radius of the distortion data (geometry
    /// version 1: the active area's half-diagonal, `sony_sensor_frame`).
    pub reach: f64,
    /// Where it lies on the radius version 0 refers all data to (the
    /// developed 3:2 frame's; below 1 for aspect crops), which the
    /// vignetting and CA data keep.
    pub frame_reach: f64,
    /// Its width over its height.
    pub aspect: f64,
    /// An interchangeable-lens body (`SonyLensSettings::interchangeable_lens`).
    pub interchangeable_lens: bool,
}

impl SonyFrame {
    /// The full 3:2 frame.
    pub const FULL: SonyFrame = SonyFrame {
        reach: 1.0,
        frame_reach: 1.0,
        aspect: 1.5,
        interchangeable_lens: false,
    };
}

/// The frame of the Sony RAW in `source` as camera geometry version 1 reads
/// its data: radius 1 of the knots is the half-diagonal of the sensor's
/// active area, the default crop's margins included (RX100 to RX100 V,
/// ZV-1, RX10, RX10 IV and the HX95's 16:9 frame of its 4:3 sensor against
/// Camera Raw: 0.3–0.9 px median instead of 0.65–1.7 with the developed
/// frame's own half-diagonal; notes: parity-gaps/G71), the developed frame
/// (aspect crops included) is the output.
fn sony_sensor_frame(source: &RawSource) -> Option<SonyFrame> {
    let ([sensor_w, sensor_h], [w, h]) = crate::raw_processing::sensor_and_frame(source)?;
    let (w, h) = (w as f64, h as f64);
    let reach = w.hypot(h) / (sensor_w as f64).hypot(sensor_h as f64);
    (w > 0.0 && h > 0.0 && (0.5..=1.0).contains(&reach)).then_some(SonyFrame {
        reach,
        aspect: w / h,
        ..SonyFrame::FULL
    })
}

/// Sony's curves for `frame`, as camera geometry version `geometry` reads
/// them. Version 0: the distortion knots at (i + ½) / n, the corners kept on
/// the corners. Version 1: the mapping Camera Raw and the cameras' JPEGs
/// show (notes: parity-gaps/G71, 45 ARW files of 41 bodies with the
/// camera's correction on): the knots lie at i / (n − 1), first on the
/// centre and last on the corner of the sensor's active area
/// (`sony_sensor_frame`), and an output radius u samples the sensor at
/// A u k(u). Fixed-lens bodies frame with A = 1, so strong barrel data
/// (RX100, ZV-1) crop inside the sensor corners; interchangeable-lens bodies
/// with the largest frame that samples no point beyond the 3:2 frame's
/// edges, A = 1 / max k on its border (corners on the corners for pincushion
/// data), before an aspect crop. The vignetting and CA data keep version 0's
/// reading (not re-measured).
///
/// Version 6 (`SONY_TABLE_GEOMETRY`) warps through the knots themselves
/// (`RadialTable`, shape-preserving cubic between them) instead of a
/// three-term fit of their linear interpolation: that fit bulged past the
/// binding edge of the RX100 III / IV / V and ZV-1 frames, so the warp
/// zoomed in 0.084 % beyond the camera's framing (2.8 px at the corners;
/// Camera Raw's mapping reconstructed through our warp on 12 compacts:
/// A = 1.0000 ± 0.0001 at the framing the warp actually used), and it cannot
/// follow wavy tables (RX0). Against Camera Raw the cubic fits closer than
/// linear interpolation on 9 of those 12 files (largest error ZV-1
/// 1.04 → 0.50 px, RX100 VI 1.17 → 0.67; notes: parity-gaps/G71).
pub fn sony_lens(
    distortion: &[f64],
    vignetting: Option<&[f64]>,
    chromatic: Option<&[f64]>,
    frame: SonyFrame,
    geometry: u64,
) -> Option<EmbeddedLensData> {
    let data = reframed(
        sony_curves_with_ca(distortion, vignetting, chromatic)?,
        frame.frame_reach,
    );
    if geometry == 0 {
        return Some(data);
    }
    let last = (data.distortion.len() - 1) as f64;
    let curve: Vec<(f64, f64)> = data
        .distortion
        .iter()
        .enumerate()
        .map(|(i, &(_, k))| (i as f64 / last / frame.reach, k))
        .collect();
    let table = (geometry >= SONY_TABLE_GEOMETRY)
        .then(|| {
            let knots: Vec<f64> = curve.iter().map(|&(_, k)| k).collect();
            RadialTable::new(&knots, 1.0 / frame.reach)
        })
        .flatten();
    let k = |u: f64| match &table {
        Some(table) => table.at(u),
        None => interpolate(&curve, u),
    };
    // The largest k on a frame's border, whose radii run from the shorter
    // side's midpoint (`short`) to the corner; the curve is linear or, as a
    // table, monotone between its knots.
    let peak = |short: f64, corner: f64| {
        curve
            .iter()
            .map(|&(r, _)| r)
            .filter(|r| (short..corner).contains(r))
            .chain([short, corner])
            .map(k)
            .fold(f64::MIN, f64::max)
    };
    let short_of = |aspect: f64| 1.0 / aspect.max(1.0 / aspect).hypot(1.0);
    // The 3:2 frame's border in output radii (an aspect crop lies inside).
    let full_corner = 1.0 / frame.frame_reach;
    let full_peak = peak(short_of(1.5) * full_corner, full_corner);
    // Interchangeable-lens bodies: the 3:2 frame's border (the camera frames
    // it, then crops to the aspect ratio: A7C 4:3 and ZV-E10 16:9 frames 0.27
    // and 0.31 JPEG px median against the camera's JPEG, 2.4 and 1.0 with
    // their own border, 0.33 with the sensor's margins included; 24 ILC
    // files: 0.42 median).
    let scale = if frame.interchangeable_lens {
        1.0 / full_peak
    } else {
        1.0
    };
    // An aspect crop whose border that framing takes beyond the 3:2 frame's
    // reach: the camera corrects the whole sensor before cropping (HX95,
    // ZV-1 II and RX0 II 16:9: up to 2.4 % beyond the top edge), but the
    // warp only has the cropped frame's pixels and would zoom in instead, so
    // such a frame keeps version 0's reading (notes: parity-gaps/G71).
    let needed = scale * peak(short_of(frame.aspect), 1.0);
    if needed > (scale * full_peak).max(1.0) + 1e-3 {
        return Some(data);
    }
    if table.is_some() {
        // The warp's table T(q) = zoom A k(zoom q): the knots at r_i / zoom.
        let zoom = framing_zoom(|u| scale * k(u))?;
        let distortion = curve
            .iter()
            .map(|&(r, value)| (r / zoom, zoom * scale * value))
            .collect();
        return Some(EmbeddedLensData {
            distortion,
            zoom,
            exact_zoom: true,
            tabulated: Some(Tabulated::Uniform),
            centre: None,
            ..data
        });
    }
    let (distortion, zoom) = framed_curve(|u| scale * interpolate(&curve, u))?;
    Some(EmbeddedLensData {
        distortion,
        zoom,
        exact_zoom: true,
        ..data
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
    let zoom = framing_zoom(&k)?;
    let curve = (1..=FORMULA_KNOTS)
        .map(|i| {
            let u = i as f64 / FORMULA_KNOTS as f64;
            (u, zoom * k(zoom * u))
        })
        .collect();
    Some((curve, zoom))
}

/// The output radius whose sample is the sensor corner: `zoom k(zoom) = 1`
/// (`framed_curve`).
fn framing_zoom(k: impl Fn(f64) -> f64) -> Option<f64> {
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
    Some(zoom)
}

/// Panasonic's DistortionInfo (the model in ExifTool's PanasonicRaw notes
/// and Rigo's RW2 notes): the corrected radius is
/// `Ru = Rd + s (a Rd³ + b Rd⁵ + c Rd⁷)` with `s = 1 / (1 + v5 / 32768)`,
/// `a = v8`, `b = v4`, `c = v11` (each / 32768). Radii are in units of
/// DistortionN (v12) pixels: the half-diagonal of the camera's full frame
/// (TZ71 4000 × 3000: 2500, LX15 5472 × 3648: 3288), so frames stored in a
/// cropped aspect ratio (TZ71 1:1: 2992 × 2992) scale their radii by their
/// own half-diagonal over N. Off unless the flag nibble (v7) is 1.
///
/// Framing: before `cameraGeometryVersion` 5 the output corner shows
/// `Ru = reach` (N units) as a minimum zoom. `framed` reads the camera's own
/// framing: the corner shows `Ru = reach (1 + v5 / 32768)`, exactly. Bodies
/// with the newer layout (v3 ≠ 0; G9, GH5, S5, GX9 II …) store v5 < 0 and
/// zoom in by that much, the older ones v5 = 0. Fitted against the camera's
/// own JPEG (124 raw.pixls.us RW2 frames of 53 bodies), the corner's
/// `Ru / reach` came out 1 + v5 / 32768 − 0.0006 … − 0.0001 on the 15 bodies
/// with v5 < 0 (G9 28 mm v5 = −80: 0.99750 vs 0.99756; GH5S v5 = −359:
/// 0.98888 vs 0.98904), the same offset as 1 − 0.0009 … + 0.0005 on those
/// with v5 = 0 (notes: parity-gaps/G71).
pub fn panasonic_curve(
    v: &[i16; 16],
    half_diagonal: Option<f64>,
    framed: bool,
) -> Option<EmbeddedLensData> {
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
    // the sensor corner; the camera's framing is ru = reach (1 + v5 / 32768).
    let zoom = ru_corner / reach;
    let framing = if framed {
        zoom / (1.0 + v[5] as f64 / 32768.0)
    } else {
        zoom
    };
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
        zoom: framing,
        exact_zoom: framed,
        polynomial: None,
        tabulated: None,
        centre: None,
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
        exact_zoom: false,
        polynomial: None,
        tabulated: None,
        centre: None,
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
    /// DistortionInfo's framing scale (offset 0x40): the corrected frame
    /// shows the sensor at `u p(u) / (1 + framing)` (see `nikon_curve`).
    pub framing: Option<f64>,
    /// VignetteInfo coefficients as stored (offsets 0x14 … 0x44, every
    /// second one) and the strength factor at offset 0x60.
    pub vignetting: [f64; 4],
    pub vignetting_strength: f64,
    /// NikonNEFInfo tag 8 (header "0100", two signed rationals at 4 and
    /// 12): the shot's optical centre in pixels of the image as recorded,
    /// from the frame's centre, x right and y down on the sensor (moves
    /// with the in-body stabilisation; see `nikon_centre`).
    pub centre: Option<[f64; 2]>,
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
    let mut centre_info = None;
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
            8 => centre_info = data,
            _ => {}
        }
    }
    let centre = centre_info
        .filter(|c| c.len() >= 20 && c.starts_with(b"0100"))
        .and_then(|c| Some([srational(c, 4)?, srational(c, 12)?]))
        .filter(|c| c.iter().all(|v| v.is_finite() && v.abs() < 1000.0));
    let distortion_info = distortion_info.filter(|d| d.len() >= 0x34 && d.starts_with(b"0100"))?;
    let vignette_info = vignette_info.filter(|v| v.len() >= 0x68 && v.starts_with(b"0100"));
    let distortion = [0x14, 0x1c, 0x24, 0x2c].map(|at| srational(distortion_info, at));
    let framing = srational(distortion_info, 0x40).filter(|v| v.abs() < 0.3);
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
        framing,
        vignetting: match vignetting {
            Some([Some(a), Some(b), Some(c), Some(d)]) => [a, b, c, d],
            _ => [0.0; 4],
        },
        vignetting_strength: strength.unwrap_or(0.0),
        centre,
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
        exact_zoom: false,
        polynomial: None,
        tabulated: None,
        centre: None,
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
///
/// `framed` (camera geometry version 2) frames as the camera does: with
/// the radius relative to the corrected frame's half-diagonal, an output
/// pixel at `u` shows the sensor at `u p(u) / (1 + f)`, f being the framing
/// scale DistortionInfo stores (the camera's full-size JPEG of 57 Z-mount
/// files of 13 bodies, FX, DX, 1:1 and 16:9 frames, to 0.04 % of the
/// radius; Camera Raw frames as the JPEG; notes: parity-gaps/G71), as the
/// exact zoom (`exact_zoom`). Unframed (and for data without that value),
/// the corners stay on the sensor's corners.
///
/// `centre` (camera geometry version 8, only with the framing) places the
/// distortion and its framing zoom at the shot's optical centre
/// ([`nikon_centre`]).
pub fn nikon_curve(
    info: &NikonLensInfo,
    framed: bool,
    centre: Option<[f64; 2]>,
) -> Option<EmbeddedLensData> {
    let [d1, d2, d3, d4] = info.distortion;
    // The fourth term was zero in every file seen; its power is unknown.
    if !matches!(info.distortion_flag, 1 | 3) || d4 != 0.0 {
        return None;
    }
    let p = |r: f64| 1.0 + r * r * (d3 + r * (d2 + r * d1));
    let framing = info.framing.filter(|_| framed);
    let (distortion, zoom) = match framing {
        Some(f) => framed_curve(|u| p(u) / (1.0 + f))?,
        None => {
            let corner = p(1.0);
            let curve = (1..=FORMULA_KNOTS)
                .map(|i| {
                    let u = i as f64 / FORMULA_KNOTS as f64;
                    (u, p(u) / corner)
                })
                .collect::<Vec<_>>();
            (curve, 1.0)
        }
    };
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
        zoom,
        exact_zoom: framing.is_some(),
        polynomial: None,
        tabulated: None,
        centre: centre.filter(|_| framing.is_some()),
        chromatic: None,
    })
}

/// The camera geometry version from which Nikon Z data correct about the
/// shot's own optical centre (`nikon_centre`).
const NIKON_CENTRE_GEOMETRY: u64 = 8;

/// Nikon's recorded optical centre (`NikonLensInfo::centre`) as an offset
/// from the centre of the developed `width` × `height` frame relative to
/// its half diagonal, turned as the development turns the image (EXIF
/// `orientation`). The camera writes it in pixels of the image as recorded
/// (a Z 6 with the lens off-centre by the same amount stores 26.3, 22.5 and
/// 19.6 px in its L, M and S RAW sizes, ×1, ×0.857, ×0.750), x right and y
/// down on the sensor. Against the camera's full-size JPEG of 57 raw.pixls.us
/// Z-mount files a centre fitted freely per file follows it with slope
/// 0.99 (x) and 0.96 (y); with it the Z f + 35-150mm at 150 mm (−66 px)
/// matches its JPEG to 0.21 px median instead of 2.4, the Z 6 + 24-70mm f/4
/// (+26 px) to 0.30 instead of 1.27 (notes: parity-gaps/G71). None without
/// an offset.
pub fn nikon_centre(
    info: &NikonLensInfo,
    width: usize,
    height: usize,
    orientation: u16,
) -> Option<[f64; 2]> {
    let [x, y] = info.centre?;
    let half_diagonal = (width as f64).hypot(height as f64) / 2.0;
    if (x == 0.0 && y == 0.0) || half_diagonal < 1.0 {
        return None;
    }
    let [x, y] = match orientation {
        2 => [-x, y],
        3 => [-x, -y],
        4 => [x, -y],
        5 => [y, x],
        6 => [-y, x],
        7 => [-y, -x],
        8 => [y, -x],
        _ => [x, y],
    };
    Some([x / half_diagonal, y / half_diagonal])
}

/// The camera geometry version from which Samsung's distortion data are
/// read (`samsung_curve`); before it SRW files have no camera data.
const SAMSUNG_GEOMETRY: u64 = 4;

/// Each value of Samsung's distortion tags is stored minus one of the
/// eleven values of the maker note's key (0xA020, ExifTool's
/// EncryptionKey), taken in turn through the tags 0xA050, 0xA055, 0xA056
/// and 0xA057 (eight values each): value i of the tag starting at `start`
/// (0, 8, 16, 24) minus key (start + i) mod 11 (all 27 raw.pixls.us SRW
/// files, big- and little-endian). The values as written, `None` without a
/// key of eleven values.
pub fn samsung_decrypt(stored: &[f64], key: &[f64], start: usize) -> Option<Vec<f64>> {
    (key.len() == 11).then(|| {
        stored
            .iter()
            .enumerate()
            .map(|(i, v)| v + key[(start + i) % 11])
            .collect()
    })
}

/// Which Samsung model a file's distortion is, with its place in the key
/// stream (`samsung_decrypt`): the one the camera applied to its own JPEG
/// (0xA057), else, in compacts that write no such tag (EX1, EX2F, WB2000;
/// their JPEGs are corrected), the lens's (0xA050). Of the 27 raw.pixls.us
/// SRW files the two whose 0xA057 is empty although 0xA050 is not (NX11 +
/// 18-55mm at 55 mm, NX210 + 60mm) have uncorrected camera JPEGs (0.06 /
/// 0.11 px median against the uncorrected frame; notes: parity-gaps/G71).
pub fn samsung_model<'a>(
    applied: Option<&'a [f64]>,
    recorded: Option<&'a [f64]>,
) -> Option<(&'a [f64], usize)> {
    applied.map(|v| (v, 24)).or(recorded.map(|v| (v, 0)))
}

/// Samsung's distortion model (maker note 0xA050 / 0xA057, decrypted): four
/// pairs of a mantissa and a power of ten, c0 … c3; an output pixel r
/// pixels from the frame's centre samples the uncorrected frame at
/// r (c0 + c1 r² + c2 r⁴ + c3 r⁶), with no zoom on top. As rendered that
/// matches Camera Raw on the six raw.pixls.us files it corrects from the
/// data the camera applied, to 0.05–0.09 px median, 0.66 px max (corners
/// moved by up to 260 px; the EX2F's shape to 0.23 px, its 16:9 frame
/// aside), and the camera's own JPEGs of five whose lenses Camera Raw
/// corrects with its own profiles to 0.05–0.23 px (notes: parity-gaps/G71). `half_diagonal` (pixels) turns it into the warp's
/// polynomial, exact, without a fit. `None` for no model (c1 … c3 zero),
/// for a c0 other than 1 (never seen; its framing is unknown) and for
/// implausible values.
pub fn samsung_curve(values: &[f64], half_diagonal: f64) -> Option<EmbeddedLensData> {
    if values.len() != 8 || !half_diagonal.is_finite() || half_diagonal <= 1.0 {
        return None;
    }
    let c: Vec<f64> = (0..4)
        .map(|i| values[2 * i] * 10f64.powf(values[2 * i + 1]))
        .collect();
    if (c[0] - 1.0).abs() > 1e-9 || c[1..].iter().all(|&v| v == 0.0) {
        return None;
    }
    let k = [1, 2, 3].map(|i| c[i] * half_diagonal.powi(2 * i as i32));
    let p = |u: f64| 1.0 + k[0] * u * u + k[1] * u.powi(4) + k[2] * u.powi(6);
    let distortion: Vec<(f64, f64)> = (1..=FORMULA_KNOTS)
        .map(|i| {
            let u = i as f64 / FORMULA_KNOTS as f64;
            (u, p(u))
        })
        .collect();
    // The source radius grows with the output radius (the warp inverts it).
    let monotonic = distortion
        .windows(2)
        .all(|w| w[0].0 * w[0].1 < w[1].0 * w[1].1);
    (monotonic && plausible(&distortion)).then_some(EmbeddedLensData {
        maker: "Samsung",
        distortion,
        vignetting: None,
        zoom: 1.0,
        exact_zoom: true,
        chromatic: None,
        polynomial: Some(k),
        tabulated: None,
        centre: None,
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

/// The recipe key naming how a camera-mode recipe's camera data were read
/// as geometry (knot positions, framing).
pub const GEOMETRY_VERSION_KEY: &str = "cameraGeometryVersion";
/// The newest: 1 (2026-10-26) reads Sony's distortion as Camera Raw does
/// (`sony_lens`); 2 (2026-10-26) frames Nikon Z data as the camera does
/// (`nikon_curve`); 3 (2026-10-26) reads Canon's distortion and the radius
/// unit of its brightness table (`canon_curves`); 4 (2026-10-26) also
/// Samsung's (`samsung_curve`); 5 (2026-10-26) frames Panasonic's and
/// Fujifilm's data as the cameras do (`panasonic_curve`, `fuji_framed`); 6
/// (2026-10-26) warps Sony's knots themselves (`sony_lens`); 7
/// (2026-10-26) Fujifilm's (`fuji_tabulated`); 8 (2026-11-01) corrects
/// Nikon Z data about the shot's own optical centre (`nikon_centre`).
/// Recipes without the key
/// keep their parameters; saving them again fills in the same as before.
pub const GEOMETRY_VERSION: u64 = 8;

/// The camera geometry version that warps Fujifilm's knot table itself.
const FUJIFILM_TABLE_GEOMETRY: u64 = 7;

/// The camera geometry version that warps Sony's knot table itself.
const SONY_TABLE_GEOMETRY: u64 = 6;

/// The camera geometry version that frames Panasonic's and Fujifilm's data
/// as the cameras do.
const PANASONIC_FUJIFILM_GEOMETRY: u64 = 5;

/// The recipe's camera geometry version: `cameraGeometryVersion`, 0 when
/// missing.
pub fn geometry_version_of(adjustments: &serde_json::Value) -> u64 {
    adjustments
        .get(GEOMETRY_VERSION_KEY)
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0)
        .min(GEOMETRY_VERSION)
}

/// The correction data stored in the RAW file, if any.
pub fn read(path: &Path) -> Option<EmbeddedLensData> {
    read_version(path, CA_VERSION, GEOMETRY_VERSION)
}

/// The correction data as camera CA version `ca_version` and camera
/// geometry version `geometry_version` read them.
pub fn read_version(
    path: &Path,
    ca_version: u64,
    geometry_version: u64,
) -> Option<EmbeddedLensData> {
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
            let data =
                fuji_curves_with_ca(&distortion, vignetting.as_deref(), chromatic.as_deref())?;
            if geometry_version < PANASONIC_FUJIFILM_GEOMETRY {
                return Some(data);
            }
            let aspect = crate::raw_processing::sensor_and_frame(&source)
                .map(|(_, [w, h])| w as f64 / h as f64)
                .filter(|aspect| aspect.is_finite() && *aspect > 0.0)
                .unwrap_or(1.5);
            if geometry_version >= FUJIFILM_TABLE_GEOMETRY {
                return fuji_tabulated(data, aspect);
            }
            fuji_framed(data, aspect)
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
            let frame_reach = match crate::capture_info::aspect_crop(&head) {
                Some(crate::capture_info::AspectCrop::Centered(w, h)) => {
                    let (w, h) = (w as f64, h as f64);
                    let full_w = w.max(h * 1.5);
                    w.hypot(h) / full_w.hypot(full_w / 1.5)
                }
                _ => 1.0,
            };
            let frame = SonyFrame {
                frame_reach,
                interchangeable_lens: settings.is_some_and(|s| s.interchangeable_lens),
                ..if geometry_version == 0 {
                    SonyFrame::FULL
                } else {
                    sony_sensor_frame(&source).unwrap_or(SonyFrame::FULL)
                }
            };
            // The lateral CA is only in the encrypted SR2 directory.
            let sr2 = rawler::decoders::arw::ArwDecoder::sr2_subifd(&source).ok();
            let chromatic = sr2.as_ref().and_then(|sr2| values(sr2, SR2_CHROMATIC));
            if let Some(distortion) = values(&ifd, SONY_DISTORTION) {
                let vignetting = values(&ifd, SONY_VIGNETTING).filter(|_| vignetting_auto);
                return sony_lens(
                    &distortion,
                    vignetting.as_deref(),
                    chromatic.as_deref(),
                    frame,
                    geometry_version,
                );
            }
            // Older bodies (NEX, early ILCE) keep the parameters only in the
            // encrypted SR2 directory (tags 0x797d / 0x7982 there), which
            // Camera Raw reads too.
            let sr2 = sr2?;
            let vignetting = values(&sr2, SR2_VIGNETTING).filter(|_| vignetting_auto);
            sony_lens(
                &values(&sr2, SR2_DISTORTION)?,
                vignetting.as_deref(),
                chromatic.as_deref(),
                frame,
                geometry_version,
            )
        }
        "dng" => sony_dng_lens(&source, geometry_version).or_else(|| adobe_profile(&source)),
        "cr3" => canon_ctmd_lens(path, geometry_version),
        "rw2" | "rwl" => {
            let path = path.to_str()?;
            let chromatic = crate::capture_info::panasonic_ca_info(path)
                .filter(|_| ca_version >= 1)
                .and_then(|(values, half_diagonal)| panasonic_chromatic(&values, half_diagonal));
            // Version 5 also finds the frame of the bodies that write their
            // aspect crop as zeros (G7, G70: version 0 read their 3:2, 16:9
            // and 1:1 frames at the 4:3 frame's radius, 0.3–1.2 % off).
            let framed = geometry_version >= PANASONIC_FUJIFILM_GEOMETRY;
            let distortion =
                crate::capture_info::panasonic_distortion_info(path).and_then(|info| {
                    let half_diagonal = if framed {
                        info.frame_half_diagonal
                    } else {
                        info.recorded_half_diagonal
                    };
                    panasonic_curve(&info.values, half_diagonal, framed)
                });
            match distortion {
                Some(data) => Some(EmbeddedLensData { chromatic, ..data }),
                // Some bodies store CA data without distortion data (TZ81).
                None => chromatic.map(|chromatic| EmbeddedLensData {
                    maker: "Panasonic",
                    distortion: vec![(0.5, 1.0), (1.0, 1.0)],
                    vignetting: None,
                    zoom: 1.0,
                    exact_zoom: false,
                    polynomial: None,
                    tabulated: None,
                    centre: None,
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
                Some(info) => {
                    let centre = (geometry_version >= NIKON_CENTRE_GEOMETRY)
                        .then(|| {
                            let frame = rawtonic_rawdecode::source_frame::frame(&source, 0)?;
                            let [_, _, width, height] = frame.rect;
                            nikon_centre(&info, width, height, frame.orientation)
                        })
                        .flatten();
                    nikon_curve(&info, geometry_version >= 2, centre)
                }
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
        // Samsung's distortion only: its lateral CA (0xA051, one red and
        // one blue scale per file) states up to ten times the CA the
        // uncorrected frames show (NX200 + 16mm: red +10 / 10,000 stored,
        // +1.1 measured), where the automatic CA leaves under 0.3 (notes:
        // parity-gaps/G71); its vignetting (0xA052) is flat on NX200 and
        // later bodies, and their cameras applied none (0xA053).
        "srw" if geometry_version >= SAMSUNG_GEOMETRY => {
            let ifd =
                IFD::new_root_with_correction(&mut reader, 0, 0, 0, 10, &[SUB_IFDS, EXIF_IFD])
                    .ok()?;
            let notes = ifd
                .find_first_ifd_with_tag(rawler::tags::ExifTag::MakerNotes)?
                .parse_makernote(
                    &mut reader,
                    rawler::formats::tiff::ifd::OffsetMode::RelativeToIFD,
                    &[],
                )
                .ok()??;
            let key = values(&notes, SAMSUNG_KEY)?;
            let applied = values(&notes, SAMSUNG_APPLIED_DISTORTION);
            let recorded = values(&notes, SAMSUNG_DISTORTION);
            let (model, start) = samsung_model(applied.as_deref(), recorded.as_deref())?;
            let (_, [w, h]) = crate::raw_processing::sensor_and_frame(&source)?;
            samsung_curve(
                &samsung_decrypt(model, &key, start)?,
                (w as f64).hypot(h as f64) / 2.0,
            )
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

/// PTLens coefficients and zoom for a curve whose zoom is the framing
/// itself (`exact_zoom`, `framed_curve`: output radius p samples p k(p) with
/// k(x) = c(x / zoom) / zoom). Only the output frame (p ≤ 1) needs to match,
/// so the zoom S is fitted with the coefficients: P(q) = S k(S q) on
/// q ≤ 1 / S, minimizing the largest radial error. The frame's corner
/// q = 1 then lies outside the output, where P(1) = 1 need not follow the
/// data's extrapolation (RX100 III data at 8.8 mm: 2.5 → 1.2 px largest
/// error on 20 MP).
fn fit_framed_distortion(curve: &[(f64, f64)], zoom: f64) -> Option<([f64; 3], f64)> {
    const STEPS: usize = 200;
    let k = |x: f64| interpolate(curve, x / zoom) / zoom;
    let fit = |s: f64| {
        let rows: Vec<[f64; 4]> = (1..=FIT_SAMPLES)
            .map(|i| {
                let q = i as f64 / FIT_SAMPLES as f64 / s;
                let target = s * k(s * q) - 1.0;
                [
                    q * (q * q * q - 1.0),
                    q * (q * q - 1.0),
                    q * (q - 1.0),
                    q * target,
                ]
            })
            .collect();
        let coefficients = solve3(&rows)?;
        let error = (1..=STEPS)
            .map(|i| {
                let p = i as f64 / STEPS as f64;
                let q = p / s;
                (q * ptlens_factor(q, coefficients) - p * k(p)).abs()
            })
            .fold(0.0, f64::max);
        Some((error, coefficients, s))
    };
    (0..=STEPS)
        .filter_map(|i| fit((zoom * (0.9 + 0.2 * i as f64 / STEPS as f64)).max(1.0)))
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, coefficients, s)| (coefficients, s))
}

/// The PTLens factor a r³ + b r² + c r + 1 − a − b − c.
fn ptlens_factor(r: f64, [a, b, c]: [f64; 3]) -> f64 {
    a * r * r * r + b * r * r + c * r + 1.0 - a - b - c
}

/// The data as parameters of the geometry warp's PTLens model.
pub fn to_params(data: &EmbeddedLensData) -> Option<LensDistortionParams> {
    let vignetting = || {
        data.vignetting
            .as_deref()
            .and_then(fit_vignetting)
            .unwrap_or([0.0; 3])
    };
    let mut params = if let Some([k1, k2, k3]) = data.polynomial {
        LensDistortionParams::polynomial(k1, k2, k3, vignetting()).with_zoom(data.zoom)
    } else if let Some(table) = data.tabulated {
        let values = data.distortion.iter().map(|&(_, k)| k).collect();
        match table {
            Tabulated::Uniform => {
                let end = data.distortion.last()?.0;
                LensDistortionParams::radial_table(values, end, vignetting())
            }
            Tabulated::Knots => {
                let radii = data.distortion.iter().map(|&(r, _)| r).collect();
                LensDistortionParams::knot_table(radii, values, vignetting())
            }
        }
        .with_zoom(data.zoom)
    } else {
        let ([a, b, c], zoom) = if data.exact_zoom && data.zoom > 1.0 {
            fit_framed_distortion(&data.distortion, data.zoom)?
        } else {
            (fit_distortion(&data.distortion)?, data.zoom)
        };
        LensDistortionParams::ptlens(a, b, c, vignetting()).with_zoom(zoom)
    }
    .with_exact_zoom(data.exact_zoom);
    if let Some(centre) = data.centre {
        params = params.with_centre(centre);
    }
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

/// Parameters from the camera's data for `source`, if it carries any, at
/// the current versions (`current_geometry_version`).
pub fn params_for(source: &Path) -> Option<LensDistortionParams> {
    params_for_version(source, CA_VERSION, current_geometry_version(source))
}

/// The camera geometry version `params_for` reads `source` with: the
/// newest, except offline from facts registered before a version that reads
/// its format differently (`known_geometry_version`).
pub fn current_geometry_version(source: &Path) -> u64 {
    crate::source_facts::registered(&source.to_string_lossy()).map_or(GEOMETRY_VERSION, |facts| {
        known_geometry_version(source, facts.version)
    })
}

/// The `source_facts` versions from which `camera_lens` is read at camera
/// geometry version 1, 2, … (older versions in `camera_lens_geometry`).
const GEOMETRY_FACTS: [u32; GEOMETRY_VERSION as usize] = [7, 8, 9, 10, 11, 12, 13, 17];

/// The file formats (extensions) whose data camera geometry version 1, 2, …
/// reads differently from the version before: 1 Sony's (ARW, and Adobe DNG
/// Converter's DNGs of Sony bodies), 2 Nikon's (NEF; NRW share its reader),
/// 3 Canon's (CR3), 4 Samsung's (SRW), 5 Panasonic's and Fujifilm's (RW2,
/// RWL, RAF), 6 Sony's again, 7 Fujifilm's again.
const GEOMETRY_FORMATS: [&[&str]; GEOMETRY_VERSION as usize] = [
    &["arw", "dng"],
    &["nef", "nrw"],
    &["cr3"],
    &["srw"],
    &["rw2", "rwl", "raf"],
    &["arw", "dng"],
    &["raf"],
    &["nef", "nrw"],
];

/// The camera geometry version facts of `facts_version` carry.
pub fn facts_geometry_version(facts_version: u32) -> u64 {
    GEOMETRY_FACTS
        .iter()
        .filter(|&&from| facts_version >= from)
        .count() as u64
}

/// The newest camera geometry version whose data for `source` facts of
/// `facts_version` hold: the versions after theirs up to the first that
/// reads its format differently read it as theirs does (a Fujifilm RAF's
/// facts from before version 1 answer a version 1 recipe, a Sony ARW's do
/// not).
pub fn known_geometry_version(source: &Path, facts_version: u32) -> u64 {
    let extension = source
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let held = facts_geometry_version(facts_version);
    (held + 1..=GEOMETRY_VERSION)
        .find(|&version| GEOMETRY_FORMATS[version as usize - 1].contains(&extension.as_str()))
        .map_or(GEOMETRY_VERSION, |changed| changed - 1)
}

/// Parameters from the camera's data for a recipe: as its camera CA and
/// geometry versions read them (`cameraCaVersion`,
/// `cameraGeometryVersion`), so a recipe saved again keeps rendering as it
/// was made.
pub fn params_for_recipe(
    source: &Path,
    adjustments: &serde_json::Value,
) -> Option<LensDistortionParams> {
    params_for_version(
        source,
        ca_version_of(adjustments),
        geometry_version_of(adjustments),
    )
}

/// The `source_facts` version from which `camera_lens` carries Panasonic
/// lateral CA data (camera CA version 1).
const PANASONIC_CA_FACTS: u32 = 3;

/// Whether `params_for_recipe` gives the camera data the recipe reads. Not
/// offline when the registered facts predate what its camera CA or
/// geometry version reads (a Panasonic preview made before its CA data was
/// read, a Sony one made before geometry version 1): saving then keeps the
/// recipe's parameters instead of replacing them with other data.
pub fn recipe_params_known(source: &Path, adjustments: &serde_json::Value) -> bool {
    crate::source_facts::registered(&source.to_string_lossy()).is_none_or(|facts| {
        (facts.version >= PANASONIC_CA_FACTS
            || ca_version_of(adjustments) == 0
            || !is_panasonic(source))
            && geometry_version_of(adjustments) <= known_geometry_version(source, facts.version)
    })
}

fn is_panasonic(source: &Path) -> bool {
    source
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("rw2") || e.eq_ignore_ascii_case("rwl"))
}

/// Parameters from the camera's data for `source` as camera CA version
/// `ca_version` and geometry version `geometry_version` read them.
pub fn params_for_version(
    source: &Path,
    ca_version: u64,
    geometry_version: u64,
) -> Option<LensDistortionParams> {
    // Read once per file (and modification time): opening a photograph,
    // its thumbnail and its export all ask, and reading the RAW header costs
    // several milliseconds.
    type Cache = std::sync::Mutex<
        std::collections::HashMap<
            (std::path::PathBuf, std::time::SystemTime, u64, u64),
            Option<LensDistortionParams>,
        >,
    >;
    // Offline: the data the file had (source_facts), read at the current
    // versions; CA version 0 had no Panasonic CA and no CA-only data,
    // older geometry versions' data are kept where they differ.
    if let Some(facts) = crate::source_facts::registered(&source.to_string_lossy()) {
        let params = facts.camera_lens_at(geometry_version);
        if ca_version == 0 && is_panasonic(source) {
            return params
                .map(LensDistortionParams::without_tca)
                .filter(|p| !p.is_identity());
        }
        return params;
    }
    static CACHE: std::sync::OnceLock<Cache> = std::sync::OnceLock::new();
    let modified = std::fs::metadata(source).and_then(|m| m.modified()).ok();
    let key = modified.map(|time| (source.to_path_buf(), time, ca_version, geometry_version));
    let cache = CACHE.get_or_init(Default::default);
    if let Some(key) = &key
        && let Some(params) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(key)
    {
        return params.clone();
    }
    let params = read_version(source, ca_version, geometry_version)
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
/// recipe the current `renderVersion` and `sharpenVersion`, and with the
/// camera's data its current `cameraCaVersion` and `cameraGeometryVersion`
/// (`current_geometry_version`).
pub fn apply_default(adjustments: &mut serde_json::Value, source: &Path) {
    if !(adjustments.is_null() || adjustments.is_object())
        || adjustments.get("lensCorrectionMode").is_some()
    {
        return;
    }
    // A never-edited RAW also starts with the current edge-fringe pass, as
    // the editor's new edits do.
    let is_raw = crate::formats::is_raw_file(source);
    // A new recipe renders as a new edit (`renderVersion`, as the editor's
    // `loadedRenderVersion`); a saved partial recipe without a lens mode
    // keeps the version it was made with.
    crate::image_processing::apply_render_default(adjustments);
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
        let geometry = current_geometry_version(source);
        if geometry > 0 {
            adjustments[GEOMETRY_VERSION_KEY] = serde_json::json!(geometry);
        }
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

/// The tables of CTMD record 14 (`canon_lens_tables`), knot values with
/// their radii.
#[derive(Debug, Clone, PartialEq)]
struct CanonLensTables {
    brightness: Vec<u16>,
    brightness_radii: Vec<u16>,
    /// Empty when the record ends before it.
    distortion: Vec<u16>,
    distortion_radii: Vec<u16>,
}

/// CTMD record 14 of a CTMD sample (`canon_ctmd_lens`).
fn canon_lens_tables(sample: &[u8]) -> Option<CanonLensTables> {
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
    // Block `index` of the record, its first `count` slots.
    let block = |index: usize, count: usize| {
        let start = 3 + index * SLOTS;
        (2..=SLOTS)
            .contains(&count)
            .then(|| words.get(start..start + count))
            .flatten()
    };
    let ascending = |radii: &[u16]| radii.windows(2).all(|w| w[0] < w[1]);
    let count = *words.first()? as usize;
    let brightness = block(0, count)?;
    let brightness_radii = block(1, count)?;
    if brightness[0] == 0 || !ascending(brightness_radii) {
        return None;
    }
    let count = words.get(2).map_or(0, |&c| c as usize);
    let (distortion, distortion_radii) = match (block(5, count), block(6, count)) {
        (Some(values), Some(radii)) if values[0] != 0 && ascending(radii) => {
            (values.to_vec(), radii.to_vec())
        }
        _ => Default::default(),
    };
    Some(CanonLensTables {
        brightness: brightness.to_vec(),
        brightness_radii: brightness_radii.to_vec(),
        distortion,
        distortion_radii,
    })
}

/// Millimetres per radius unit of CTMD record 14: 1/4096 of the 35 mm
/// frame's half diagonal. The knots then lie at whole millimetres (757: 4,
/// 1893: 10, 3596: 19, 4088: 21.6, 4543: 24 mm; R50 V: 379: 2, 3218: 17).
const CANON_RADIUS_MM: f64 = 21.633307652783937 / 4096.0;
/// The unit as read before camera geometry version 3.
const CANON_RADIUS_MM_V0: f64 = 0.005;
/// The part of a distortion value's departure from 16384 that is
/// corrected. Not derived from the format: fitted on Camera Raw's own
/// correction of an R5 Mark II + RF 135 and an R6 Mark III + RF 50 (0.80 and
/// 0.81 with the radius unit free, which then landed on
/// `CANON_RADIUS_MM`), then checked held out on the camera's own corrected
/// JPEG of an R50 V + RF 28 mm (other body, sensor size, lens, sign of the
/// distortion and reference): 0.15 px median with 0.8, 1.1 px with the full
/// value (`the_distortion_share_reproduces_the_cameras_own_correction`;
/// notes: parity-gaps/G71).
const CANON_DISTORTION_SHARE: f64 = 0.8;

/// The curves of CTMD record 14 for an image whose half diagonal on the
/// sensor is `half_diagonal_mm`, as camera geometry version `geometry`
/// reads them. Brightness: 8191 = centre. Distortion (since version 3,
/// none before): a corrected radius u samples the camera's image at
/// u k(u), k = 1 + `CANON_DISTORTION_SHARE` (value / 16384 − 1); data
/// that reach beyond the corners (k(1) > 1, pincushion) keep the corners
/// on the sensor's corners as Camera Raw does, others keep the camera's
/// framing (zoomed in by the radius that reaches the corner). Radii in
/// `CANON_RADIUS_MM`, 5 µm before version 3.
fn canon_curves(
    tables: &CanonLensTables,
    half_diagonal_mm: f64,
    geometry: u64,
) -> Option<EmbeddedLensData> {
    let mm = if geometry >= 3 {
        CANON_RADIUS_MM
    } else {
        CANON_RADIUS_MM_V0
    };
    let radius = |units: u16| f64::from(units) * mm / half_diagonal_mm;
    let centre = f64::from(*tables.brightness.first()?);
    let vignetting = tables
        .brightness_radii
        .iter()
        .zip(&tables.brightness)
        .map(|(&units, &value)| (radius(units), f64::from(value) / centre))
        .collect();
    let identity = || (vec![(0.0, 1.0), (1.0, 1.0)], 1.0, false);
    let (distortion, zoom, exact_zoom) = if geometry >= 3 && !tables.distortion.is_empty() {
        // Knots beyond twice the half diagonal (small crops) are not needed.
        let knots: Vec<(f64, f64)> = tables
            .distortion_radii
            .iter()
            .zip(&tables.distortion)
            .map(|(&units, &value)| {
                let k = f64::from(value) / 16384.0;
                (radius(units), 1.0 + CANON_DISTORTION_SHARE * (k - 1.0))
            })
            .filter(|&(r, _)| r <= 2.0)
            .collect();
        if knots.len() < 2 || !plausible(&knots) {
            identity()
        } else if interpolate(&knots, 1.0) >= 1.0 {
            (knots, 1.0, false)
        } else {
            let (curve, zoom) = framed_curve(|u| interpolate(&knots, u))?;
            (curve, zoom, true)
        }
    } else {
        identity()
    };
    Some(EmbeddedLensData {
        maker: "Canon",
        distortion,
        vignetting: Some(vignetting),
        zoom,
        exact_zoom,
        chromatic: None,
        polynomial: None,
        tabulated: None,
        centre: None,
    })
}

/// Canon's lens correction data in CTMD record 14 of newer CR3s (EOS R5
/// Mark II, R6 Mark III; Camera Raw's "Camera Settings" profile for them).
/// After three counts (17) come 23-slot blocks of little-endian u16:
/// brightness, radii, two tables and radii (around 16384, not used: no
/// lateral CA in the files they describe), distortion and radii
/// (`canon_curves`). Radii are on the sensor, normalized here by half the
/// recorded image's diagonal (FocalPlaneXResolution), so crop modes read
/// the same tables.
fn canon_ctmd_lens(path: &Path, geometry: u64) -> Option<EmbeddedLensData> {
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
    let tables = canon_lens_tables(&sample)?;
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
    canon_curves(&tables, width.hypot(height) / 2.0 / per_mm, geometry)
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
fn sony_dng_lens(source: &RawSource, geometry: u64) -> Option<EmbeddedLensData> {
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
    let frame = SonyFrame {
        frame_reach: user_crop_diagonal(decoder.as_ref()).unwrap_or(1.0),
        interchangeable_lens: settings.interchangeable_lens,
        ..if geometry == 0 {
            SonyFrame::FULL
        } else {
            sony_sensor_frame(source).unwrap_or(SonyFrame::FULL)
        }
    };
    sony_lens(&distortion, vignetting.as_deref(), None, frame, geometry)
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
        exact_zoom: false,
        polynomial: None,
        tabulated: None,
        centre: None,
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

/// The lensfun profile of the lens the RAW at `source` was taken with, for
/// any camera, at the shot's focal length, aperture and distance: only for
/// an EXIF lens name that names one lensfun lens exactly
/// (`lens_correction::find_lens_exact`), never a guess — generic names
/// ("24-70mm") matched loosely picked another maker's lens and made
/// Lightroom imports worse (notes:
/// benchmark-results/lc2-lr-render-match-2026-11-01). For a correction the
/// photographer asked for (a Lightroom import with profile corrections on)
/// where the default (`lensfun_default`, camera data) has none. Not cached:
/// read once per import.
pub fn lensfun_for_photo(source: &Path) -> Option<(String, String, LensDistortionParams)> {
    std::panic::catch_unwind(|| lensfun_uncached(source, true))
        .ok()
        .flatten()
}

fn lensfun_default_uncached(source: &Path) -> Option<(String, String, LensDistortionParams)> {
    lensfun_uncached(source, false)
}

fn lensfun_uncached(
    source: &Path,
    any_lens: bool,
) -> Option<(String, String, LensDistortionParams)> {
    let extension = source.extension()?.to_str()?.to_ascii_lowercase();
    if !any_lens && !matches!(extension.as_str(), "cr3" | "cr2" | "dng") {
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
    let lens = if any_lens {
        let lens_name = exif
            .lens_model
            .clone()
            .or_else(|| exif_lens_model(source))
            .unwrap_or_default();
        let maker = exif
            .lens_make
            .clone()
            .filter(|m| !m.trim().is_empty())
            .unwrap_or_else(|| metadata.make.clone());
        crate::lens_correction::find_lens_exact(&db, &maker, &lens_name)?
    } else if matches!(extension.as_str(), "cr3" | "dng")
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
        nikon_blob_framed(distortion_flag, Some(0.064596))
    }

    fn nikon_blob_framed(distortion_flag: u8, framing: Option<f64>) -> Vec<u8> {
        nikon_blob_centred(distortion_flag, framing, None)
    }

    /// With NikonNEFInfo tag 8 holding the optical centre `centre` (px).
    fn nikon_blob_centred(
        distortion_flag: u8,
        framing: Option<f64>,
        centre: Option<[f64; 2]>,
    ) -> Vec<u8> {
        let rational = |v: f64| {
            let mut b = ((v * 1048576.0).round() as i32).to_le_bytes().to_vec();
            b.extend_from_slice(&1048576u32.to_le_bytes());
            b
        };
        let mut dist = b"0100".to_vec();
        dist.extend_from_slice(&[distortion_flag, 1, 0, 0]);
        dist.extend(rational(21.6));
        dist.extend_from_slice(&4u32.to_le_bytes());
        for v in [0.018975, -0.012295, 0.041512, 0.0, 0.0] {
            dist.extend(rational(v));
        }
        dist.resize(0x40, 0);
        if let Some(framing) = framing {
            dist.extend(rational(framing));
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
        let centre = centre.map(|[x, y]| {
            let mut c = b"0100".to_vec();
            for v in [x, y] {
                c.extend_from_slice(&((v * 256.0).round() as i32).to_le_bytes());
                c.extend_from_slice(&256u32.to_le_bytes());
            }
            c.extend_from_slice(&[0, 0, 0x4a, 0x7f]);
            c
        });
        let mut entries = vec![(5u16, dist), (6u16, vig)];
        entries.extend(centre.map(|c| (8u16, c)));
        let mut tiff = b"II*\0".to_vec();
        tiff.extend_from_slice(&8u32.to_le_bytes());
        tiff.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        let mut offset = 8 + 2 + entries.len() * 12 + 4;
        for (tag, data) in &entries {
            tiff.extend_from_slice(&tag.to_le_bytes());
            tiff.extend_from_slice(&7u16.to_le_bytes());
            tiff.extend_from_slice(&(data.len() as u32).to_le_bytes());
            tiff.extend_from_slice(&(offset as u32).to_le_bytes());
            offset += data.len();
        }
        tiff.extend_from_slice(&0u32.to_le_bytes());
        for (_, data) in &entries {
            tiff.extend(data);
        }
        let mut blob = b"Nikon\0\x01\x01\0\0".to_vec();
        blob.extend(tiff);
        blob
    }

    #[test]
    fn nikon_data_matches_the_dng_converter_opcodes() {
        let info = parse_nikon_nef_info(&nikon_blob(3)).unwrap();
        assert_eq!(info.distortion_flag, 3);
        assert!((info.vignetting_strength - 541.0 / 1024.0).abs() < 1e-12);
        let data = nikon_curve(&info, false, None).unwrap();
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
        assert_eq!(nikon_curve(&optional, false, None), Some(data));
        // DSLR blocks (flag 0 or 2) are not used.
        for flag in [0, 2] {
            assert!(
                nikon_curve(
                    &parse_nikon_nef_info(&nikon_blob(flag)).unwrap(),
                    false,
                    None
                )
                .is_none()
            );
        }
    }

    #[test]
    fn nikon_framing_follows_the_scale_the_camera_stores() {
        let info = parse_nikon_nef_info(&nikon_blob(3)).unwrap();
        assert!((info.framing.unwrap() - 0.064596).abs() < 1e-6);
        let [d1, d2, d3, _] = info.distortion;
        let p = |r: f64| 1.0 + r * r * (d3 + r * (d2 + r * d1));
        // The camera's JPEG (and Camera Raw): an output pixel at radius u
        // shows the sensor at u p(u) / (1 + framing), so its corners lie
        // inside the sensor's (Z 6 at 52 mm: 0.980 of the half-diagonal).
        let data = nikon_curve(&info, true, None).unwrap();
        assert!(data.exact_zoom);
        assert!((data.zoom - 1.01398).abs() < 1e-4, "zoom {}", data.zoom);
        for i in 1..=20 {
            let u = i as f64 / 20.0;
            let warp = warp_source_radius(&data, u);
            let camera = u * p(u) / (1.0 + 0.064596);
            assert!((warp - camera).abs() < 2e-4, "u {u}: {warp} vs {camera}");
        }
        // Without the framing value, or for recipes of an earlier camera
        // geometry version, the corners stay on the sensor's corners.
        let unframed = parse_nikon_nef_info(&nikon_blob_framed(3, None)).unwrap();
        assert_eq!(unframed.framing, None);
        assert_eq!(
            nikon_curve(&unframed, true, None),
            nikon_curve(&info, false, None)
        );
        let old = nikon_curve(&info, false, None).unwrap();
        assert_eq!((old.zoom, old.exact_zoom), (1.0, false));
    }

    #[test]
    fn nikon_optical_centre_is_read_and_turned_with_the_image() {
        // A Z 6 + 24-70mm f/4 S at 52 mm records (−7.52, +26.30) px.
        let blob = nikon_blob_centred(3, Some(0.064596), Some([-7.52, 26.30]));
        let info = parse_nikon_nef_info(&blob).unwrap();
        let [x, y] = info.centre.unwrap();
        assert!(
            (x + 7.52).abs() < 0.01 && (y - 26.30).abs() < 0.01,
            "{x} {y}"
        );
        assert_eq!(parse_nikon_nef_info(&nikon_blob(3)).unwrap().centre, None);
        let (w, h) = (6048usize, 4024usize);
        let hd = (w as f64).hypot(h as f64) / 2.0;
        let at = |orientation| {
            nikon_centre(&info, w, h, orientation)
                .unwrap()
                .map(|v| v * hd)
        };
        let close =
            |a: [f64; 2], b: [f64; 2]| (a[0] - b[0]).abs() < 1e-9 && (a[1] - b[1]).abs() < 1e-9;
        assert!(close(at(1), [x, y]));
        // Turned 90° clockwise for display (6): the sensor's right is down.
        assert!(close(at(6), [-y, x]));
        assert!(close(at(8), [y, -x]));
        assert!(close(at(3), [-x, -y]));
        // No offset, no centre.
        let zero =
            parse_nikon_nef_info(&nikon_blob_centred(3, Some(0.064596), Some([0.0, 0.0]))).unwrap();
        assert_eq!(nikon_centre(&zero, w, h, 1), None);
        // Only with the camera's framing, into the warp's parameters.
        let centre = nikon_centre(&info, w, h, 1);
        assert_eq!(nikon_curve(&info, false, centre).unwrap().centre, None);
        let data = nikon_curve(&info, true, centre).unwrap();
        assert_eq!(data.centre, centre);
        let params = serde_json::to_value(to_params(&data).unwrap()).unwrap();
        assert!((params["centre_x"].as_f64().unwrap() - x / hd).abs() < 1e-12);
        assert!((params["centre_y"].as_f64().unwrap() - y / hd).abs() < 1e-12);
        let centred =
            serde_json::to_value(to_params(&nikon_curve(&info, true, None).unwrap()).unwrap())
                .unwrap();
        assert!(centred.get("centre_x").is_none() && centred.get("centre_y").is_none());
    }

    #[test]
    fn offline_facts_carry_the_geometry_version_they_were_read_at() {
        // Previews from before Sony's knots read neither; facts version 7
        // has Sony's (1), 8 also Nikon's framing (2), 9 Canon's
        // distortion (3), 10 Samsung's data (4), 11 Panasonic's and Fujifilm's
        // framing (5), 12 Sony's knot table (6), 13 Fujifilm's (7), 17
        // Nikon's optical centre (8).
        assert_eq!(facts_geometry_version(6), 0);
        assert_eq!(facts_geometry_version(7), 1);
        assert_eq!(facts_geometry_version(8), 2);
        assert_eq!(facts_geometry_version(9), 3);
        assert_eq!(facts_geometry_version(10), 4);
        assert_eq!(facts_geometry_version(11), 5);
        assert_eq!(facts_geometry_version(12), 6);
        assert_eq!(facts_geometry_version(13), 7);
        assert_eq!(facts_geometry_version(16), 7);
        assert_eq!(facts_geometry_version(17), 8);
        assert_eq!(
            facts_geometry_version(crate::source_facts::VERSION),
            GEOMETRY_VERSION
        );
        // A NEF's version 7 facts predate Nikon's framing, a CR3's version
        // 8 facts Canon's distortion, an SRW's version 9 facts Samsung's
        // data, an RW2's or RAF's version 10 facts their framing, an ARW's
        // or DNG's version 7 to 11 facts Sony's knot table, a RAF's version
        // 11 and 12 facts Fujifilm's, a NEF's version 8 to 16 facts Nikon's
        // optical centre; an ORF's hold.
        assert_eq!(known_geometry_version(Path::new("a.nef"), 7), 1);
        assert_eq!(known_geometry_version(Path::new("a.NRW"), 7), 1);
        assert_eq!(known_geometry_version(Path::new("a.arw"), 7), 5);
        assert_eq!(known_geometry_version(Path::new("a.arw"), 11), 5);
        assert_eq!(known_geometry_version(Path::new("a.DNG"), 11), 5);
        assert_eq!(known_geometry_version(Path::new("a.arw"), 12), 8);
        assert_eq!(known_geometry_version(Path::new("a.nef"), 8), 7);
        assert_eq!(known_geometry_version(Path::new("a.nef"), 16), 7);
        assert_eq!(known_geometry_version(Path::new("a.nef"), 17), 8);
        assert_eq!(known_geometry_version(Path::new("a.CR3"), 8), 2);
        assert_eq!(known_geometry_version(Path::new("a.cr3"), 9), 8);
        assert_eq!(known_geometry_version(Path::new("a.srw"), 9), 3);
        assert_eq!(known_geometry_version(Path::new("a.SRW"), 10), 8);
        assert_eq!(known_geometry_version(Path::new("a.RW2"), 10), 4);
        assert_eq!(known_geometry_version(Path::new("a.raf"), 10), 4);
        assert_eq!(known_geometry_version(Path::new("a.RAF"), 11), 6);
        assert_eq!(known_geometry_version(Path::new("a.raf"), 12), 6);
        assert_eq!(known_geometry_version(Path::new("a.raf"), 13), 8);
        assert_eq!(known_geometry_version(Path::new("a.rw2"), 12), 8);
        assert_eq!(known_geometry_version(Path::new("a.rwl"), 11), 8);
        assert_eq!(known_geometry_version(Path::new("a.orf"), 6), 8);
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
        let data = panasonic_curve(&FZ330, Some(2500.0), false).unwrap();
        // Ru(Rd = 1) = 1 + a + b + c with s = 1.
        let expected = 1.0 + (8820.0 - 2282.0 - 663.0) / 32768.0;
        assert!((data.zoom - expected).abs() < 1e-6, "zoom {}", data.zoom);
        // The curve the warp takes maps corner to corner.
        let last = data.distortion.last().unwrap();
        assert!((last.0 - 1.0).abs() < 1e-12 && (last.1 - 1.0).abs() < 1e-9);
        // Off when the flag nibble is not 1.
        let mut off = FZ330;
        off[7] = 0;
        assert!(panasonic_curve(&off, Some(2500.0), false).is_none());
    }

    #[test]
    fn panasonic_radius_follows_distortion_n_for_cropped_aspect_frames() {
        // A 1:1 frame of a 4000 × 3000 sensor reaches only 2115.6 / 2500 of
        // the model's radius: less correction and less zoom.
        let full = panasonic_curve(&FZ330, Some(2500.0), false).unwrap();
        let square = panasonic_curve(&FZ330, Some(2115.6), false).unwrap();
        assert!(square.zoom < full.zoom && square.zoom > 1.0);
    }

    /// DC-G9 + 12-60 at 28 mm (raw.pixls.us): the newer layout (v3 ≠ 0)
    /// with DistortionScale v5 = −80.
    const G9: [i16; 16] = [
        20764, -10328, 198, 566, -303, -80, 252, -10239, 163, 175, 434, 122, 3240, 675, -25326,
        16235,
    ];

    #[test]
    fn panasonic_geometry_version_5_frames_at_the_stored_scale() {
        let ru_corner = 1.0 + (163.0 - 303.0 + 122.0) / 32768.0 / (1.0 - 80.0 / 32768.0);
        // Version 0: the output corner shows Ru = reach (here 1), as a
        // minimum zoom.
        let old = panasonic_curve(&G9, Some(3240.0), false).unwrap();
        assert!((old.zoom - ru_corner).abs() < 1e-9 && !old.exact_zoom);
        // Version 5: Ru = reach (1 + v5 / 32768) at the corner, exactly.
        let framed = panasonic_curve(&G9, Some(3240.0), true).unwrap();
        let expected = ru_corner / (1.0 - 80.0 / 32768.0);
        assert!(
            (framed.zoom - expected).abs() < 1e-9,
            "zoom {}",
            framed.zoom
        );
        assert!(framed.exact_zoom);
        assert_eq!(framed.distortion, old.distortion);
        // The older layout stores v5 = 0: the same framing, exact.
        let fz = panasonic_curve(&FZ330, Some(2500.0), true).unwrap();
        let fz_old = panasonic_curve(&FZ330, Some(2500.0), false).unwrap();
        assert!((fz.zoom - fz_old.zoom).abs() < 1e-12 && fz.exact_zoom);
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
            exact_zoom: false,
            polynomial: None,
            tabulated: None,
            centre: None,
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
        rw2_header_framed(distortion, ca, [8, 8, 3456, 4600], None)
    }

    /// An RW2 header with sensor borders `sensor` and, if given, aspect-crop
    /// borders `crop` (top, left, bottom, right).
    fn rw2_header_framed(
        distortion: &[i16; 16],
        ca: &[i16; 32],
        sensor: [u32; 4],
        crop: Option<[u32; 4]>,
    ) -> Vec<u8> {
        let border = |tag: u16, value: u32| (tag, 3u16, 1u32, value.to_le_bytes().to_vec());
        let mut entries: Vec<(u16, u16, u32, Vec<u8>)> = (0x0004..=0x0007)
            .zip(sensor)
            .map(|(tag, value)| border(tag, value))
            .collect();
        if let Some(crop) = crop {
            entries.extend(
                (0x002f..=0x0032)
                    .zip(crop)
                    .map(|(tag, value)| border(tag, value)),
            );
        }
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
    fn panasonic_geometry_version_5_finds_frames_whose_crop_tags_are_zeros() {
        // DMC-G7 at 14 mm in 3:2 (raw.pixls.us): crop tags written as zeros,
        // the sensor tags hold the 4592 × 3064 frame; DistortionN 2870 is
        // the 4:3 frame's half-diagonal.
        const G7: [i16; 16] = [
            29259, 10554, 390, 0, 208, 0, 276, 1, 3543, 304, 516, -699, 2870, 804, -30387, 15950,
        ];
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("g7.RW2");
        let header = rw2_header_framed(&G7, &[0; 32], [8, 8, 3072, 4600], Some([0; 4]));
        std::fs::write(&path, header).unwrap();
        let reach = 0.5 * 4592f64.hypot(3064.0) / 2870.0;
        let at = |half_diagonal| panasonic_curve(&G7, half_diagonal, true).unwrap().zoom;
        let old = read_version(&path, 1, 0).unwrap();
        assert_eq!(old.zoom, panasonic_curve(&G7, None, false).unwrap().zoom);
        let new = read_version(&path, 1, 5).unwrap();
        assert!((new.zoom - at(Some(reach * 2870.0))).abs() < 1e-12);
        assert!((new.zoom - old.zoom).abs() > 1e-3);
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
        let current = serde_json::json!({
            CA_VERSION_KEY: CA_VERSION,
            GEOMETRY_VERSION_KEY: GEOMETRY_VERSION,
        });
        assert_eq!(
            params_for(&with_distortion),
            params_for_recipe(&with_distortion, &current)
        );
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

    /// Where the warp's output radius `u` samples the source with `params`
    /// (lens zoom, then the PTLens curve), radii relative to the half
    /// diagonal.
    fn warp_source_radius(data: &EmbeddedLensData, u: f64) -> f64 {
        let params = serde_json::to_value(to_params(data).unwrap()).unwrap();
        let get = |key: &str| params.get(key).and_then(|v| v.as_f64());
        let coefficients = [get("k1").unwrap(), get("k2").unwrap(), get("k3").unwrap()];
        let q = u / get("zoom").unwrap_or(1.0);
        q * ptlens(q, coefficients)
    }

    // RX100 III at 8.8 mm (DistortionCorrParams as stored): strong barrel,
    // the last knot well below 1.
    const RX100M3_DISTORTION_VALUES: [f64; 17] = [
        11.0, 1265.0, 1218.0, 1083.0, 860.0, 562.0, 209.0, -179.0, -580.0, -972.0, -1353.0,
        -1725.0, 0.0, 0.0, 0.0, 0.0, 0.0,
    ];

    #[test]
    fn sony_geometry_version_1_knots_span_centre_to_corner_without_reframing() {
        // Camera Raw's mapping (G71): output radius u samples u k(u), the
        // knots at i / (n − 1), not normalized to the corner — unless that
        // samples beyond the sensor corner (k(1) > 1): then the corners
        // stay on the corners.
        for values in [&SONY_DISTORTION_VALUES[..], &RX100M3_DISTORTION_VALUES[..]] {
            let n = values[0] as usize;
            let k = |u: f64| {
                let curve: Vec<(f64, f64)> = (0..n)
                    .map(|i| (i as f64 / (n - 1) as f64, 1.0 + values[1 + i] / 16384.0))
                    .collect();
                interpolate(&curve, u)
            };
            for (reach, tolerance) in [(1.0, 4e-4), (0.784, 2e-4)] {
                let frame = SonyFrame {
                    reach,
                    ..SonyFrame::FULL
                };
                let data = sony_lens(values, None, None, frame, 1).unwrap();
                let corner = data.zoom.min(1.0);
                assert_eq!(corner < 1.0, k(reach) > 1.0);
                for i in 1..=20 {
                    let u = i as f64 / 20.0;
                    let expected = corner * u * k(corner * u * reach);
                    let got = warp_source_radius(&data, u);
                    assert!(
                        (got - expected).abs() < tolerance,
                        "n {n} reach {reach} u {u}: {got} vs {expected}"
                    );
                }
            }
        }
        // Version 0 keeps the half-step knots, corners on the corners.
        let old = sony_lens(&RX100M3_DISTORTION_VALUES, None, None, SonyFrame::FULL, 0).unwrap();
        assert_eq!(old, sony_curve(&RX100M3_DISTORTION_VALUES, None).unwrap());
        assert_eq!(old.distortion[0].0, 0.5 / 11.0);
        assert_eq!(old.zoom, 1.0);
    }

    // RX0 at 7.7 mm (as stored): a wavy table.
    const RX0_DISTORTION_VALUES: [f64; 17] = [
        11.0, 89.0, 77.0, 85.0, 113.0, 109.0, 53.0, -59.0, -205.0, -333.0, -394.0, -383.0, 0.0,
        0.0, 0.0, 0.0, 0.0,
    ];

    #[test]
    fn sony_geometry_version_6_warps_the_knot_table_itself() {
        // Version 1's three-term fit cannot follow the RX100 III's table or
        // the RX0's wavy one (0.4–0.5 JPEG px against the cameras' own
        // JPEGs; notes: parity-gaps/G71). Version 6 warps through the knots
        // themselves: where an output radius lands on knot i, the warp
        // samples A u k_i, whatever the interpolation between the knots.
        // Strong barrel data reach the default crop's margins beyond the
        // developed frame (RX100 III: 1.4 px at the top edge's middle);
        // without those pixels the warp zooms in uniformly instead (as the
        // camera's JPEG frames), keeping the shape.
        let (w, h) = (5472u32, 3648u32);
        let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
        let hd = cx.hypot(cy);
        let active = (w as f64).hypot(h as f64) / 5504f64.hypot(3672.0);
        let short = 1.0 / 1.5f64.hypot(1.0);
        for (values, interchangeable_lens, reach, zoom_in) in [
            (&RX100M3_DISTORTION_VALUES, false, 1.0, 0.0),
            (&RX100M3_DISTORTION_VALUES, false, active, 1.1e-3),
            (&RX0_DISTORTION_VALUES, false, active, 2.5e-4),
            (&SONY_DISTORTION_VALUES, false, active, 0.0),
            (&RX100M3_DISTORTION_VALUES, true, active, 0.0),
            (&SONY_DISTORTION_VALUES, true, active, 0.0),
        ] {
            let n = values[0] as usize;
            let knots: Vec<f64> = values[1..=n].iter().map(|v| 1.0 + v / 16384.0).collect();
            let k = crate::lens_correction::RadialTable::new(&knots, 1.0).unwrap();
            let frame = SonyFrame {
                reach,
                interchangeable_lens,
                ..SonyFrame::FULL
            };
            // Interchangeable-lens bodies: no blank edge on the frame's
            // border (the interpolant is monotone between the knots).
            let scale = if interchangeable_lens {
                let border = (0..n)
                    .map(|i| i as f64 / (n - 1) as f64)
                    .filter(|r| (short * reach..reach).contains(r))
                    .chain([short * reach, reach]);
                1.0 / border.map(|r| k.at(r)).fold(f64::MIN, f64::max)
            } else {
                1.0
            };
            let data = sony_lens(values, None, None, frame, 6).unwrap();
            assert_eq!(data.tabulated, Some(Tabulated::Uniform));
            // Data that would sample beyond the frame's corner keep the
            // corners on the corners (as version 1).
            let corner = data.zoom.min(1.0);
            let recipe = serde_json::json!({
                "lensCorrectionMode": CAMERA_MODE,
                "lensDistortionParams": to_params(&data).unwrap(),
            });
            let warp = crate::image_processing::WarpPoints::new(
                &crate::image_processing::get_geometry_params_from_json(&recipe),
                w,
                h,
            );
            // The camera's sample at the knots that lie inside the frame
            // (along its long and short axes and its diagonal), and the
            // warp's at output radius u z, z the uniform zoom.
            let source = |u: f64, angle: f64| {
                let (x, y) = (cx + u * hd * f64::cos(angle), cy - u * hd * f64::sin(angle));
                let inside = (0.0..w as f64).contains(&x) && (0.0..h as f64).contains(&y);
                let (sx, sy) = warp.to_source(x as f32, y as f32).unwrap();
                inside.then(|| (sx as f64 - cx).hypot(sy as f64 - cy) / hd)
            };
            let knot_points: Vec<(f64, f64, f64)> = knots
                .iter()
                .enumerate()
                .skip(1)
                .flat_map(|(i, &knot)| {
                    let r = i as f64 / (n - 1) as f64 / reach;
                    [0.0, cy.atan2(cx), std::f64::consts::FRAC_PI_2]
                        .map(|angle| (r / corner, angle, scale * r * knot))
                })
                .filter(|&(u, angle, _)| source(u * (1.0 + zoom_in), angle).is_some())
                .collect();
            assert!(knot_points.len() >= n, "{} points", knot_points.len());
            // The zoom: where the warp samples the camera's outermost knot.
            let &(u, angle, expected) = knot_points
                .iter()
                .max_by(|a, b| a.2.total_cmp(&b.2))
                .unwrap();
            let (mut lo, mut hi) = (1.0 - 1e-2, 1.0 + 1e-2);
            for _ in 0..50 {
                let mid = 0.5 * (lo + hi);
                if source(u * mid, angle).unwrap_or(f64::MAX) < expected {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            let zoom = 0.5 * (lo + hi);
            assert!(
                (zoom - 1.0 - zoom_in).abs() < 1.5e-4,
                "n {n} ILC {interchangeable_lens} reach {reach}: zoom {zoom}"
            );
            for (u, angle, expected) in knot_points {
                let got = source(u * zoom, angle).unwrap();
                assert!(
                    (got - expected).abs() * hd < 0.05,
                    "n {n} ILC {interchangeable_lens} reach {reach} u {u} angle {angle}: {} px",
                    (got - expected) * hd
                );
            }
        }
        // Versions 1 to 5 read Sony's data as before.
        let frame = SonyFrame {
            reach: active,
            ..SonyFrame::FULL
        };
        let old = sony_lens(&RX100M3_DISTORTION_VALUES, None, None, frame, 5).unwrap();
        assert_eq!(
            old,
            sony_lens(&RX100M3_DISTORTION_VALUES, None, None, frame, 1).unwrap()
        );
        assert!(old.tabulated.is_none());
    }

    #[test]
    fn sony_interchangeable_lens_bodies_frame_without_a_blank_edge() {
        // As on the ILCE-6100's camera JPEG: A = 1 / max k on the frame's
        // border, the largest frame that samples nothing beyond the sensor.
        let n = 11;
        let k = |u: f64| {
            let curve: Vec<(f64, f64)> = (0..n)
                .map(|i| {
                    let v = RX100M3_DISTORTION_VALUES[1 + i];
                    (i as f64 / (n - 1) as f64, 1.0 + v / 16384.0)
                })
                .collect();
            interpolate(&curve, u)
        };
        let frame = SonyFrame {
            interchangeable_lens: true,
            ..SonyFrame::FULL
        };
        let data = sony_lens(&RX100M3_DISTORTION_VALUES, None, None, frame, 1).unwrap();
        let short = 1.0 / 1.5f64.hypot(1.0);
        let scale = 1.0 / k(short).max(k(1.0));
        assert!(
            (scale - 1.0 / k(short)).abs() < 1e-12,
            "barrel: the short side binds"
        );
        for i in 1..=20 {
            let u = i as f64 / 20.0;
            let expected = scale * u * k(u);
            let got = warp_source_radius(&data, u);
            assert!((got - expected).abs() < 4e-4, "u {u}: {got} vs {expected}");
        }
        // The short side's middle samples the sensor's edge.
        assert!((warp_source_radius(&data, short) - short).abs() < 4e-4);
        // An aspect crop (4:3 of the 3:2 sensor) keeps the sensor's framing.
        let reach = 4.0f64.hypot(3.0) / 4.5f64.hypot(3.0);
        let cropped = SonyFrame {
            reach,
            frame_reach: reach,
            aspect: 4.0 / 3.0,
            ..frame
        };
        let data = sony_lens(&RX100M3_DISTORTION_VALUES, None, None, cropped, 1).unwrap();
        for i in 1..=20 {
            let u = i as f64 / 20.0;
            let expected = scale * u * k(u * reach);
            let got = warp_source_radius(&data, u);
            assert!(
                (got - expected).abs() < 4e-4,
                "4:3 u {u}: {got} vs {expected}"
            );
        }
    }

    #[test]
    fn sony_frames_without_the_pixels_their_framing_samples_keep_version_0() {
        // ZV-1 II at 6.9 mm (as stored): k = 1.024 at a 16:9 frame's top
        // edge, which the camera corrects from the sensor beyond the crop.
        const ZV1M2: [f64; 17] = [
            11.0, 1036.0, 1064.0, 1000.0, 844.0, 585.0, 230.0, -207.0, -702.0, -1218.0, -1728.0,
            -2225.0, 0.0, 0.0, 0.0, 0.0, 0.0,
        ];
        let wide = SonyFrame {
            reach: 5472.0f64.hypot(3080.0) / 5504.0f64.hypot(3672.0),
            frame_reach: 5472.0f64.hypot(3080.0) / 5472.0f64.hypot(3648.0),
            aspect: 5472.0 / 3080.0,
            interchangeable_lens: false,
        };
        let data = sony_lens(&ZV1M2, None, None, wide, 1).unwrap();
        assert_eq!(data, sony_lens(&ZV1M2, None, None, wide, 0).unwrap());
        assert!(!data.exact_zoom);
        // RX100 VI data stay inside the same frame: version 1.
        const RX100M6: [f64; 17] = [
            11.0, -827.0, -821.0, -801.0, -767.0, -719.0, -653.0, -571.0, -468.0, -337.0, -180.0,
            0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
        ];
        assert!(sony_lens(&RX100M6, None, None, wide, 1).unwrap().exact_zoom);
        // Data that need more than the sensor in a full frame are no crop's
        // doing: version 1, framed inside.
        let full = sony_lens(&SONY_DISTORTION_VALUES, None, None, SonyFrame::FULL, 1).unwrap();
        assert!(full.exact_zoom);
    }

    // NX200 + NX 16mm F2.4 (maker note 0xA020 and 0xA057 as stored).
    const NX200_KEY: [f64; 11] = [
        305.0, 72.0, 737.0, 456.0, 282.0, 307.0, 519.0, 724.0, 13.0, 505.0, 193.0,
    ];
    const NX200_STORED: [f64; 8] = [
        -304.0, -72.0, -9313.0, -468.0, 1390.0, -326.0, -519.0, -724.0,
    ];
    const NX200_APPLIED_STORED: [f64; 8] = [
        -736.0, -456.0, -8858.0, -319.0, 1153.0, -743.0, -13.0, -505.0,
    ];

    #[test]
    fn samsung_distortion_is_a_pixel_radius_polynomial() {
        let values = samsung_decrypt(&NX200_STORED, &NX200_KEY, 0).unwrap();
        assert_eq!(values, [1.0, 0.0, -8576.0, -12.0, 1672.0, -19.0, 0.0, 0.0]);
        // 0xA057 continues the key stream after 0xA050, 0xA055 and 0xA056.
        let (applied, start) =
            samsung_model(Some(&NX200_APPLIED_STORED), Some(&NX200_STORED)).unwrap();
        assert_eq!(samsung_decrypt(applied, &NX200_KEY, start).unwrap(), values);
        assert!(samsung_decrypt(&NX200_STORED, &NX200_KEY[..10], 0).is_none());
        // c0 + c1 r² + c2 r⁴ + c3 r⁶, r in pixels: over the 5472 × 3648
        // frame's half-diagonal, exact (no fit), no zoom.
        let half_diagonal = 5472.0f64.hypot(3648.0) / 2.0;
        let data = samsung_curve(&values, half_diagonal).unwrap();
        let [k1, k2, k3] = data.polynomial.unwrap();
        assert!((k1 - -8576e-12 * half_diagonal.powi(2)).abs() < 1e-12);
        assert!((k2 - 1672e-19 * half_diagonal.powi(4)).abs() < 1e-12);
        assert_eq!(k3, 0.0);
        assert!(data.exact_zoom && data.zoom == 1.0 && data.chromatic.is_none());
        let corner = data.distortion.last().unwrap();
        assert!(corner.0 == 1.0 && (corner.1 - (1.0 + k1 + k2)).abs() < 1e-12);
        let params = serde_json::to_value(to_params(&data).unwrap()).unwrap();
        assert_eq!(params["model"], 0);
        assert_eq!(params["exact_zoom"], true);
        assert!((params["k1"].as_f64().unwrap() - k1).abs() < 1e-12);
        // No model: all coefficients zero (NX1, NX500 files), or zero but
        // for c0 (NX11 + 18-55mm at 55 mm: the camera applied none).
        assert!(samsung_curve(&[0.0; 8], half_diagonal).is_none());
        assert!(samsung_curve(&[1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0], half_diagonal).is_none());
        assert!(
            samsung_curve(&[1.0, 0.0, 0.0, -40.0, 0.0, -40.0, 0.0, 0.0], half_diagonal).is_none()
        );
        assert!(
            samsung_curve(
                &[0.0, 0.0, -8576.0, -12.0, 0.0, 0.0, 0.0, 0.0],
                half_diagonal
            )
            .is_none()
        );
        // Garbage: corners moved by far more than any lens.
        assert!(
            samsung_curve(
                &[1.0, 0.0, -8576.0, -10.0, 0.0, 0.0, 0.0, 0.0],
                half_diagonal
            )
            .is_none()
        );
    }

    #[test]
    fn samsung_reads_the_model_the_camera_applied() {
        let recorded = [1.0, 0.0, 2360.0, -12.0, 6256.0, -20.0, 0.0, 0.0];
        let applied = [1.0, 0.0, 0.0, -40.0, 0.0, -40.0, 0.0, 0.0];
        // NX11 + 18-55mm at 55 mm: 0xA050 holds the lens's model, 0xA057
        // none; the camera's JPEG is uncorrected.
        assert_eq!(
            samsung_model(Some(&applied), Some(&recorded)),
            Some((&applied[..], 24))
        );
        // Compacts (EX1, EX2F, WB2000) write no 0xA057: their 0xA050.
        assert_eq!(
            samsung_model(None, Some(&recorded)),
            Some((&recorded[..], 0))
        );
    }

    #[test]
    fn samsung_data_are_read_from_their_geometry_version() {
        use crate::source_facts::{SourceFacts, forget, register};
        let source = Path::new("/offline/volume/Job/Capture/SAM_0001.SRW");
        let path = source.to_string_lossy();
        let params =
            LensDistortionParams::polynomial(-0.09, 0.02, 0.0, [0.0; 3]).with_exact_zoom(true);
        // Facts from before Samsung's version: their (no) data answer the
        // versions before it only.
        register(
            &path,
            SourceFacts {
                version: 7,
                camera_lens: None,
                ..Default::default()
            },
        );
        assert_eq!(current_geometry_version(source), SAMSUNG_GEOMETRY - 1);
        let new = serde_json::json!({GEOMETRY_VERSION_KEY: SAMSUNG_GEOMETRY});
        assert!(!recipe_params_known(source, &new));
        forget(&path);
        register(
            &path,
            SourceFacts {
                version: crate::source_facts::VERSION,
                camera_lens: Some(params.clone()),
                camera_lens_geometry: (0..SAMSUNG_GEOMETRY).map(|v| (v, None)).collect(),
                ..Default::default()
            },
        );
        assert_eq!(params_for_recipe(source, &new), Some(params));
        assert_eq!(params_for_recipe(source, &serde_json::json!({})), None);
        forget(&path);
    }

    #[test]
    fn recipes_keep_the_camera_geometry_version_they_were_made_with() {
        use crate::source_facts::{SourceFacts, VERSION, forget, register};
        let source = Path::new("/offline/volume/Job/Capture/DSC00001.ARW");
        let path = source.to_string_lossy();
        let v1 = LensDistortionParams::ptlens(0.01, -0.02, 0.0, [0.0; 3]).with_zoom(1.05);
        let v0 = LensDistortionParams::ptlens(0.02, -0.03, 0.01, [0.0; 3]);
        register(
            &path,
            SourceFacts {
                version: VERSION,
                camera_lens: Some(v1.clone()),
                camera_lens_geometry: [(0, Some(v0.clone()))].into_iter().collect(),
                ..Default::default()
            },
        );
        let new = serde_json::json!({GEOMETRY_VERSION_KEY: GEOMETRY_VERSION});
        let old = serde_json::json!({});
        assert_eq!(params_for_recipe(source, &new), Some(v1.clone()));
        assert_eq!(params_for_recipe(source, &old), Some(v0.clone()));
        assert!(recipe_params_known(source, &new) && recipe_params_known(source, &old));
        let mut never = serde_json::Value::Null;
        apply_default(&mut never, source);
        assert_eq!(never[GEOMETRY_VERSION_KEY], GEOMETRY_VERSION);
        assert_eq!(geometry_version_of(&never), GEOMETRY_VERSION);
        // Facts from before the geometry version carry version 0's data: a
        // never-edited photo gets no key, and a newer recipe keeps its
        // parameters instead of taking them.
        register(
            &path,
            SourceFacts {
                version: 6,
                camera_lens: Some(v1),
                ..Default::default()
            },
        );
        assert_eq!(current_geometry_version(source), 0);
        assert!(!recipe_params_known(source, &new) && recipe_params_known(source, &old));
        let mut never = serde_json::Value::Null;
        apply_default(&mut never, source);
        assert_eq!(never["lensCorrectionMode"], CAMERA_MODE);
        assert!(never.get(GEOMETRY_VERSION_KEY).is_none());
        forget(&path);
        // Formats the geometry versions read as before: the old facts answer
        // them (Panasonic's and Fujifilm's up to version 4, which read them
        // as version 0).
        for name in ["P1000001.RW2", "DSCF0001.RAF"] {
            let other = Path::new("/offline/volume/Job/Capture").join(name);
            register(
                &other.to_string_lossy(),
                SourceFacts {
                    version: 6,
                    camera_lens: Some(v0.clone()),
                    ..Default::default()
                },
            );
            assert_eq!(current_geometry_version(&other), 4, "{name}");
            let v4_recipe = serde_json::json!({GEOMETRY_VERSION_KEY: 4});
            assert!(recipe_params_known(&other, &v4_recipe), "{name}");
            assert_eq!(params_for_recipe(&other, &v4_recipe), Some(v0.clone()));
            assert!(!recipe_params_known(&other, &new), "{name}");
            forget(&other.to_string_lossy());
        }
        // An ORF's facts answer every version: none reads it differently.
        let orf = Path::new("/offline/volume/Job/Capture/P0000001.ORF");
        register(
            &orf.to_string_lossy(),
            SourceFacts {
                version: 6,
                camera_lens: Some(v0.clone()),
                ..Default::default()
            },
        );
        assert_eq!(current_geometry_version(orf), GEOMETRY_VERSION);
        assert!(recipe_params_known(orf, &new));
        assert_eq!(params_for_recipe(orf, &new), Some(v0.clone()));
        forget(&orf.to_string_lossy());
        // A NEF's facts from before version 2 answer up to version 1 only.
        let nef = Path::new("/offline/volume/Job/Capture/_DSC0001.NEF");
        register(
            &nef.to_string_lossy(),
            SourceFacts {
                version: 7,
                camera_lens: Some(v0.clone()),
                ..Default::default()
            },
        );
        assert_eq!(current_geometry_version(nef), 1);
        assert!(recipe_params_known(
            nef,
            &serde_json::json!({GEOMETRY_VERSION_KEY: 1})
        ));
        assert!(!recipe_params_known(nef, &new));
        forget(&nef.to_string_lossy());
    }

    // Fujifilm X-T5 with XF18mmF2 R (raw.pixls.us; values as stored): nine
    // knots from √(1/8), barrel.
    const X_T5_XF18: [f64; 19] = [
        515.8888889,
        0.3535211268,
        0.5,
        0.6126760563,
        0.7070422535,
        0.7908450704,
        0.8661971831,
        0.9352112676,
        1.0,
        1.06056338,
        -0.772567749,
        -1.670425415,
        -2.579528809,
        -3.496658325,
        -4.48286438,
        -5.54776001,
        -6.70904541,
        -8.008453369,
        -9.307861328,
    ];

    #[test]
    fn fuji_geometry_version_5_runs_from_the_centre_and_frames_the_border() {
        let old = fuji_curves(&X_T5_XF18, None).unwrap();
        let framed = fuji_framed(old.clone(), 1.5).unwrap();
        assert!(framed.exact_zoom && !old.exact_zoom);
        // k(r) = 1 + (−0.7726 %) (r / 0.35352)² inside the first knot,
        // linear in r² between knots.
        let k = |r: f64| fuji_k(&old.distortion, r);
        assert!((k(0.0) - 1.0).abs() < 1e-12);
        assert!((k(0.25) - (1.0 - 0.00772567749 * (0.25f64 / 0.3535211268).powi(2))).abs() < 1e-12);
        let (r0, r1) = (0.5f64, 0.6126760563f64);
        let mid = ((r0 * r0 + r1 * r1) / 2.0).sqrt();
        assert!((k(mid) - (1.0 - 0.01 * (1.670425415 + 2.579528809) / 2.0)).abs() < 1e-12);
        // Barrel data: the 3:2 frame's short-side midpoint (radius 1 / √3.25)
        // samples the sensor's edge, the rest of the border lies inside.
        let short = 1.0 / 3.25f64.sqrt();
        let sample = |u: f64| interpolate(&framed.distortion, u / framed.zoom) / framed.zoom;
        let scale = 1.0 / k(short);
        for u in [short, 0.7, 0.9, 1.0] {
            let expected = u * scale * k(u);
            assert!((u * sample(u) - expected).abs() < 2e-4, "u {u}");
            assert!(u * sample(u) <= u.max(short) + 2e-4);
        }
        assert!((short * sample(short) - short).abs() < 2e-4);
        // Data without distortion stay without it.
        let mut none = X_T5_XF18;
        none[10..].fill(0.0);
        let flat = fuji_framed(fuji_curves(&none, None).unwrap(), 1.5).unwrap();
        assert!((flat.zoom - 1.0).abs() < 1e-9);
    }

    // X-S20 + XF16-80mm at 16 mm (GeometricDistortionParams as stored):
    // a wide lens whose curve the three-term PTLens fit cannot follow.
    const X_S20_XF16_80_AT_16: [f64; 19] = [
        515.8888889,
        0.3535211268,
        0.5,
        0.6126760563,
        0.7070422535,
        0.7908450704,
        0.8661971831,
        0.9352112676,
        1.0,
        1.06056338,
        -1.19178772,
        -2.569900513,
        -3.965667725,
        -5.33253479,
        -6.6512146,
        -7.862289429,
        -8.858123779,
        -9.425109863,
        -9.992095947,
    ];

    #[test]
    fn fuji_geometry_version_7_warps_the_knot_table_itself() {
        // Version 5's PTLens fit of the camera's curve errs by up to ±3.3 px
        // on this 26 MP frame (notes: parity-gaps/G71); version 7 warps
        // through the knots (linear in r², framed at A = 1 / max k on the
        // border), so the warp samples A u k(u) everywhere in the frame.
        let (w, h) = (6240u32, 4160u32);
        let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
        let hd = cx.hypot(cy);
        for (values, wide) in [(&X_S20_XF16_80_AT_16, true), (&X_T5_XF18, false)] {
            let data = fuji_curves(values, None).unwrap();
            let peak = fuji_border_peak(&data.distortion, 1.5);
            let camera = |u: f64| u * fuji_k(&data.distortion, u) / peak;
            let warp_of = |data: &EmbeddedLensData| {
                let recipe = serde_json::json!({
                    "lensCorrectionMode": CAMERA_MODE,
                    "lensDistortionParams": to_params(data).unwrap(),
                });
                crate::image_processing::WarpPoints::new(
                    &crate::image_processing::get_geometry_params_from_json(&recipe),
                    w,
                    h,
                )
            };
            // Points along the long and short axes and the diagonal.
            let points: Vec<(f64, f64)> = (1..=40)
                .flat_map(|i| {
                    let t = i as f64 / 40.0;
                    [
                        (cx + t * (cx - 1.0), cy),
                        (cx, cy - t * (cy - 1.0)),
                        (cx + t * (cx - 1.0), cy + t * (cy - 1.0)),
                    ]
                })
                .collect();
            // The largest radial error after the warp's uniform zoom (its
            // auto-crop may zoom in a hair beyond the camera's framing,
            // where the frame's edge touches the border): (zoom, px).
            let worst = |warp: &crate::image_processing::WarpPoints| {
                let pairs: Vec<(f64, f64)> = points
                    .iter()
                    .map(|&(x, y)| {
                        let (sx, sy) = warp.to_source(x as f32, y as f32).unwrap();
                        let u = (x - cx).hypot(y - cy) / hd;
                        ((sx as f64 - cx).hypot(sy as f64 - cy), camera(u) * hd)
                    })
                    .collect();
                let zoom = pairs.iter().map(|(_, want)| want * want).sum::<f64>()
                    / pairs.iter().map(|(got, want)| got * want).sum::<f64>();
                let error = pairs
                    .iter()
                    .map(|(got, want)| (got * zoom - want).abs())
                    .fold(0.0, f64::max);
                (zoom, error)
            };
            let tabulated = fuji_tabulated(data.clone(), 1.5).unwrap();
            assert_eq!(tabulated.tabulated, Some(Tabulated::Knots));
            let framed = fuji_framed(data.clone(), 1.5).unwrap();
            assert!((tabulated.zoom - framed.zoom).abs() < 1e-12);
            let (zoom, table_error) = worst(&warp_of(&tabulated));
            assert!((zoom - 1.0).abs() < 1.5e-4, "zoom {zoom}");
            assert!(table_error < 0.05, "table {table_error} px");
            if wide {
                let (_, fit_error) = worst(&warp_of(&framed));
                assert!(fit_error > 1.0, "PTLens fit {fit_error} px");
            }
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
        // No camera data: only the current render version, sharpening
        // response and frame version, as a new edit.
        assert_eq!(
            never,
            serde_json::json!({"renderVersion": crate::image_processing::RENDER_VERSION,
                "sharpenVersion": crate::image_processing::SHARPEN_VERSION,
                "sourceFrameVersion": crate::source_frame::VERSION})
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
        // A reset recipe (empty) renders as a new edit.
        let mut reset = serde_json::json!({});
        apply_default(&mut reset, &jpeg);
        assert_eq!(
            reset[crate::image_processing::RENDER_VERSION_KEY],
            crate::image_processing::RENDER_VERSION
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
    use super::*;

    // EOS R6 Mark III + RF 50mm F1.4 L VCM at f/7.1 (raw.pixls.us
    // IMG_4241.CR3), CTMD record 14.
    const BRIGHTNESS: [u16; 17] = [
        8191, 8079, 7767, 7550, 7302, 7028, 6734, 6428, 6275, 6124, 5975, 5889, 5834, 5766, 5692,
        5505, 5095,
    ];
    const RADII: [u16; 17] = [
        0, 757, 1514, 1893, 2271, 2650, 3028, 3407, 3596, 3786, 3975, 4088, 4164, 4259, 4353, 4448,
        4543,
    ];
    const DISTORTION: [u16; 17] = [
        16384, 16379, 16412, 16437, 16466, 16499, 16532, 16562, 16576, 16586, 16594, 16596, 16596,
        16595, 16593, 16588, 16582,
    ];
    // EOS R50 V + RF28mm F2.8 STM (raw.pixls.us IMG_0034.CR3): strong
    // barrel, 20 knots.
    const BARREL_RADII: [u16; 20] = [
        0, 379, 757, 1136, 1514, 1893, 2271, 2650, 3028, 3218, 3407, 3502, 3596, 3691, 3786, 3880,
        3975, 4088, 4164, 4259,
    ];
    const BARREL: [u16; 20] = [
        16384, 16282, 16256, 16202, 16116, 16003, 15863, 15703, 15547, 15478, 15416, 15386, 15355,
        15322, 15285, 15245, 15198, 15132, 15081, 15008,
    ];

    /// A CTMD sample: a type-1 record, then record 14 with `blocks` (each
    /// padded to 23 slots) after the three counts.
    fn sample(counts: [u16; 3], blocks: &[&[u16]]) -> Vec<u8> {
        let mut words = counts.to_vec();
        for block in blocks {
            words.extend_from_slice(block);
            words.extend(std::iter::repeat_n(0, 23 - block.len()));
        }
        let payload: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        let mut sample = Vec::new();
        sample.extend_from_slice(&24u32.to_le_bytes());
        sample.extend_from_slice(&1u16.to_le_bytes());
        sample.extend_from_slice(&[0; 18]);
        sample.extend_from_slice(&((12 + payload.len()) as u32).to_le_bytes());
        sample.extend_from_slice(&14u16.to_le_bytes());
        sample.extend_from_slice(&[0; 6]);
        sample.extend_from_slice(&payload);
        sample
    }

    fn r6_tables() -> CanonLensTables {
        // The two tables in between (around 16384) are not used.
        let other = [16384u16; 17];
        let sample = sample(
            [17, 17, 17],
            &[
                &BRIGHTNESS,
                &RADII,
                &other,
                &other,
                &RADII,
                &DISTORTION,
                &RADII,
            ],
        );
        canon_lens_tables(&sample).expect("tables")
    }

    #[test]
    fn reads_the_brightness_and_distortion_tables_after_other_records() {
        let tables = r6_tables();
        assert_eq!(tables.brightness, BRIGHTNESS);
        assert_eq!(tables.brightness_radii, RADII);
        assert_eq!(tables.distortion, DISTORTION);
        assert_eq!(tables.distortion_radii, RADII);
        // Without record 14: nothing; without the distortion blocks: the
        // brightness alone.
        let other = [16384u16; 17];
        let full = sample(
            [17, 17, 17],
            &[
                &BRIGHTNESS,
                &RADII,
                &other,
                &other,
                &RADII,
                &DISTORTION,
                &RADII,
            ],
        );
        assert!(canon_lens_tables(&full[..24]).is_none());
        let short = canon_lens_tables(&sample([17, 17, 17], &[&BRIGHTNESS, &RADII])).unwrap();
        assert_eq!(short.brightness, BRIGHTNESS);
        assert!(short.distortion.is_empty());
    }

    /// Where the warp's output radius `u` (relative to the half diagonal)
    /// samples the camera's image with the parameters of `data`.
    fn source_radius(data: &EmbeddedLensData, u: f64) -> f64 {
        let params = serde_json::to_value(to_params(data).unwrap()).unwrap();
        let get = |key: &str| params.get(key).and_then(|v| v.as_f64());
        let [a, b, c] = [get("k1").unwrap(), get("k2").unwrap(), get("k3").unwrap()];
        let q = u / get("zoom").unwrap_or(1.0);
        q * (a * q * q * q + b * q * q + c * q + 1.0 - a - b - c)
    }

    #[test]
    fn distortion_is_read_at_geometry_version_3() {
        // R6 Mark III full frame: 6960 x 4640 at 5.16 µm, 21.59 mm half
        // diagonal. Knots lie at whole millimetres when a unit is 1/4096 of
        // the 35 mm frame's half diagonal (757: 4 mm, 4088: 21.6 mm, 4543:
        // 24 mm); the corrected radius u samples the camera's image at
        // u k(u) with k = 1 + 0.8 (value / 16384 - 1), framed so the
        // corners stay on the sensor's corners (k rises to the corner).
        let half_diagonal = 6960f64.hypot(4640.0) / 2.0 * 25.4 / 4922.206506;
        let tables = r6_tables();
        let data = canon_curves(&tables, half_diagonal, 3).unwrap();
        let mm_per_unit = 21.633307652783937 / 4096.0;
        let k = |u: f64| {
            let r = u * half_diagonal / mm_per_unit;
            let knots: Vec<(f64, f64)> = RADII
                .iter()
                .zip(DISTORTION)
                .map(|(&r, v)| (f64::from(r), f64::from(v) / 16384.0))
                .collect();
            1.0 + 0.8 * (interpolate(&knots, r) - 1.0)
        };
        assert!(k(1.0) > 1.0);
        assert_eq!(data.zoom, 1.0);
        for i in 1..=20 {
            let u = i as f64 / 20.0;
            let expected = u * k(u) / k(1.0);
            let got = source_radius(&data, u);
            // 1e-4 of the half diagonal: 0.4 px.
            assert!((got - expected).abs() < 1e-4, "u {u}: {got} vs {expected}");
        }
        // The brightness table on the same radii.
        let vignetting = data.vignetting.as_ref().unwrap();
        assert!((vignetting[1].0 - 757.0 * mm_per_unit / half_diagonal).abs() < 1e-12);
        // Older versions: no distortion, the radii in 5 µm as before.
        for geometry in 0..3 {
            let old = canon_curves(&tables, half_diagonal, geometry).unwrap();
            assert_eq!(old.distortion, vec![(0.0, 1.0), (1.0, 1.0)]);
            assert_eq!(
                old.vignetting.as_ref().unwrap()[1].0,
                757.0 * 0.005 / half_diagonal
            );
        }
    }

    #[test]
    fn the_distortion_share_reproduces_the_cameras_own_correction() {
        // Held out from choosing CANON_DISTORTION_SHARE (fitted on Camera
        // Raw's fields of an R5 Mark II and an R6 Mark III): the R50 V's own
        // corrected JPEG of raw.pixls.us IMG_0034.CR3 (RF 28 mm, in-camera
        // distortion correction on) against our uncorrected render, binned
        // (notes: benchmark-results/g71c-canon-2026-10-26): output radius,
        // and the radius it shows from the sensor over it, relative to the
        // 6000 x 4000 frame's half diagonal (3606 px).
        const MEASURED: [(f64, f64); 8] = [
            (0.2027, 0.99476),
            (0.3151, 0.99350),
            (0.4054, 0.99184),
            (0.4926, 0.98982),
            (0.5371, 0.98854),
            (0.6429, 0.98537),
            (0.7164, 0.98245),
            (0.8109, 0.97848),
        ];
        let half_diagonal_px = 6000f64.hypot(4000.0) / 2.0;
        let half_diagonal = half_diagonal_px * 25.4 / 6825.938567;
        let brightness = [8191u16; 20];
        let other = [16384u16; 20];
        let sample = sample(
            [20, 20, 20],
            &[
                &brightness,
                &BARREL_RADII,
                &other,
                &other,
                &BARREL_RADII,
                &BARREL,
                &BARREL_RADII,
            ],
        );
        let data = canon_curves(&canon_lens_tables(&sample).unwrap(), half_diagonal, 3).unwrap();
        for (u, ratio) in MEASURED {
            let error = (source_radius(&data, u) - u * ratio).abs() * half_diagonal_px;
            assert!(error < 0.5, "u {u}: {error} px");
        }
    }

    #[test]
    fn barrel_data_keep_the_cameras_framing() {
        // R50 V (APS-C, 6000 x 4000 at 3.72 µm) + RF 28: the corrected
        // corners sample inside the sensor's corners, as in the camera's
        // own corrected JPEG: u samples u k(u), zoomed in by the radius
        // that reaches the corner.
        let half_diagonal = 6000f64.hypot(4000.0) / 2.0 * 25.4 / 6825.938567;
        let brightness = [8191u16; 20];
        let other = [16384u16; 20];
        let sample = sample(
            [20, 20, 20],
            &[
                &brightness,
                &BARREL_RADII,
                &other,
                &other,
                &BARREL_RADII,
                &BARREL,
                &BARREL_RADII,
            ],
        );
        let tables = canon_lens_tables(&sample).unwrap();
        let data = canon_curves(&tables, half_diagonal, 3).unwrap();
        assert!(data.zoom > 1.0 && data.exact_zoom);
        let mm_per_unit = 21.633307652783937 / 4096.0;
        let k = |u: f64| {
            let r = u * half_diagonal / mm_per_unit;
            let knots: Vec<(f64, f64)> = BARREL_RADII
                .iter()
                .zip(BARREL)
                .map(|(&r, v)| (f64::from(r), f64::from(v) / 16384.0))
                .collect();
            1.0 + 0.8 * (interpolate(&knots, r) - 1.0)
        };
        for i in 1..=20 {
            let u = i as f64 / 20.0;
            let got = source_radius(&data, u);
            assert!(
                (got - u * k(u)).abs() < 2e-4,
                "u {u}: {got} vs {}",
                u * k(u)
            );
        }
    }
}
