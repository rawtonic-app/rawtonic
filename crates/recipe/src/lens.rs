//! Global lens correction: the mode and profile, the amounts and switches of
//! the distortion, vignetting and lateral CA corrections, manual lens
//! vignetting and the automatic lateral CA switch, as stored without
//! clamping.
//!
//! Like geometry, the lens stage ignores section visibility. Amount ranges
//! follow StudioLensCorrection. Which profile applies (Lensfun lookup, the
//! camera's own data, the `lensProfileFor` stamp) stays with the engine's
//! `lens_recipe`; this module only reads and names what the recipe holds.

use crate::tone::slider_value;
use crate::ts::{TsField, TsSection, TsType, TsUnion};
use crate::{Key, KeyValue, StoredRef};
use serde_json::Value;
use std::borrow::Cow;

/// `auto`, `manual` or `camera`; any other stored string is carried as is.
pub const CORRECTION_MODE: Key<CorrectionMode> =
    Key::new("lensCorrectionMode", CorrectionMode::MANUAL, 1);
pub const MAKER: Key<Option<String>> = Key::new("lensMaker", None, 1);
pub const MODEL: Key<Option<String>> = Key::new("lensModel", None, 1);
pub const DISTORTION_PARAMS: Key<ProfileParams> =
    Key::new("lensDistortionParams", ProfileParams::none(), 1);
/// What `lensDistortionParams` were resolved for; written by the save.
pub const PROFILE_FOR: Key<Option<String>> = Key::new("lensProfileFor", None, 1);
pub const DISTORTION_AMOUNT: Key<f64> = Key::new("lensDistortionAmount", 100.0, 1);
pub const VIGNETTE_AMOUNT: Key<f64> = Key::new("lensVignetteAmount", 100.0, 1);
pub const TCA_AMOUNT: Key<f64> = Key::new("lensTcaAmount", 100.0, 1);
pub const DISTORTION_ENABLED: Key<bool> = Key::new("lensDistortionEnabled", true, 1);
pub const TCA_ENABLED: Key<bool> = Key::new("lensTcaEnabled", true, 1);
pub const VIGNETTE_ENABLED: Key<bool> = Key::new("lensVignetteEnabled", true, 1);
pub const MANUAL_VIGNETTE_AMOUNT: Key<f64> = Key::new("lensManualVignetteAmount", 0.0, 1);
pub const MANUAL_VIGNETTE_MIDPOINT: Key<f64> = Key::new("lensManualVignetteMidpoint", 50.0, 1);
/// The Remove chromatic aberration switch; absent means "by version".
pub const AUTO_CHROMATIC_ABERRATION: Key<bool> = Key::new("autoChromaticAberration", false, 1);
/// The automatic lateral CA estimate (`lateral_ca::VERSION`); the engine
/// reads it through `RecipeVersions`.
pub const AUTO_CA_VERSION: Key<u64> = Key::new("autoCaVersion", 1, 1);
/// Which cameras' CA data camera mode reads (`embedded_lens::CA_VERSION`).
pub const CAMERA_CA_VERSION: Key<u64> = Key::new("cameraCaVersion", 0, 1);
/// How camera mode reads the camera's geometry (`embedded_lens::GEOMETRY_VERSION`).
pub const CAMERA_GEOMETRY_VERSION: Key<u64> = Key::new("cameraGeometryVersion", 0, 1);

slider_value!(
    /// Distortion, vignetting or lateral CA amount: 0…200 percent, step 1.
    LensAmount,
    0.0,
    200.0,
    1.0
);
slider_value!(
    /// Manual lens vignetting: −100…100, step 1 (positive brightens the corners).
    VignetteAmount,
    -100.0,
    100.0,
    1.0
);
slider_value!(
    /// Manual lens vignetting midpoint: 0…100, step 1.
    VignetteMidpoint,
    0.0,
    100.0,
    1.0
);

/// The lens correction mode as stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CorrectionMode(Cow<'static, str>);

impl CorrectionMode {
    const MANUAL: Self = Self(Cow::Borrowed("manual"));

    pub fn as_str(&self) -> &str {
        &self.0
    }
    /// The camera's own measured data, applied exactly.
    pub fn is_camera(&self) -> bool {
        self.0 == "camera"
    }
}
impl KeyValue for CorrectionMode {
    fn from_json(json: &Value) -> Option<Self> {
        json.as_str().map(|mode| Self(Cow::Owned(mode.to_owned())))
    }
    fn to_json(&self) -> Value {
        Value::String(self.0.to_string())
    }
}
impl TsType for CorrectionMode {
    const TS: &'static str = "LensCorrectionMode";
}

/// `lensDistortionParams` as stored (any JSON); [`Profile`] is how it renders.
#[derive(Clone, Debug, PartialEq)]
pub struct ProfileParams(Value);

impl ProfileParams {
    pub const fn none() -> Self {
        Self(Value::Null)
    }
    /// The stored JSON, unchanged.
    pub fn as_value(&self) -> &Value {
        &self.0
    }
}
impl KeyValue for ProfileParams {
    fn from_json(json: &Value) -> Option<Self> {
        Some(Self(json.clone()))
    }
    fn to_json(&self) -> Value {
        self.0.clone()
    }
}
impl TsType for ProfileParams {
    const TS: &'static str = "LensDistortionParams | null";
    const IMPORT: Option<&'static str> = Some("LensDistortionParams");
}

/// The lens profile numbers as the warp reads them: each from the stored
/// object by `as_f64`/`as_u64`/`as_bool`, its default when missing or of
/// another type (a profile that is not an object holds none).
#[derive(Clone, Debug, PartialEq)]
pub struct Profile {
    pub k1: f64,
    pub k2: f64,
    pub k3: f64,
    pub model: u64,
    /// Knots of a model 2 profile; a knot that is not a number is NaN.
    pub table: Vec<f64>,
    pub table_end: f64,
    pub table_radii: Vec<f64>,
    pub zoom: f64,
    pub exact_zoom: bool,
    /// `centre_x`, `centre_y`; non-finite values count as 0.
    pub centre: [f64; 2],
    pub tca_vr: f64,
    pub tca_vb: f64,
    pub tca_vr2: f64,
    pub tca_vb2: f64,
    pub vig_k1: f64,
    pub vig_k2: f64,
    pub vig_k3: f64,
}

impl Profile {
    fn resolve(params: &ProfileParams) -> Self {
        let object = params.0.as_object();
        let number = |key: &str, default: f64| {
            object
                .and_then(|p| p.get(key))
                .and_then(Value::as_f64)
                .unwrap_or(default)
        };
        let list = |key: &str| -> Vec<f64> {
            object
                .and_then(|p| p.get(key))
                .and_then(Value::as_array)
                .map(|knots| {
                    knots
                        .iter()
                        .map(|v| v.as_f64().unwrap_or(f64::NAN))
                        .collect()
                })
                .unwrap_or_default()
        };
        Self {
            k1: number("k1", 0.0),
            k2: number("k2", 0.0),
            k3: number("k3", 0.0),
            model: object
                .and_then(|p| p.get("model"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
            table: list("table"),
            table_end: number("table_end", 0.0),
            table_radii: list("table_radii"),
            zoom: number("zoom", 1.0),
            exact_zoom: object
                .and_then(|p| p.get("exact_zoom"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
            centre: ["centre_x", "centre_y"].map(|key| {
                object
                    .and_then(|p| p.get(key))
                    .and_then(Value::as_f64)
                    .filter(|v| v.is_finite())
                    .unwrap_or(0.0)
            }),
            tca_vr: number("tca_vr", 1.0),
            tca_vb: number("tca_vb", 1.0),
            tca_vr2: number("tca_vr2", 0.0),
            tca_vb2: number("tca_vb2", 0.0),
            vig_k1: number("vig_k1", 0.0),
            vig_k2: number("vig_k2", 0.0),
            vig_k3: number("vig_k3", 0.0),
        }
    }
}

/// Lens correction as rendered. No visibility gates; numeric values remain
/// stored.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Lens {
    pub mode: CorrectionMode,
    pub distortion_amount: LensAmount,
    pub vignette_amount: LensAmount,
    pub tca_amount: LensAmount,
    pub distortion_enabled: bool,
    pub tca_enabled: bool,
    pub vignette_enabled: bool,
    pub manual_vignette_amount: VignetteAmount,
    pub manual_vignette_midpoint: VignetteMidpoint,
    pub profile: Profile,
    /// The Remove chromatic aberration switch; None: decided by the
    /// estimate version.
    pub auto_ca: Option<bool>,
}

impl Lens {
    pub(crate) fn resolve(stored: StoredRef<'_>) -> Self {
        Self {
            mode: stored.get_or_default(&CORRECTION_MODE),
            distortion_amount: LensAmount::new(stored.get_or_default(&DISTORTION_AMOUNT)),
            vignette_amount: LensAmount::new(stored.get_or_default(&VIGNETTE_AMOUNT)),
            tca_amount: LensAmount::new(stored.get_or_default(&TCA_AMOUNT)),
            distortion_enabled: stored.get_or_default(&DISTORTION_ENABLED),
            tca_enabled: stored.get_or_default(&TCA_ENABLED),
            vignette_enabled: stored.get_or_default(&VIGNETTE_ENABLED),
            manual_vignette_amount: VignetteAmount::new(
                stored.get_or_default(&MANUAL_VIGNETTE_AMOUNT),
            ),
            manual_vignette_midpoint: VignetteMidpoint::new(
                stored.get_or_default(&MANUAL_VIGNETTE_MIDPOINT),
            ),
            profile: Profile::resolve(&stored.get_or_default(&DISTORTION_PARAMS)),
            auto_ca: stored.get(&AUTO_CHROMATIC_ABERRATION),
        }
    }
}

const MODES: TsUnion = TsUnion {
    name: "LensCorrectionMode",
    doc: "'camera': the RAW's own correction data (Fujifilm, Sony, Panasonic, …).",
    values: &["auto", "manual", "camera"],
};

/// Lens keys in the generated frontend recipe type.
pub const TS: TsSection = TsSection {
    name: "lens",
    fields: &[
        TsField::new(CORRECTION_MODE, ""),
        TsField::new(
            CAMERA_CA_VERSION,
            "Which cameras' CA data camera mode reads (crates/engine/src/embedded_lens.rs `CA_VERSION`): 1 = also Panasonic's; missing = Fujifilm and Sony only (recipes from before it, so saving them again keeps their parameters).",
        ),
        TsField::new(
            CAMERA_GEOMETRY_VERSION,
            "How camera mode reads the camera data's geometry (embedded_lens.rs `GEOMETRY_VERSION`): 1 = Sony's distortion as Camera Raw reads it; 2 = also Nikon Z framed as the camera frames it; 3 = also Canon's CTMD distortion; 4 = also Samsung's distortion; 5 = also Panasonic's and Fujifilm's framing as the cameras frame it; 6 = also Sony's knot table warped as it is (lens model 2); 7 = also Fujifilm's (lens model 3); 8 = also Nikon Z corrected about the shot's own optical centre (`centre_x`, `centre_y`); 9 = also Fujifilm's distortion knots on the whole sensor in a crop mode (Sports Finder); 10 = also on normal frames where Fujifilm's knots reach past the frame's corner; missing = as before (saving such recipes again keeps their parameters).",
        ),
        TsField::new(DISTORTION_AMOUNT, ""),
        TsField::new(VIGNETTE_AMOUNT, ""),
        TsField::new(TCA_AMOUNT, ""),
        TsField::new(DISTORTION_ENABLED, ""),
        TsField::new(TCA_ENABLED, ""),
        TsField::new(VIGNETTE_ENABLED, ""),
        TsField::new(
            MANUAL_VIGNETTE_AMOUNT,
            "Manual lens vignetting (manual_vignette.rs): a radial exposure gain of the uncropped frame, −100…+100 (positive brightens the corners); missing = none. Lightroom's Lens Vignetting.",
        ),
        TsField::new(
            MANUAL_VIGNETTE_MIDPOINT,
            "Its midpoint, 0…100 (missing = 50): lower reaches further in.",
        ),
        TsField::new(DISTORTION_PARAMS, ""),
        TsField::new(MAKER, ""),
        TsField::new(
            PROFILE_FOR,
            "The lens choice `lensDistortionParams` were resolved for; written by the backend's save (edits.rs, LensLookup) and never copied: a save keeps the parameters while it matches the recipe's lens choice.",
        ),
        TsField::new(MODEL, ""),
        TsField::new(
            AUTO_CHROMATIC_ABERRATION,
            "Lateral chromatic aberration measured on the photo and corrected (crates/engine/src/lateral_ca.rs): true = on (replacing the lens data's own CA correction), false = off; missing = off before `autoCaVersion` 2, from it on wherever the lens correction has no CA data of its own, from 3 also where it has (replacing them only where it measures clearly better).",
        ),
        TsField::new(
            AUTO_CA_VERSION,
            "The automatic lateral CA estimate (lateral_ca.rs `VERSION`): 2 = refined at full resolution, applied only when trustworthy, on by default where the lens data has no CA correction; 3 = the same, and where the lens data has one it competes with it; missing = 1 (recipes from before). New RAW edits only.",
        ),
    ],
    unions: &[MODES],
};

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn values() -> Vec<Value> {
        vec![
            Value::Null,
            json!("9"),
            json!("camera"),
            json!(true),
            json!(false),
            json!([]),
            json!({}),
            json!(-377.25),
            json!(0),
            json!(80),
            json!(999.75),
            json!(u64::MAX),
            json!(-1),
        ]
    }

    fn edge_recipes(key: &str) -> Vec<Value> {
        let mut recipes = vec![Value::Null, json!([]), json!("bad"), json!({})];
        for value in values() {
            recipes.push(json!({key: value}));
            recipes.push(
                json!({key: value, "sectionVisibility": {"geometry": false, "details": false}}),
            );
        }
        recipes
    }

    fn lens(recipe: &Value) -> Lens {
        crate::normalize(StoredRef::new(recipe), false).lens
    }

    #[test]
    fn amounts_switches_and_manual_vignette_match_old_reads() {
        type Read = fn(&Lens) -> f64;
        let numbers: [(&str, f64, Read); 5] = [
            ("lensDistortionAmount", 100.0, |l| l.distortion_amount.get()),
            ("lensVignetteAmount", 100.0, |l| l.vignette_amount.get()),
            ("lensTcaAmount", 100.0, |l| l.tca_amount.get()),
            ("lensManualVignetteAmount", 0.0, |l| {
                l.manual_vignette_amount.get()
            }),
            ("lensManualVignetteMidpoint", 50.0, |l| {
                l.manual_vignette_midpoint.get()
            }),
        ];
        for (key, default, read) in numbers {
            for recipe in edge_recipes(key) {
                assert_eq!(
                    read(&lens(&recipe)),
                    recipe[key].as_f64().unwrap_or(default),
                    "{key}: {recipe}"
                );
            }
        }
        type Switch = fn(&Lens) -> bool;
        let switches: [(&str, Switch); 3] = [
            ("lensDistortionEnabled", |l| l.distortion_enabled),
            ("lensTcaEnabled", |l| l.tca_enabled),
            ("lensVignetteEnabled", |l| l.vignette_enabled),
        ];
        for (key, read) in switches {
            for recipe in edge_recipes(key) {
                assert_eq!(
                    read(&lens(&recipe)),
                    recipe[key].as_bool().unwrap_or(true),
                    "{key}: {recipe}"
                );
            }
        }
    }

    #[test]
    fn mode_and_auto_ca_switch_match_old_reads() {
        for recipe in edge_recipes("lensCorrectionMode") {
            let mode = lens(&recipe).mode;
            assert_eq!(
                mode.is_camera(),
                recipe["lensCorrectionMode"].as_str() == Some("camera"),
                "{recipe}"
            );
            assert_eq!(
                mode.as_str(),
                recipe["lensCorrectionMode"].as_str().unwrap_or("manual"),
                "{recipe}"
            );
        }
        for recipe in edge_recipes("autoChromaticAberration") {
            assert_eq!(
                lens(&recipe).auto_ca,
                recipe
                    .get("autoChromaticAberration")
                    .and_then(Value::as_bool),
                "{recipe}"
            );
        }
    }

    /// The engine's previous read of one profile number.
    fn old(params: &Value, key: &str, default: f64) -> f64 {
        params
            .as_object()
            .and_then(|p| p.get(key).and_then(|k| k.as_f64()))
            .unwrap_or(default)
    }

    #[test]
    fn profile_matches_old_reads() {
        let mut params = vec![
            Value::Null,
            json!([1, 2]),
            json!("p"),
            json!({}),
            json!({"k1": "0.1", "k2": null, "model": "2", "table": "x", "exact_zoom": "true"}),
            json!({"k1": 0.1, "k2": -0.2, "k3": 3, "model": 4294967297u64, "table": [1, "x", null, 0.5],
                "table_end": 1.5, "table_radii": [0, 0.5, "y"], "zoom": 1.2, "exact_zoom": true,
                "centre_x": 0.25, "centre_y": -0.5, "tca_vr": 1.001, "tca_vb": 0.999,
                "tca_vr2": 0.0002, "tca_vb2": -0.0001, "vig_k1": -0.3, "vig_k2": 0.1, "vig_k3": -0.01}),
            json!({"model": -1, "zoom": -3, "centre_x": 1e308, "centre_y": "n"}),
        ];
        for key in [
            "k1", "model", "table", "zoom", "centre_x", "tca_vr", "vig_k3",
        ] {
            for value in values() {
                params.push(json!({key: value}));
            }
        }
        for p in params {
            let recipe = json!({"lensDistortionParams": p});
            let profile = lens(&recipe).profile;
            let table = |key: &str| -> Vec<f64> {
                p.as_object()
                    .and_then(|p| p.get(key).and_then(|t| t.as_array()))
                    .map(|t| t.iter().map(|v| v.as_f64().unwrap_or(f64::NAN)).collect())
                    .unwrap_or_default()
            };
            let same = |a: &[f64], b: &[f64]| {
                a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())
            };
            assert_eq!(
                [
                    profile.k1,
                    profile.k2,
                    profile.k3,
                    profile.table_end,
                    profile.zoom
                ],
                [
                    old(&p, "k1", 0.0),
                    old(&p, "k2", 0.0),
                    old(&p, "k3", 0.0),
                    old(&p, "table_end", 0.0),
                    old(&p, "zoom", 1.0)
                ],
                "{p}"
            );
            assert_eq!(
                [
                    profile.tca_vr,
                    profile.tca_vb,
                    profile.tca_vr2,
                    profile.tca_vb2,
                    profile.vig_k1,
                    profile.vig_k2,
                    profile.vig_k3
                ],
                [
                    old(&p, "tca_vr", 1.0),
                    old(&p, "tca_vb", 1.0),
                    old(&p, "tca_vr2", 0.0),
                    old(&p, "tca_vb2", 0.0),
                    old(&p, "vig_k1", 0.0),
                    old(&p, "vig_k2", 0.0),
                    old(&p, "vig_k3", 0.0)
                ],
                "{p}"
            );
            assert_eq!(
                profile.model,
                p.as_object()
                    .and_then(|p| p.get("model").and_then(|m| m.as_u64()))
                    .unwrap_or(0),
                "{p}"
            );
            assert!(same(&profile.table, &table("table")), "{p}");
            assert!(same(&profile.table_radii, &table("table_radii")), "{p}");
            assert_eq!(
                profile.exact_zoom,
                p.as_object()
                    .and_then(|p| p.get("exact_zoom").and_then(|k| k.as_bool()))
                    .unwrap_or(false),
                "{p}"
            );
            let centre = ["centre_x", "centre_y"].map(|key| {
                p.as_object()
                    .and_then(|p| p.get(key).and_then(|k| k.as_f64()))
                    .filter(|v| v.is_finite())
                    .unwrap_or(0.0)
            });
            assert_eq!(profile.centre, centre, "{p}");
        }
    }

    #[test]
    fn resolving_lens_does_not_change_stored_fields() {
        let recipe = json!({"lensCorrectionMode": "camera", "lensMaker": null, "lensModel": 7,
            "lensDistortionParams": {"k1": 0.1, "future": {"x": [null]}}, "lensProfileFor": "[1]",
            "lensManualVignetteAmount": null, "futureLens": 1});
        let stored = crate::StoredRecipe::from_value(recipe.clone());
        let _ = crate::normalize(stored.view(), false);
        assert_eq!(stored.into_value(), recipe);
    }

    #[test]
    fn lens_value_objects_carry_ranges_without_clamping() {
        assert_eq!((LensAmount::MIN, LensAmount::MAX), (0.0, 200.0));
        assert!(!LensAmount::new(999.5).in_range());
        assert_eq!(LensAmount::new(999.5).get(), 999.5);
        assert!(!VignetteAmount::new(-250.0).in_range());
        assert_eq!(VignetteAmount::new(-250.0).get(), -250.0);
        assert!(!VignetteMidpoint::new(400.0).in_range());
    }

    #[test]
    fn correction_mode_union_names_the_modes() {
        assert_eq!(MODES.values, ["auto", "manual", "camera"]);
        for mode in MODES.values {
            let json = json!(mode);
            assert_eq!(CorrectionMode::from_json(&json).unwrap().to_json(), json);
        }
    }
}
