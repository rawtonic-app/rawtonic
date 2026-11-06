//! Post-crop vignette, grain, LUT, optical glow and depth-based lens blur.
//!
//! Slider ranges follow StudioDevelop and StudioLut; values never clamp here.
//! The renderer retains the existing conversion order and clamps. Lens blur's
//! missing-value defaults (50 amount, 15 fades) differ from the frontend's
//! initial values (40, 20); resolving a partial saved recipe keeps the former.

use crate::tone::{Amount, slider_value};
use crate::ts::{TsField, TsSection, TsType, TsUnion};
use crate::{Key, KeyValue, Section, StoredRef};
use serde_json::Value;

pub const VIGNETTE_AMOUNT: Key<f64> = Key::new("vignetteAmount", 0.0, 1);
pub const VIGNETTE_MIDPOINT: Key<f64> = Key::new("vignetteMidpoint", 50.0, 1);
pub const VIGNETTE_ROUNDNESS: Key<f64> = Key::new("vignetteRoundness", 0.0, 1);
pub const VIGNETTE_FEATHER: Key<f64> = Key::new("vignetteFeather", 50.0, 1);
pub const VIGNETTE_HIGHLIGHTS: Key<f64> = Key::new("vignetteHighlights", 0.0, 1);
pub const GRAIN_AMOUNT: Key<f64> = Key::new("grainAmount", 0.0, 1);
pub const GRAIN_SIZE: Key<f64> = Key::new("grainSize", 25.0, 1);
pub const GRAIN_ROUGHNESS: Key<f64> = Key::new("grainRoughness", 50.0, 1);
pub const GRAIN_COLOR: Key<f64> = Key::new("grainColor", 0.0, 1);
pub const GLOW_AMOUNT: Key<f64> = Key::new("glowAmount", 0.0, 1);
pub const HALATION_AMOUNT: Key<f64> = Key::new("halationAmount", 0.0, 1);
pub const FLARE_AMOUNT: Key<f64> = Key::new("flareAmount", 0.0, 1);
pub const LUT_INTENSITY: Key<f64> = Key::new("lutIntensity", 100.0, 1);
pub const LUT_SIZE: Key<f64> = Key::new("lutSize", 0.0, 1);
pub const LENS_BLUR_AMOUNT: Key<f64> = Key::new("lensBlurAmount", 50.0, 1);
pub const LENS_BLUR_DIFFUSION: Key<f64> = Key::new("lensBlurDiffusion", 0.0, 1);
pub const LENS_BLUR_MIN_DEPTH: Key<f64> = Key::new("lensBlurMinDepth", 20.0, 1);
pub const LENS_BLUR_MAX_DEPTH: Key<f64> = Key::new("lensBlurMaxDepth", 100.0, 1);
pub const LENS_BLUR_MIN_FADE: Key<f64> = Key::new("lensBlurMinFade", 15.0, 1);
pub const LENS_BLUR_MAX_FADE: Key<f64> = Key::new("lensBlurMaxFade", 15.0, 1);
pub const GRAIN_VERSION: Key<u64> = Key::new("grainVersion", 1, 1);
pub const GRAIN_STRUCTURE: Key<GrainStructure> =
    Key::new("grainStructure", GrainStructure::Even, 1);
pub const VIGNETTE_STYLE: Key<VignetteStyle> =
    Key::new("vignetteStyle", VignetteStyle::HighlightPriority, 1);
pub const LENS_BLUR_SHAPE: Key<LensBlurShape> = Key::new("lensBlurShape", LensBlurShape::Circle, 1);
pub const LENS_BLUR_ENABLED: Key<bool> = Key::new("lensBlurEnabled", false, 1);
pub const LUT_IS_SCENE_REFERRED: Key<bool> = Key::new("lutIsSceneReferred", false, 1);
pub use crate::resources::LUT_PATH;
pub const LUT_NAME: Key<Option<String>> = Key::new("lutName", None, 1);
pub const LUT_DATA: Key<Option<String>> = Key::new("lutData", None, 1);
pub const LENS_BLUR_DEPTH_MAP: Key<Option<String>> = Key::new("lensBlurDepthMap", None, 1);

pub const KEYS: [&Key<f64>; 20] = [
    &VIGNETTE_AMOUNT,
    &VIGNETTE_MIDPOINT,
    &VIGNETTE_ROUNDNESS,
    &VIGNETTE_FEATHER,
    &VIGNETTE_HIGHLIGHTS,
    &GRAIN_AMOUNT,
    &GRAIN_SIZE,
    &GRAIN_ROUGHNESS,
    &GRAIN_COLOR,
    &GLOW_AMOUNT,
    &HALATION_AMOUNT,
    &FLARE_AMOUNT,
    &LUT_INTENSITY,
    &LUT_SIZE,
    &LENS_BLUR_AMOUNT,
    &LENS_BLUR_DIFFUSION,
    &LENS_BLUR_MIN_DEPTH,
    &LENS_BLUR_MAX_DEPTH,
    &LENS_BLUR_MIN_FADE,
    &LENS_BLUR_MAX_FADE,
];

slider_value!(
    /// Effects percentage: 0–100, step 1.
    Percent, 0.0, 100.0, 1.0
);
slider_value!(
    /// LUT Amount: 0–200 percent, step 1.
    LutAmount, 0.0, 200.0, 1.0
);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GrainStructure {
    Even,
    Soft,
    Clumped,
    Speckled,
    Gritty,
}
impl GrainStructure {
    pub const ALL: [Self; 5] = [
        Self::Even,
        Self::Soft,
        Self::Clumped,
        Self::Speckled,
        Self::Gritty,
    ];
    pub const NAMES: [&'static str; 5] = [
        Self::Even.name(),
        Self::Soft.name(),
        Self::Clumped.name(),
        Self::Speckled.name(),
        Self::Gritty.name(),
    ];
    pub const fn name(self) -> &'static str {
        match self {
            Self::Even => "even",
            Self::Soft => "soft",
            Self::Clumped => "clumped",
            Self::Speckled => "speckled",
            Self::Gritty => "gritty",
        }
    }
    pub const fn uniform(self) -> u32 {
        self as u32
    }
}
impl KeyValue for GrainStructure {
    fn from_json(json: &Value) -> Option<Self> {
        let name = json.as_str()?;
        Self::NAMES
            .iter()
            .position(|n| *n == name)
            .map(|i| Self::ALL[i])
    }
    fn to_json(&self) -> Value {
        Value::from(self.name())
    }
}
impl TsType for GrainStructure {
    const TS: &'static str = "GrainStructure";
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VignetteStyle {
    HighlightPriority,
    ColorPriority,
    PaintOverlay,
}
impl VignetteStyle {
    pub const ALL: [Self; 3] = [
        Self::HighlightPriority,
        Self::ColorPriority,
        Self::PaintOverlay,
    ];
    pub const NAMES: [&'static str; 3] = [
        Self::HighlightPriority.name(),
        Self::ColorPriority.name(),
        Self::PaintOverlay.name(),
    ];
    pub const fn name(self) -> &'static str {
        match self {
            Self::HighlightPriority => "highlightPriority",
            Self::ColorPriority => "colorPriority",
            Self::PaintOverlay => "paintOverlay",
        }
    }
    pub const fn uniform(self) -> u32 {
        self as u32
    }
}
impl KeyValue for VignetteStyle {
    fn from_json(json: &Value) -> Option<Self> {
        let name = json.as_str()?;
        Self::NAMES
            .iter()
            .position(|n| *n == name)
            .map(|i| Self::ALL[i])
    }
    fn to_json(&self) -> Value {
        Value::from(self.name())
    }
}
impl TsType for VignetteStyle {
    const TS: &'static str = "VignetteStyle";
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LensBlurShape {
    Circle,
    Hexagon,
    Octagon,
    Ring,
}
impl LensBlurShape {
    pub const ALL: [Self; 4] = [Self::Circle, Self::Hexagon, Self::Octagon, Self::Ring];
    pub const NAMES: [&'static str; 4] = [
        Self::Circle.name(),
        Self::Hexagon.name(),
        Self::Octagon.name(),
        Self::Ring.name(),
    ];
    pub const fn name(self) -> &'static str {
        match self {
            Self::Circle => "circle",
            Self::Hexagon => "hexagon",
            Self::Octagon => "octagon",
            Self::Ring => "ring",
        }
    }
}
impl KeyValue for LensBlurShape {
    fn from_json(json: &Value) -> Option<Self> {
        let name = json.as_str()?;
        Self::NAMES
            .iter()
            .position(|n| *n == name)
            .map(|i| Self::ALL[i])
    }
    fn to_json(&self) -> Value {
        Value::from(self.name())
    }
}
impl TsType for LensBlurShape {
    const TS: &'static str = "LensBlurShape";
}

/// Effects as rendered. Amounts/shape parameters default when effects are
/// hidden; grain structure and colour remain stored even when hidden, just as
/// before. LUT resources and lens-blur data are borrowed, never decoded or copied
/// here. The version is stored/defaulted; RecipeVersions keeps its existing clamp.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Effects<'a> {
    pub visible: bool,
    pub vignette_amount: Amount,
    pub vignette_midpoint: Percent,
    pub vignette_roundness: Amount,
    pub vignette_feather: Percent,
    pub vignette_highlights: Percent,
    pub grain_amount: Percent,
    pub grain_size: Percent,
    pub grain_roughness: Percent,
    pub grain_color: Percent,
    pub glow_amount: Percent,
    pub halation_amount: Percent,
    pub flare_amount: Percent,
    pub lut_intensity: LutAmount,
    pub lut_size: f64,
    pub lens_blur_amount: Percent,
    pub lens_blur_diffusion: Percent,
    pub lens_blur_min_depth: Percent,
    pub lens_blur_max_depth: Percent,
    pub lens_blur_min_fade: Percent,
    pub lens_blur_max_fade: Percent,
    pub vignette_style: VignetteStyle,
    pub grain_structure: GrainStructure,
    pub lut_path: Option<&'a str>,
    pub lut_name: Option<&'a str>,
    pub lut_data: Option<&'a str>,
    pub lut_is_scene_referred: bool,
    pub lens_blur_enabled: bool,
    pub lens_blur_shape: LensBlurShape,
    pub lens_blur_depth_map: Option<&'a str>,
}

impl<'a> Effects<'a> {
    pub(crate) fn resolve(stored: StoredRef<'a>) -> Self {
        let visible = stored.section_visible(Section::EFFECTS);
        let visible_read = |key: &Key<f64>| {
            if visible {
                stored.get_or_default(key)
            } else {
                key.default()
            }
        };
        Self {
            visible,
            vignette_amount: Amount::new(visible_read(&VIGNETTE_AMOUNT)),
            vignette_midpoint: Percent::new(visible_read(&VIGNETTE_MIDPOINT)),
            vignette_roundness: Amount::new(visible_read(&VIGNETTE_ROUNDNESS)),
            vignette_feather: Percent::new(visible_read(&VIGNETTE_FEATHER)),
            vignette_highlights: Percent::new(visible_read(&VIGNETTE_HIGHLIGHTS)),
            grain_amount: Percent::new(visible_read(&GRAIN_AMOUNT)),
            grain_size: Percent::new(visible_read(&GRAIN_SIZE)),
            grain_roughness: Percent::new(visible_read(&GRAIN_ROUGHNESS)),
            grain_color: Percent::new(stored.get_or_default(&GRAIN_COLOR)),
            glow_amount: Percent::new(visible_read(&GLOW_AMOUNT)),
            halation_amount: Percent::new(visible_read(&HALATION_AMOUNT)),
            flare_amount: Percent::new(visible_read(&FLARE_AMOUNT)),
            lut_intensity: LutAmount::new(visible_read(&LUT_INTENSITY)),
            lut_size: stored.get_or_default(&LUT_SIZE),
            lens_blur_amount: Percent::new(stored.get_or_default(&LENS_BLUR_AMOUNT)),
            lens_blur_diffusion: Percent::new(stored.get_or_default(&LENS_BLUR_DIFFUSION)),
            lens_blur_min_depth: Percent::new(stored.get_or_default(&LENS_BLUR_MIN_DEPTH)),
            lens_blur_max_depth: Percent::new(stored.get_or_default(&LENS_BLUR_MAX_DEPTH)),
            lens_blur_min_fade: Percent::new(stored.get_or_default(&LENS_BLUR_MIN_FADE)),
            lens_blur_max_fade: Percent::new(stored.get_or_default(&LENS_BLUR_MAX_FADE)),
            vignette_style: if visible {
                stored.get_or_default(&VIGNETTE_STYLE)
            } else {
                VIGNETTE_STYLE.default()
            },
            grain_structure: stored.get_or_default(&GRAIN_STRUCTURE),
            lut_path: stored.get_str(&LUT_PATH),
            lut_name: stored.get_str(&LUT_NAME),
            lut_data: stored.get_str(&LUT_DATA),
            lut_is_scene_referred: visible && stored.get_or_default(&LUT_IS_SCENE_REFERRED),
            lens_blur_enabled: stored.get_or_default(&LENS_BLUR_ENABLED),
            lens_blur_shape: stored.get_or_default(&LENS_BLUR_SHAPE),
            lens_blur_depth_map: stored.get_str(&LENS_BLUR_DEPTH_MAP),
        }
    }
}

pub const TS: TsSection = TsSection {
    name: "effects",
    fields: &[
        TsField::new(VIGNETTE_AMOUNT, ""),
        TsField::new(VIGNETTE_MIDPOINT, ""),
        TsField::new(VIGNETTE_ROUNDNESS, ""),
        TsField::new(VIGNETTE_FEATHER, ""),
        TsField::new(VIGNETTE_HIGHLIGHTS, ""),
        TsField::new(GRAIN_AMOUNT, ""),
        TsField::new(GRAIN_SIZE, ""),
        TsField::new(GRAIN_ROUGHNESS, ""),
        TsField::new(
            GRAIN_COLOR,
            "Film grain's colour (grain version 3): 0 one neutral layer like black-and-white film, 100 independent dye layers; missing = 0.",
        ),
        TsField::new(GLOW_AMOUNT, ""),
        TsField::new(HALATION_AMOUNT, ""),
        TsField::new(FLARE_AMOUNT, ""),
        TsField::new(LUT_INTENSITY, ""),
        TsField::new(LUT_SIZE, ""),
        TsField::new(LENS_BLUR_AMOUNT, ""),
        TsField::new(LENS_BLUR_DIFFUSION, ""),
        TsField::new(LENS_BLUR_MIN_DEPTH, ""),
        TsField::new(LENS_BLUR_MAX_DEPTH, ""),
        TsField::new(LENS_BLUR_MIN_FADE, ""),
        TsField::new(LENS_BLUR_MAX_FADE, ""),
        TsField::new(
            GRAIN_VERSION,
            "The grain a recipe renders (GRAIN_VERSION); missing = 1.",
        ),
        TsField::new(
            GRAIN_STRUCTURE,
            "Grain structure; missing = 'even', the original grain (docs/RECIPE_SCHEMA.md).",
        ),
        TsField::new(
            VIGNETTE_STYLE,
            "Lightroom's post-crop vignette style and Highlights (0–100).",
        ),
        TsField::new(LENS_BLUR_SHAPE, ""),
        TsField::new(LENS_BLUR_ENABLED, ""),
        TsField::new(LUT_IS_SCENE_REFERRED, ""),
        TsField::new(LUT_PATH, ""),
        TsField::new(LUT_NAME, ""),
        TsField::new(LUT_DATA, ""),
        TsField::new(LENS_BLUR_DEPTH_MAP, ""),
    ],
    unions: &[
        TsUnion {
            name: GrainStructure::TS,
            doc: "",
            values: &GrainStructure::NAMES,
        },
        TsUnion {
            name: VignetteStyle::TS,
            doc: "",
            values: &VignetteStyle::NAMES,
        },
        TsUnion {
            name: LensBlurShape::TS,
            doc: "",
            values: &LensBlurShape::NAMES,
        },
    ],
};

/// Resolve just the effects needed by a CPU stage, without reading other sections.
pub fn resolve(stored: StoredRef<'_>) -> Effects<'_> {
    Effects::resolve(stored)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{StoredRecipe, normalize};
    use serde_json::json;

    // Independently pinned old reads: names, defaults and visibility do not
    // come from the registry. These are raw stored values, before GPU casts.
    type Getter = fn(&Effects<'_>) -> f64;
    const OLD_NUMBERS: &[(&str, f64, bool, Getter)] = &[
        ("vignetteAmount", 0.0, true, |e| e.vignette_amount.get()),
        ("vignetteMidpoint", 50.0, true, |e| {
            e.vignette_midpoint.get()
        }),
        ("vignetteRoundness", 0.0, true, |e| {
            e.vignette_roundness.get()
        }),
        ("vignetteFeather", 50.0, true, |e| e.vignette_feather.get()),
        ("vignetteHighlights", 0.0, true, |e| {
            e.vignette_highlights.get()
        }),
        ("grainAmount", 0.0, true, |e| e.grain_amount.get()),
        ("grainSize", 25.0, true, |e| e.grain_size.get()),
        ("grainRoughness", 50.0, true, |e| e.grain_roughness.get()),
        ("grainColor", 0.0, false, |e| e.grain_color.get()),
        ("glowAmount", 0.0, true, |e| e.glow_amount.get()),
        ("halationAmount", 0.0, true, |e| e.halation_amount.get()),
        ("flareAmount", 0.0, true, |e| e.flare_amount.get()),
        ("lutIntensity", 100.0, true, |e| e.lut_intensity.get()),
        ("lutSize", 0.0, false, |e| e.lut_size),
        ("lensBlurAmount", 50.0, false, |e| e.lens_blur_amount.get()),
        ("lensBlurDiffusion", 0.0, false, |e| {
            e.lens_blur_diffusion.get()
        }),
        ("lensBlurMinDepth", 20.0, false, |e| {
            e.lens_blur_min_depth.get()
        }),
        ("lensBlurMaxDepth", 100.0, false, |e| {
            e.lens_blur_max_depth.get()
        }),
        ("lensBlurMinFade", 15.0, false, |e| {
            e.lens_blur_min_fade.get()
        }),
        ("lensBlurMaxFade", 15.0, false, |e| {
            e.lens_blur_max_fade.get()
        }),
    ];

    fn visibility(recipe: &Value) -> bool {
        recipe
            .get("sectionVisibility")
            .and_then(|v| v.get("effects"))
            .and_then(Value::as_bool)
            .unwrap_or(true)
    }

    #[test]
    fn every_numeric_field_matches_its_old_read() {
        assert_eq!(OLD_NUMBERS.len(), KEYS.len());
        for ((name, default, gated, get), key) in OLD_NUMBERS.iter().zip(KEYS) {
            assert_eq!(*name, key.name());
            for root in [
                Value::Null,
                json!(false),
                json!(7),
                json!("recipe"),
                json!([]),
                json!({}),
            ] {
                assert_eq!(
                    get(&normalize(StoredRef::new(&root), true).effects),
                    *default,
                    "{name}: {root}"
                );
            }
            for value in [
                Value::Null,
                json!(false),
                json!("25"),
                json!([]),
                json!({}),
                json!(-1e300),
                json!(-137.5),
                json!(0),
                json!(52.75),
                json!(999),
                json!(1e300),
            ] {
                for sections in [
                    Value::Null,
                    json!(false),
                    json!({}),
                    json!({"effects": false}),
                    json!({"effects": true}),
                    json!({"effects": "false"}),
                    json!({"basic":false,"details":false,"presence":false}),
                ] {
                    let recipe = json!({(*name): value, "sectionVisibility":sections});
                    let old = if *gated && !visibility(&recipe) {
                        *default
                    } else {
                        recipe[*name].as_f64().unwrap_or(*default)
                    };
                    for is_raw in [false, true] {
                        assert_eq!(
                            get(&normalize(StoredRef::new(&recipe), is_raw).effects),
                            old,
                            "{name}: {recipe}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn enum_fields_match_the_old_shader_choices() {
        for value in [
            Value::Null,
            json!(false),
            json!(0),
            json!([]),
            json!({}),
            json!(""),
            json!("unknown"),
            json!("even"),
            json!("soft"),
            json!("clumped"),
            json!("speckled"),
            json!("gritty"),
            json!("colorPriority"),
            json!("paintOverlay"),
            json!("circle"),
            json!("hexagon"),
            json!("octagon"),
            json!("ring"),
        ] {
            for visible in [true, false] {
                let recipe = json!({"grainStructure":value,"vignetteStyle":value,"lensBlurShape":value,"sectionVisibility":{"effects":visible}});
                let effects = normalize(StoredRef::new(&recipe), false).effects;
                let old_grain = match recipe["grainStructure"].as_str() {
                    Some("soft") => 1,
                    Some("clumped") => 2,
                    Some("speckled") => 3,
                    Some("gritty") => 4,
                    _ => 0,
                };
                let old_vignette = if visible {
                    match recipe["vignetteStyle"].as_str() {
                        Some("colorPriority") => 1,
                        Some("paintOverlay") => 2,
                        _ => 0,
                    }
                } else {
                    0
                };
                let old_shape = match recipe["lensBlurShape"].as_str() {
                    Some("hexagon") => "hexagon",
                    Some("octagon") => "octagon",
                    Some("ring") => "ring",
                    _ => "circle",
                };
                assert_eq!(effects.grain_structure.uniform(), old_grain);
                assert_eq!(effects.vignette_style.uniform(), old_vignette);
                assert_eq!(effects.lens_blur_shape.name(), old_shape);
            }
        }
    }

    #[test]
    fn reference_and_switch_fields_match_old_reads_without_copying_payloads() {
        for value in [
            Value::Null,
            json!(false),
            json!(true),
            json!(-1),
            json!(0),
            json!(1),
            json!(u64::MAX),
            json!(1.5),
            json!([]),
            json!({}),
            json!(""),
            json!("resource.cube"),
            json!("data:image/png;base64,AAAA"),
        ] {
            for visible in [true, false] {
                let recipe = json!({"lutPath":value,"lutName":value,"lutData":value,"lensBlurDepthMap":value,"lutIsSceneReferred":value,"lensBlurEnabled":value,"grainVersion":value,"sectionVisibility":{"effects":visible}});
                let effects = normalize(StoredRef::new(&recipe), false).effects;
                for (name, actual) in [
                    ("lutPath", effects.lut_path),
                    ("lutName", effects.lut_name),
                    ("lutData", effects.lut_data),
                    ("lensBlurDepthMap", effects.lens_blur_depth_map),
                ] {
                    assert_eq!(actual, recipe[name].as_str());
                    if let Some(actual) = actual {
                        assert!(std::ptr::eq(actual, recipe[name].as_str().unwrap()));
                    }
                }
                assert_eq!(
                    effects.lut_is_scene_referred,
                    visible && recipe["lutIsSceneReferred"].as_bool().unwrap_or(false)
                );
                assert_eq!(
                    effects.lens_blur_enabled,
                    recipe["lensBlurEnabled"].as_bool().unwrap_or(false)
                );
                assert_eq!(
                    StoredRef::new(&recipe).get_or_default(&GRAIN_VERSION),
                    recipe["grainVersion"].as_u64().unwrap_or(1)
                );
            }
        }
    }

    #[test]
    fn resolution_leaves_stored_effects_lossless() {
        let original = json!({"grainStructure":"future","grainVersion":999,"vignetteStyle":null,"lutPath":null,"lensBlurShape":"future-shape","future":{"nested":[null,{"unknown":true}]},"lensBlurDepthMap":"data:unchanged"});
        let stored: StoredRecipe = serde_json::from_value(original.clone()).unwrap();
        let _ = normalize(stored.view(), true);
        assert_eq!(serde_json::to_value(stored).unwrap(), original);
    }

    #[test]
    fn typed_codecs_round_trip_and_ranges_do_not_clamp() {
        for key in KEYS {
            for v in [-999.5, 0.0, 999.5] {
                let mut stored = StoredRecipe::default();
                stored.set(key, v);
                assert_eq!(stored.get(key), Some(v));
            }
        }
        for name in ["even", "soft", "clumped", "speckled", "gritty"] {
            let v = GrainStructure::from_json(&json!(name)).unwrap();
            assert_eq!(v.to_json(), json!(name));
        }
        for name in ["highlightPriority", "colorPriority", "paintOverlay"] {
            let v = VignetteStyle::from_json(&json!(name)).unwrap();
            assert_eq!(v.to_json(), json!(name));
        }
        for name in ["circle", "hexagon", "octagon", "ring"] {
            let v = LensBlurShape::from_json(&json!(name)).unwrap();
            assert_eq!(v.to_json(), json!(name));
        }
        let mut stored = StoredRecipe::default();
        stored.set(&LUT_PATH, Some("look.cube".into()));
        assert_eq!(stored.get(&LUT_PATH), Some(Some("look.cube".into())));
        stored.set(&LUT_PATH, None);
        assert_eq!(stored.get(&LUT_PATH), Some(None));
        stored.set(&GRAIN_VERSION, u64::MAX);
        assert_eq!(stored.get(&GRAIN_VERSION), Some(u64::MAX));
        assert!(!Percent::new(-1.0).in_range());
        assert_eq!(Percent::new(120.0).get(), 120.0);
        assert!(LutAmount::new(200.0).in_range());
        assert!(!LutAmount::new(201.0).in_range());
    }
}
