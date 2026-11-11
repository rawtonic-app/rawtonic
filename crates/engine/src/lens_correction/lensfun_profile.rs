//! Lensfun database profiles under the database's own conventions
//! (`LensDistortionParams::convention` [`CONVENTION`], 2026-11-12): what
//! Lensfun's modifier applies, in the warp's units, without the
//! approximations of `Lens::get_distortion_params` (which pinned recipes
//! keep). Written from Lensfun's documented conventions (its comments and
//! measured output, notes: benchmark-results/lfc-lensfun-conventions-2026-11-12
//! and lfb-lens-maths-2026-11-12); no Lensfun code.
//!
//! - Distortion keeps the profile's own scale: Lensfun rescales a PTLens or
//!   poly3 profile so the corrected image keeps the focal length (the
//!   undistorted radius divided by d = 1 − a − b − c), which the warp's
//!   model 1 normalises away by mapping corner to corner. A PTLens profile
//!   becomes model [`MODEL_PTLENS_UNSCALED`], poly3 and poly5 the even
//!   polynomial (model 0), each exact.
//! - Lateral CA keeps poly3's linear radius term (`tca_vr1`, `tca_vb1`).
//! - Distortion and CA are interpolated over the focal length with
//!   Lensfun's cubic Hermite spline through the two calibrations on either
//!   side, on coefficients multiplied by their focal length (they follow
//!   1/f); vignetting jointly over focal length, aperture and distance by
//!   inverse distance weighting (power 3.5) in normalised focal length,
//!   4 / aperture and 0.1 / distance, and none where the nearest
//!   calibration is farther than 1 in those units.
//! - The distortion runs about the middle of the pixel grid, as Lensfun's
//!   (`image_processing::lens_centre`; earlier warps use width / 2).
//! - The warp fills the frame exactly (`convention` ≥ 1:
//!   `image_processing::compute_lens_auto_crop_scale` scans every border
//!   pixel, no margin, never below 1) — Rawtonic's framing, not Lensfun's
//!   auto scale.
use super::{CalibrationElement, Distortion, Lens, LensDistortionParams, Tca, Vignetting};

/// `LensDistortionParams::convention` of profiles resolved here.
pub const CONVENTION: u32 = 1;

/// The warp's PTLens model without the corner normalisation: an output
/// radius r (relative to the half diagonal) samples r (1 + k3 r + k2 r² +
/// k1 r³).
pub const MODEL_PTLENS_UNSCALED: u32 = 4;

/// Lensfun's default focus distance where the photo states none (metres).
const DEFAULT_DISTANCE: f32 = 1000.0;
/// The aperture assumed where the photo states none (as
/// `get_distortion_params`).
const DEFAULT_APERTURE: f32 = 3.5;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DistortionModel {
    Poly3,
    Poly5,
    PtLens,
}

fn distortion_terms(d: &Distortion) -> Option<(DistortionModel, [f64; 3])> {
    let v = |x: Option<f32>| x.map_or(0.0, f64::from);
    match d.model.as_str() {
        "poly3" => Some((DistortionModel::Poly3, [v(d.k1), 0.0, 0.0])),
        "poly5" => Some((DistortionModel::Poly5, [v(d.k1), v(d.k2), 0.0])),
        "ptlens" => Some((DistortionModel::PtLens, [v(d.a), v(d.b), v(d.c)])),
        _ => None,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TcaModel {
    Linear,
    Poly3,
}

/// Lateral CA terms in the database's order: linear (kr, kb), poly3 (vr,
/// vb, cr, cb, br, bb).
fn tca_terms(t: &Tca) -> Option<(TcaModel, [f64; 6])> {
    let v = |x: Option<f32>, default: f64| x.map_or(default, f64::from);
    match t.model.as_str() {
        "linear" => Some((
            TcaModel::Linear,
            [v(t.kr, 1.0), v(t.kb, 1.0), 0.0, 0.0, 0.0, 0.0],
        )),
        "poly3" => Some((
            TcaModel::Poly3,
            [
                v(t.vr, 1.0),
                v(t.vb, 1.0),
                v(t.cr, 0.0),
                v(t.cb, 0.0),
                v(t.br, 0.0),
                v(t.bb, 0.0),
            ],
        )),
        _ => None,
    }
}

/// The cubic Hermite segment between `y1` (t = 0) and `y2` (t = 1) whose
/// tangents are the central differences over the neighbours `y0`, `y3`
/// where they exist, else the segment's own difference.
fn hermite(y0: Option<f64>, y1: f64, y2: f64, y3: Option<f64>, t: f64) -> f64 {
    let m1 = y0.map_or(y2 - y1, |y0| 0.5 * (y2 - y0));
    let m2 = y3.map_or(y2 - y1, |y3| 0.5 * (y3 - y1));
    let (t2, t3) = (t * t, t * t * t);
    (2.0 * t3 - 3.0 * t2 + 1.0) * y1
        + (t3 - 2.0 * t2 + t) * m1
        + (-2.0 * t3 + 3.0 * t2) * y2
        + (t3 - t2) * m2
}

/// Terms at `focal` from calibrations (focal, terms) of one model: a
/// calibration at exactly `focal` as it is; between calibrations the
/// Hermite spline through the two nearest on either side, on terms
/// multiplied by their focal length where `per_focal` says so (and divided
/// by `focal` after); outside them the nearest.
fn interpolate_over_focal<const N: usize>(
    knots: &[(f32, [f64; N])],
    focal: f32,
    per_focal: impl Fn(usize) -> bool,
) -> Option<[f64; N]> {
    if let Some((_, terms)) = knots.iter().find(|(f, _)| *f == focal) {
        return Some(*terms);
    }
    // The two nearest below and above (the first of equally near ones).
    let nearest = |below: bool| {
        let mut side: Vec<&(f32, [f64; N])> = knots
            .iter()
            .filter(|(f, _)| if below { *f < focal } else { *f > focal })
            .collect();
        side.sort_by(|a, b| {
            let (da, db) = ((focal - a.0).abs(), (focal - b.0).abs());
            da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
        });
        (side.first().copied(), side.get(1).copied())
    };
    let (below, below2) = nearest(true);
    let (above, above2) = nearest(false);
    let (lo, hi) = match (below, above) {
        (Some(lo), Some(hi)) => (lo, hi),
        (Some(only), None) | (None, Some(only)) => return Some(only.1),
        (None, None) => return None,
    };
    let t = f64::from(focal - lo.0) / f64::from(hi.0 - lo.0);
    Some(std::array::from_fn(|i| {
        let scale = |f: f32| if per_focal(i) { f64::from(f) } else { 1.0 };
        let y = |k: &(f32, [f64; N])| k.1[i] * scale(k.0);
        hermite(below2.map(y), y(lo), y(hi), above2.map(y), t) / scale(focal)
    }))
}

/// Calibrations of one model: (focal length, terms).
type Knots<const N: usize> = Vec<(f32, [f64; N])>;

/// The calibrations of the first model among `items` (Lensfun takes the
/// first model a lens names and skips calibrations of another).
fn first_model<T, M: PartialEq + Copy, const N: usize>(
    items: impl Iterator<Item = T>,
    terms: impl Fn(T) -> Option<(f32, M, [f64; N])>,
) -> Option<(M, Knots<N>)> {
    let mut model = None;
    let mut knots = Vec::new();
    for (focal, m, t) in items.filter_map(terms) {
        match model {
            None => model = Some(m),
            Some(first) if first != m => continue,
            _ => {}
        }
        knots.push((focal, t));
    }
    model.map(|m| (m, knots))
}

/// The leading decimal number of `text` as C's `atof` reads it (0 when it
/// has none).
fn leading_number(text: &str) -> f64 {
    let end = text
        .char_indices()
        .scan(false, |dot, (i, c)| match c {
            '0'..='9' => Some(i + 1),
            '.' if !*dot => {
                *dot = true;
                Some(i + 1)
            }
            _ => None,
        })
        .last()
        .unwrap_or(0);
    text[..end].parse().unwrap_or(0.0)
}

/// The focal range a lens name states (Lensfun's naming conventions:
/// "24-70mm f/2.8", "1:2.8 24-70mm", "2.8/24-70"), None for adapters and
/// converters.
fn focal_range_of_name(name: &str) -> Option<(Option<f64>, Option<f64>)> {
    use std::sync::LazyLock;
    static ACCESSORY: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"[0-9](?:\.[0.9]+)?x").unwrap());
    // (expression, group of the shortest focal length, of the longest).
    static NAMES: LazyLock<[(regex::Regex, usize, usize); 3]> = LazyLock::new(|| {
        let re = |s: &str| regex::Regex::new(s).unwrap();
        [
            (
                re(
                    r"^[^:]*?([0-9]+[0-9.]*)-?([0-9]+[0-9.]*)?mm[[:space:]]+(?:f/|f|1/|1:)?[0-9.]+(?:-[0-9.]+)?",
                ),
                1,
                2,
            ),
            (
                re(r"^.*?1:[0-9.]+-?(?:[0-9.]+)?[[:space:]]+([0-9.]+)-?([0-9.]+)?"),
                1,
                2,
            ),
            (
                re(r"^.*?[0-9.]+-?(?:[0-9.]+)?[[:space:]]*/[[:space:]]*([0-9.]+)-?([0-9.]+)?"),
                1,
                2,
            ),
        ]
    });
    let accessory = [
        "adapter",
        "reducer",
        "booster",
        "extender",
        "converter",
        "magnifier",
    ];
    if accessory.iter().any(|a| name.contains(a)) || ACCESSORY.is_match(name) {
        return None;
    }
    NAMES.iter().find_map(|(re, min, max)| {
        let caps = re.captures(name)?;
        let number = |g: usize| caps.get(g).map(|m| leading_number(m.as_str()));
        Some((number(*min), number(*max)))
    })
}

impl Lens {
    /// The lens's focal range as Lensfun sets it: its `<focal>` element,
    /// else the range its name states extended by its calibrations' focal
    /// lengths (what vignetting interpolation normalises focal lengths by).
    pub(crate) fn focal_range(&self) -> (f64, f64) {
        let given = self.focal.as_ref();
        let given_value = given.and_then(|f| f.value).map(f64::from);
        let given_min = given.and_then(|f| f.min).map(f64::from).or(given_value);
        let given_max = given.and_then(|f| f.max).map(f64::from).or(given_value);
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        if let Some((min, max)) = focal_range_of_name(&self.get_canonical_model_name()) {
            if let Some(min) = min {
                lo = min;
            }
            if let Some(max) = max {
                hi = max;
            }
        }
        for element in self.calibration.iter().flat_map(|c| &c.elements) {
            let focal = f64::from(match element {
                CalibrationElement::Distortion(d) => d.focal,
                CalibrationElement::Tca(t) => t.focal,
                CalibrationElement::Vignetting(v) => v.focal,
            });
            lo = lo.min(focal);
            hi = hi.max(focal);
        }
        let min = given_min
            .filter(|f| *f != 0.0)
            .or(lo.is_finite().then_some(lo))
            .unwrap_or(0.0);
        let max = given_max
            .filter(|f| *f != 0.0)
            .or(hi.is_finite().then_some(hi))
            .filter(|f| *f != 0.0)
            .unwrap_or(min);
        (min, max)
    }

    /// The pa vignetting terms (k1, k2, k3, Lensfun's units) at the shot's
    /// focal length, aperture and distance: inverse distance weighting over
    /// the calibrations, None when the nearest is too far.
    fn vignetting_at(&self, focal: f32, aperture: f32, distance: f32) -> Option<[f64; 3]> {
        let calibrations: Vec<&Vignetting> = self
            .calibration
            .iter()
            .flat_map(|c| &c.elements)
            .filter_map(|e| match e {
                CalibrationElement::Vignetting(v) => Some(v),
                _ => None,
            })
            .collect();
        let first = calibrations.first()?.model.clone();
        let (min, max) = self.focal_range();
        let span = max - min;
        let normalised = |f: f64| {
            if span != 0.0 {
                (f - min) / span
            } else {
                f - min
            }
        };
        let (f1, a1, d1) = (
            normalised(f64::from(focal)),
            4.0 / f64::from(aperture),
            0.1 / f64::from(distance),
        );
        let mut terms = [0.0; 3];
        let (mut total, mut nearest) = (0.0, f64::INFINITY);
        for v in calibrations.iter().filter(|v| v.model == first) {
            let separation = (normalised(f64::from(v.focal)) - f1)
                .hypot(4.0 / f64::from(v.aperture) - a1)
                .hypot(0.1 / f64::from(v.distance.unwrap_or(0.0)) - d1);
            let own = [v.k1, v.k2, v.k3].map(|k| k.map_or(0.0, f64::from));
            if separation < 1e-4 {
                return Some(own);
            }
            nearest = nearest.min(separation);
            let weight = separation.powf(-3.5);
            if !weight.is_finite() {
                continue;
            }
            for (sum, k) in terms.iter_mut().zip(own) {
                *sum += weight * k;
            }
            total += weight;
        }
        (nearest <= 1.0 && total > 0.0).then(|| terms.map(|t| t / total))
    }

    /// The profile at the shot's focal length, aperture and distance under
    /// Lensfun's conventions ([`CONVENTION`]), in the warp's units: radius 1
    /// = the photo's half-diagonal. `camera_crop` is the frame's crop factor
    /// (the lens's own when unknown).
    pub fn profile_params(
        &self,
        focal: f32,
        aperture: Option<f32>,
        distance: Option<f32>,
        camera_crop: Option<f32>,
    ) -> Option<LensDistortionParams> {
        let elements = &self.calibration.as_ref()?.elements;
        let lens_crop = self
            .cropfactor
            .filter(|c| c.is_finite() && *c > 0.0)
            .map(f64::from)
            .unwrap_or(1.0);
        let camera_crop = camera_crop
            .filter(|c| c.is_finite() && *c > 0.0)
            .map(f64::from)
            .unwrap_or(lens_crop);
        let aspect = self
            .aspect_ratio
            .as_deref()
            .and_then(super::parse_aspect_ratio)
            .unwrap_or(1.5);
        // Hugin's radius per photo radius: distortion and CA measure from
        // half the shorter side of a calibration-aspect frame, vignetting
        // from its half diagonal; both at the calibration's crop factor.
        let vignetting_scale = lens_crop / camera_crop;
        let s = vignetting_scale * aspect.hypot(1.0);

        let mut params = LensDistortionParams::ptlens(0.0, 0.0, 0.0, [0.0; 3]);
        params.model = 0;
        params.convention = CONVENTION;

        let distortion = first_model(elements.iter(), |e| match e {
            CalibrationElement::Distortion(d) => distortion_terms(d).map(|(m, t)| (d.focal, m, t)),
            _ => None,
        });
        if let Some((model, knots)) = distortion
            && let Some([a, b, c]) = interpolate_over_focal(&knots, focal, |_| true)
        {
            match model {
                DistortionModel::PtLens => {
                    let d = 1.0 - a - b - c;
                    params.model = MODEL_PTLENS_UNSCALED;
                    params.k1 = a * s.powi(3) / d.powi(4);
                    params.k2 = b * s.powi(2) / d.powi(3);
                    params.k3 = c * s / d.powi(2);
                }
                DistortionModel::Poly3 => {
                    let d = 1.0 - a;
                    params.k1 = a * s * s / d.powi(3);
                }
                DistortionModel::Poly5 => {
                    params.k1 = a * s.powi(2);
                    params.k2 = b * s.powi(4);
                }
            }
            if ![params.k1, params.k2, params.k3]
                .iter()
                .all(|k| k.is_finite())
            {
                (params.model, params.k1, params.k2, params.k3) = (0, 0.0, 0.0, 0.0);
            }
        }

        let tca = first_model(elements.iter(), |e| match e {
            CalibrationElement::Tca(t) => tca_terms(t).map(|(m, terms)| (t.focal, m, terms)),
            _ => None,
        });
        if let Some((_, knots)) = tca
            && let Some([vr, vb, cr, cb, br, bb]) =
                interpolate_over_focal(&knots, focal, |i| i >= 2)
        {
            params.tca_vr = vr;
            params.tca_vb = vb;
            params.tca_vr1 = cr * s;
            params.tca_vb1 = cb * s;
            params.tca_vr2 = br * s * s;
            params.tca_vb2 = bb * s * s;
        }

        if let Some([k1, k2, k3]) = self.vignetting_at(
            focal,
            aperture.unwrap_or(DEFAULT_APERTURE),
            distance.unwrap_or(DEFAULT_DISTANCE),
        ) {
            let s2 = vignetting_scale * vignetting_scale;
            params.vig_k1 = k1 * s2;
            params.vig_k2 = k2 * s2 * s2;
            params.vig_k3 = k3 * s2 * s2 * s2;
        }
        Some(params)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lens_correction::LensDatabase;

    fn lens(xml: &str) -> Lens {
        quick_xml::de::from_str::<LensDatabase>(&format!("<lensdatabase>{xml}</lensdatabase>"))
            .unwrap()
            .lenses
            .remove(0)
    }

    #[test]
    fn names_state_focal_ranges_as_lensfun_reads_them() {
        let range = focal_range_of_name;
        assert_eq!(
            range("Tamron AF 80-210mm f/4.5-5.6 280D"),
            Some((Some(80.0), Some(210.0)))
        );
        assert_eq!(range("Canon EF 50mm f/1.8 STM"), Some((Some(50.0), None)));
        // Upper-case F is not an aperture there.
        assert_eq!(range("M.Zuiko Digital ED 12-40mm F2.8 Pro"), None);
        assert_eq!(range("Summicron-TL 1:2/23 Asph."), Some((Some(23.0), None)));
        assert_eq!(
            range("Elmarit-M 1:2.8 21-28 mm"),
            Some((Some(21.0), Some(28.0)))
        );
        assert_eq!(range("Canon Extender EF 1.4x"), None);
        assert_eq!(range("Sony VCL-ECF1 fisheye converter"), None);
    }

    #[test]
    fn the_focal_range_extends_the_names_by_the_calibrations() {
        let zoom = lens(
            r#"<lens><maker>T</maker><model>T 80-210mm f/4.5-5.6</model><mount>M</mount>
            <calibration><vignetting model="pa" focal="85" aperture="4.5" distance="10" k1="-0.1"/>
            <vignetting model="pa" focal="200" aperture="5.6" distance="10" k1="-0.2"/></calibration></lens>"#,
        );
        assert_eq!(zoom.focal_range(), (80.0, 210.0));
        let named_short = lens(
            r#"<lens><maker>T</maker><model>T 8mm f/2.8</model><mount>M</mount>
            <calibration><vignetting model="pa" focal="8.4" aperture="2.8" distance="10" k1="-0.1"/></calibration></lens>"#,
        );
        assert_eq!(named_short.focal_range(), (8.0, f64::from(8.4f32)));
        let given = lens(
            r#"<lens><maker>T</maker><model>T 24-70mm f/2.8</model><mount>M</mount><focal min="24" max="70"/>
            <calibration><vignetting model="pa" focal="35" aperture="2.8" distance="10" k1="-0.1"/></calibration></lens>"#,
        );
        assert_eq!(given.focal_range(), (24.0, 70.0));
    }

    /// The spline passes through its calibrations and, between two, follows
    /// a 1/f coefficient exactly where the neighbours carry it on (k f is
    /// constant).
    #[test]
    fn distortion_follows_the_focal_length_as_lensfun_interpolates_it() {
        let knots: Vec<(f32, [f64; 1])> = [16.0f32, 24.0, 35.0, 50.0]
            .iter()
            .map(|&f| (f, [0.7 / f64::from(f)]))
            .collect();
        for f in [16.0f32, 20.0, 30.0, 42.0, 50.0] {
            let [k] = interpolate_over_focal(&knots, f, |_| true).unwrap();
            assert!((k - 0.7 / f64::from(f)).abs() < 1e-12, "{f}: {k}");
        }
        // Outside the calibrations: the nearest one.
        assert_eq!(
            interpolate_over_focal(&knots, 10.0, |_| true),
            Some([0.7 / 16.0])
        );
        // Unscaled terms (CA's constant scale) run linearly between two.
        let two: Vec<(f32, [f64; 1])> = vec![(20.0, [1.0]), (40.0, [2.0])];
        assert_eq!(interpolate_over_focal(&two, 30.0, |_| false), Some([1.5]));
    }

    #[test]
    fn vignetting_weighs_every_calibration_by_its_distance() {
        let l = lens(
            r#"<lens><maker>T</maker><model>T 50mm f/1.8</model><mount>M</mount>
            <calibration>
            <vignetting model="pa" focal="50" aperture="2" distance="10" k1="-0.4" k2="0" k3="0"/>
            <vignetting model="pa" focal="50" aperture="4" distance="10" k1="-0.2" k2="0" k3="0"/>
            </calibration></lens>"#,
        );
        // On a calibration: its own terms.
        assert_eq!(
            l.vignetting_at(50.0, 4.0, 10.0),
            Some([f64::from(-0.2f32), 0.0, 0.0])
        );
        // Between f/2 and f/4: 4 / aperture 1.25 lies 0.75 from f/2's 2 and
        // 0.25 from f/4's 1.
        let [k1, ..] = l.vignetting_at(50.0, 3.2, 10.0).unwrap();
        let (w2, w4) = (0.75f64.powf(-3.5), 0.25f64.powf(-3.5));
        assert!(
            (k1 - (f64::from(-0.4f32) * w2 + f64::from(-0.2f32) * w4) / (w2 + w4)).abs() < 1e-6,
            "{k1}"
        );
        // f/32 (4 / aperture 0.125) lies 0.875 from f/4: still weighted;
        // f/0.5 (8) lies 6 from f/2: no vignetting correction.
        assert!(l.vignetting_at(50.0, 32.0, 10.0).is_some());
        assert_eq!(l.vignetting_at(50.0, 0.5, 10.0), None);
    }

    /// A PTLens profile keeps its own scale: the corner's displacement is
    /// Lensfun's, not normalised to zero; poly3's linear CA term stays.
    #[test]
    fn profiles_keep_their_scale_and_linear_ca_term() {
        let l = lens(
            r#"<lens><maker>T</maker><model>T 28mm f/2.8</model><mount>M</mount>
            <cropfactor>1.0</cropfactor><aspect-ratio>3:2</aspect-ratio>
            <calibration>
            <distortion model="ptlens" focal="28" a="0.023" b="-0.084" c="0.053"/>
            <tca model="poly3" focal="28" vr="1.0003" vb="0.9998" cr="0.0004" cb="-0.0002" br="0.0001" bb="0"/>
            </calibration></lens>"#,
        );
        let p = l.profile_params(28.0, Some(2.8), None, Some(1.0)).unwrap();
        assert_eq!((p.model, p.convention), (MODEL_PTLENS_UNSCALED, CONVENTION));
        let s = 1.5f64.hypot(1.0);
        let (a, b, c) = (
            f64::from(0.023f32),
            f64::from(-0.084f32),
            f64::from(0.053f32),
        );
        let d = 1.0 - a - b - c;
        // The corner (r = 1) samples Hugin's distortion of the undistorted
        // radius s / d, back in photo units.
        let x = s / d;
        let hugin = x * (a * x.powi(3) + b * x * x + c * x + d);
        let warp = 1.0 + p.k3 + p.k2 + p.k1;
        assert!((warp - hugin / s).abs() < 1e-12, "{warp} {}", hugin / s);
        assert!((p.tca_vr1 - f64::from(0.0004f32) * s).abs() < 1e-12);
        assert!((p.tca_vb1 + f64::from(0.0002f32) * s).abs() < 1e-12);
        assert!((p.tca_vr2 - f64::from(0.0001f32) * s * s).abs() < 1e-12);
    }
}
