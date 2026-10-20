//! Mask layer definitions as stored in a recipe; `mask_generation` rasterises them.
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(crate = "serde")]
#[serde(rename_all = "camelCase")]
pub enum SubMaskMode {
    Additive,
    Subtractive,
    Intersect,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
#[serde(crate = "serde")]
#[serde(rename_all = "camelCase")]
pub struct SubMask {
    pub id: String,
    #[serde(rename = "type")]
    pub mask_type: String,
    pub visible: bool,
    #[serde(default)]
    pub invert: bool,
    #[serde(default = "default_opacity")]
    pub opacity: f32,
    pub mode: SubMaskMode,
    pub parameters: Value,
}

pub fn default_opacity() -> f32 {
    100.0
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
#[serde(crate = "serde")]
#[serde(rename_all = "camelCase")]
pub struct MaskDefinition {
    pub id: String,
    pub name: String,
    pub visible: bool,
    pub invert: bool,
    #[serde(default = "default_opacity")]
    pub opacity: f32,
    pub adjustments: Value,
    pub sub_masks: Vec<SubMask>,
    /// 2: the layer's mask is computed in floating point and kept at 16 bits
    /// (`mask_generation::generate_layer_mask`). 3: the same, with Refine
    /// edge also reading a thin mask's far side (`precise::refine_edge`).
    /// 4: the same, with AI masks grown and feathered from the photo around
    /// the crop (`precise::PhotoMargin`). Absent (every layer before 2026-10-18): the 8-bit mask, unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask_version: Option<u32>,
    /// Refine edge: radius in pixels of the full-resolution photo within
    /// which the mask's edge follows the photo's (`maskVersion` 2 and up).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refine_edge: Option<f32>,
}

impl MaskDefinition {
    pub fn produces_bitmap(&self) -> bool {
        self.visible && !self.sub_masks.is_empty()
    }

    /// The mask is computed at 16 bits (`maskVersion` 2).
    pub fn precise(&self) -> bool {
        self.mask_version.is_some_and(|version| version >= 2)
    }

    /// The Refine edge radius in full-resolution pixels, when it applies.
    pub fn refine_radius(&self) -> Option<f32> {
        self.refine_edge
            .filter(|radius| self.precise() && radius.is_finite() && *radius > 0.0)
    }

    pub fn requires_warped_image(&self) -> bool {
        self.refine_radius().is_some()
            || self.sub_masks.iter().any(|sm| {
                matches!(sm.mask_type.as_str(), "color" | "luminance" | "luma-range")
                    || (sm.mask_type == "brush"
                        && crate::mask_parameters::with_resolved(&sm.parameters, |p| {
                            p["lines"]
                                .as_array()
                                .is_some_and(|lines| lines.iter().any(|l| l["autoMask"] == true))
                        }))
            })
    }
}

/// Parameters of the AI mask types; the bitmaps come from the app's models.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct AiSubjectMaskParameters {
    pub start_x: f64,
    pub start_y: f64,
    pub end_x: f64,
    pub end_y: f64,
    #[serde(default)]
    pub mask_data_base64: Option<String>,
    #[serde(default)]
    pub rotation: Option<f32>,
    #[serde(default)]
    pub flip_horizontal: Option<bool>,
    #[serde(default)]
    pub flip_vertical: Option<bool>,
    #[serde(default)]
    pub orientation_steps: Option<u8>,
    /// The lens and transform adjustments the bitmap was computed in
    /// (`mask_generation::mask_warp`): a later change moves the mask along.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warp: Option<serde_json::Value>,
}
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct AiSkyMaskParameters {
    #[serde(default)]
    pub mask_data_base64: Option<String>,
    #[serde(default)]
    pub rotation: Option<f32>,
    #[serde(default)]
    pub flip_horizontal: Option<bool>,
    #[serde(default)]
    pub flip_vertical: Option<bool>,
    #[serde(default)]
    pub orientation_steps: Option<u8>,
    /// The lens and transform adjustments the bitmap was computed in
    /// (`mask_generation::mask_warp`): a later change moves the mask along.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warp: Option<serde_json::Value>,
}
/// A People mask: parts (`faceSkin`, `lips`, … as named by
/// `rawtonic_faces::Part`) of everyone or of one person (the index of their
/// face, left to right), kept so the mask can be computed again.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct AiPeopleMaskParameters {
    #[serde(default)]
    pub parts: Vec<String>,
    #[serde(default)]
    pub person: Option<usize>,
    #[serde(default)]
    pub mask_data_base64: Option<String>,
    #[serde(default)]
    pub rotation: Option<f32>,
    #[serde(default)]
    pub flip_horizontal: Option<bool>,
    #[serde(default)]
    pub flip_vertical: Option<bool>,
    #[serde(default)]
    pub orientation_steps: Option<u8>,
    /// The lens and transform adjustments the bitmap was computed in
    /// (`mask_generation::mask_warp`): a later change moves the mask along.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warp: Option<serde_json::Value>,
}
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct AiForegroundMaskParameters {
    #[serde(default)]
    pub mask_data_base64: Option<String>,
    #[serde(default)]
    pub rotation: Option<f32>,
    #[serde(default)]
    pub flip_horizontal: Option<bool>,
    #[serde(default)]
    pub flip_vertical: Option<bool>,
    #[serde(default)]
    pub orientation_steps: Option<u8>,
    /// The lens and transform adjustments the bitmap was computed in
    /// (`mask_generation::mask_warp`): a later change moves the mask along.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warp: Option<serde_json::Value>,
}
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct AiDepthMaskParameters {
    #[serde(default)]
    pub min_depth: f32,
    #[serde(default)]
    pub max_depth: f32,
    #[serde(default)]
    pub min_fade: f32,
    #[serde(default)]
    pub max_fade: f32,
    #[serde(default)]
    pub feather: f32,
    #[serde(default)]
    pub mask_data_base64: Option<String>,
    #[serde(default)]
    pub rotation: Option<f32>,
    #[serde(default)]
    pub flip_horizontal: Option<bool>,
    #[serde(default)]
    pub flip_vertical: Option<bool>,
    #[serde(default)]
    pub orientation_steps: Option<u8>,
    /// The lens and transform adjustments the bitmap was computed in
    /// (`mask_generation::mask_warp`): a later change moves the mask along.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warp: Option<serde_json::Value>,
}

/// Mask component types the AI detects on each photograph: an Object
/// selection (by its box), Subject, Sky, depth range and People. Pasted or
/// synced onto another photograph they never keep the source's bitmap.
pub const DETECTED_TYPES: [&str; 5] = [
    "ai-subject",
    "ai-foreground",
    "ai-sky",
    "ai-depth",
    "ai-people",
];

/// The parameter that marks a component pasted or synced from another
/// photograph whose detection has not run on this one yet.
pub const DETECT_PENDING: &str = "detectPending";

/// Whether a component's parameters carry a bitmap.
fn has_bitmap(parameters: &Value) -> bool {
    ["maskDataBase64", "mask_data_base64"]
        .iter()
        .any(|key| parameters.get(key).is_some_and(|v| !v.is_null()))
}

/// How many components of shown layers (`masks`) wait for their AI
/// detection on this photograph (`DETECT_PENDING` without a bitmap).
/// Rendered without it, such a layer would apply its adjustments to the
/// wrong part of the photograph, or nowhere.
pub fn pending_detections(adjustments: &Value) -> usize {
    let shown = |value: &Value| value.get("visible").and_then(Value::as_bool) != Some(false);
    adjustments
        .get("masks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|layer| shown(layer))
        .flat_map(|layer| {
            layer
                .get("subMasks")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter(|sub| {
            shown(sub)
                && sub.get("parameters").is_some_and(|p| {
                    p.get(DETECT_PENDING).and_then(Value::as_bool) == Some(true) && !has_bitmap(p)
                })
        })
        .count()
}

/// Layers pasted or synced (`pasted`, the source's chosen layers) into a
/// photograph's own (`own`): a pasted layer replaces the photograph's layer
/// of the same id (pasted before: a second sync updates it instead of
/// adding it again) and the others are added on top; the photograph's own
/// layers stay, unless `replace` (Edit all selected: every photograph has
/// the primary's layers). Onto another photograph than the source
/// (`same_photo` false: a virtual copy of the same photograph shares its
/// pixels) the pasted layers' AI detections drop the source's bitmap and
/// wait for their own (`DETECT_PENDING`), except People components without
/// parts (nothing to detect).
pub fn paste_layers(own: &Value, pasted: &Value, same_photo: bool, replace: bool) -> Value {
    let mut pasted: Vec<Value> = pasted.as_array().cloned().unwrap_or_default();
    if !same_photo {
        for layer in &mut pasted {
            let Some(subs) = layer.get_mut("subMasks").and_then(Value::as_array_mut) else {
                continue;
            };
            for sub in subs {
                let detected = sub
                    .get("type")
                    .and_then(Value::as_str)
                    .is_some_and(|t| DETECTED_TYPES.contains(&t));
                let Some(parameters) = sub.get_mut("parameters").and_then(Value::as_object_mut)
                else {
                    continue;
                };
                let nothing_to_detect = parameters
                    .get("parts")
                    .and_then(Value::as_array)
                    .is_some_and(Vec::is_empty);
                if !detected || nothing_to_detect {
                    continue;
                }
                for key in ["maskDataBase64", "mask_data_base64"] {
                    if parameters.contains_key(key) {
                        parameters.insert(key.into(), Value::Null);
                    }
                }
                parameters.insert("maskDataBase64".into(), Value::Null);
                parameters.insert(DETECT_PENDING.into(), Value::Bool(true));
            }
        }
    }
    if replace {
        return Value::Array(pasted);
    }
    let id = |layer: &Value| layer.get("id").and_then(Value::as_str).map(str::to_string);
    let mut layers: Vec<Value> = own.as_array().cloned().unwrap_or_default();
    for layer in pasted {
        match layers
            .iter_mut()
            .find(|own| id(own).is_some() && id(own) == id(&layer))
        {
            Some(slot) => *slot = layer,
            None => layers.push(layer),
        }
    }
    Value::Array(layers)
}

#[cfg(test)]
mod paste_tests {
    use super::*;
    use serde_json::json;

    fn layer(id: &str, sub: Value) -> Value {
        json!({"id": id, "name": id, "visible": true, "subMasks": [sub]})
    }

    #[test]
    fn pasted_layers_replace_their_own_earlier_paste_and_join_the_others() {
        let own = json!([
            layer(
                "dodge",
                json!({"id": "b", "type": "brush", "visible": true, "parameters": {"lines": []}})
            ),
            layer(
                "sky",
                json!({"id": "s-old", "type": "linear", "visible": true, "parameters": {}})
            ),
        ]);
        let pasted = json!([
            layer(
                "sky",
                json!({"id": "s", "type": "linear", "visible": true, "parameters": {"range": 50}})
            ),
            layer(
                "vignette",
                json!({"id": "r", "type": "radial", "visible": true, "parameters": {}})
            ),
        ]);
        let merged = paste_layers(&own, &pasted, false, false);
        let ids: Vec<&str> = merged
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["dodge", "sky", "vignette"]);
        assert_eq!(merged[1]["subMasks"][0]["parameters"]["range"], 50);
        // Pasting again changes nothing more.
        assert_eq!(paste_layers(&merged, &pasted, false, false), merged);
        // Edit all selected: the primary's layers, the photograph's own gone.
        let mirrored = paste_layers(&own, &pasted, false, true);
        assert_eq!(mirrored.as_array().unwrap().len(), 2);
        assert_eq!(mirrored[0]["id"], "sky");
    }

    #[test]
    fn detections_wait_for_their_own_photograph_but_not_on_a_virtual_copy() {
        let sky = |data: Value| {
            layer(
                "sky",
                json!({"id": "s", "type": "ai-sky", "visible": true, "parameters": {"maskDataBase64": data, "grow": 10}}),
            )
        };
        let people = json!({"id": "p", "type": "ai-people", "visible": true, "parameters": {"parts": [], "maskDataBase64": null}});
        let pasted = json!([sky(json!("data:source")), layer("people", people)]);
        let other = paste_layers(&json!([]), &pasted, false, false);
        let parameters = &other[0]["subMasks"][0]["parameters"];
        assert_eq!(parameters["maskDataBase64"], Value::Null);
        assert_eq!(parameters[DETECT_PENDING], true);
        assert_eq!(parameters["grow"], 10, "its settings stay");
        assert!(
            other[1]["subMasks"][0]["parameters"]
                .get(DETECT_PENDING)
                .is_none()
        );
        let adjustments = json!({ "masks": other });
        assert_eq!(pending_detections(&adjustments), 1);
        let same = paste_layers(&json!([]), &pasted, true, false);
        assert_eq!(
            same[0]["subMasks"][0]["parameters"]["maskDataBase64"],
            "data:source"
        );
        assert_eq!(pending_detections(&json!({ "masks": same })), 0);
    }

    #[test]
    fn only_shown_layers_without_their_detection_are_pending() {
        let pending = |visible: bool, data: Value| {
            json!({"masks": [{"id": "l", "visible": visible, "subMasks": [
                {"id": "s", "type": "ai-sky", "visible": true, "parameters": {"maskDataBase64": data, "detectPending": true}}]}]})
        };
        assert_eq!(pending_detections(&pending(true, Value::Null)), 1);
        assert_eq!(pending_detections(&pending(false, Value::Null)), 0);
        assert_eq!(pending_detections(&pending(true, json!("data:own"))), 0);
        // A layer imported from a Lightroom preset waits without the mark
        // (computed when opened) and is not counted.
        assert_eq!(
            pending_detections(&json!({"masks": [{"id": "l", "visible": true, "subMasks": [
                {"id": "s", "type": "ai-sky", "visible": true, "parameters": {"maskDataBase64": null}}]}]})),
            0
        );
    }
}
