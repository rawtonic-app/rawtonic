//! Mask containers and local adjustments as stored and as rendered.
//!
//! Container decoding remains all-or-nothing, as the old serde reader was.
//! Geometry/payload JSON is borrowed during normalization; brush points and
//! embedded bitmaps are copied only when the engine needs an owned definition.
use crate::ts::{TsField, TsSection};
use crate::{Key, KeyValue, Section, StoredRef};
use crate::{color, detail, effects, tone};
use serde::Deserialize;
use serde_json::Value;

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(crate = "serde")]
#[serde(rename_all = "camelCase")]
pub enum SubMaskMode {
    Additive,
    Subtractive,
    Intersect,
}

impl KeyValue for SubMaskMode {
    fn from_json(value: &Value) -> Option<Self> {
        Self::deserialize(value).ok()
    }
    fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

pub const MASKS: Key<Value> = Key::new("masks", Value::Null, 1);
pub const ID: Key<String> = Key::new("id", String::new(), 1);
pub const NAME: Key<String> = Key::new("name", String::new(), 1);
pub const VISIBLE: Key<bool> = Key::new("visible", true, 1);
pub const INVERT: Key<bool> = Key::new("invert", false, 1);
pub const OPACITY: Key<f64> = Key::new("opacity", 100.0, 1);
pub const ADJUSTMENTS: Key<Value> = Key::new("adjustments", Value::Null, 1);
pub const SUB_MASKS: Key<Value> = Key::new("subMasks", Value::Null, 1);
pub const MASK_VERSION: Key<u64> = Key::new("maskVersion", 0, 1);
pub const REFINE_EDGE: Key<f64> = Key::new("refineEdge", 0.0, 1);
pub const TYPE: Key<String> = Key::new("type", String::new(), 1);
pub const MODE: Key<SubMaskMode> = Key::new("mode", SubMaskMode::Additive, 1);
pub const PARAMETERS: Key<Value> = Key::new("parameters", Value::Null, 1);
pub const CENTER_X: Key<f64> = Key::new("centerX", 0.0, 1);
pub const CENTER_Y: Key<f64> = Key::new("centerY", 0.0, 1);
pub const RADIUS_X: Key<f64> = Key::new("radiusX", 0.0, 1);
pub const RADIUS_Y: Key<f64> = Key::new("radiusY", 0.0, 1);
pub const START_X: Key<f64> = Key::new("startX", 0.0, 1);
pub const START_Y: Key<f64> = Key::new("startY", 0.0, 1);
pub const END_X: Key<f64> = Key::new("endX", 0.0, 1);
pub const END_Y: Key<f64> = Key::new("endY", 0.0, 1);
pub const RANGE: Key<f64> = Key::new("range", 50.0, 1);
pub const RANGE_END: Key<f64> = Key::new("rangeEnd", 0.0, 1);
pub const BAND: Key<Value> = Key::new("band", Value::Null, 1);
pub const LINES: Key<Value> = Key::new("lines", Value::Null, 1);
pub const POINTS: Key<Value> = Key::new("points", Value::Null, 1);
pub const X: Key<f64> = Key::new("x", 0.0, 1);
pub const Y: Key<f64> = Key::new("y", 0.0, 1);
pub const P: Key<f64> = Key::new("p", 0.0, 1);
pub const BRUSH_SIZE: Key<f64> = Key::new("brushSize", 0.0, 1);
pub const FEATHER: Key<f64> = Key::new("feather", 0.0, 1);
pub const FLOW: Key<f64> = Key::new("flow", 0.0, 1);
pub const HARDNESS: Key<f64> = Key::new("hardness", 0.0, 1);
pub const LR_CENTER_WEIGHT: Key<f64> = Key::new("lrCenterWeight", 0.0, 1);
pub const AUTO_MASK: Key<bool> = Key::new("autoMask", false, 1);
pub const AUTO_MASK_VERSION: Key<f64> = Key::new("autoMaskVersion", 0.0, 1);
pub const DAB_VERSION: Key<f64> = Key::new("dabVersion", 0.0, 1);
pub const PRESSURE_SIZE: Key<bool> = Key::new("pressureSize", false, 1);
pub const PRESSURE_FLOW: Key<bool> = Key::new("pressureFlow", false, 1);
pub const FRAME: Key<Value> = Key::new("frame", Value::Null, 1);
pub const WIDTH: Key<f64> = Key::new("width", 0.0, 1);
pub const HEIGHT: Key<f64> = Key::new("height", 0.0, 1);
pub const TARGET_X: Key<f64> = Key::new("targetX", 0.0, 1);
pub const TARGET_Y: Key<f64> = Key::new("targetY", 0.0, 1);
pub const TOLERANCE: Key<f64> = Key::new("tolerance", 20.0, 1);
pub const REFINE: Key<f64> = Key::new("refine", 0.0, 1);
pub const SAMPLES: Key<Value> = Key::new("samples", Value::Null, 1);
pub const RANGE_LOW: Key<f64> = Key::new("rangeLow", 0.0, 1);
pub const RANGE_HIGH: Key<f64> = Key::new("rangeHigh", 100.0, 1);
pub const FALLOFF: Key<f64> = Key::new("falloff", 10.0, 1);
pub const LR_RANGE: Key<Value> = Key::new("lrRange", Value::Null, 1);
pub const MIN_DEPTH: Key<f64> = Key::new("minDepth", 0.0, 1);
pub const MAX_DEPTH: Key<f64> = Key::new("maxDepth", 0.0, 1);
pub const MIN_FADE: Key<f64> = Key::new("minFade", 0.0, 1);
pub const MAX_FADE: Key<f64> = Key::new("maxFade", 0.0, 1);
pub const GROW: Key<f64> = Key::new("grow", 0.0, 1);
pub const PARTS: Key<Value> = Key::new("parts", Value::Null, 1);
pub const PERSON: Key<f64> = Key::new("person", 0.0, 1);
pub const MASK_DATA_BASE64: Key<String> = Key::new("maskDataBase64", String::new(), 1);
pub const MASK_DATA_BASE64_LEGACY: Key<String> = Key::new("mask_data_base64", String::new(), 1);
pub const WARP: Key<Value> = Key::new("warp", Value::Null, 1);
pub const TOOL: Key<String> = Key::new("tool", String::new(), 1);
pub const SOURCE_X: Key<f64> = Key::new("sourceX", 0.0, 1);
pub const SOURCE_Y: Key<f64> = Key::new("sourceY", 0.0, 1);
pub const LIQUIFY_MODE: Key<String> = Key::new("liquifyMode", String::new(), 1);
pub const CURVES: Key<Value> = Key::new("curves", Value::Null, 1);
pub const POINT_CURVES: Key<Value> = Key::new("pointCurves", Value::Null, 1);
pub const PARAMETRIC_CURVE: Key<Value> = Key::new("parametricCurve", Value::Null, 1);
pub const CURVE_MODE: Key<Value> = Key::new("curveMode", Value::Null, 1);
pub use crate::stored::SECTION_VISIBILITY;
pub const DEFRINGE: Key<f64> = Key::new("defringe", 0.0, 1);

/// One borrowed component. Unknown types and parameter fields stay stored.
#[derive(Clone, Debug, PartialEq)]
pub struct SubMaskRef<'a> {
    pub id: &'a str,
    pub mask_type: &'a str,
    pub visible: bool,
    pub invert: bool,
    pub opacity: f32,
    pub mode: SubMaskMode,
    pub parameters: &'a Value,
}

/// A validated mask layer. Its nested adjustments are resolved on demand
/// ([`Mask::adjustments`]), independently of the photo's defaults and
/// visibility, so a caller that reads only the containers does not resolve
/// them.
#[derive(Clone, Debug, PartialEq)]
pub struct Mask<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub visible: bool,
    pub invert: bool,
    pub opacity: f32,
    pub sub_masks: Vec<SubMaskRef<'a>>,
    pub mask_version: Option<u32>,
    pub refine_edge: Option<f32>,
    stored_adjustments: &'a Value,
}

/// Whether a layer renders a bitmap: shown, with at least one component.
/// The one rule for resolved layers ([`Mask::produces_bitmap`]) and the
/// engine's rasterization inputs.
pub fn produces_bitmap(visible: bool, sub_mask_count: usize) -> bool {
    visible && sub_mask_count > 0
}

impl<'a> Mask<'a> {
    pub fn produces_bitmap(&self) -> bool {
        produces_bitmap(self.visible, self.sub_masks.len())
    }
    /// The layer's local adjustments as they render ([`resolve_local`]).
    pub fn adjustments(&self) -> LocalAdjustments<'a> {
        resolve_local(StoredRef::new(self.stored_adjustments))
    }
    /// The nested stored adjustments, unchanged (owned only by the rasterization adapter).
    pub fn stored_adjustments(&self) -> &'a Value {
        self.stored_adjustments
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Masks<'a> {
    pub layers: Vec<Mask<'a>>,
}

// These reads mirror serde's required/optional field rules. Using borrowed
// strings/JSON avoids copying stroke arrays and bitmap payloads in normalize.
fn required<'a, T: KeyValue>(v: StoredRef<'a>, key: &Key<T>) -> Option<&'a Value> {
    v.as_value().as_object()?.get(key.name())
}
fn optional<'a, T>(v: StoredRef<'a>, key: &Key<T>) -> &'a Value {
    v.as_value().get(key.name()).unwrap_or(&Value::Null)
}
fn opacity(v: StoredRef<'_>) -> Option<f32> {
    match v.as_value().get(OPACITY.name()) {
        None => Some(100.0),
        Some(value) => f32::deserialize(value).ok(),
    }
}
fn optional_serde<'de, T: Deserialize<'de>, K>(
    v: StoredRef<'de>,
    key: &Key<K>,
) -> Option<Option<T>> {
    let value = optional(v, key);
    if value.is_null() {
        Some(None)
    } else {
        T::deserialize(value).ok().map(Some)
    }
}
impl<'a> Masks<'a> {
    pub(crate) fn resolve(stored: StoredRef<'a>) -> Self {
        let layers = stored
            .as_value()
            .get(MASKS.name())
            .and_then(Value::as_array)
            .and_then(|layers| layers.iter().map(Self::layer).collect::<Option<Vec<_>>>())
            .unwrap_or_default();
        Self { layers }
    }
    fn layer(value: &'a Value) -> Option<Mask<'a>> {
        let s = StoredRef::new(value);
        let stored_adjustments = required(s, &ADJUSTMENTS)?;
        let sub_masks = required(s, &SUB_MASKS)?
            .as_array()?
            .iter()
            .map(|value| {
                let s = StoredRef::new(value);
                Some(SubMaskRef {
                    id: required(s, &ID)?.as_str()?,
                    mask_type: required(s, &TYPE)?.as_str()?,
                    visible: required(s, &VISIBLE)?.as_bool()?,
                    invert: match value.get(INVERT.name()) {
                        None => false,
                        Some(v) => v.as_bool()?,
                    },
                    opacity: opacity(s)?,
                    mode: s.get(&MODE)?,
                    parameters: required(s, &PARAMETERS)?,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Mask {
            id: required(s, &ID)?.as_str()?,
            name: required(s, &NAME)?.as_str()?,
            visible: required(s, &VISIBLE)?.as_bool()?,
            invert: required(s, &INVERT)?.as_bool()?,
            opacity: opacity(s)?,
            sub_masks,
            mask_version: optional_serde(s, &MASK_VERSION)?,
            refine_edge: optional_serde(s, &REFINE_EDGE)?,
            stored_adjustments,
        })
    }
}

/// Resolve only the mask containers (their local adjustments on demand), for
/// a stage that does not consume the photo's other sections.
pub fn resolve(stored: StoredRef<'_>) -> Masks<'_> {
    Masks::resolve(stored)
}

/// Mask-local values use the same keys/codecs as their photo counterparts,
/// with the mask reader's neutral defaults. In particular sharpening threshold
/// is 0 when absent (15 in the photo), and capture defaults never apply.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct LocalAdjustments<'a> {
    pub is_null: bool,
    pub exposure: tone::Ev,
    pub brightness: tone::Brightness,
    pub contrast: tone::Amount,
    pub highlights: tone::Amount,
    pub shadows: tone::Amount,
    pub whites: tone::Amount,
    pub blacks: tone::Amount,
    pub temperature: tone::Amount,
    pub tint: tone::Amount,
    pub vibrance: tone::Amount,
    pub saturation: tone::Amount,
    pub hue: color::Hue,
    pub sharpness: detail::Sharpness,
    pub sharpness_threshold: detail::Threshold,
    pub luma_noise_reduction: detail::Percent,
    pub color_noise_reduction: detail::Percent,
    pub clarity: tone::Amount,
    pub dehaze: tone::Amount,
    pub structure: tone::Amount,
    pub moire_amount: detail::Percent,
    pub moire_pattern: detail::Percent,
    pub glow_amount: effects::Percent,
    pub halation_amount: effects::Percent,
    pub flare_amount: effects::Percent,
    pub defringe: tone::Amount,
    pub color_visible: bool,
    pub grading_visible: bool,
    pub grading: color::Grading,
    pub hsl: color::Hsl,
    pub color_ranges: Vec<color::ColorRange>,
    pub curves: &'a Value,
}

/// Resolve only a layer's nested StoredRef. Used for already decoded/mutated
/// mask definitions too; it never constructs or saves a top-level recipe.
pub fn resolve_local(stored: StoredRef<'_>) -> LocalAdjustments<'_> {
    let is_null = stored.as_value().is_null();
    let read = |section: Section, key: &Key<f64>, tool: Option<Section>, default: f64| {
        if stored.section_visible(section) && tool.is_none_or(|s| stored.section_visible(s)) {
            stored.get(key).unwrap_or(default)
        } else {
            0.0
        }
    };
    let color_visible = stored.section_visible(Section::COLOR);
    let grading_visible = stored.section_visible(Section::COLOR_GRADING);
    let color_tool = |s| color_visible && stored.section_visible(s);
    LocalAdjustments {
        is_null,
        exposure: tone::Ev::new(read(Section::BASIC, &tone::EXPOSURE, None, 0.0)),
        brightness: tone::Brightness::new(read(Section::BASIC, &tone::BRIGHTNESS, None, 0.0)),
        contrast: tone::Amount::new(read(Section::BASIC, &tone::CONTRAST, None, 0.0)),
        highlights: tone::Amount::new(read(Section::BASIC, &tone::HIGHLIGHTS, None, 0.0)),
        shadows: tone::Amount::new(read(Section::BASIC, &tone::SHADOWS, None, 0.0)),
        whites: tone::Amount::new(read(Section::BASIC, &tone::WHITES, None, 0.0)),
        blacks: tone::Amount::new(read(Section::BASIC, &tone::BLACKS, None, 0.0)),
        temperature: tone::Amount::new(read(
            Section::COLOR,
            &color::TEMPERATURE,
            Some(Section::WHITE_BALANCE),
            0.0,
        )),
        tint: tone::Amount::new(read(
            Section::COLOR,
            &color::TINT,
            Some(Section::WHITE_BALANCE),
            0.0,
        )),
        vibrance: tone::Amount::new(read(
            Section::COLOR,
            &color::VIBRANCE,
            Some(Section::PRESENCE),
            0.0,
        )),
        saturation: tone::Amount::new(read(
            Section::COLOR,
            &color::SATURATION,
            Some(Section::PRESENCE),
            0.0,
        )),
        hue: color::Hue::new(read(Section::COLOR, &color::HUE, None, 0.0)),
        sharpness: detail::Sharpness::new(read(
            Section::DETAILS,
            &detail::SHARPNESS,
            detail::tool_switch(detail::SHARPNESS.name()),
            0.0,
        )),
        sharpness_threshold: detail::Threshold::new(read(
            Section::DETAILS,
            &detail::SHARPNESS_THRESHOLD,
            detail::tool_switch(detail::SHARPNESS_THRESHOLD.name()),
            0.0,
        )),
        luma_noise_reduction: detail::Percent::new(read(
            Section::DETAILS,
            &detail::LUMA_NOISE_REDUCTION,
            detail::tool_switch(detail::LUMA_NOISE_REDUCTION.name()),
            0.0,
        )),
        color_noise_reduction: detail::Percent::new(read(
            Section::DETAILS,
            &detail::COLOR_NOISE_REDUCTION,
            detail::tool_switch(detail::COLOR_NOISE_REDUCTION.name()),
            0.0,
        )),
        clarity: tone::Amount::new(read(
            Section::DETAILS,
            &detail::CLARITY,
            detail::tool_switch(detail::CLARITY.name()),
            0.0,
        )),
        dehaze: tone::Amount::new(read(
            Section::DETAILS,
            &detail::DEHAZE,
            detail::tool_switch(detail::DEHAZE.name()),
            0.0,
        )),
        structure: tone::Amount::new(read(
            Section::DETAILS,
            &detail::STRUCTURE,
            detail::tool_switch(detail::STRUCTURE.name()),
            0.0,
        )),
        moire_amount: detail::Percent::new(read(
            Section::DETAILS,
            &detail::MOIRE_AMOUNT,
            detail::tool_switch(detail::MOIRE_AMOUNT.name()),
            0.0,
        )),
        moire_pattern: detail::Percent::new(read(
            Section::DETAILS,
            &detail::MOIRE_PATTERN,
            detail::tool_switch(detail::MOIRE_PATTERN.name()),
            30.0,
        )),
        glow_amount: effects::Percent::new(read(
            Section::EFFECTS,
            &effects::GLOW_AMOUNT,
            None,
            0.0,
        )),
        halation_amount: effects::Percent::new(read(
            Section::EFFECTS,
            &effects::HALATION_AMOUNT,
            None,
            0.0,
        )),
        flare_amount: effects::Percent::new(read(
            Section::EFFECTS,
            &effects::FLARE_AMOUNT,
            None,
            0.0,
        )),
        defringe: tone::Amount::new(read(Section::DETAILS, &DEFRINGE, None, 0.0)),
        color_visible,
        grading_visible,
        grading: stored.get_or_default(&color::COLOR_GRADING),
        hsl: if color_tool(Section::COLOR_MIXER) {
            stored.get_or_default(&color::HSL)
        } else {
            color::HSL.default()
        },
        color_ranges: if color_tool(Section::COLOR_EDITOR) {
            stored
                .as_value()
                .get(color::COLOR_RANGES.name())
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .take(4)
                .map(|v| color::ColorRange::resolve(StoredRef::new(v)))
                .collect()
        } else {
            Vec::new()
        },
        curves: if stored.section_visible(Section::CURVES) {
            optional(stored, &CURVES)
        } else {
            &Value::Null
        },
    }
}

pub const TS: TsSection = TsSection {
    name: "masks",
    fields: &[TsField::json(
        MASKS,
        "MaskContainer[]",
        Some("MaskContainer"),
    )],
    unions: &[],
};

/// Local frontend fields reuse these exact keys and value types; requiredness
/// remains the frontend object's contract, independent of a saved partial recipe.
pub const LOCAL_TS: &[TsField] = &[
    TsField::new(ID, ""),
    TsField::json(
        SECTION_VISIBILITY,
        "SectionVisibility",
        Some("SectionVisibility"),
    ),
    TsField::new(DEFRINGE, ""),
    TsField::new(detail::MOIRE_AMOUNT, ""),
    TsField::new(detail::MOIRE_PATTERN, ""),
    TsField::new(tone::EXPOSURE, ""),
    TsField::new(tone::BRIGHTNESS, ""),
    TsField::new(tone::CONTRAST, ""),
    TsField::new(tone::HIGHLIGHTS, ""),
    TsField::new(tone::SHADOWS, ""),
    TsField::new(tone::WHITES, ""),
    TsField::new(tone::BLACKS, ""),
    TsField::new(color::TEMPERATURE, ""),
    TsField::new(color::TINT, ""),
    TsField::new(color::VIBRANCE, ""),
    TsField::new(color::SATURATION, ""),
    TsField::new(color::HUE, ""),
    TsField::new(detail::SHARPNESS, ""),
    TsField::new(detail::SHARPNESS_THRESHOLD, ""),
    TsField::new(detail::LUMA_NOISE_REDUCTION, ""),
    TsField::new(detail::COLOR_NOISE_REDUCTION, ""),
    TsField::new(detail::CLARITY, ""),
    TsField::new(detail::DEHAZE, ""),
    TsField::new(detail::STRUCTURE, ""),
    TsField::new(effects::GLOW_AMOUNT, ""),
    TsField::new(effects::HALATION_AMOUNT, ""),
    TsField::new(effects::FLARE_AMOUNT, ""),
    TsField::new(color::COLOR_GRADING, ""),
    TsField::new(color::HSL, ""),
    TsField::new(color::COLOR_RANGES, ""),
    TsField::json(CURVES, "Curves", Some("Curves")),
    TsField::json(POINT_CURVES, "Curves", Some("Curves")),
    TsField::json(PARAMETRIC_CURVE, "ParametricCurve", Some("ParametricCurve")),
    TsField::json(CURVE_MODE, "'point' | 'parametric'", None),
];
/// Stored point-curve channel names, shared by photo and local curves.
pub const CURVE_CHANNELS: [Key<Value>; 5] = [
    Key::new("luma", Value::Null, 1),
    Key::new("red", Value::Null, 1),
    Key::new("green", Value::Null, 1),
    Key::new("blue", Value::Null, 1),
    Key::new("luminance", Value::Null, 1),
];
pub const CURVE_VERSION: Key<u64> = Key::new("version", 1, 1);

pub const CONTAINER_TS: &[TsField] = &[
    TsField::new(ID, ""),
    TsField::new(NAME, ""),
    TsField::new(VISIBLE, ""),
    TsField::new(INVERT, ""),
    TsField::new(OPACITY, ""),
    TsField::new(
        MASK_VERSION,
        "2: 16-bit masks; 3: also Refine edge on both sides of a thin mask; 4: also AI masks grown and feathered around the crop; absent: 8 bits.",
    ),
    TsField::new(
        REFINE_EDGE,
        "Refine edge radius in pixels of the full photo (maskVersion 2+).",
    ),
    TsField::json(ADJUSTMENTS, "MaskAdjustments", Some("MaskAdjustments")),
    TsField::json(SUB_MASKS, "SubMask[]", Some("SubMask")),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StoredRecipe;
    use serde_json::json;

    fn recipe() -> Value {
        json!({"masks":[{"id":"layer","name":"A","visible":true,"invert":false,
            "adjustments":{},"subMasks":[{"id":"component","type":"linear","visible":true,
                "mode":"additive","parameters":{"startX":10,"future":{"x":1}}}]}]})
    }

    #[test]
    fn mask_field_names_match_the_stored_schema() {
        for (actual, expected) in [
            (MASKS.name(), "masks"),
            (ID.name(), "id"),
            (NAME.name(), "name"),
            (VISIBLE.name(), "visible"),
            (INVERT.name(), "invert"),
            (OPACITY.name(), "opacity"),
            (ADJUSTMENTS.name(), "adjustments"),
            (SUB_MASKS.name(), "subMasks"),
            (MASK_VERSION.name(), "maskVersion"),
            (REFINE_EDGE.name(), "refineEdge"),
            (TYPE.name(), "type"),
            (MODE.name(), "mode"),
            (PARAMETERS.name(), "parameters"),
            (CENTER_X.name(), "centerX"),
            (CENTER_Y.name(), "centerY"),
            (RADIUS_X.name(), "radiusX"),
            (RADIUS_Y.name(), "radiusY"),
            (START_X.name(), "startX"),
            (START_Y.name(), "startY"),
            (END_X.name(), "endX"),
            (END_Y.name(), "endY"),
            (RANGE.name(), "range"),
            (RANGE_END.name(), "rangeEnd"),
            (BAND.name(), "band"),
            (LINES.name(), "lines"),
            (POINTS.name(), "points"),
            (X.name(), "x"),
            (Y.name(), "y"),
            (P.name(), "p"),
            (BRUSH_SIZE.name(), "brushSize"),
            (FEATHER.name(), "feather"),
            (FLOW.name(), "flow"),
            (HARDNESS.name(), "hardness"),
            (LR_CENTER_WEIGHT.name(), "lrCenterWeight"),
            (AUTO_MASK.name(), "autoMask"),
            (AUTO_MASK_VERSION.name(), "autoMaskVersion"),
            (DAB_VERSION.name(), "dabVersion"),
            (PRESSURE_SIZE.name(), "pressureSize"),
            (PRESSURE_FLOW.name(), "pressureFlow"),
            (FRAME.name(), "frame"),
            (WIDTH.name(), "width"),
            (HEIGHT.name(), "height"),
            (TARGET_X.name(), "targetX"),
            (TARGET_Y.name(), "targetY"),
            (TOLERANCE.name(), "tolerance"),
            (REFINE.name(), "refine"),
            (SAMPLES.name(), "samples"),
            (RANGE_LOW.name(), "rangeLow"),
            (RANGE_HIGH.name(), "rangeHigh"),
            (FALLOFF.name(), "falloff"),
            (LR_RANGE.name(), "lrRange"),
            (MIN_DEPTH.name(), "minDepth"),
            (MAX_DEPTH.name(), "maxDepth"),
            (MIN_FADE.name(), "minFade"),
            (MAX_FADE.name(), "maxFade"),
            (GROW.name(), "grow"),
            (PARTS.name(), "parts"),
            (PERSON.name(), "person"),
            (MASK_DATA_BASE64.name(), "maskDataBase64"),
            (MASK_DATA_BASE64_LEGACY.name(), "mask_data_base64"),
            (WARP.name(), "warp"),
            (TOOL.name(), "tool"),
            (SOURCE_X.name(), "sourceX"),
            (SOURCE_Y.name(), "sourceY"),
            (LIQUIFY_MODE.name(), "liquifyMode"),
            (CURVES.name(), "curves"),
            (POINT_CURVES.name(), "pointCurves"),
            (PARAMETRIC_CURVE.name(), "parametricCurve"),
            (CURVE_MODE.name(), "curveMode"),
            (DEFRINGE.name(), "defringe"),
            (CURVE_VERSION.name(), "version"),
        ] {
            assert_eq!(actual, expected);
        }
        assert_eq!(
            crate::resources::FILE_KEYS.map(|k| k.name()),
            ["lutPath", "cameraProfile", "raster_path"]
        );
        assert_eq!(
            CURVE_CHANNELS.map(|key| key.into_name()),
            ["luma", "red", "green", "blue", "luminance"]
        );
    }

    #[test]
    fn resolve_borrows_payloads_and_does_not_change_saved_masks() {
        let original = json!({"masks":[{"id":"layer","name":"A","visible":true,"invert":true,"opacity":35,
            "adjustments":{"sharpnessThreshold":null,"exposure":-5.5,"curves":{"future":[null]},"future":123},
            "futureLayer":{"unknown":null},"subMasks":[{"id":"sky","type":"future","visible":false,"invert":true,
                "mode":"intersect","opacity":21,"parameters":{"maskDataBase64":"unchanged-payload","future":[null]}}]}],"futureRoot":true});
        let stored = StoredRecipe::from_value(original.clone());
        let masks = resolve(stored.view());
        assert!(std::ptr::eq(
            masks.layers[0].sub_masks[0].parameters,
            &stored.view().as_value()["masks"][0]["subMasks"][0]["parameters"]
        ));
        assert!(std::ptr::eq(
            masks.layers[0].adjustments().curves,
            &stored.view().as_value()["masks"][0]["adjustments"]["curves"]
        ));
        assert_eq!(masks.layers[0].adjustments().sharpness_threshold.get(), 0.0);
        assert_eq!(stored.into_value(), original);
    }

    #[test]
    fn mask_keys_round_trip_without_filling_defaults_or_dropping_future_fields() {
        let mut stored = StoredRecipe::from_value(recipe());
        let masks = stored.get(&MASKS).unwrap();
        stored.set(&MASKS, masks.clone());
        assert_eq!(stored.get(&MASKS), Some(masks));
        let mut stored = StoredRecipe::from_value(json!({"masks":null,"future":42}));
        let before = stored.clone();
        let _ = resolve(stored.view());
        assert_eq!(stored, before);
        assert_eq!(stored.remove(&MASKS), Some(Value::Null));
        assert_eq!(stored.into_value(), json!({"future":42}));
    }
}
