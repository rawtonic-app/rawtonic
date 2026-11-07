//! Global crop, orientation and perspective, as stored without clamping.
//!
//! Geometry ignores section visibility, as the existing geometry stage does.
//! Slider ranges follow StudioCropControls and StudioDevelop. Crop coordinates
//! are pixels (no slider range); an aspect ratio can be any stored number.

use crate::tone::{Amount, slider_value};
use crate::ts::{TsField, TsSection, TsType};
use crate::{Key, KeyValue, StoredRef};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const CROP: Key<Option<Crop>> = Key::new("crop", None, 1);
pub const ASPECT_RATIO: Key<Option<f64>> = Key::new("aspectRatio", None, 1);
pub const ROTATION: Key<f64> = Key::new("rotation", 0.0, 1);
pub const ORIENTATION_STEPS: Key<OrientationSteps> =
    Key::new("orientationSteps", OrientationSteps(0), 1);
pub const FLIP_HORIZONTAL: Key<bool> = Key::new("flipHorizontal", false, 1);
pub const FLIP_VERTICAL: Key<bool> = Key::new("flipVertical", false, 1);
pub const TRANSFORM_DISTORTION: Key<f64> = Key::new("transformDistortion", 0.0, 1);
pub const TRANSFORM_VERTICAL: Key<f64> = Key::new("transformVertical", 0.0, 1);
pub const TRANSFORM_HORIZONTAL: Key<f64> = Key::new("transformHorizontal", 0.0, 1);
pub const TRANSFORM_ROTATE: Key<f64> = Key::new("transformRotate", 0.0, 1);
pub const TRANSFORM_ASPECT: Key<f64> = Key::new("transformAspect", 0.0, 1);
pub const TRANSFORM_SCALE: Key<f64> = Key::new("transformScale", 100.0, 1);
pub const TRANSFORM_X_OFFSET: Key<f64> = Key::new("transformXOffset", 0.0, 1);
pub const TRANSFORM_Y_OFFSET: Key<f64> = Key::new("transformYOffset", 0.0, 1);
pub const GUIDED_PERSPECTIVE: Key<GuidedPerspective> = Key::new(
    "guidedPerspective",
    GuidedPerspective {
        enabled: false,
        lines: Vec::new(),
        upright: None,
        auto_crop: None,
    },
    1,
);

/// Numeric slider keys (other value types have their own typed constants).
pub const KEYS: [&Key<f64>; 9] = [
    &ROTATION,
    &TRANSFORM_DISTORTION,
    &TRANSFORM_VERTICAL,
    &TRANSFORM_HORIZONTAL,
    &TRANSFORM_ROTATE,
    &TRANSFORM_ASPECT,
    &TRANSFORM_SCALE,
    &TRANSFORM_X_OFFSET,
    &TRANSFORM_Y_OFFSET,
];

slider_value!(
    /// Straighten/transform rotation: −45…45 degrees, step 0.1.
    Degrees,
    -45.0,
    45.0,
    0.1
);
slider_value!(
    /// Transform scale: 50…150 percent, step 1.
    Scale,
    50.0,
    150.0,
    1.0
);

/// Quarter turns as read by `as_u64`; consumers retain their existing casts/modulo.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OrientationSteps(u64);
impl OrientationSteps {
    pub const MIN: u64 = 0;
    pub const MAX: u64 = 3;
    pub const STEP: u64 = 1;
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
    pub const fn get(self) -> u64 {
        self.0
    }
    pub const fn in_range(self) -> bool {
        self.0 <= Self::MAX
    }
}
impl KeyValue for OrientationSteps {
    fn from_json(json: &Value) -> Option<Self> {
        json.as_u64().map(Self)
    }
    fn to_json(&self) -> Value {
        Value::from(self.0)
    }
}
impl TsType for OrientationSteps {
    const TS: &'static str = "number";
}

/// Crop rectangle in pixels. Decoding requires all four numbers, as before.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub struct Crop {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}
impl KeyValue for Option<Crop> {
    fn from_json(json: &Value) -> Option<Self> {
        serde_json::from_value(json.clone()).ok()
    }
    fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}
impl TsType for Option<Crop> {
    const TS: &'static str = "Crop | null";
    const IMPORT: Option<&'static str> = Some("Crop");
}
impl KeyValue for Option<f64> {
    fn from_json(json: &Value) -> Option<Self> {
        if json.is_null() {
            Some(None)
        } else {
            json.as_f64().map(Some)
        }
    }
    fn to_json(&self) -> Value {
        self.map(Value::from).unwrap_or(Value::Null)
    }
}
impl TsType for Option<f64> {
    const TS: &'static str = "number | null";
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum GuideOrientation {
    Vertical,
    Horizontal,
}
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub struct GuidePoint {
    pub x: f64,
    pub y: f64,
}
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct GuideLine {
    pub id: String,
    #[serde(rename = "type")]
    pub orientation: GuideOrientation,
    pub p1: GuidePoint,
    pub p2: GuidePoint,
}

/// The geometry stage's independently decoded guide fields. Unknown saved
/// fields remain in StoredRecipe; this resolved object is never saved over it.
#[derive(Serialize, Debug, Clone, Default, PartialEq)]
pub struct GuidedPerspective {
    pub enabled: bool,
    pub lines: Vec<GuideLine>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upright: Option<String>,
    #[serde(rename = "autoCrop", skip_serializing_if = "Option::is_none")]
    pub auto_crop: Option<bool>,
}
impl GuidedPerspective {
    pub fn fill(&self) -> bool {
        self.upright.as_deref().is_some_and(|mode| mode != "off") && self.auto_crop != Some(false)
    }
}
impl KeyValue for GuidedPerspective {
    fn from_json(json: &Value) -> Option<Self> {
        let fields = json.as_object()?;
        Some(Self {
            enabled: fields
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            // Preserve all-or-nothing line decoding, including required ids.
            lines: fields
                .get("lines")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default(),
            upright: fields
                .get("upright")
                .and_then(Value::as_str)
                .map(str::to_owned),
            auto_crop: fields.get("autoCrop").and_then(Value::as_bool),
        })
    }
    fn to_json(&self) -> Value {
        // A non-finite coordinate is written as null, never an error.
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}
impl TsType for GuidedPerspective {
    const TS: &'static str = "GuidedPerspective";
    const IMPORT: Option<&'static str> = Some("GuidedPerspective");
}

/// Geometry as rendered. No visibility gates; numeric values remain stored.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Geometry {
    pub crop: Option<Crop>,
    pub aspect_ratio: Option<f64>,
    pub rotation: Degrees,
    pub orientation_steps: OrientationSteps,
    pub flip_horizontal: bool,
    pub flip_vertical: bool,
    pub distortion: Amount,
    pub vertical: Amount,
    pub horizontal: Amount,
    pub rotate: Degrees,
    pub aspect: Amount,
    pub scale: Scale,
    pub x_offset: Amount,
    pub y_offset: Amount,
    pub guided: GuidedPerspective,
}
impl Geometry {
    pub(crate) fn resolve(stored: StoredRef<'_>) -> Self {
        Self {
            crop: stored.get_or_default(&CROP),
            aspect_ratio: stored.get_or_default(&ASPECT_RATIO),
            rotation: Degrees::new(stored.get_or_default(&ROTATION)),
            orientation_steps: stored.get_or_default(&ORIENTATION_STEPS),
            flip_horizontal: stored.get_or_default(&FLIP_HORIZONTAL),
            flip_vertical: stored.get_or_default(&FLIP_VERTICAL),
            distortion: Amount::new(stored.get_or_default(&TRANSFORM_DISTORTION)),
            vertical: Amount::new(stored.get_or_default(&TRANSFORM_VERTICAL)),
            horizontal: Amount::new(stored.get_or_default(&TRANSFORM_HORIZONTAL)),
            rotate: Degrees::new(stored.get_or_default(&TRANSFORM_ROTATE)),
            aspect: Amount::new(stored.get_or_default(&TRANSFORM_ASPECT)),
            scale: Scale::new(stored.get_or_default(&TRANSFORM_SCALE)),
            x_offset: Amount::new(stored.get_or_default(&TRANSFORM_X_OFFSET)),
            y_offset: Amount::new(stored.get_or_default(&TRANSFORM_Y_OFFSET)),
            guided: stored.get_or_default(&GUIDED_PERSPECTIVE),
        }
    }
}

/// Geometry keys in the generated frontend recipe type.
pub const TS: TsSection = TsSection {
    name: "geometry",
    fields: &[
        TsField::new(CROP, ""),
        TsField::new(ASPECT_RATIO, ""),
        TsField::new(ROTATION, ""),
        TsField::new(ORIENTATION_STEPS, ""),
        TsField::new(FLIP_HORIZONTAL, ""),
        TsField::new(FLIP_VERTICAL, ""),
        TsField::new(TRANSFORM_DISTORTION, ""),
        TsField::new(TRANSFORM_VERTICAL, ""),
        TsField::new(TRANSFORM_HORIZONTAL, ""),
        TsField::new(TRANSFORM_ROTATE, ""),
        TsField::new(TRANSFORM_ASPECT, ""),
        TsField::new(TRANSFORM_SCALE, ""),
        TsField::new(TRANSFORM_X_OFFSET, ""),
        TsField::new(TRANSFORM_Y_OFFSET, ""),
        TsField::new(GUIDED_PERSPECTIVE, ""),
    ],
    unions: &[],
};

/// Resolve only this section for a stage that does not consume other sections.
pub fn resolve(stored: StoredRef<'_>) -> Geometry {
    Geometry::resolve(stored)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn edge_recipes(key: &str) -> Vec<Value> {
        let mut recipes = vec![
            Value::Null,
            json!([]),
            json!("bad"),
            json!(false),
            json!({}),
        ];
        for value in [
            Value::Null,
            json!("9"),
            json!(false),
            json!([]),
            json!({}),
            json!(-377.25),
            json!(999.75),
            json!(0),
            json!(2),
            json!(256),
            json!(u64::MAX),
            json!(-1),
            json!(1.0),
        ] {
            for visibility in [
                Value::Null,
                json!([]),
                json!({"geometry": false, "crop": false}),
                json!({"geometry": "off"}),
            ] {
                recipes.push(json!({key: value, "sectionVisibility": visibility}));
            }
        }
        recipes
    }

    #[test]
    fn geometry_sliders_match_old_reads() {
        for (key, default, value) in [
            (
                "rotation",
                0.0,
                (|g: &Geometry| g.rotation.get()) as fn(&Geometry) -> f64,
            ),
            ("transformDistortion", 0.0, |g| g.distortion.get()),
            ("transformVertical", 0.0, |g| g.vertical.get()),
            ("transformHorizontal", 0.0, |g| g.horizontal.get()),
            ("transformRotate", 0.0, |g| g.rotate.get()),
            ("transformAspect", 0.0, |g| g.aspect.get()),
            ("transformScale", 100.0, |g| g.scale.get()),
            ("transformXOffset", 0.0, |g| g.x_offset.get()),
            ("transformYOffset", 0.0, |g| g.y_offset.get()),
        ] {
            for recipe in edge_recipes(key) {
                let resolved = crate::normalize(StoredRef::new(&recipe), false);
                assert_eq!(
                    value(&resolved.geometry),
                    recipe[key].as_f64().unwrap_or(default),
                    "{key}: {recipe}"
                );
            }
        }
    }

    #[test]
    fn orientation_flips_and_ratio_match_old_reads() {
        for key in [
            "orientationSteps",
            "flipHorizontal",
            "flipVertical",
            "aspectRatio",
        ] {
            for recipe in edge_recipes(key) {
                let g = crate::normalize(StoredRef::new(&recipe), false).geometry;
                assert_eq!(
                    g.orientation_steps.get(),
                    recipe["orientationSteps"].as_u64().unwrap_or(0),
                    "{recipe}"
                );
                assert_eq!(
                    g.flip_horizontal,
                    recipe["flipHorizontal"].as_bool().unwrap_or(false),
                    "{recipe}"
                );
                assert_eq!(
                    g.flip_vertical,
                    recipe["flipVertical"].as_bool().unwrap_or(false),
                    "{recipe}"
                );
                assert_eq!(g.aspect_ratio, recipe["aspectRatio"].as_f64(), "{recipe}");
            }
        }
        for key in ["flipHorizontal", "flipVertical"] {
            let recipe = json!({key: true, "sectionVisibility": {"geometry": false}});
            let g = crate::normalize(StoredRef::new(&recipe), false).geometry;
            assert_eq!(g.flip_horizontal, key == "flipHorizontal");
            assert_eq!(g.flip_vertical, key == "flipVertical");
        }
    }

    #[test]
    fn crop_matches_old_required_rectangle_decode() {
        let mut recipes = edge_recipes("crop");
        for crop in [
            json!({"x": -3.5, "y": 2, "width": 999.75, "height": -7, "unit": "px", "future": 7}),
            json!({"x": 3, "y": 2, "width": 99}),
            json!({"x": "3", "y": 2, "width": 99, "height": 75}),
        ] {
            recipes.push(json!({"crop": crop}));
        }
        for recipe in recipes {
            let g = crate::normalize(StoredRef::new(&recipe), false).geometry;
            let old = serde_json::from_value::<Crop>(recipe["crop"].clone()).ok();
            assert_eq!(g.crop, old, "{recipe}");
        }
    }

    #[test]
    fn guides_match_independent_fields_and_all_or_nothing_line_decode() {
        let line = json!({"id":"v1", "type":"vertical", "p1":{"x":0.1, "y":0.2}, "p2":{"x":0.3, "y":0.9}, "future": 1});
        let mut recipes = edge_recipes("guidedPerspective");
        for enabled in [Value::Null, json!("true"), json!(true), json!(false)] {
            for lines in [
                Value::Null,
                json!([]),
                json!([line]),
                json!([line, {"type":"vertical"}]),
            ] {
                for upright in [
                    Value::Null,
                    json!(false),
                    json!("off"),
                    json!("future"),
                    json!("vertical"),
                ] {
                    for auto_crop in [Value::Null, json!(false), json!(true), json!("false")] {
                        recipes.push(json!({"guidedPerspective": {"enabled":enabled, "lines":lines, "upright":upright, "autoCrop": auto_crop}}));
                    }
                }
            }
        }
        for recipe in recipes {
            let g = crate::normalize(StoredRef::new(&recipe), false)
                .geometry
                .guided;
            let old = recipe.get("guidedPerspective");
            assert_eq!(
                g.enabled,
                old.and_then(|g| g.get("enabled"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                "{recipe}"
            );
            let lines: Vec<GuideLine> = old
                .and_then(|g| g.get("lines"))
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default();
            assert_eq!(g.lines, lines, "{recipe}");
            assert_eq!(
                g.fill(),
                old.and_then(|g| g.get("upright"))
                    .and_then(Value::as_str)
                    .is_some_and(|mode| mode != "off")
                    && old.and_then(|g| g.get("autoCrop")).and_then(Value::as_bool) != Some(false),
                "{recipe}"
            );
        }
    }

    #[test]
    fn resolving_geometry_does_not_change_stored_fields() {
        let recipe = json!({"crop": {"x":3,"y":7,"width":90,"height":60,"unit":"px","future":null},
            "aspectRatio":null,"orientationSteps":256,"rotation":99.5,
            "guidedPerspective":{"enabled":true,"lines":[{"id":"v1","type":"vertical", "p1":{"x":0.1,"y":0.2}, "p2":{"x":0.3,"y":0.8},"future":[null]}],"autoCrop":null,"upright":"future","unknown":{"x":null}},
            "futureGeometry": {"v": 7}});
        let stored = crate::StoredRecipe::from_value(recipe.clone());
        let _ = crate::normalize(stored.view(), false);
        assert_eq!(stored.into_value(), recipe);
    }

    #[test]
    fn geometry_value_objects_carry_ranges_without_clamping() {
        assert_eq!(
            (Degrees::MIN, Degrees::MAX, Degrees::STEP),
            (-45.0, 45.0, 0.1)
        );
        assert!(!Degrees::new(99.5).in_range());
        assert_eq!(Degrees::new(99.5).get(), 99.5);
        assert_eq!((Scale::MIN, Scale::MAX, Scale::STEP), (50.0, 150.0, 1.0));
        assert!(!Scale::new(-999.0).in_range());
        assert_eq!(Scale::new(-999.0).get(), -999.0);
        assert!(!OrientationSteps::new(256).in_range());
        assert_eq!(OrientationSteps::new(256).get(), 256);
    }
}
