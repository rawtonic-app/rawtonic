//! Sky and Subject selections an export computes for a photo that was never
//! opened (a Lightroom or Capture One import, settings pasted or synced
//! onto it): the recipe holds the selection but not its bitmap, which the
//! editor computes when the photo opens. The export computes it the way the
//! editor does — the same warped photo, the same network, the same
//! parameters merged into the component — so a photo exports the same
//! pixels whether it was opened first or not. The networks run behind
//! `SelectionDetector` (the app's loaded models, the CLI's model folder).

use std::sync::Arc;

use image::DynamicImage;
use serde_json::{Map, Value, json};

use crate::mask_generation::{mask_warp, prepare_shared_warped_mask_image};
use crate::masks::{AiSkyMaskParameters, DETECT_PENDING, has_bitmap, nothing_to_detect};

/// The selections a network makes on its own, without the user pointing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AutoSelection {
    /// `ai-sky`: Select Sky.
    Sky,
    /// `ai-foreground`: Select Subject.
    Subject,
}

impl AutoSelection {
    /// The selection a component type is, if a network makes it alone.
    pub fn of(kind: &str) -> Option<Self> {
        match kind {
            "ai-sky" => Some(Self::Sky),
            "ai-foreground" => Some(Self::Subject),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Sky => "Sky",
            Self::Subject => "Subject",
        }
    }
}

/// Runs the network of a selection.
pub trait SelectionDetector: Send + Sync {
    /// The mask of `selection` on `warped` (the photo with its lens and
    /// transform geometry, linear light when `linear`), as the PNG data URL
    /// a recipe stores, at `warped`'s size.
    fn detect(
        &self,
        selection: AutoSelection,
        warped: &DynamicImage,
        linear: bool,
    ) -> Result<String, String>;
}

/// One component an export detected: where it is and its parameters now.
#[derive(Clone, Debug, PartialEq)]
pub struct Detected {
    pub layer_id: String,
    pub component_id: String,
    pub selection: AutoSelection,
    pub parameters: Value,
}

fn shown(value: &Value) -> bool {
    value.get("visible").and_then(Value::as_bool) != Some(false)
}

/// The shown Sky and Subject components of shown layers without their
/// bitmap (`layer`, `component` indices): rendered as they are, their
/// layers would apply nowhere.
pub fn undetected(adjustments: &Value) -> Vec<(usize, usize, AutoSelection)> {
    let mut found = Vec::new();
    let Some(layers) = adjustments.get("masks").and_then(Value::as_array) else {
        return found;
    };
    for (l, layer) in layers.iter().enumerate().filter(|(_, l)| shown(l)) {
        let Some(subs) = layer.get("subMasks").and_then(Value::as_array) else {
            continue;
        };
        for (s, sub) in subs.iter().enumerate().filter(|(_, s)| shown(s)) {
            let Some(selection) = sub
                .get("type")
                .and_then(Value::as_str)
                .and_then(AutoSelection::of)
            else {
                continue;
            };
            let parameters = sub.get("parameters").unwrap_or(&Value::Null);
            if !has_bitmap(parameters) && !nothing_to_detect(parameters) {
                found.push((l, s, selection));
            }
        }
    }
    found
}

/// What the export says about an undetected selection: its layer's name.
pub fn undetected_message(adjustments: &Value, layer: usize, selection: AutoSelection) -> String {
    let name = adjustments["masks"][layer]["name"]
        .as_str()
        .filter(|n| !n.is_empty())
        .unwrap_or("a layer");
    format!("layer “{name}”: its {} selection", selection.name())
}

/// The parameters the editor gives a component when its selection
/// arrives (`useAiMasking.ts` `applyAiResult`): its own, with the bitmap
/// and the frame it was computed in (straightening, flips, orientation,
/// `mask_warp`), no longer waiting for a detection. An empty component
/// first gets the editor's defaults.
pub fn detected_parameters(own: &Value, adjustments: &Value, data_url: String) -> Value {
    let mut parameters = match own.as_object() {
        Some(own) if !own.is_empty() => own.clone(),
        _ => json!({"maskDataBase64": null, "grow": 0, "feather": 0})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    };
    let result = AiSkyMaskParameters {
        mask_data_base64: Some(data_url),
        rotation: Some(adjustments["rotation"].as_f64().unwrap_or(0.0) as f32),
        flip_horizontal: Some(adjustments["flipHorizontal"].as_bool().unwrap_or(false)),
        flip_vertical: Some(adjustments["flipVertical"].as_bool().unwrap_or(false)),
        orientation_steps: Some(
            adjustments["orientationSteps"]
                .as_u64()
                .map_or(0, |v| v.min(u8::MAX as u64) as u8),
        ),
        warp: Some(mask_warp(adjustments)),
    };
    if let Ok(Value::Object(result)) = serde_json::to_value(result) {
        parameters.extend(result);
    }
    parameters.remove(DETECT_PENDING);
    Value::Object(parameters)
}

/// Computes every shown Sky and Subject selection of `adjustments` that has
/// no bitmap (`undetected`) on `source` (the export's decode, linear when
/// `is_raw`) and puts it into its component; each selection runs once per
/// photo. `started` is called once before the first network runs (the
/// export shows it). An error names the layers it leaves without their
/// selection; nothing is changed then.
pub fn detect_missing(
    adjustments: &mut Value,
    source: &Arc<DynamicImage>,
    is_raw: bool,
    detector: Option<&dyn SelectionDetector>,
    started: &mut dyn FnMut(),
) -> Result<Vec<Detected>, String> {
    let wanted = undetected(adjustments);
    if wanted.is_empty() {
        return Ok(Vec::new());
    }
    let fail = |error: &str| {
        let layers: Vec<String> = wanted
            .iter()
            .map(|&(l, _, selection)| undetected_message(adjustments, l, selection))
            .collect();
        format!("{}: {error}", layers.join("; "))
    };
    let Some(detector) = detector else {
        return Err(fail("no AI model to compute it here"));
    };
    started();
    let geometry = mask_warp(adjustments);
    let warped = prepare_shared_warped_mask_image(source, is_raw, &geometry)?;
    let mut masks: Map<String, Value> = Map::new();
    let mut detected = Vec::new();
    for &(l, s, selection) in &wanted {
        let key = selection.name().to_string();
        if !masks.contains_key(&key) {
            let data = detector
                .detect(selection, &warped, is_raw)
                .map_err(|e| fail(&e))?;
            masks.insert(key.clone(), Value::String(data));
        }
        let data = masks[&key].as_str().unwrap_or_default().to_string();
        let sub = &adjustments["masks"][l]["subMasks"][s];
        detected.push(Detected {
            layer_id: adjustments["masks"][l]["id"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            component_id: sub["id"].as_str().unwrap_or_default().to_string(),
            selection,
            parameters: detected_parameters(&sub["parameters"], adjustments, data),
        });
    }
    for (&(l, s, _), found) in wanted.iter().zip(&detected) {
        adjustments["masks"][l]["subMasks"][s]["parameters"] = found.parameters.clone();
    }
    Ok(detected)
}

/// Puts what an export detected into the photo's stored recipe `stored`,
/// as opening the photo would have: only into components that are still
/// as the export found them (same id and type, no bitmap) and whose photo
/// geometry is still the one the selection was computed in. Returns
/// whether anything changed.
pub fn keep_detected(stored: &mut Value, detected: &[Detected]) -> bool {
    let geometry = mask_warp(stored);
    let frame = |a: &Value| {
        [
            a["rotation"].as_f64().unwrap_or(0.0) as f32 as f64,
            a["flipHorizontal"].as_bool().unwrap_or(false) as u8 as f64,
            a["flipVertical"].as_bool().unwrap_or(false) as u8 as f64,
            a["orientationSteps"].as_u64().unwrap_or(0) as f64,
        ]
    };
    let stored_frame = frame(stored);
    let mut changed = false;
    for found in detected {
        let p = &found.parameters;
        let computed_frame = [
            p["rotation"].as_f64().unwrap_or(0.0),
            p["flipHorizontal"].as_bool().unwrap_or(false) as u8 as f64,
            p["flipVertical"].as_bool().unwrap_or(false) as u8 as f64,
            p["orientationSteps"].as_u64().unwrap_or(0) as f64,
        ];
        if p["warp"] != geometry || computed_frame != stored_frame {
            continue;
        }
        let Some(layers) = stored.get_mut("masks").and_then(Value::as_array_mut) else {
            return changed;
        };
        let Some(sub) = layers
            .iter_mut()
            .filter(|layer| layer["id"].as_str() == Some(found.layer_id.as_str()))
            .filter_map(|layer| layer.get_mut("subMasks").and_then(Value::as_array_mut))
            .flatten()
            .find(|sub| sub["id"].as_str() == Some(found.component_id.as_str()))
        else {
            continue;
        };
        if sub["type"].as_str().and_then(AutoSelection::of) != Some(found.selection)
            || has_bitmap(&sub["parameters"])
        {
            continue;
        }
        // The component's own settings now (Grow, Feather changed since).
        let own = sub["parameters"].clone();
        let mut merged = detected_parameters(&own, &Value::Null, String::new());
        for key in [
            "maskDataBase64",
            "rotation",
            "flipHorizontal",
            "flipVertical",
            "orientationSteps",
            "warp",
        ] {
            merged[key] = p[key].clone();
        }
        sub["parameters"] = merged;
        changed = true;
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed(std::sync::Mutex<Vec<AutoSelection>>);
    impl SelectionDetector for Fixed {
        fn detect(
            &self,
            s: AutoSelection,
            warped: &DynamicImage,
            _: bool,
        ) -> Result<String, String> {
            self.0.lock().unwrap().push(s);
            Ok(format!(
                "data:{}:{}x{}",
                s.name(),
                warped.width(),
                warped.height()
            ))
        }
    }

    fn recipe() -> Value {
        json!({"rotation": 1.5, "orientationSteps": 1, "transformRotate": 2.0, "masks": [
            {"id": "a", "name": "Sky", "visible": true, "subMasks": [
                {"id": "s", "type": "ai-sky", "visible": true, "parameters": {"maskDataBase64": null, "grow": 10, "feather": 0}}]},
            {"id": "b", "name": "Sky 2", "visible": true, "subMasks": [
                {"id": "t", "type": "ai-sky", "visible": true, "parameters": {"maskDataBase64": null, "detectPending": true, "grow": 0}},
                {"id": "u", "type": "ai-foreground", "visible": true, "parameters": {}}]},
            {"id": "c", "name": "Off", "visible": false, "subMasks": [
                {"id": "v", "type": "ai-sky", "parameters": {"maskDataBase64": null}}]},
            {"id": "d", "name": "Object", "visible": true, "subMasks": [
                {"id": "w", "type": "ai-subject", "parameters": {"maskDataBase64": null}}]}]})
    }

    #[test]
    fn missing_selections_are_detected_once_each_as_the_editor_stores_them() {
        let mut adjustments = recipe();
        let source = Arc::new(DynamicImage::new_rgb8(6, 4));
        let detector = Fixed(Default::default());
        let mut calls = 0;
        let found = detect_missing(
            &mut adjustments,
            &source,
            false,
            Some(&detector),
            &mut || calls += 1,
        )
        .unwrap();
        assert_eq!(calls, 1);
        assert_eq!(
            *detector.0.lock().unwrap(),
            [AutoSelection::Sky, AutoSelection::Subject]
        );
        assert_eq!(
            found.len(),
            3,
            "hidden layers and Object selections are left"
        );
        let sky = &adjustments["masks"][0]["subMasks"][0]["parameters"];
        assert_eq!(sky["maskDataBase64"], "data:Sky:6x4");
        assert_eq!(sky["grow"], 10);
        assert_eq!(sky["rotation"], json!(1.5f32));
        assert_eq!(sky["orientationSteps"], 1);
        assert_eq!(sky["flipHorizontal"], false);
        assert_eq!(sky["warp"], json!({"transformRotate": 2.0}));
        let pasted = &adjustments["masks"][1]["subMasks"][0]["parameters"];
        assert!(pasted.get(DETECT_PENDING).is_none());
        let subject = &adjustments["masks"][1]["subMasks"][1]["parameters"];
        assert_eq!(subject["grow"], 0);
        assert_eq!(subject["feather"], 0);
        assert_eq!(subject["maskDataBase64"], "data:Subject:6x4");
        assert!(undetected(&adjustments).is_empty());
        // Nothing left: no network runs.
        let again = detect_missing(
            &mut adjustments,
            &source,
            false,
            Some(&detector),
            &mut || panic!("nothing to detect"),
        );
        assert_eq!(again, Ok(Vec::new()));
    }

    #[test]
    fn without_a_model_the_layers_are_named_and_nothing_changes() {
        let mut adjustments = recipe();
        let before = adjustments.clone();
        let source = Arc::new(DynamicImage::new_rgb8(2, 2));
        let error = detect_missing(&mut adjustments, &source, false, None, &mut || {}).unwrap_err();
        assert!(error.contains("layer “Sky”: its Sky selection"), "{error}");
        assert!(
            error.contains("layer “Sky 2”: its Subject selection"),
            "{error}"
        );
        assert_eq!(adjustments, before);

        struct Missing;
        impl SelectionDetector for Missing {
            fn detect(
                &self,
                _: AutoSelection,
                _: &DynamicImage,
                _: bool,
            ) -> Result<String, String> {
                Err("the Sky model is not installed".into())
            }
        }
        let error = detect_missing(&mut adjustments, &source, false, Some(&Missing), &mut || {})
            .unwrap_err();
        assert!(error.ends_with("the Sky model is not installed"), "{error}");
        assert_eq!(adjustments, before);
    }

    #[test]
    fn the_stored_recipe_keeps_a_detection_only_where_it_still_fits() {
        let mut exported = recipe();
        let source = Arc::new(DynamicImage::new_rgb8(6, 4));
        let found = detect_missing(
            &mut exported,
            &source,
            false,
            Some(&Fixed(Default::default())),
            &mut || {},
        )
        .unwrap();
        // Grow changed since the export was submitted: kept with the data.
        let mut stored = recipe();
        stored["masks"][0]["subMasks"][0]["parameters"]["grow"] = json!(30);
        assert!(keep_detected(&mut stored, &found));
        assert_eq!(stored["masks"][0]["subMasks"][0]["parameters"]["grow"], 30);
        assert_eq!(
            stored["masks"][0]["subMasks"][0]["parameters"]["maskDataBase64"],
            "data:Sky:6x4"
        );
        let mut same = exported.clone();
        same["masks"][0]["subMasks"][0]["parameters"]["grow"] = json!(30);
        assert_eq!(stored, same);
        // Another geometry since: the selection would sit off the photo.
        let mut turned = recipe();
        turned["transformRotate"] = json!(4.0);
        assert!(!keep_detected(&mut turned.clone(), &found));
        let mut straightened = recipe();
        straightened["rotation"] = json!(2.0);
        assert!(!keep_detected(&mut straightened, &found));
        // Detected meanwhile (the photo was opened): left as it is.
        let mut opened = recipe();
        opened["masks"][0]["subMasks"][0]["parameters"]["maskDataBase64"] = json!("data:own");
        opened["masks"][1]["subMasks"][0]["parameters"]["maskDataBase64"] = json!("data:own");
        opened["masks"][1]["subMasks"][1]["parameters"]["maskDataBase64"] = json!("data:own");
        assert!(!keep_detected(&mut opened, &found));
    }
}
