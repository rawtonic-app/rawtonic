//! What the rendering reads from a RAW file besides its pixels, kept with
//! its previews so that an offline photograph (RAW missing or not
//! downloaded) renders as it did online.
//!
//! Online, the base look (baseline exposure, Camera Raw look, embedded
//! tone curve, make look table), the decode's colour (As Shot balance,
//! camera matrices: Kelvin white balance and camera profiles), the noise
//! level measured on the decode, the diffraction blur from the f-number,
//! and the camera's or lensfun's default lens correction are all read from
//! the file. Offline, each of those readers found nothing: the R5 II
//! rendered 24 L* darker in a lifted corner (no baseline exposure, no
//! vignetting correction) and white balance fell back to relative sliders.
//!
//! `collect` gathers them from the file when a preview is written; the app
//! stores them in the preview's header and `register`s them for the source
//! path when the photograph is opened offline. The readers consult the
//! registry first, so nothing changes while the file can be read (nothing
//! is registered then, and exports never run on previews).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};

use crate::baseline_exposure::{BaseLook, PROFILE_CURVE_SAMPLES};
use crate::lens_correction::LensDistortionParams;

/// Version of what `collect` gathers; raise when a reader starts to depend
/// on something new, so previews without it are re-made. 3: `camera_lens`
/// carries Panasonic lateral CA data (`embedded_lens::CA_VERSION` 1).
/// 4: `diffraction_sigma` from the 2026-10-18 lens database (bodies it newly
/// covers) and `diffraction_sigma_v1` for recipes from before it.
/// 5: `lensfun_default` scaled to the body's crop factor (camera list
/// fallback by loose name) and with the profile's whole lateral CA.
/// 6: `image_key_median` (lowered Highlights of tone version 3).
/// 7: `camera_lens` at camera geometry version 1
/// (`embedded_lens::GEOMETRY_VERSION`), `camera_lens_geometry` for recipes
/// of older geometry versions.
/// 8: `camera_lens` at camera geometry version 2 (Nikon Z framing).
/// 9: `camera_lens` at camera geometry version 3 (Canon distortion).
/// 10: `camera_lens` at camera geometry version 4 (Samsung's distortion).
/// 11: `camera_lens` at camera geometry version 5 (Panasonic and Fujifilm
/// framing).
/// 12: `camera_lens` at camera geometry version 6 (Sony's knot table).
/// 13: `camera_lens` at camera geometry version 7 (Fujifilm's knot table).
/// 14: `frames` (`source_frame::frames`), so that an offline photograph
/// opens only on previews of its recipe's frame.
/// 15: `camera_crop`, so that an offline never-edited photograph starts
/// with the camera's crop.
/// 16: `base_look.capture_gain_ev` (renderVersion 9's extended-low-ISO
/// pull), so that an offline Sony photograph at ISO 50 develops with it.
pub const VERSION: u32 = 16;

/// The file facts the rendering of one photograph uses.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct SourceFacts {
    pub version: u32,
    /// `baseline_exposure::base_look`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_look: Option<StoredBaseLook>,
    /// `camera_profile::image_colour` at the As Shot balance (None: the
    /// decode's colour cannot be had from metadata, e.g. four-colour
    /// sensors).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colour: Option<StoredColour>,
    /// `noise_level::for_path` (measured on the decode).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub noise_level: Option<f32>,
    /// `image_key::for_path` (measured on the decode): its 10th and 90th
    /// percentile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_key: Option<[f32; 2]>,
    /// Its median (version 6 on; kept apart so older headers still parse).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_key_median: Option<f32>,
    /// `diffraction` Airy σ in pixels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diffraction_sigma: Option<f32>,
    /// `diffraction` version 1's σ, when it differs from `diffraction_sigma`
    /// (a body only the 2026-10-18 database knows).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub diffraction_v1_differs: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diffraction_sigma_v1: Option<f32>,
    /// `embedded_lens::params_for`: the camera's lens correction data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera_lens: Option<LensDistortionParams>,
    /// `camera_lens` at older camera geometry versions, where it differs
    /// (version 7 on).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub camera_lens_geometry: std::collections::BTreeMap<u64, Option<LensDistortionParams>>,
    /// `embedded_lens::lensfun_default`: maker, model and parameters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lensfun_default: Option<(String, String, LensDistortionParams)>,
    /// `source_frame::frames`: which frame versions frame the RAW alike
    /// (absent in facts from before them: an offline photograph then opens
    /// on its previews whatever frame they show).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frames: Option<Vec<u32>>,
    /// `camera_crop::crops` of the newest frame: the aspect ratio set in the
    /// camera, which new edits start with as their crop (x, y, width,
    /// height; empty where the camera set none). Absent in facts from
    /// before it: an offline never-edited photograph then starts without it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera_crop: Option<Vec<u32>>,
}

/// `BaseLook` in a serializable form (its curve as a list).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StoredBaseLook {
    pub baseline_ev: f32,
    pub camera_raw: bool,
    pub neutral_colour: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_curve: Option<Vec<f32>>,
    pub no_black_render: bool,
    pub phone_defaults: bool,
    /// `BaseLook::capture_gain_ev` (facts version 16; older facts are
    /// re-made by Build Previews before they are used offline).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub capture_gain_ev: f32,
}

fn is_zero(value: &f32) -> bool {
    *value == 0.0
}

impl From<BaseLook> for StoredBaseLook {
    fn from(look: BaseLook) -> Self {
        StoredBaseLook {
            baseline_ev: look.baseline_ev,
            camera_raw: look.camera_raw,
            neutral_colour: look.neutral_colour,
            profile_curve: look.profile_curve.map(|curve| curve.to_vec()),
            no_black_render: look.no_black_render,
            phone_defaults: look.phone_defaults,
            capture_gain_ev: look.capture_gain_ev,
        }
    }
}

impl StoredBaseLook {
    pub fn to_base_look(&self) -> BaseLook {
        BaseLook {
            baseline_ev: self.baseline_ev,
            camera_raw: self.camera_raw,
            neutral_colour: self.neutral_colour,
            profile_curve: self
                .profile_curve
                .as_deref()
                .and_then(|curve| <[f32; PROFILE_CURVE_SAMPLES]>::try_from(curve).ok()),
            no_black_render: self.no_black_render,
            phone_defaults: self.phone_defaults,
            capture_gain_ev: self.capture_gain_ev,
        }
    }
}

/// `raw_processing::ImageColor` in a serializable form, with what
/// `image_color_at` needs for other balances.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StoredColour {
    pub camera_to_srgb: [[f32; 3]; 3],
    /// White-balance multipliers; JSON has no NaN (the unused fourth of
    /// three-colour sensors), so those are null.
    pub wb: [Option<f32>; 4],
    /// Calibration matrices by EXIF illuminant code.
    pub calibrations: Vec<(u16, Vec<f32>)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit_calibration: Option<crate::color_calibration::CameraCalibration>,
    pub forward_matrix: bool,
}

impl StoredColour {
    pub fn new(colour: &crate::raw_processing::ImageColor, forward_matrix: bool) -> Self {
        let mut calibrations: Vec<(u16, Vec<f32>)> = colour
            .calibrations
            .iter()
            .map(|(illuminant, matrix)| (*illuminant as u16, matrix.clone()))
            .collect();
        calibrations.sort_by_key(|(code, _)| *code);
        StoredColour {
            camera_to_srgb: colour.camera_to_srgb,
            wb: colour.wb.map(|v| v.is_finite().then_some(v)),
            calibrations,
            unit_calibration: colour.unit_calibration,
            forward_matrix,
        }
    }

    fn calibrations(
        &self,
    ) -> HashMap<rawler::imgop::xyz::Illuminant, rawler::imgop::xyz::FlatColorMatrix> {
        self.calibrations
            .iter()
            .filter_map(|(code, matrix)| {
                Some((
                    rawler::imgop::xyz::Illuminant::try_from(*code).ok()?,
                    matrix.clone(),
                ))
            })
            .collect()
    }

    /// The colour at the As Shot balance (`wb` None) or at `wb`.
    pub fn colour(&self, wb: Option<[f32; 4]>) -> Option<crate::raw_processing::ImageColor> {
        let calibrations = self.calibrations();
        let mut colour = match wb {
            None => crate::raw_processing::ImageColor {
                camera_to_srgb: self.camera_to_srgb,
                wb: self.wb.map(|v| v.unwrap_or(f32::NAN)),
                calibrations,
                unit_calibration: None,
            },
            Some(wb) => {
                crate::raw_processing::image_color_at(&calibrations, self.forward_matrix, wb)?
            }
        };
        colour.unit_calibration = self.unit_calibration;
        Some(colour)
    }
}

impl SourceFacts {
    /// The camera's lens data at camera geometry version `geometry`, one
    /// these facts carry (`embedded_lens::facts_geometry_version`).
    pub fn camera_lens_at(&self, geometry: u64) -> Option<LensDistortionParams> {
        match self.camera_lens_geometry.get(&geometry) {
            Some(params) => params.clone(),
            None => self.camera_lens.clone(),
        }
    }

    /// The measured image key ([10th, 90th, 50th] percentile, as
    /// `image_key::measure`); the median 0 (unknown) in facts from before
    /// version 6.
    pub fn measured_image_key(&self) -> Option<crate::image_key::ImageKey> {
        self.image_key
            .map(|[low, high]| [low, high, self.image_key_median.unwrap_or(0.0)])
    }

    /// Keeps `key` (`measured_image_key`'s form).
    pub fn set_image_key(&mut self, key: Option<crate::image_key::ImageKey>) {
        self.image_key = key.map(|[low, high, _]| [low, high]);
        self.image_key_median = key.map(|[_, _, median]| median);
    }
}

/// Gathers the facts of the RAW at `path` (it must be readable). Read once
/// per file and modification time.
pub fn collect(path: &str) -> SourceFacts {
    type Cache = Mutex<HashMap<(String, std::time::SystemTime), SourceFacts>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let key = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .map(|time| (path.to_string(), time));
    if let Some(key) = &key
        && let Some(found) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(key)
    {
        let mut found = found.clone();
        // Measured on the decode, which may have happened since.
        found.noise_level = crate::noise_level::for_path(path).or(found.noise_level);
        if let Some(key) = crate::image_key::for_path(path) {
            found.set_image_key(Some(key));
        }
        return found;
    }
    let source = std::path::Path::new(path);
    let diffraction = crate::diffraction::sigmas_for_path(path);
    let camera_lens_at = |geometry| {
        crate::embedded_lens::params_for_version(source, crate::embedded_lens::CA_VERSION, geometry)
    };
    let camera_lens = camera_lens_at(crate::embedded_lens::GEOMETRY_VERSION);
    let camera_lens_geometry = (0..crate::embedded_lens::GEOMETRY_VERSION)
        .map(|geometry| (geometry, camera_lens_at(geometry)))
        .filter(|(_, params)| *params != camera_lens)
        .collect();
    let mut facts = SourceFacts {
        version: VERSION,
        base_look: Some(crate::baseline_exposure::base_look(path).into()),
        colour: crate::camera_profile::image_colour_details(path)
            .map(|(colour, forward)| StoredColour::new(&colour, forward)),
        noise_level: crate::noise_level::for_path(path),
        image_key: None,
        image_key_median: None,
        diffraction_sigma: diffraction.current,
        diffraction_v1_differs: diffraction.v1 != diffraction.current,
        diffraction_sigma_v1: diffraction
            .v1
            .filter(|_| diffraction.v1 != diffraction.current),
        camera_lens,
        camera_lens_geometry,
        lensfun_default: crate::embedded_lens::lensfun_default(source),
        frames: crate::source_frame::frames(path),
        camera_crop: crate::camera_crop::crops(path)
            .get(crate::source_frame::VERSION as usize)
            .copied()
            .flatten()
            .map_or_else(Vec::new, |c| vec![c.x, c.y, c.width, c.height])
            .into(),
    };
    facts.set_image_key(crate::image_key::for_path(path));
    if let Some(key) = key {
        let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
        if cache.len() > 1024 {
            cache.clear();
        }
        cache.insert(key, facts.clone());
    }
    facts
}

fn registry() -> &'static Mutex<HashMap<String, Arc<SourceFacts>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, Arc<SourceFacts>>>> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}

/// The source file of `path` (a virtual copy's `?vc=` suffix removed).
fn source_key(path: &str) -> &str {
    path.split_once("?vc=").map_or(path, |(source, _)| source)
}

/// The readers use `facts` for the RAW at `path` from now on (it is
/// offline and opens on its previews). What older facts do not keep (an
/// older preview read after a newer one: the file's `frames`, the camera's
/// crop) stays as the facts registered before told it: they are facts of
/// the same file, and losing them would let an older 1:1 preview undo
/// which frame the photograph's recipes develop.
pub fn register(path: &str, mut facts: SourceFacts) {
    let mut registry = registry().lock().unwrap_or_else(|e| e.into_inner());
    if registry.len() > 4096 {
        registry.clear();
    }
    let key = source_key(path).to_string();
    if let Some(known) = registry.get(&key) {
        if facts.frames.is_none() {
            facts.frames = known.frames.clone();
        }
        if facts.camera_crop.is_none() {
            facts.camera_crop = known.camera_crop.clone();
        }
    }
    registry.insert(key, Arc::new(facts));
}

/// The RAW at `path` can be read again: its readers go back to the file.
pub fn forget(path: &str) {
    registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(source_key(path));
}

/// The facts registered for the RAW at `path`, if it is offline.
pub fn registered(path: &str) -> Option<Arc<SourceFacts>> {
    let registry = registry().lock().unwrap_or_else(|e| e.into_inner());
    if registry.is_empty() {
        return None;
    }
    registry.get(source_key(path)).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn facts_round_trip_through_json_and_the_registry() {
        let mut curve = [0.0f32; PROFILE_CURVE_SAMPLES];
        curve
            .iter_mut()
            .enumerate()
            .for_each(|(i, v)| *v = i as f32 / 7.0);
        let look = BaseLook {
            baseline_ev: 0.66,
            camera_raw: true,
            neutral_colour: true,
            profile_curve: Some(curve),
            no_black_render: true,
            phone_defaults: false,
            capture_gain_ev: -1.0,
        };
        let colour = crate::raw_processing::ImageColor {
            camera_to_srgb: [[1.5, -0.4, -0.1], [-0.2, 1.4, -0.2], [0.0, -0.5, 1.5]],
            wb: [2.1, 1.0, 1.6, f32::NAN],
            calibrations: [
                (
                    rawler::imgop::xyz::Illuminant::A,
                    vec![0.8, -0.2, 0.0, -0.4, 1.2, 0.2, -0.1, 0.2, 0.6],
                ),
                (
                    rawler::imgop::xyz::Illuminant::D65,
                    vec![0.7, -0.1, -0.1, -0.4, 1.2, 0.2, -0.1, 0.2, 0.7],
                ),
            ]
            .into_iter()
            .collect(),
            unit_calibration: None,
        };
        let facts = SourceFacts {
            version: VERSION,
            base_look: Some(look.into()),
            colour: Some(StoredColour::new(&colour, false)),
            noise_level: Some(0.012),
            image_key: Some([0.01, 0.21]),
            image_key_median: Some(0.05),
            diffraction_sigma: None,
            diffraction_v1_differs: false,
            diffraction_sigma_v1: None,
            camera_lens: None,
            camera_lens_geometry: Default::default(),
            lensfun_default: None,
            frames: Some(vec![0, 1]),
            camera_crop: Some(vec![0, 376, 4000, 2248]),
        };
        let json = serde_json::to_string(&facts).unwrap();
        let back: SourceFacts = serde_json::from_str(&json).unwrap();
        assert_eq!(back.base_look.as_ref().unwrap().to_base_look(), look);
        let stored = back.colour.as_ref().unwrap();
        let restored = stored.colour(None).unwrap();
        assert_eq!(restored.camera_to_srgb, colour.camera_to_srgb);
        assert_eq!(restored.calibrations, colour.calibrations);
        assert_eq!(restored.wb[..3], colour.wb[..3]);
        assert!(restored.wb[3].is_nan());
        // Another balance: the matrices are selected and adapted for it.
        assert!(stored.colour(Some([1.4, 1.0, 2.3, 0.0])).is_some());
        // Old headers without facts, or with fewer fields, still parse.
        assert_eq!(
            serde_json::from_str::<SourceFacts>(r#"{"version":1}"#).unwrap(),
            SourceFacts {
                version: 1,
                ..Default::default()
            }
        );

        let path = "/offline/volume/Job/Capture/IMG_1.CR3";
        assert!(registered(path).is_none());
        register(path, back.clone());
        assert_eq!(registered(path).as_deref(), Some(&back));
        assert_eq!(registered(&format!("{path}?vc=2")).as_deref(), Some(&back));
        assert_eq!(
            crate::baseline_exposure::base_look(path),
            look,
            "the base look reader uses the registered facts"
        );
        forget(path);
        assert!(registered(path).is_none());
    }

    /// FR1 r3: an older 1:1 preview read after a newer Standard one (its
    /// facts without `frames`) must not undo which frame the photograph's
    /// recipes develop offline; newer facts still replace older ones.
    #[test]
    fn older_facts_keep_the_known_frames() {
        let path = "/offline/volume/Job/Capture/IMG_FRAMES.CR3";
        let newer = SourceFacts {
            version: VERSION,
            noise_level: Some(0.02),
            frames: Some(vec![0, 1]),
            camera_crop: Some(Vec::new()),
            ..Default::default()
        };
        register(path, newer);
        register(
            path,
            SourceFacts {
                version: 13,
                noise_level: Some(0.01),
                ..Default::default()
            },
        );
        let kept = registered(path).unwrap();
        assert_eq!(kept.frames, Some(vec![0, 1]));
        assert_eq!(kept.camera_crop, Some(Vec::new()));
        // The older preview's own facts otherwise (its version included).
        assert_eq!((kept.version, kept.noise_level), (13, Some(0.01)));
        assert_eq!(crate::source_frame::effective(path, 1), 1);
        // Facts that know the frames say what they say.
        register(
            path,
            SourceFacts {
                version: VERSION,
                frames: Some(vec![0, 0]),
                ..Default::default()
            },
        );
        assert_eq!(registered(path).unwrap().frames, Some(vec![0, 0]));
        forget(path);
        assert!(registered(path).is_none());
    }
}
