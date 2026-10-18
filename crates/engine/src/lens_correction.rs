use fuzzy_matcher::FuzzyMatcher;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::fs;
use walkdir::WalkDir;

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct Distortion {
    #[serde(rename = "@model")]
    pub model: String,
    #[serde(rename = "@focal")]
    pub focal: f32,
    #[serde(rename = "@real-focal")]
    pub real_focal: Option<f32>,
    #[serde(rename = "@k1")]
    pub k1: Option<f32>,
    #[serde(rename = "@k2")]
    pub k2: Option<f32>,
    #[serde(rename = "@k3")]
    pub k3: Option<f32>,
    #[serde(rename = "@a")]
    pub a: Option<f32>,
    #[serde(rename = "@b")]
    pub b: Option<f32>,
    #[serde(rename = "@c")]
    pub c: Option<f32>,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct Tca {
    #[serde(rename = "@model")]
    pub model: String,
    #[serde(rename = "@focal")]
    pub focal: f32,
    #[serde(rename = "@vr")]
    pub vr: Option<f32>,
    #[serde(rename = "@vb")]
    pub vb: Option<f32>,
    #[serde(rename = "@cr")]
    pub cr: Option<f32>,
    #[serde(rename = "@cb")]
    pub cb: Option<f32>,
    #[serde(rename = "@br")]
    pub br: Option<f32>,
    #[serde(rename = "@bb")]
    pub bb: Option<f32>,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct Vignetting {
    #[serde(rename = "@model")]
    pub model: String,
    #[serde(rename = "@focal")]
    pub focal: f32,
    #[serde(rename = "@aperture")]
    pub aperture: f32,
    #[serde(rename = "@distance")]
    pub distance: Option<f32>,
    #[serde(rename = "@k1")]
    pub k1: Option<f32>,
    #[serde(rename = "@k2")]
    pub k2: Option<f32>,
    #[serde(rename = "@k3")]
    pub k3: Option<f32>,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum CalibrationElement {
    Distortion(Distortion),
    Tca(Tca),
    Vignetting(Vignetting),
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct Calibration {
    #[serde(rename = "$value", default)]
    pub elements: Vec<CalibrationElement>,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct Focal {
    #[serde(rename = "@value")]
    pub value: Option<f32>,
    #[serde(rename = "@min")]
    pub min: Option<f32>,
    #[serde(rename = "@max")]
    pub max: Option<f32>,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct Aperture {
    #[serde(rename = "@min")]
    pub min: Option<f32>,
    #[serde(rename = "@max")]
    pub max: Option<f32>,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub struct Lens {
    #[serde(default)]
    pub maker: Vec<MultiName>,
    #[serde(default)]
    pub model: Vec<MultiName>,
    #[serde(default)]
    pub mount: Vec<String>,
    pub cropfactor: Option<f32>,
    pub calibration: Option<Calibration>,
    #[serde(rename = "type")]
    pub type_: Option<String>,
    pub focal: Option<Focal>,
    pub aspect_ratio: Option<String>,
    pub center: Option<String>,
    pub compat: Option<String>,
    pub notes: Option<String>,
    pub aperture: Option<Aperture>,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub struct Camera {
    pub maker: Vec<MultiName>,
    pub model: Vec<MultiName>,
    pub mount: String,
    pub cropfactor: f32,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct LensDatabase {
    #[serde(rename = "camera", default)]
    pub cameras: Vec<Camera>,
    #[serde(rename = "lens", default)]
    pub lenses: Vec<Lens>,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct MultiName {
    #[serde(rename = "@lang")]
    lang: Option<String>,
    #[serde(rename = "$value")]
    value: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct LensDistortionParams {
    k1: f64,
    k2: f64,
    k3: f64,
    model: u32,
    tca_vr: f64,
    tca_vb: f64,
    vig_k1: f64,
    vig_k2: f64,
    vig_k3: f64,
    /// Minimum zoom of the corrected frame: the camera's own framing for
    /// models that crop inside the sensor corners (Panasonic, Olympus).
    #[serde(default = "unit_zoom", skip_serializing_if = "is_unit_zoom")]
    zoom: f64,
    /// Growth of the red and blue scales with the squared radius relative
    /// to the half diagonal (the camera's own CA data, embedded_lens.rs).
    #[serde(default, skip_serializing_if = "is_zero")]
    tca_vr2: f64,
    #[serde(default, skip_serializing_if = "is_zero")]
    tca_vb2: f64,
}

fn is_zero(value: &f64) -> bool {
    value.abs() < 1e-12
}

fn unit_zoom() -> f64 {
    1.0
}

fn is_unit_zoom(zoom: &f64) -> bool {
    (*zoom - 1.0).abs() < 1e-9
}

impl LensDistortionParams {
    /// A PTLens distortion (model 1) with pa-model vignetting and no TCA, as
    /// fitted from a camera's own correction data (embedded_lens.rs).
    pub fn ptlens(a: f64, b: f64, c: f64, vignetting: [f64; 3]) -> Self {
        Self {
            k1: a,
            k2: b,
            k3: c,
            model: 1,
            tca_vr: 1.0,
            tca_vb: 1.0,
            vig_k1: vignetting[0],
            vig_k2: vignetting[1],
            vig_k3: vignetting[2],
            zoom: 1.0,
            tca_vr2: 0.0,
            tca_vb2: 0.0,
        }
    }

    /// The same parameters with lateral CA: red and blue scaled to green by
    /// `1 + a + b r²` ((a, b) per channel, r relative to the half diagonal).
    pub fn with_tca(mut self, red: (f64, f64), blue: (f64, f64)) -> Self {
        self.tca_vr = 1.0 + red.0;
        self.tca_vr2 = red.1;
        self.tca_vb = 1.0 + blue.0;
        self.tca_vb2 = blue.1;
        self
    }

    /// The same parameters without lateral CA.
    pub fn without_tca(self) -> Self {
        Self {
            tca_vr: 1.0,
            tca_vb: 1.0,
            tca_vr2: 0.0,
            tca_vb2: 0.0,
            ..self
        }
    }

    /// No distortion, vignetting, CA or zoom: the warp leaves the image as
    /// it is.
    pub fn is_identity(&self) -> bool {
        [
            self.k1,
            self.k2,
            self.k3,
            self.vig_k1,
            self.vig_k2,
            self.vig_k3,
        ]
        .iter()
        .all(|v| v.abs() < 1e-12)
            && (self.tca_vr - 1.0).abs() < 1e-12
            && (self.tca_vb - 1.0).abs() < 1e-12
            && self.tca_vr2.abs() < 1e-12
            && self.tca_vb2.abs() < 1e-12
            && is_unit_zoom(&self.zoom)
    }

    /// The same parameters with a minimum zoom (at least 1).
    pub fn with_zoom(mut self, zoom: f64) -> Self {
        self.zoom = if zoom.is_finite() { zoom.max(1.0) } else { 1.0 };
        self
    }
}

fn strip_maker_prefix(name: &str, maker: &str) -> String {
    if name.to_lowercase().starts_with(&maker.to_lowercase())
        && let Some(rest) = name.get(maker.len()..)
    {
        let trimmed = rest.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    name.to_string()
}

fn pick_name(names: &[MultiName], fallback: &str) -> String {
    names
        .iter()
        .find(|n| n.lang.as_deref() == Some("en"))
        .or_else(|| names.first())
        .map(|n| n.value.clone())
        .unwrap_or_else(|| fallback.to_string())
}

fn any_name_matches(names: &[MultiName], needle: &str) -> bool {
    names
        .iter()
        .any(|n| n.value.trim().eq_ignore_ascii_case(needle))
}

impl Camera {
    pub fn get_maker(&self) -> String {
        pick_name(&self.maker, "Misc")
    }

    pub fn get_model(&self) -> String {
        pick_name(&self.model, "Unknown Model")
    }
}

impl Lens {
    pub fn get_full_model_name(&self) -> String {
        self.model
            .iter()
            .find(|m| m.lang.as_deref() == Some("en"))
            .or_else(|| self.model.first())
            .map(|m| m.value.clone())
            .unwrap_or_else(|| "Unknown Model".to_string())
    }

    pub fn get_canonical_model_name(&self) -> String {
        self.model
            .iter()
            .find(|m| m.lang.is_none())
            .or_else(|| self.model.first())
            .map(|m| m.value.clone())
            .unwrap_or_else(|| "Unknown Model".to_string())
    }

    pub fn get_name(&self) -> String {
        let raw_name = self.get_full_model_name();
        let maker = self.get_maker();

        if raw_name.to_lowercase().starts_with(&maker.to_lowercase())
            && let Some(rest) = raw_name.get(maker.len()..)
        {
            let stripped = rest.trim();
            if !stripped.is_empty() {
                return stripped.to_string();
            }
        }

        raw_name
    }

    pub fn get_maker(&self) -> String {
        self.maker
            .iter()
            .find(|m| m.lang.as_deref() == Some("en"))
            .or_else(|| self.maker.first())
            .map(|m| m.value.clone())
            .unwrap_or_else(|| "Misc".to_string())
    }

    pub fn get_display_name(&self, all_maker_lenses: &[&Lens]) -> String {
        let my_short = self.get_name();
        let short_count = all_maker_lenses
            .iter()
            .filter(|l| l.get_name() == my_short)
            .count();

        if short_count <= 1 {
            return my_short;
        }

        let maker = self.get_maker();
        let my_canonical_short = strip_maker_prefix(&self.get_canonical_model_name(), &maker);

        let canonical_short_count = all_maker_lenses
            .iter()
            .filter(|l| {
                strip_maker_prefix(&l.get_canonical_model_name(), &l.get_maker())
                    == my_canonical_short
            })
            .count();

        if canonical_short_count <= 1 {
            return my_canonical_short;
        }

        let my_canonical = self.get_canonical_model_name();
        let canonical_count = all_maker_lenses
            .iter()
            .filter(|l| l.get_canonical_model_name() == my_canonical)
            .count();

        if canonical_count <= 1 {
            return my_canonical;
        }

        if let Some(cf) = self.cropfactor {
            format!("{} (crop {:.1}x)", my_canonical_short, cf)
        } else {
            my_canonical_short
        }
    }

    /// The profile at the shot's focal length, aperture and distance, in
    /// the warp's units: radius 1 = the photo's half-diagonal, distortion as
    /// a PTLens fit that maps corners to corners. `camera_crop` is the
    /// body's crop factor (the lens's own when unknown).
    pub fn get_distortion_params(
        &self,
        focal_length: f32,
        aperture: Option<f32>,
        distance: Option<f32>,
        camera_crop: Option<f32>,
    ) -> Option<LensDistortionParams> {
        let cal = self.calibration.as_ref()?;

        let mut distortions: Vec<&Distortion> = cal
            .elements
            .iter()
            .filter_map(|e| {
                if let CalibrationElement::Distortion(d) = e {
                    Some(d)
                } else {
                    None
                }
            })
            .collect();

        let mut tcas: Vec<&Tca> = cal
            .elements
            .iter()
            .filter_map(|e| {
                if let CalibrationElement::Tca(t) = e {
                    Some(t)
                } else {
                    None
                }
            })
            .collect();

        let mut vignettings: Vec<&Vignetting> = cal
            .elements
            .iter()
            .filter_map(|e| {
                if let CalibrationElement::Vignetting(v) = e {
                    Some(v)
                } else {
                    None
                }
            })
            .collect();

        let (k1, k2, k3, model) = if distortions.is_empty() {
            (0.0, 0.0, 0.0, 0)
        } else {
            distortions.sort_by(|a, b| a.focal.partial_cmp(&b.focal).unwrap_or(Ordering::Equal));

            if let Some(exact) = distortions
                .iter()
                .find(|d| (d.focal - focal_length).abs() < 1e-5)
            {
                extract_dist_params(exact)
            } else if focal_length < distortions[0].focal {
                extract_dist_params(distortions[0])
            } else if focal_length > distortions.last().unwrap().focal {
                extract_dist_params(distortions.last().unwrap())
            } else {
                let mut res = (0.0, 0.0, 0.0, 0);
                for pair in distortions.windows(2) {
                    let (d1, d2) = (&pair[0], &pair[1]);

                    if focal_length >= d1.focal && focal_length <= d2.focal {
                        let p1 = extract_dist_params(d1);
                        let p2 = extract_dist_params(d2);

                        let range = d2.focal - d1.focal;
                        if range.abs() < 1e-5 || p1.3 != p2.3 {
                            res = p1;
                        } else {
                            let t = (focal_length - d1.focal) / range;
                            res = (
                                p1.0 + t as f64 * (p2.0 - p1.0),
                                p1.1 + t as f64 * (p2.1 - p1.1),
                                p1.2 + t as f64 * (p2.2 - p1.2),
                                p1.3,
                            );
                        }
                        break;
                    }
                }
                res
            }
        };

        let (tca_vr, tca_vb) = if tcas.is_empty() {
            (1.0, 1.0)
        } else {
            tcas.sort_by(|a, b| a.focal.partial_cmp(&b.focal).unwrap_or(Ordering::Equal));

            if let Some(exact) = tcas.iter().find(|d| (d.focal - focal_length).abs() < 1e-5) {
                extract_tca_params(exact)
            } else if focal_length < tcas[0].focal {
                extract_tca_params(tcas[0])
            } else if focal_length > tcas.last().unwrap().focal {
                extract_tca_params(tcas.last().unwrap())
            } else {
                let mut res = (1.0, 1.0);
                for pair in tcas.windows(2) {
                    let (d1, d2) = (&pair[0], &pair[1]);
                    if focal_length >= d1.focal && focal_length <= d2.focal {
                        let p1 = extract_tca_params(d1);
                        let p2 = extract_tca_params(d2);

                        let range = d2.focal - d1.focal;
                        if range.abs() < 1e-5 {
                            res = p1;
                        } else {
                            let t = (focal_length - d1.focal) / range;
                            res = (
                                p1.0 + t as f64 * (p2.0 - p1.0),
                                p1.1 + t as f64 * (p2.1 - p1.1),
                            );
                        }
                        break;
                    }
                }
                res
            }
        };

        let (vig_k1, vig_k2, vig_k3) = if vignettings.is_empty() {
            (0.0, 0.0, 0.0)
        } else {
            let target_aperture = aperture.unwrap_or(3.5);
            let target_distance = distance.unwrap_or(1000.0);

            vignettings.sort_by(|a, b| a.focal.partial_cmp(&b.focal).unwrap_or(Ordering::Equal));

            let find_best_vig = |items: &[&Vignetting]| -> (f64, f64, f64) {
                let best_aperture_item = items.iter().min_by(|a, b| {
                    (a.aperture - target_aperture)
                        .abs()
                        .partial_cmp(&(b.aperture - target_aperture).abs())
                        .unwrap_or(Ordering::Equal)
                });
                if let Some(best_ap) = best_aperture_item {
                    let candidates: Vec<&&Vignetting> = items
                        .iter()
                        .filter(|x| (x.aperture - best_ap.aperture).abs() < 0.01)
                        .collect();
                    let best_dist = candidates.into_iter().min_by(|a, b| {
                        let da = a.distance.unwrap_or(1000.0);
                        let db = b.distance.unwrap_or(1000.0);
                        (da - target_distance)
                            .abs()
                            .partial_cmp(&(db - target_distance).abs())
                            .unwrap_or(Ordering::Equal)
                    });
                    extract_vig_params(best_dist.unwrap_or(best_ap))
                } else {
                    (0.0, 0.0, 0.0)
                }
            };

            if focal_length <= vignettings[0].focal + 0.01 {
                let group: Vec<&Vignetting> = vignettings
                    .iter()
                    .filter(|x| (x.focal - vignettings[0].focal).abs() < 0.01)
                    .copied()
                    .collect();
                find_best_vig(&group)
            } else if focal_length >= vignettings.last().unwrap().focal - 0.01 {
                let last_focal = vignettings.last().unwrap().focal;
                let group: Vec<&Vignetting> = vignettings
                    .iter()
                    .filter(|x| (x.focal - last_focal).abs() < 0.01)
                    .copied()
                    .collect();
                find_best_vig(&group)
            } else {
                let mut res = (0.0, 0.0, 0.0);
                let unique_focals: Vec<f32> = {
                    let mut f: Vec<f32> = vignettings.iter().map(|v| v.focal).collect();
                    f.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
                    f.dedup_by(|a, b| (*a - *b).abs() < 0.01);
                    f
                };
                for pair in unique_focals.windows(2) {
                    let (f1, f2) = (pair[0], pair[1]);
                    if focal_length >= f1 && focal_length <= f2 {
                        let group1: Vec<&Vignetting> = vignettings
                            .iter()
                            .filter(|x| (x.focal - f1).abs() < 0.01)
                            .copied()
                            .collect();
                        let group2: Vec<&Vignetting> = vignettings
                            .iter()
                            .filter(|x| (x.focal - f2).abs() < 0.01)
                            .copied()
                            .collect();

                        let p1 = find_best_vig(&group1);
                        let p2 = find_best_vig(&group2);

                        let range = f2 - f1;
                        if range.abs() > 0.01 {
                            let t = (focal_length - f1) / range;
                            res = (
                                p1.0 + t as f64 * (p2.0 - p1.0),
                                p1.1 + t as f64 * (p2.1 - p1.1),
                                p1.2 + t as f64 * (p2.2 - p1.2),
                            );
                        } else {
                            res = p1;
                        }
                        break;
                    }
                }
                res
            }
        };

        // lensfun's database keeps Hugin's units at the lens's calibration
        // crop factor (lensfun mod-coord.cpp / mod-color.cpp): distortion
        // radius 1 = half the shorter side of a frame of the calibration
        // aspect ratio, vignetting radius 1 = half the diagonal. The warp
        // measures radii in the photo's half-diagonal.
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
            .and_then(parse_aspect_ratio)
            .unwrap_or(1.5);
        let vignetting_scale = lens_crop / camera_crop;
        let distortion_scale = vignetting_scale * aspect.hypot(1.0);
        let (k1, k2, k3, model) =
            to_warp_distortion((k1, k2, k3, model), distortion_scale).unwrap_or((0.0, 0.0, 0.0, 0));
        let s2 = vignetting_scale * vignetting_scale;

        Some(LensDistortionParams {
            k1,
            k2,
            k3,
            model,
            tca_vr,
            tca_vb,
            vig_k1: vig_k1 * s2,
            vig_k2: vig_k2 * s2 * s2,
            vig_k3: vig_k3 * s2 * s2 * s2,
            zoom: 1.0,
            tca_vr2: 0.0,
            tca_vb2: 0.0,
        })
    }
}

/// "4:3", "3:2" or a plain ratio such as "1.5" (lensfun's aspect-ratio).
fn parse_aspect_ratio(text: &str) -> Option<f64> {
    let ratio = match text.split_once(':') {
        Some((w, h)) => w.trim().parse::<f64>().ok()? / h.trim().parse::<f64>().ok()?,
        None => text.trim().parse::<f64>().ok()?,
    };
    (ratio.is_finite() && ratio > 0.0).then_some(ratio.max(1.0 / ratio))
}

/// A lensfun distortion (model from `extract_dist_params`, coefficients in
/// Hugin units) as the warp's PTLens model over the photo's half-diagonal,
/// where Hugin radius = `scale` × photo radius. `None` for no distortion.
fn to_warp_distortion(
    (k1, k2, k3, model): (f64, f64, f64, u32),
    scale: f64,
) -> Option<(f64, f64, f64, u32)> {
    if k1 == 0.0 && k2 == 0.0 && k3 == 0.0 {
        return None;
    }
    // rd / ru at Hugin radius x.
    let ratio = |x: f64| match model {
        DIST_POLY3 => 1.0 - k1 + k1 * x * x,
        DIST_POLY5 => 1.0 + k1 * x * x + k2 * x.powi(4),
        _ => k1 * x.powi(3) + k2 * x * x + k3 * x + (1.0 - k1 - k2 - k3),
    };
    let curve: Vec<(f64, f64)> = (1..=64)
        .map(|i| {
            let r = i as f64 / 64.0;
            (r, ratio(scale * r))
        })
        .collect();
    let [a, b, c] = crate::embedded_lens::fit_distortion(&curve)?;
    [a, b, c]
        .iter()
        .all(|v| v.is_finite())
        .then_some((a, b, c, 1))
}

const DIST_POLY3: u32 = 2;
const DIST_POLY5: u32 = 3;

/// The database coefficients and model: PTLens (1) as a, b, c; poly3 (2) as
/// k1; poly5 (3) as k1, k2.
fn extract_dist_params(dist: &Distortion) -> (f64, f64, f64, u32) {
    match dist.model.as_str() {
        "poly3" => (dist.k1.unwrap_or(0.0) as f64, 0.0, 0.0, DIST_POLY3),
        "poly5" => (
            dist.k1.unwrap_or(0.0) as f64,
            dist.k2.unwrap_or(0.0) as f64,
            0.0,
            DIST_POLY5,
        ),
        "ptlens" => {
            let a = dist.a.unwrap_or(0.0) as f64;
            let b = dist.b.unwrap_or(0.0) as f64;
            let c = dist.c.unwrap_or(0.0) as f64;
            (a, b, c, 1)
        }
        _ => (0.0, 0.0, 0.0, 0),
    }
}

fn extract_tca_params(tca: &Tca) -> (f64, f64) {
    (tca.vr.unwrap_or(1.0) as f64, tca.vb.unwrap_or(1.0) as f64)
}

fn extract_vig_params(vig: &Vignetting) -> (f64, f64, f64) {
    (
        vig.k1.unwrap_or(0.0) as f64,
        vig.k2.unwrap_or(0.0) as f64,
        vig.k3.unwrap_or(0.0) as f64,
    )
}

/// Parses every Lensfun XML file below `resource_path` into one database.
pub fn load_lensfun_dir(resource_path: &std::path::Path) -> LensDatabase {
    let mut combined_db = LensDatabase {
        cameras: Vec::new(),
        lenses: Vec::new(),
    };

    if !resource_path.exists() {
        log::error!("Lensfun DB directory not found at: {:?}", resource_path);
        return combined_db;
    }

    for entry in WalkDir::new(resource_path)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "xml"))
    {
        let path = entry.path();
        log::info!("Processing file: {:?}", path);
        match fs::read_to_string(path) {
            Ok(xml_content) => match quick_xml::de::from_str::<LensDatabase>(&xml_content) {
                Ok(mut db) => {
                    combined_db.cameras.append(&mut db.cameras);
                    combined_db.lenses.append(&mut db.lenses);
                }
                Err(e) => {
                    log::error!("Failed to parse Lensfun XML file {:?}: {}", path, e);
                }
            },
            Err(e) => log::error!("Failed to read Lensfun XML file {:?}: {}", path, e),
        }
    }
    combined_db
}

pub fn lenses_for_maker<'a>(db: &'a LensDatabase, maker: &str) -> Vec<&'a Lens> {
    db.lenses
        .iter()
        .filter(|l| l.get_maker() == maker)
        .collect()
}

pub fn find_lens_by_camera_mount<'a>(
    db: &'a LensDatabase,
    camera_maker: &str,
    camera_model: &str,
) -> Option<&'a Lens> {
    let clean_maker = camera_maker.trim().trim_matches('"');
    let clean_model = camera_model.trim().trim_matches('"');

    if clean_maker.is_empty() || clean_model.is_empty() {
        return None;
    }

    let mounts: Vec<&str> = db
        .cameras
        .iter()
        .filter(|c| {
            any_name_matches(&c.maker, clean_maker) && any_name_matches(&c.model, clean_model)
        })
        .map(|c| c.mount.trim())
        .filter(|m| !m.is_empty())
        .collect();

    if mounts.is_empty() {
        return None;
    }

    let mut hits: Vec<&Lens> = Vec::new();
    for lens in &db.lenses {
        if lens
            .mount
            .iter()
            .any(|m| mounts.iter().any(|c| m.trim().eq_ignore_ascii_case(c)))
        {
            hits.push(lens);
            if hits.len() > 1 {
                log::info!(
                    "Mount fallback for {} {}: more than one lens on mount, giving up.",
                    clean_maker,
                    clean_model
                );
                return None;
            }
        }
    }

    let lens = hits.into_iter().next()?;
    log::info!(
        "Mount fallback for {} {}: using fixed lens {} {}",
        clean_maker,
        clean_model,
        lens.get_maker(),
        lens.get_full_model_name()
    );
    Some(lens)
}

pub fn lens_result(db: &LensDatabase, lens: &Lens) -> (String, String) {
    let lens_maker = lens.get_maker();
    let maker_lenses = lenses_for_maker(db, &lens_maker);
    (lens_maker, lens.get_display_name(&maker_lenses))
}

pub fn find_best_lens_match(
    db: &LensDatabase,
    maker: &str,
    model: &str,
    camera_model: &str,
) -> Option<(String, String)> {
    let clean_model = model.trim().trim_matches('"');

    if !clean_model.is_empty()
        && let Some(hit) = find_lens_by_fuzzy_model(db, maker, clean_model)
    {
        return Some(hit);
    }
    if let Some(hit) =
        without_model_code(clean_model).and_then(|name| find_lens_by_fuzzy_model(db, maker, name))
    {
        return Some(hit);
    }

    find_lens_by_camera_mount(db, maker, camera_model).map(|lens| lens_result(db, lens))
}

/// The EXIF lens name without a trailing maker's model code ("17-70mm F/2.8
/// DiIII-A VC RXD B070X" → "… VC RXD"), which lensfun's names often lack or
/// spell for another mount ("B070"): the fuzzy match needs every character
/// of the EXIF name in the lensfun name. A code mixes letters and digits and
/// is no focal length or aperture.
fn without_model_code(name: &str) -> Option<&str> {
    let (rest, code) = name.trim().rsplit_once(' ')?;
    let is_code = code.len() >= 4
        && code.chars().all(|c| c.is_ascii_alphanumeric())
        && code.chars().any(|c| c.is_ascii_digit())
        && code.chars().any(|c| c.is_ascii_alphabetic())
        && !code.to_ascii_lowercase().contains("mm");
    (is_code && rest.trim().len() >= 4).then(|| rest.trim())
}

fn find_lens_by_fuzzy_model(
    db: &LensDatabase,
    maker: &str,
    model: &str,
) -> Option<(String, String)> {
    let clean_maker = maker.trim().trim_matches('"').to_string();
    let clean_model = model.trim().trim_matches('"').to_string();
    let matcher = fuzzy_matcher::skim::SkimMatcherV2::default().ignore_case();

    let lenses_from_maker: Vec<&Lens> = db
        .lenses
        .iter()
        .filter(|lens| lens.get_maker().eq_ignore_ascii_case(&clean_maker))
        .collect();

    if !lenses_from_maker.is_empty() {
        let best_match = lenses_from_maker
            .iter()
            .filter_map(|lens| {
                let english_name = lens.get_full_model_name();
                let canonical_name = lens.get_canonical_model_name();

                let score_english = matcher
                    .fuzzy_match(&english_name, &clean_model)
                    .unwrap_or(0);
                let score_canonical = matcher
                    .fuzzy_match(&canonical_name, &clean_model)
                    .unwrap_or(0);
                let score = score_english.max(score_canonical);

                if score > 0 {
                    let best_name = if score_canonical > score_english {
                        &canonical_name
                    } else {
                        &english_name
                    };
                    let length_penalty =
                        (best_name.len() as i64 - clean_model.len() as i64).max(0) / 2;
                    let adjusted_score = score - length_penalty;
                    Some((adjusted_score, *lens))
                } else {
                    None
                }
            })
            .max_by_key(|(score, _)| *score);

        if let Some((_, best_lens)) = best_match {
            return Some((
                best_lens.get_maker(),
                best_lens.get_display_name(&lenses_from_maker),
            ));
        }
    }

    let best_match_fallback = db
        .lenses
        .iter()
        .filter_map(|lens| {
            let english_name = lens.get_full_model_name();
            let canonical_name = lens.get_canonical_model_name();

            let score_english = matcher
                .fuzzy_match(&english_name, &clean_model)
                .unwrap_or(0);
            let score_canonical = matcher
                .fuzzy_match(&canonical_name, &clean_model)
                .unwrap_or(0);
            let score = score_english.max(score_canonical);

            if score > 0 { Some((score, lens)) } else { None }
        })
        .max_by_key(|(score, _): &(i64, _)| *score);

    if let Some((_, best_lens)) = best_match_fallback {
        let lens_maker = best_lens.get_maker();
        let maker_lenses = lenses_for_maker(db, &lens_maker);
        return Some((lens_maker, best_lens.get_display_name(&maker_lenses)));
    }

    None
}

/// The bundled database read from the source tree (tests).
#[cfg(test)]
pub fn embedded_lens_db() -> LensDatabase {
    let mut combined = LensDatabase {
        cameras: Vec::new(),
        lenses: Vec::new(),
    };
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src-tauri/lensfun_db");
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        if let Ok(xml) = fs::read_to_string(entry.path())
            && let Ok(mut db) = quick_xml::de::from_str::<LensDatabase>(&xml)
        {
            combined.cameras.append(&mut db.cameras);
            combined.lenses.append(&mut db.lenses);
        }
    }
    combined
}

/// The lens database loaded at start-up, for code without app state.
static GLOBAL_LENS_DB: std::sync::OnceLock<std::sync::Arc<LensDatabase>> =
    std::sync::OnceLock::new();

pub fn set_global_lens_db(db: std::sync::Arc<LensDatabase>) {
    let _ = GLOBAL_LENS_DB.set(db);
}

/// Set while the start-up thread parses the database (`mark_lens_db_loading`).
static LENS_DB_LOADING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The database is parsed on a background thread at start-up (25-45 ms),
/// so a headless export of a file that needs no lens profile does not wait
/// for it.
pub fn mark_lens_db_loading() {
    LENS_DB_LOADING.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// The lens database; waits for the start-up load if it is still running.
pub fn global_lens_db() -> Option<std::sync::Arc<LensDatabase>> {
    if let Some(db) = GLOBAL_LENS_DB.get() {
        return Some(db.clone());
    }
    if LENS_DB_LOADING.load(std::sync::atomic::Ordering::SeqCst) {
        return Some(GLOBAL_LENS_DB.wait().clone());
    }
    None
}

/// Lowercase letters, digits and dots only, without a leading maker name:
/// "Canon RF 28mm F2.8 STM" and EXIF's "RF28mm F2.8 STM" both become
/// "rf28mmf2.8stm".
fn normalized_lens_name(name: &str, maker: &str) -> String {
    let lower = name.trim().to_ascii_lowercase();
    let maker = maker.trim().to_ascii_lowercase();
    let rest = lower.strip_prefix(maker.as_str()).unwrap_or(&lower);
    // Third-party lenses on a Canon body: lensfun names them with their own
    // maker ("Sigma 150-600mm f/5-6.3 DG OS HSM | C"), the EXIF without it
    // and with Sigma's line spelled out ("150-600mm F5-6.3 DG OS HSM |
    // Contemporary 015").
    let rest = rest.trim().strip_prefix("sigma ").unwrap_or(rest.trim());
    let rest = match rest.split_once('|') {
        Some((lens, line)) => {
            let line = line.trim();
            let short = ["contemporary", "art", "sports"]
                .iter()
                .find(|full| line.starts_with(*full))
                .map(|full| &full[..1])
                .unwrap_or(line);
            format!("{lens}|{short}")
        }
        None => rest.to_string(),
    };
    rest.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '.' || *c == '-')
        .collect()
}

/// The lens whose name equals the EXIF lens model after normalization; no
/// fuzzy guesses, for corrections applied without asking.
pub fn find_lens_exact<'a>(db: &'a LensDatabase, maker: &str, exif_lens: &str) -> Option<&'a Lens> {
    let wanted = normalized_lens_name(exif_lens, maker);
    if wanted.len() < 4 {
        return None;
    }
    db.lenses.iter().find(|lens| {
        lens.calibration.is_some()
            && lens
                .model
                .iter()
                .any(|name| normalized_lens_name(&name.value, maker) == wanted)
    })
}

/// The body's crop factor from lensfun's camera list.
pub fn camera_crop_factor(db: &LensDatabase, maker: &str, model: &str) -> Option<f32> {
    let full = format!("{} {}", maker.trim(), model.trim());
    db.cameras
        .iter()
        .find(|camera| {
            any_name_matches(&camera.model, model.trim()) || any_name_matches(&camera.model, &full)
        })
        .map(|camera| camera.cropfactor)
        .filter(|crop| crop.is_finite() && *crop > 0.0)
}

/// Crop factor by the model's letters and digits only, for callers that
/// may guess (rawler names the Z 7II "Z 7 2", lensfun "Nikon Z 7_2").
pub fn camera_crop_factor_loose(db: &LensDatabase, model: &str) -> Option<f32> {
    let key = |s: &str| {
        s.chars()
            .filter(char::is_ascii_alphanumeric)
            .collect::<String>()
            .to_ascii_lowercase()
    };
    let wanted = key(model);
    if wanted.len() < 3 {
        return None;
    }
    db.cameras
        .iter()
        .find(|camera| {
            camera.model.iter().any(|name| {
                let name = key(&name.value);
                name == wanted || (name.ends_with(&wanted) && name.len() <= wanted.len() + 8)
            })
        })
        .map(|camera| camera.cropfactor)
        .filter(|crop| crop.is_finite() && *crop > 0.0)
}

pub fn resolve_lens_params(
    db: &LensDatabase,
    maker: &str,
    model: &str,
    focal_length: f32,
    aperture: Option<f32>,
    distance: Option<f32>,
    camera_crop: Option<f32>,
) -> Option<LensDistortionParams> {
    let maker_lenses = lenses_for_maker(db, maker);
    if let Some(lens) = maker_lenses
        .iter()
        .find(|l| l.get_display_name(&maker_lenses) == model)
    {
        lens.get_distortion_params(focal_length, aperture, distance, camera_crop)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// lensfun's Canon RF 28mm F2.8 STM entry (full-frame calibration).
    const RF28: &str = r#"<lensdatabase><lens>
        <maker>Canon</maker><model>Canon RF 28mm F2.8 STM</model><mount>Canon RF</mount>
        <cropfactor>1.0</cropfactor>
        <calibration>
            <distortion model="ptlens" focal="28" a="0.023" b="-0.084" c="0.053"/>
            <vignetting model="pa" focal="28" aperture="2.8" distance="1000" k1="-2.0777" k2="2.2485" k3="-1.0106"/>
        </calibration>
    </lens></lensdatabase>"#;

    fn rf28() -> Lens {
        quick_xml::de::from_str::<LensDatabase>(RF28)
            .unwrap()
            .lenses
            .remove(0)
    }

    fn brightness(p: &LensDistortionParams, r: f64) -> f64 {
        let r2 = r * r;
        1.0 + r2 * (p.vig_k1 + r2 * (p.vig_k2 + r2 * p.vig_k3))
    }

    #[test]
    fn vignetting_radius_follows_the_crop_factor() {
        let lens = rf28();
        // Full frame: radius 1 is lensfun's own unit (half-diagonal).
        let ff = lens
            .get_distortion_params(28.0, Some(2.8), None, Some(1.0))
            .unwrap();
        assert!((brightness(&ff, 1.0) - (1.0 - 2.0777 + 2.2485 - 1.0106)).abs() < 1e-4);
        // APS-C (1.6×): the photo's corner lies at 1 / 1.6 of the lens's.
        let apsc = lens
            .get_distortion_params(28.0, Some(2.8), None, Some(1.6))
            .unwrap();
        let x: f64 = 1.0 / 1.6;
        let expected = 1.0 - 2.0777 * x.powi(2) + 2.2485 * x.powi(4) - 1.0106 * x.powi(6);
        assert!((brightness(&apsc, 1.0) - expected).abs() < 1e-4);
    }

    /// Prints the lens matched for each line of `$LENS_MATCH_CASES` (make,
    /// camera model and EXIF lens model, tab-separated) in the database at
    /// `$LENSFUN_DIR`: to compare two database versions.
    #[test]
    #[ignore = "tool: needs LENS_MATCH_CASES and LENSFUN_DIR"]
    fn print_lens_matches() {
        let db = load_lensfun_dir(std::path::Path::new(&std::env::var("LENSFUN_DIR").unwrap()));
        let cases = fs::read_to_string(std::env::var("LENS_MATCH_CASES").unwrap()).unwrap();
        for line in cases.lines() {
            let mut fields = line.split('\t');
            let (make, camera, lens) = (
                fields.next().unwrap_or(""),
                fields.next().unwrap_or(""),
                fields.next().unwrap_or(""),
            );
            let found = find_best_lens_match(&db, make, lens, camera);
            println!("MATCH\t{make}\t{camera}\t{lens}\t{found:?}");
        }
    }

    /// EXIF names from public sample RAWs (raw.pixls.us) against the bundled
    /// database: lenses matched before its update keep their entry (the
    /// Sony 16-50 kit zoom under its new name, same calibration), and lenses
    /// the update added are found (16-50 II, the Tamron 17-70 on Fujifilm X),
    /// also when the EXIF name ends in the maker's model code (B070X, F013).
    #[test]
    fn bundled_database_matches_sample_raw_lenses() {
        let db = embedded_lens_db();
        let cases = [
            (
                "FUJIFILM",
                "X-T5",
                "XF18mmF2 R",
                Some(("Fujifilm", "XF 18mm f/2 R")),
            ),
            (
                "NIKON CORPORATION",
                "NIKON Z 8",
                "NIKKOR Z 14-24mm f/2.8 S",
                Some(("Nikon", "Nikkor Z 14-24mm f/2.8 S")),
            ),
            (
                "SONY",
                "ILCE-7M4",
                "FE 50mm F2.5 G",
                Some(("Sony", "FE 50mm f/2.5 G")),
            ),
            (
                "SONY",
                "ILCE-7CR",
                "24-70mm F2.8 DG DN | Art 019",
                Some(("Sigma", "24-70mm F2.8 DG DN | Art 019")),
            ),
            (
                "NIKON CORPORATION",
                "NIKON Z f",
                "TAMRON 35-150mm F/2-2.8 Di III VXD A058Z",
                Some(("Tamron", "35-150mm F/2-2.8 Di III VXD")),
            ),
            (
                "SONY",
                "ILCE-5100",
                "E PZ 16-50mm F3.5-5.6 OSS",
                Some(("Sony", "E 16-50mm f/3.5-5.6 OSS PZ (Vers. I/II)")),
            ),
            (
                "SONY",
                "ILCE-6400A",
                "E PZ 16-50mm F3.5-5.6 OSS II",
                Some(("Sony", "E 16-50mm f/3.5-5.6 OSS PZ (Vers. I/II)")),
            ),
            (
                "FUJIFILM",
                "X-T30 III",
                "17-70mm F/2.8 DiIII-A VC RXD B070X",
                Some(("Tamron", "17-70mm F/2.8 Di III-A VC RXD")),
            ),
            (
                "SIGMA",
                "SIGMA fp L",
                "TAMRON SP 45mm F/1.8 Di VC USD F013",
                Some(("Tamron", "SP 45mm F/1.8 Di VC USD")),
            ),
        ];
        for (make, camera, lens, expected) in cases {
            let found = find_best_lens_match(&db, make, lens, camera);
            let expected = expected.map(|(m, l)| (m.to_string(), l.to_string()));
            assert_eq!(found, expected, "{make} {camera} {lens}");
        }
        // A model code is dropped only when nothing matched with it, and
        // only a letters-and-digits code, never a focal length or aperture.
        assert_eq!(
            without_model_code("17-70mm F/2.8 DiIII-A VC RXD B070X"),
            Some("17-70mm F/2.8 DiIII-A VC RXD")
        );
        assert_eq!(without_model_code("RF 50mm F1.4"), None);
        assert_eq!(without_model_code("XF 18mm F2 R"), None);
        assert_eq!(without_model_code("EF 70-200mm"), None);
    }

    #[test]
    fn exif_lens_names_match_lensfun_names_exactly() {
        let db = quick_xml::de::from_str::<LensDatabase>(RF28).unwrap();
        assert!(find_lens_exact(&db, "Canon", "RF28mm F2.8 STM").is_some());
        assert!(find_lens_exact(&db, "Canon", "RF28mm F2.8").is_none());
        assert!(find_lens_exact(&db, "Canon", "RF35mm F1.8 MACRO IS STM").is_none());
        // Sigma lines: "| Contemporary 015" in the EXIF is lensfun's "| C".
        assert_eq!(
            normalized_lens_name("150-600mm F5-6.3 DG OS HSM | Contemporary 015", "Canon"),
            normalized_lens_name("Sigma 150-600mm f/5-6.3 DG OS HSM | C", "Canon")
        );
        assert_ne!(
            normalized_lens_name("150-600mm F5-6.3 DG OS HSM | Sports 014", "Canon"),
            normalized_lens_name("Sigma 150-600mm f/5-6.3 DG OS HSM | C", "Canon")
        );
    }

    #[test]
    fn distortion_is_evaluated_in_hugin_units() {
        let lens = rf28();
        let p = lens
            .get_distortion_params(28.0, None, None, Some(1.0))
            .unwrap();
        assert_eq!(p.model, 1);
        // Hugin radius = 1.803 × photo radius for a 3:2 full-frame photo.
        let (a, b, c) = (0.023, -0.084, 0.053);
        let hugin = |x: f64| a * x.powi(3) + b * x * x + c * x + (1.0 - a - b - c);
        let warp = |r: f64| p.k1 * r.powi(3) + p.k2 * r * r + p.k3 * r + (1.0 - p.k1 - p.k2 - p.k3);
        let s = 1.5f64.hypot(1.0);
        for r in [0.25, 0.5, 0.75, 1.0] {
            let expected = hugin(s * r) / hugin(s);
            assert!(
                (warp(r) - expected).abs() < 2e-3,
                "r {r}: {} vs {expected}",
                warp(r)
            );
        }
    }
}
