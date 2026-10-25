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
                                || crate::lightroom_masks::has_auto_mask_stroke(p)
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

/// Mask component types the AI detects on each photograph: Subject, Sky,
/// depth range and People (detected again when the photograph is opened) and
/// an Object selection (pointed at again by the user). Pasted or synced onto
/// another photograph they never keep the source's bitmap.
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

/// Whether a component's parameters leave nothing to detect: a People
/// selection without parts (renders nothing on any photograph).
fn nothing_to_detect(parameters: &Value) -> bool {
    parameters
        .get("parts")
        .and_then(Value::as_array)
        .is_some_and(Vec::is_empty)
}

/// How many layers of `masks` the renderer applies (shown, with a
/// component: `MaskDefinition::produces_bitmap`); it applies at most
/// `image_processing::MAX_MASKS`.
pub fn rendered_layers(masks: &Value) -> usize {
    masks
        .as_array()
        .into_iter()
        .flatten()
        .filter(|layer| {
            layer.get("visible").and_then(Value::as_bool) != Some(false)
                && layer
                    .get("subMasks")
                    .and_then(Value::as_array)
                    .is_some_and(|subs| !subs.is_empty())
        })
        .count()
}

/// A component of a shown layer waiting for its AI detection on this
/// photograph (`DETECT_PENDING` without a bitmap, something to detect): its
/// layer's name and its type. Rendered without it, the layer would apply its adjustments to the
/// wrong part of the photograph, or nowhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingDetection {
    pub layer: String,
    pub kind: String,
}

/// The components of shown layers waiting for their AI detection.
pub fn pending_detections(adjustments: &Value) -> Vec<PendingDetection> {
    let shown = |value: &Value| value.get("visible").and_then(Value::as_bool) != Some(false);
    let text = |value: &Value, key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
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
                .filter(|sub| {
                    shown(sub)
                        && sub.get("parameters").is_some_and(|p| {
                            p.get(DETECT_PENDING).and_then(Value::as_bool) == Some(true)
                                && !has_bitmap(p)
                                && !nothing_to_detect(p)
                        })
                })
                .map(move |sub| PendingDetection {
                    layer: text(layer, "name"),
                    kind: text(sub, "type"),
                })
        })
        .collect()
}

/// What to do about `pending` (`pending_detections`), naming each layer.
pub fn pending_detections_message(pending: &[PendingDetection]) -> String {
    pending
        .iter()
        .map(|p| {
            let layer = if p.layer.is_empty() { "a layer" } else { p.layer.as_str() };
            if p.kind == "ai-subject" {
                format!("layer “{layer}”: point at its object again in Develop (an Object selection belongs to the photograph it was made on)")
            } else {
                format!("layer “{layer}”: open the photograph in Develop to detect its {} selection", match p.kind.as_str() {
                    "ai-sky" => "Sky",
                    "ai-foreground" => "Subject",
                    "ai-depth" => "depth range",
                    "ai-people" => "People",
                    _ => "AI",
                })
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Detection output is tied to one photograph. Keep selection intent and
/// refinement settings when saving a preset or transferring its layers.
/// Keep in sync with `DETECTION_RESULT_KEYS` in src/utils/settingsClipboard.ts.
const DETECTION_RESULT_KEYS: [&str; 8] = [
    "maskDataBase64",
    "mask_data_base64",
    DETECT_PENDING,
    "rotation",
    "flipHorizontal",
    "flipVertical",
    "orientationSteps",
    "warp",
];

fn clear_detection(sub: &mut Value) -> bool {
    if !sub
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|t| DETECTED_TYPES.contains(&t))
    {
        return false;
    }
    let Some(sub) = sub.as_object_mut() else {
        return false;
    };
    let Some(parameters) = sub
        .entry("parameters")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
    else {
        return false;
    };
    for key in DETECTION_RESULT_KEYS {
        parameters.remove(key);
    }
    true
}

/// Clear AI detections before applying layers to a different photograph or
/// from a preset (including legacy presets with bitmaps). Used by paste,
/// preset application and new captures. Object prompts stay recorded, but
/// their bitmap stays inactive/pending until the user points at an object
/// on the target photo; they are never detected from source coordinates.
/// A photo's own loaded recipe never passes through this function.
pub fn prepare_layers_for_photo(layers: &mut Value, new_id: &mut dyn FnMut() -> String) {
    for layer in layers.as_array_mut().into_iter().flatten() {
        for sub in layer
            .get_mut("subMasks")
            .and_then(Value::as_array_mut)
            .into_iter()
            .flatten()
        {
            let snake_bitmap = sub
                .get("parameters")
                .is_some_and(|p| p.get("mask_data_base64").is_some());
            if !clear_detection(sub) {
                continue;
            }
            let parameters = &mut sub["parameters"];
            parameters["maskDataBase64"] = Value::Null;
            if snake_bitmap {
                parameters["mask_data_base64"] = Value::Null;
            }
            if nothing_to_detect(parameters) {
                continue;
            }
            parameters[DETECT_PENDING] = Value::Bool(true);
            sub["id"] = Value::String(new_id());
        }
    }
}

/// A preset stores AI selection parameters only, never photo bitmaps or
/// their geometry. Return a copy so saving/exporting does not change the
/// photo's own recipe, and loading needs no preset-file migration.
pub fn without_detections(adjustments: &Value) -> Value {
    let mut out = adjustments.clone();
    for layer in out
        .get_mut("masks")
        .and_then(Value::as_array_mut)
        .into_iter()
        .flatten()
    {
        for sub in layer
            .get_mut("subMasks")
            .and_then(Value::as_array_mut)
            .into_iter()
            .flatten()
        {
            clear_detection(sub);
        }
    }
    out
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
/// parts (nothing to detect). Each waiting component gets an id of its own
/// (`new_id`): the editor's renderer keeps bitmaps by component id, and its
/// detection here must not take the place of a bitmap held under the id it
/// had (the source's, or this photograph's before an earlier paste).
pub fn paste_layers(
    own: &Value,
    pasted: &Value,
    same_photo: bool,
    replace: bool,
    new_id: &mut dyn FnMut() -> String,
) -> Value {
    let mut pasted = pasted.clone();
    if !same_photo {
        prepare_layers_for_photo(&mut pasted, new_id);
    }
    let pasted = match pasted {
        Value::Array(layers) => layers,
        _ => Vec::new(),
    };
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

    /// New component ids for the tests: `new-0`, `new-1`…
    fn new_ids() -> impl FnMut() -> String {
        let mut n = 0;
        move || {
            n += 1;
            format!("new-{}", n - 1)
        }
    }

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
        let merged = paste_layers(&own, &pasted, false, false, &mut new_ids());
        let ids: Vec<&str> = merged
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["dodge", "sky", "vignette"]);
        assert_eq!(merged[1]["subMasks"][0]["parameters"]["range"], 50);
        // Pasting again changes nothing more.
        assert_eq!(
            paste_layers(&merged, &pasted, false, false, &mut new_ids()),
            merged
        );
        // Edit all selected: the primary's layers, the photograph's own gone.
        let mirrored = paste_layers(&own, &pasted, false, true, &mut new_ids());
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
        let other = paste_layers(&json!([]), &pasted, false, false, &mut new_ids());
        let parameters = &other[0]["subMasks"][0]["parameters"];
        assert_eq!(parameters["maskDataBase64"], Value::Null);
        assert_eq!(parameters[DETECT_PENDING], true);
        assert_eq!(parameters["grow"], 10, "its settings stay");
        assert_eq!(other[0]["subMasks"][0]["id"], "new-0", "an id of its own");
        assert_eq!(
            other[1]["subMasks"][0]["id"], "p",
            "nothing to detect: kept"
        );
        assert!(
            other[1]["subMasks"][0]["parameters"]
                .get(DETECT_PENDING)
                .is_none()
        );
        let adjustments = json!({ "masks": other });
        assert_eq!(
            pending_detections(&adjustments),
            [PendingDetection {
                layer: "sky".into(),
                kind: "ai-sky".into()
            }]
        );
        let same = paste_layers(&json!([]), &pasted, true, false, &mut new_ids());
        assert_eq!(
            same[0]["subMasks"][0]["parameters"]["maskDataBase64"],
            "data:source"
        );
        assert!(pending_detections(&json!({ "masks": same })).is_empty());
    }

    #[test]
    fn cross_photo_paste_clears_detection_geometry_and_empty_people_bitmaps() {
        let source = json!([
            layer(
                "sky",
                json!({"id": "s", "type": "ai-sky", "parameters": {
                    "maskDataBase64": "data:source", "mask_data_base64": "data:legacy",
                    "rotation": 10, "flipHorizontal": true, "flipVertical": true,
                    "orientationSteps": 1, "warp": {"perspective": 5}, "detectPending": false,
                    "grow": 10, "feather": 4
                }})
            ),
            layer(
                "empty",
                json!({"id": "p", "type": "ai-people", "parameters": {
                    "parts": [], "maskDataBase64": "data:stale", "detectPending": true, "warp": {}
                }})
            )
        ]);
        let pasted = paste_layers(&json!([]), &source, false, true, &mut new_ids());
        assert_eq!(
            pasted[0]["subMasks"][0]["parameters"],
            json!({
                "maskDataBase64": null, "mask_data_base64": null, "detectPending": true,
                "grow": 10, "feather": 4
            })
        );
        assert_eq!(pasted[0]["subMasks"][0]["id"], "new-0");
        assert_eq!(
            pasted[1]["subMasks"][0]["parameters"],
            json!({
                "parts": [], "maskDataBase64": null
            })
        );
        assert_eq!(pasted[1]["subMasks"][0]["id"], "p");
        assert_eq!(
            paste_layers(&json!([]), &source, true, true, &mut new_ids()),
            source,
            "same-photo virtual copies retain their bitmap and its geometry"
        );
    }

    #[test]
    fn only_shown_layers_without_their_detection_are_pending() {
        let pending = |visible: bool, data: Value| {
            json!({"masks": [{"id": "l", "visible": visible, "subMasks": [
                {"id": "s", "type": "ai-sky", "visible": true, "parameters": {"maskDataBase64": data, "detectPending": true}}]}]})
        };
        assert_eq!(pending_detections(&pending(true, Value::Null)).len(), 1);
        assert!(pending_detections(&pending(false, Value::Null)).is_empty());
        assert!(pending_detections(&pending(true, json!("data:own"))).is_empty());
        // A pasted People selection whose parts were all deselected before
        // its detection ran: nothing left to detect, so nothing waits.
        assert!(
            pending_detections(&json!({"masks": [{"id": "l", "visible": true, "subMasks": [
                {"id": "s", "type": "ai-people", "visible": true,
                 "parameters": {"maskDataBase64": null, "detectPending": true, "parts": []}}]}]}))
            .is_empty()
        );
        // A layer imported from a Lightroom preset waits without the mark
        // (computed when opened) and is not counted.
        assert!(
            pending_detections(&json!({"masks": [{"id": "l", "visible": true, "subMasks": [
                {"id": "s", "type": "ai-sky", "visible": true, "parameters": {"maskDataBase64": null}}]}]}))
            .is_empty()
        );
        let message = pending_detections_message(&[
            PendingDetection {
                layer: "Sky".into(),
                kind: "ai-sky".into(),
            },
            PendingDetection {
                layer: "Car".into(),
                kind: "ai-subject".into(),
            },
        ]);
        assert!(
            message.contains(
                "layer “Sky”: open the photograph in Develop to detect its Sky selection"
            )
        );
        assert!(message.contains("layer “Car”: point at its object again"));
    }
}
