//! Sky, Subject, People, landscape-part and depth range selections computed for a photo
//! that was never opened (a Lightroom or Capture One import, a preset,
//! settings pasted or synced onto it, a capture given a preset): the recipe
//! holds the selection but not its bitmap, which the editor computes when
//! the photo opens. Every export computes it with the editor's inputs — the
//! same warped photo, the same network, the same parameters merged into the
//! component — and the app's selection queue (after a batch apply or a
//! capture) computes it the same way, from a decode without AI Denoise. An
//! export of a never-opened photo is byte-identical to the export of the
//! recipe holding the masks it computed (`crates/export/tests/
//! ai_selections_export.rs`); Develop and the queue are not compared there.
//! The networks run behind `SelectionDetector` (the app's loaded models, the
//! CLI's model folder). Object selections are not among them: they need the
//! user to point at the object on each photo. People and depth range
//! components are computed only when they wait for this photo's detection
//! (`DETECT_PENDING`: a preset, paste, sync or capture); one saved without
//! its bitmap otherwise (an edit left before its detection finished) exports
//! as it always did, without it.

use rawtonic_recipe::masks;
use std::sync::Arc;

use image::DynamicImage;
use rawtonic_recipe::geometry;
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
    /// `ai-people`: People parts (`parts`) of everyone or of `person`.
    People,
    /// `ai-depth`: a depth range; the bitmap is the photo's depth map, the
    /// range (`minDepth` … `maxDepth`) applies when it renders.
    Depth,
    /// `ai-landscape`: parts of the landscape (`parts`, `landscape_parts`).
    Landscape,
}

/// A set of `masks::LANDSCAPE_PARTS` (bit `i`: the `i`-th part).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LandscapeParts(pub u8);

impl LandscapeParts {
    /// The set a component's `parts` name; `None` when one is not a known
    /// part (made by a newer version) or none is named.
    pub fn of(parts: &Value) -> Option<Self> {
        let mut bits = 0u8;
        for part in parts.as_array()? {
            let i = masks::LANDSCAPE_PARTS
                .iter()
                .position(|&known| Some(known) == part.as_str())?;
            bits |= 1 << i;
        }
        (bits != 0).then_some(Self(bits))
    }

    /// The parts' keys, in the model's order.
    pub fn keys(self) -> Vec<&'static str> {
        masks::LANDSCAPE_PARTS
            .iter()
            .enumerate()
            .filter(|(i, _)| self.0 & (1 << i) != 0)
            .map(|(_, &key)| key)
            .collect()
    }
}

/// The landscape parts a component's `parameters` select; `None` when it
/// names none or one this version does not know (nothing to detect).
pub fn landscape_parts(parameters: &Value) -> Option<LandscapeParts> {
    parameters
        .get(masks::PARTS.name())
        .and_then(LandscapeParts::of)
}

impl AutoSelection {
    /// The selection a component type is, if a network makes it alone.
    pub fn of(kind: &str) -> Option<Self> {
        match kind {
            "ai-sky" => Some(Self::Sky),
            "ai-foreground" => Some(Self::Subject),
            "ai-people" => Some(Self::People),
            "ai-depth" => Some(Self::Depth),
            "ai-landscape" => Some(Self::Landscape),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Sky => "Sky",
            Self::Subject => "Subject",
            Self::People => "People",
            Self::Depth => "depth range",
            Self::Landscape => "Landscape",
        }
    }
}

/// The People parts a component selects (`parts`); none: nothing to
/// detect (it renders nothing on any photograph, as in the editor).
pub fn people_parts(parameters: &Value) -> Vec<String> {
    parameters
        .get(masks::PARTS.name())
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|p| p.as_str().map(str::to_string))
        .collect()
}

/// The one person a People component selects (`person`, left to right),
/// `None` for everyone.
pub fn people_person(parameters: &Value) -> Option<usize> {
    parameters
        .get("person")
        .and_then(Value::as_u64)
        .map(|p| p.min(usize::MAX as u64) as usize)
}

/// What one network run computes for a component: components with the
/// same key on one photo share it (two Sky layers, the same parts).
fn run_key(selection: AutoSelection, parameters: &Value) -> String {
    match selection {
        AutoSelection::Sky | AutoSelection::Depth => selection.name().to_string(),
        AutoSelection::Subject => format!("Subject {}", subject_version(parameters)),
        AutoSelection::People => format!(
            "People {:?} {:?}",
            people_parts(parameters),
            people_person(parameters)
        ),
        AutoSelection::Landscape => format!(
            "Landscape {}",
            landscape_parts(parameters).map_or(0, |p| p.0)
        ),
    }
}

/// The `subjectVersion` of a Select Subject component's parameters: 1
/// when absent (selections saved before version 2 compute as they did),
/// `rawtonic_segment::SUBJECT_VERSION` for new ones.
pub fn subject_version(parameters: &Value) -> u32 {
    parameters
        .get(crate::recipe_versions::SUBJECT_VERSION_KEY)
        .and_then(Value::as_u64)
        .map_or(1, |v| v.min(u32::MAX as u64) as u32)
}

/// Runs the network of a selection.
pub trait SelectionDetector: Send + Sync {
    /// The mask of `selection` on `warped` (the photo with its lens and
    /// transform geometry, linear light when `linear`), as the PNG data URL
    /// a recipe stores, at `warped`'s size. `parameters` are the
    /// component's: a Subject's `subjectVersion` (`subject_version`), a
    /// People selection's parts and person (`people_parts`,
    /// `people_person`).
    fn detect(
        &self,
        selection: AutoSelection,
        parameters: &Value,
        warped: &DynamicImage,
        linear: bool,
    ) -> Result<String, String>;

    /// The masks of several landscape selections of `warped` (in the order
    /// of `sets`), as `detect` gives each, or each at its network's grid
    /// (smaller than `warped`; recorded as `BITMAP_GRID_OF`); an
    /// implementation runs the network once for all of them.
    fn detect_landscape(
        &self,
        sets: &[LandscapeParts],
        warped: &DynamicImage,
        linear: bool,
    ) -> Result<Vec<String>, String> {
        sets.iter()
            .map(|set| {
                let parameters = serde_json::json!({ masks::PARTS.name(): set.keys() });
                self.detect(AutoSelection::Landscape, &parameters, warped, linear)
            })
            .collect()
    }
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
    value.get(masks::VISIBLE.name()).and_then(Value::as_bool) != Some(false)
}

/// The shown Sky, Subject, People and depth range components of shown
/// layers without their bitmap (`layer`, `component` indices): rendered as
/// they are, their layers would apply nowhere. So are those whose bitmap
/// was migrated from a camera TIFF's thumbnail (`detects_again`). A People component without
/// parts has nothing to detect; People and depth ranges count only while
/// they wait for this photo's detection (`DETECT_PENDING`).
pub fn undetected(adjustments: &Value) -> Vec<(usize, usize, AutoSelection)> {
    let mut found = Vec::new();
    let Some(layers) = adjustments
        .get(masks::MASKS.name())
        .and_then(Value::as_array)
    else {
        return found;
    };
    for (l, layer) in layers.iter().enumerate().filter(|(_, l)| shown(l)) {
        let Some(subs) = layer.get(masks::SUB_MASKS.name()).and_then(Value::as_array) else {
            continue;
        };
        for (s, sub) in subs.iter().enumerate().filter(|(_, s)| shown(s)) {
            let Some(selection) = sub
                .get(masks::TYPE.name())
                .and_then(Value::as_str)
                .and_then(AutoSelection::of)
            else {
                continue;
            };
            let parameters = sub.get(masks::PARAMETERS.name()).unwrap_or(&Value::Null);
            let no_parts = (selection == AutoSelection::People
                && people_parts(parameters).is_empty())
                || (selection == AutoSelection::Landscape && landscape_parts(parameters).is_none());
            // People and depth ranges only where they wait for this photo
            // (see the module): older saved recipes export as before.
            let waits = matches!(
                selection,
                AutoSelection::Sky | AutoSelection::Subject | AutoSelection::Landscape
            ) || parameters.get(DETECT_PENDING).and_then(Value::as_bool) == Some(true);
            if ((waits && !has_bitmap(parameters)) || detects_again(parameters))
                && !nothing_to_detect(parameters)
                && !no_parts
            {
                found.push((l, s, selection));
            }
        }
    }
    found
}

/// Whether a component holds a bitmap migrated from a camera TIFF's
/// thumbnail that waits to be computed again on the full frame
/// (`recipe_frame::DETECT_AGAIN`).
pub fn detects_again(parameters: &Value) -> bool {
    parameters
        .get(crate::recipe_frame::DETECT_AGAIN)
        .and_then(Value::as_bool)
        == Some(true)
}

/// Whether `undetected` lists a component with nothing to render without
/// its selection: every one but a migrated bitmap that renders as it is
/// where its selection cannot be computed (`detects_again`).
pub fn missing_without_stand_ins(adjustments: &Value) -> bool {
    undetected(adjustments).iter().any(|&(l, s, _)| {
        let parameters = &adjustments[masks::MASKS.name()][l][masks::SUB_MASKS.name()][s]
            [masks::PARAMETERS.name()];
        !(detects_again(parameters) && has_bitmap(parameters))
    })
}

/// What the export says about an undetected selection: its layer's name.
pub fn undetected_message(adjustments: &Value, layer: usize, selection: AutoSelection) -> String {
    let name = adjustments[masks::MASKS.name()][layer][masks::NAME.name()]
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
        _ => json!({masks::MASK_DATA_BASE64.into_name(): null, masks::GROW.name(): 0, masks::FEATHER.name(): 0})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    };
    let result = AiSkyMaskParameters {
        mask_data_base64: Some(data_url),
        rotation: Some(
            adjustments[geometry::ROTATION.name()]
                .as_f64()
                .unwrap_or(0.0) as f32,
        ),
        flip_horizontal: Some(
            adjustments[geometry::FLIP_HORIZONTAL.name()]
                .as_bool()
                .unwrap_or(false),
        ),
        flip_vertical: Some(
            adjustments[geometry::FLIP_VERTICAL.name()]
                .as_bool()
                .unwrap_or(false),
        ),
        orientation_steps: Some(
            adjustments[geometry::ORIENTATION_STEPS.name()]
                .as_u64()
                .map_or(0, |v| v.min(u8::MAX as u64) as u8),
        ),
        warp: Some(mask_warp(adjustments)),
        bitmap_grid_of: None,
    };
    if let Ok(Value::Object(result)) = serde_json::to_value(result) {
        parameters.extend(result);
    }
    // Computed on this frame: no longer a stand-in to stretch or replace.
    for key in [
        DETECT_PENDING,
        crate::recipe_frame::DETECT_AGAIN,
        crate::mask_generation::BITMAP_FRAME,
        // Set again by a Landscape detection (`detect_missing`).
        crate::mask_generation::BITMAP_GRID_OF,
    ] {
        parameters.remove(key);
    }
    Value::Object(parameters)
}

/// Computes every shown automatic selection of `adjustments` that has
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
    // A bitmap migrated from a camera TIFF's thumbnail renders as it is
    // where its selection cannot be computed here (no model on this
    // computer); the others have nothing to render without it.
    let stands_in = |l: usize, s: usize| {
        let parameters = &adjustments[masks::MASKS.name()][l][masks::SUB_MASKS.name()][s]
            [masks::PARAMETERS.name()];
        detects_again(parameters) && has_bitmap(parameters)
    };
    let fail = |error: &str| {
        let layers: Vec<String> = wanted
            .iter()
            .filter(|&&(l, s, _)| !stands_in(l, s))
            .map(|&(l, _, selection)| undetected_message(adjustments, l, selection))
            .collect();
        format!("{}: {error}", layers.join("; "))
    };
    let required = wanted.iter().any(|&(l, s, _)| !stands_in(l, s));
    let Some(detector) = detector else {
        return if required {
            Err(fail("no AI model to compute it here"))
        } else {
            Ok(Vec::new())
        };
    };
    started();
    let geometry = mask_warp(adjustments);
    let warped = prepare_shared_warped_mask_image(source, is_raw, &geometry)?;
    let mut masks: Map<String, Value> = Map::new();
    // The landscape selections wanted, from one run of their network.
    let mut sets: Vec<LandscapeParts> = Vec::new();
    for &(l, s, selection) in &wanted {
        let parameters = &adjustments[masks::MASKS.name()][l][masks::SUB_MASKS.name()][s]
            [masks::PARAMETERS.name()];
        if selection == AutoSelection::Landscape
            && let Some(parts) = landscape_parts(parameters)
            && !sets.contains(&parts)
        {
            sets.push(parts);
        }
    }
    if !sets.is_empty() {
        match detector.detect_landscape(&sets, &warped, is_raw) {
            Ok(found) if found.len() == sets.len() => {
                for (parts, data) in sets.iter().zip(found) {
                    let key = run_key(
                        AutoSelection::Landscape,
                        &serde_json::json!({ masks::PARTS.name(): parts.keys() }),
                    );
                    masks.insert(key, Value::String(data));
                }
            }
            Ok(_) => return Err(fail("the landscape model returned too few masks")),
            // Each component then fails, or keeps its stand-in, below.
            Err(_) => {}
        }
    }
    let mut detected = Vec::new();
    let mut placed = Vec::new();
    for &(l, s, selection) in &wanted {
        let sub = &adjustments[masks::MASKS.name()][l][masks::SUB_MASKS.name()][s];
        let parameters = &sub[masks::PARAMETERS.name()];
        let key = run_key(selection, parameters);
        if !masks.contains_key(&key) {
            match detector.detect(selection, parameters, &warped, is_raw) {
                Ok(data) => {
                    masks.insert(key.clone(), Value::String(data));
                }
                Err(_) if stands_in(l, s) => continue,
                Err(e) => return Err(fail(&e)),
            }
        }
        let data = masks[&key].as_str().unwrap_or_default().to_string();
        let mut parameters = detected_parameters(&sub[masks::PARAMETERS.name()], adjustments, data);
        // A Landscape mask comes at its network's grid, as the editor stores
        // it: it records the photo's size it covers (`BITMAP_GRID_OF`).
        if selection == AutoSelection::Landscape
            && let Some(dims) = parameters[masks::MASK_DATA_BASE64.name()]
                .as_str()
                .and_then(crate::mask_generation::ai_bitmap_dimensions)
            && dims != (warped.width(), warped.height())
        {
            parameters[crate::mask_generation::BITMAP_GRID_OF] =
                json!({"width": warped.width(), "height": warped.height()});
        }
        detected.push(Detected {
            layer_id: adjustments[masks::MASKS.name()][l][masks::ID.name()]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            component_id: sub[masks::ID.name()]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            selection,
            parameters,
        });
        placed.push((l, s));
    }
    for (&(l, s), found) in placed.iter().zip(&detected) {
        adjustments[masks::MASKS.name()][l][masks::SUB_MASKS.name()][s][masks::PARAMETERS.name()] =
            found.parameters.clone();
    }
    Ok(detected)
}

/// Puts what an export detected into the photo's stored recipe `stored`,
/// as opening the photo would have: only into components that are still
/// as the export found them (same id and type, no bitmap) and whose photo
/// geometry is still the one the selection was computed in. Returns
/// whether anything changed.
pub fn keep_detected(stored: &mut Value, detected: &[Detected]) -> bool {
    let frame = stored.clone();
    keep_detected_in(stored, &frame, detected)
}

/// `keep_detected` with the photo's geometry read from `frame`: the stored
/// recipe prepared as the detection's was (`normalize` for a render adds
/// version keys and resolves the lens, which the stored recipe may not hold
/// yet: a capture, a recipe never opened).
pub fn keep_detected_in(stored: &mut Value, frame: &Value, detected: &[Detected]) -> bool {
    let warp = mask_warp(frame);
    let frame_of = |a: &Value| {
        [
            a[geometry::ROTATION.name()].as_f64().unwrap_or(0.0) as f32 as f64,
            a[geometry::FLIP_HORIZONTAL.name()]
                .as_bool()
                .unwrap_or(false) as u8 as f64,
            a[geometry::FLIP_VERTICAL.name()].as_bool().unwrap_or(false) as u8 as f64,
            a[geometry::ORIENTATION_STEPS.name()].as_u64().unwrap_or(0) as f64,
        ]
    };
    let stored_frame = frame_of(frame);
    let mut changed = false;
    for found in detected {
        let p = &found.parameters;
        let computed_frame = [
            p[geometry::ROTATION.name()].as_f64().unwrap_or(0.0),
            p[geometry::FLIP_HORIZONTAL.name()]
                .as_bool()
                .unwrap_or(false) as u8 as f64,
            p[geometry::FLIP_VERTICAL.name()].as_bool().unwrap_or(false) as u8 as f64,
            p[geometry::ORIENTATION_STEPS.name()].as_u64().unwrap_or(0) as f64,
        ];
        if p[masks::WARP.name()] != warp || computed_frame != stored_frame {
            continue;
        }
        let Some(layers) = stored
            .get_mut(masks::MASKS.name())
            .and_then(Value::as_array_mut)
        else {
            return changed;
        };
        let Some(sub) = layers
            .iter_mut()
            .filter(|layer| layer[masks::ID.name()].as_str() == Some(found.layer_id.as_str()))
            .filter_map(|layer| {
                layer
                    .get_mut(masks::SUB_MASKS.name())
                    .and_then(Value::as_array_mut)
            })
            .flatten()
            .find(|sub| sub[masks::ID.name()].as_str() == Some(found.component_id.as_str()))
        else {
            continue;
        };
        if sub[masks::TYPE.name()].as_str().and_then(AutoSelection::of) != Some(found.selection)
            || (has_bitmap(&sub[masks::PARAMETERS.name()])
                && !detects_again(&sub[masks::PARAMETERS.name()]))
            || run_key(found.selection, &sub[masks::PARAMETERS.name()])
                != run_key(found.selection, p)
        {
            // (Other parts, another person or Subject version since: the
            // bitmap is not this component's any more.)
            continue;
        }
        // The component's own settings now (Grow, Feather changed since).
        let own = sub[masks::PARAMETERS.name()].clone();
        let mut merged = detected_parameters(&own, &Value::Null, String::new());
        for key in [
            masks::MASK_DATA_BASE64.into_name(),
            geometry::ROTATION.name(),
            geometry::FLIP_HORIZONTAL.name(),
            geometry::FLIP_VERTICAL.name(),
            geometry::ORIENTATION_STEPS.name(),
            masks::WARP.name(),
        ] {
            merged[key] = p[key].clone();
        }
        // A grid bitmap keeps the photo size it covers.
        if let (Some(of), Some(merged)) = (
            p.get(crate::mask_generation::BITMAP_GRID_OF),
            merged.as_object_mut(),
        ) {
            merged.insert(crate::mask_generation::BITMAP_GRID_OF.into(), of.clone());
        }
        sub[masks::PARAMETERS.name()] = merged;
        changed = true;
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed(std::sync::Mutex<Vec<(AutoSelection, u32)>>);
    impl SelectionDetector for Fixed {
        fn detect(
            &self,
            s: AutoSelection,
            parameters: &Value,
            warped: &DynamicImage,
            _: bool,
        ) -> Result<String, String> {
            let version = match s {
                AutoSelection::Subject => subject_version(parameters),
                AutoSelection::People => people_parts(parameters).len() as u32,
                _ => 1,
            };
            self.0.lock().unwrap().push((s, version));
            Ok(format!(
                "data:{}{}:{}x{}",
                s.name(),
                if s == AutoSelection::Subject && version > 1 {
                    format!(" v{version}")
                } else {
                    String::new()
                },
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
            [(AutoSelection::Sky, 1), (AutoSelection::Subject, 1)]
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

    /// A selection migrated from a camera TIFF's thumbnail
    /// (`recipe_frame::DETECT_AGAIN`) is computed again on the full frame
    /// although it holds a bitmap; its refinements stay, and the stand-in's
    /// frame and mark go.
    #[test]
    fn migrated_selections_are_detected_again() {
        let mut adjustments = json!({"masks": [
            {"id": "a", "name": "Sky", "visible": true, "subMasks": [
                {"id": "s", "type": "ai-sky", "visible": true, "parameters": {"maskDataBase64": "data:thumb",
                 "bitmapFrame": {"width": 288, "height": 192}, "detectAgain": true, "grow": 7, "feather": 20}},
                {"id": "t", "type": "ai-people", "visible": true, "parameters": {"maskDataBase64": "data:thumb",
                 "bitmapFrame": {"width": 288, "height": 192}, "detectAgain": true, "parts": ["hair"]}},
                {"id": "u", "type": "ai-subject", "visible": true, "parameters": {"maskDataBase64": "data:thumb",
                 "bitmapFrame": {"width": 288, "height": 192}}}]}]});
        assert_eq!(
            undetected(&adjustments)
                .iter()
                .map(|w| w.2)
                .collect::<Vec<_>>(),
            [AutoSelection::Sky, AutoSelection::People]
        );
        let source = Arc::new(DynamicImage::new_rgb8(6, 4));
        let detector = Fixed(Default::default());
        let found = detect_missing(
            &mut adjustments,
            &source,
            false,
            Some(&detector),
            &mut || {},
        )
        .unwrap();
        assert_eq!(found.len(), 2);
        let sky = &adjustments["masks"][0]["subMasks"][0]["parameters"];
        assert_eq!(sky["maskDataBase64"], "data:Sky:6x4");
        assert_eq!(sky["grow"], 7);
        assert_eq!(sky["feather"], 20);
        assert!(sky.get("bitmapFrame").is_none() && sky.get("detectAgain").is_none());
        // The Object selection keeps its migrated bitmap.
        let object = &adjustments["masks"][0]["subMasks"][2]["parameters"];
        assert_eq!(object["maskDataBase64"], "data:thumb");
        assert!(undetected(&adjustments).is_empty());
    }

    /// Without a model here a migrated bitmap renders as it is: the export
    /// keeps it instead of failing (a selection without a bitmap still
    /// fails, as before).
    #[test]
    fn a_migrated_selection_without_a_model_keeps_its_bitmap() {
        struct Failing;
        impl SelectionDetector for Failing {
            fn detect(
                &self,
                _: AutoSelection,
                _: &Value,
                _: &DynamicImage,
                _: bool,
            ) -> Result<String, String> {
                Err("the sky model is not installed".into())
            }
        }
        let migrated = json!({"masks": [{"id": "a", "name": "Sky", "visible": true, "subMasks": [
            {"id": "s", "type": "ai-sky", "visible": true, "parameters": {"maskDataBase64": "data:thumb",
             "bitmapFrame": {"width": 288, "height": 192}, "detectAgain": true}}]}]});
        let source = Arc::new(DynamicImage::new_rgb8(6, 4));
        for detector in [None, Some(&Failing as &dyn SelectionDetector)] {
            let mut adjustments = migrated.clone();
            let found = detect_missing(&mut adjustments, &source, false, detector, &mut || {});
            assert_eq!(found, Ok(Vec::new()));
            assert_eq!(adjustments, migrated);
        }
        let mut missing = migrated.clone();
        missing["masks"][0]["subMasks"][0]["parameters"]["maskDataBase64"] = Value::Null;
        assert!(detect_missing(&mut missing, &source, false, Some(&Failing), &mut || {}).is_err());
    }

    #[test]
    fn each_subject_computes_at_its_own_version() {
        // A selection saved before version 2 (no `subjectVersion`) keeps
        // computing as it did; a new one uses its version.
        let mut adjustments = json!({"masks": [
            {"id": "a", "name": "Old", "visible": true, "subMasks": [
                {"id": "o", "type": "ai-foreground", "visible": true, "parameters": {"maskDataBase64": null}}]},
            {"id": "b", "name": "New", "visible": true, "subMasks": [
                {"id": "n", "type": "ai-foreground", "visible": true, "parameters": {"maskDataBase64": null, "subjectVersion": 2}},
                {"id": "m", "type": "ai-foreground", "visible": true, "invert": true, "parameters": {"maskDataBase64": null, "subjectVersion": 2}}]}]});
        let source = Arc::new(DynamicImage::new_rgb8(6, 4));
        let detector = Fixed(Default::default());
        detect_missing(
            &mut adjustments,
            &source,
            false,
            Some(&detector),
            &mut || {},
        )
        .unwrap();
        assert_eq!(
            *detector.0.lock().unwrap(),
            [(AutoSelection::Subject, 1), (AutoSelection::Subject, 2)]
        );
        let data = |l: usize, s: usize| {
            adjustments["masks"][l]["subMasks"][s]["parameters"]["maskDataBase64"].clone()
        };
        assert_eq!(data(0, 0), "data:Subject:6x4");
        assert_eq!(data(1, 0), "data:Subject v2:6x4");
        assert_eq!(data(1, 1), "data:Subject v2:6x4");
        assert_eq!(
            adjustments["masks"][1]["subMasks"][0]["parameters"]["subjectVersion"],
            2
        );
    }

    #[test]
    fn people_and_depth_ranges_are_detected_with_their_own_settings() {
        // An adaptive preset applied to a closed photo: People parts and a
        // depth range wait for this photo's detection; a People selection
        // without parts and an Object selection are left.
        let mut adjustments = json!({"masks": [
            {"id": "a", "name": "Skin", "visible": true, "subMasks": [
                {"id": "p", "type": "ai-people", "visible": true, "parameters": {"maskDataBase64": null, "detectPending": true, "parts": ["faceSkin", "bodySkin"], "person": null}},
                {"id": "q", "type": "ai-people", "visible": true, "parameters": {"maskDataBase64": null, "detectPending": true, "parts": ["faceSkin", "bodySkin"], "person": null}}]},
            {"id": "b", "name": "None", "visible": true, "subMasks": [
                {"id": "n", "type": "ai-people", "visible": true, "parameters": {"maskDataBase64": null, "detectPending": true, "parts": []}},
                {"id": "o", "type": "ai-people", "visible": true, "parameters": {"maskDataBase64": null}},
                // Saved without its bitmap, not waiting (left before its
                // detection finished): exported without it, as before.
                {"id": "s", "type": "ai-people", "visible": true, "parameters": {"maskDataBase64": null, "parts": ["hair"]}},
                {"id": "t", "type": "ai-depth", "visible": true, "parameters": {"maskDataBase64": null, "minDepth": 10}}]},
            {"id": "c", "name": "Far", "visible": true, "subMasks": [
                {"id": "d", "type": "ai-depth", "visible": true, "parameters": {"maskDataBase64": null, "detectPending": true, "minDepth": 60, "maxDepth": 100, "minFade": 10, "maxFade": 0, "feather": 5}}]}]});
        let source = Arc::new(DynamicImage::new_rgb8(6, 4));
        let detector = Fixed(Default::default());
        let found = detect_missing(
            &mut adjustments,
            &source,
            false,
            Some(&detector),
            &mut || {},
        )
        .unwrap();
        assert_eq!(found.len(), 3);
        assert_eq!(
            *detector.0.lock().unwrap(),
            [(AutoSelection::People, 2), (AutoSelection::Depth, 1)],
            "one run per kind and parts"
        );
        let people = &adjustments["masks"][0]["subMasks"][1]["parameters"];
        assert_eq!(people["maskDataBase64"], "data:People:6x4");
        assert_eq!(people["parts"], json!(["faceSkin", "bodySkin"]));
        assert!(people.get(DETECT_PENDING).is_none());
        let depth = &adjustments["masks"][2]["subMasks"][0]["parameters"];
        assert_eq!(depth["maskDataBase64"], "data:depth range:6x4");
        assert_eq!(depth["minDepth"], 60);
        assert_eq!(depth["feather"], 5);
        assert!(undetected(&adjustments).is_empty());
        assert!(crate::masks::pending_detections(&adjustments).is_empty());
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
                _: &Value,
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
    fn a_detection_on_the_prepared_recipe_is_kept_in_the_stored_one() {
        // A capture's stored recipe holds only the preset's layers; the
        // detection ran on it prepared for a render (lens resolved), so the
        // geometry is compared in that frame, and the stored recipe gets the
        // bitmap with the frame it was computed in.
        let stored = json!({"masks": [{"id": "a", "name": "Sky", "visible": true, "subMasks": [
            {"id": "s", "type": "ai-sky", "visible": true, "parameters": {"maskDataBase64": null, "detectPending": true}}]}]});
        let mut prepared = stored.clone();
        prepared["lensDistortionAmount"] = json!(100.0);
        prepared["lensDistortionParams"] = json!({"k1": 0.01});
        let found = detect_missing(
            &mut prepared,
            &Arc::new(DynamicImage::new_rgb8(6, 4)),
            false,
            Some(&Fixed(Default::default())),
            &mut || {},
        )
        .unwrap();
        assert!(!keep_detected(&mut stored.clone(), &found), "another frame");
        let mut kept = stored.clone();
        assert!(keep_detected_in(&mut kept, &prepared, &found));
        assert_eq!(
            kept["masks"][0]["subMasks"][0]["parameters"]["maskDataBase64"],
            "data:Sky:6x4"
        );
        assert!(
            kept.get("lensDistortionParams").is_none(),
            "only the masks change"
        );
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
        // Another People request since (parts changed): not its bitmap.
        let mut people = json!({"masks": [{"id": "a", "name": "Skin", "visible": true, "subMasks": [
            {"id": "p", "type": "ai-people", "visible": true, "parameters": {"maskDataBase64": null, "detectPending": true, "parts": ["faceSkin"]}}]}]});
        let found_people = detect_missing(
            &mut people.clone(),
            &source,
            false,
            Some(&Fixed(Default::default())),
            &mut || {},
        )
        .unwrap();
        people["masks"][0]["subMasks"][0]["parameters"]["parts"] = json!(["hair"]);
        assert!(!keep_detected(&mut people, &found_people));
        let mut opened = recipe();
        opened["masks"][0]["subMasks"][0]["parameters"]["maskDataBase64"] = json!("data:own");
        opened["masks"][1]["subMasks"][0]["parameters"]["maskDataBase64"] = json!("data:own");
        opened["masks"][1]["subMasks"][1]["parameters"]["maskDataBase64"] = json!("data:own");
        assert!(!keep_detected(&mut opened, &found));
    }

    struct Landscapes(std::sync::Mutex<Vec<Vec<Vec<&'static str>>>>);
    impl SelectionDetector for Landscapes {
        fn detect(
            &self,
            _: AutoSelection,
            _: &Value,
            _: &DynamicImage,
            _: bool,
        ) -> Result<String, String> {
            Err("one part at a time".into())
        }
        fn detect_landscape(
            &self,
            sets: &[LandscapeParts],
            _: &DynamicImage,
            _: bool,
        ) -> Result<Vec<String>, String> {
            let sets: Vec<Vec<&'static str>> = sets.iter().map(|s| s.keys()).collect();
            let found = sets
                .iter()
                .map(|keys| format!("data:{}", keys.join("+")))
                .collect();
            self.0.lock().unwrap().push(sets);
            Ok(found)
        }
    }

    #[test]
    fn landscape_parts_of_a_photo_come_from_one_run() {
        let part = |id: &str, parts: Value| {
            json!({"id": id, "type": "ai-landscape", "visible": true,
                "parameters": {"parts": parts, "maskDataBase64": null, "detectPending": true, "grow": 5}})
        };
        let mut adjustments = json!({"masks": [
            {"id": "a", "name": "Land", "visible": true, "subMasks": [
                part("w", json!(["water"])), part("m", json!(["architecture", "mountains"]))]},
            {"id": "b", "name": "Water again", "visible": true, "subMasks": [part("x", json!(["water"]))]},
            // A part this version does not know, or none, is not a selection.
            {"id": "c", "name": "Unknown", "visible": true, "subMasks": [
                part("y", json!(["water", "lava"])), part("z", json!([]))]}]});
        assert_eq!(undetected(&adjustments).len(), 3);
        let detector = Landscapes(Default::default());
        let found = detect_missing(
            &mut adjustments,
            &Arc::new(DynamicImage::new_rgb8(6, 4)),
            false,
            Some(&detector),
            &mut || {},
        )
        .unwrap();
        assert_eq!(
            *detector.0.lock().unwrap(),
            [vec![vec!["water"], vec!["mountains", "architecture"]]]
        );
        assert_eq!(found.len(), 3);
        let params =
            |l: usize, s: usize| adjustments["masks"][l]["subMasks"][s]["parameters"].clone();
        assert_eq!(params(0, 0)["maskDataBase64"], "data:water");
        assert_eq!(params(0, 0)["parts"], json!(["water"]));
        assert_eq!(params(0, 0)["grow"], 5);
        assert!(params(0, 0).get("detectPending").is_none());
        assert_eq!(
            params(0, 1)["maskDataBase64"],
            "data:mountains+architecture"
        );
        assert_eq!(params(1, 0)["maskDataBase64"], "data:water");
        assert!(params(2, 0)["maskDataBase64"].is_null());
        assert!(params(2, 1)["maskDataBase64"].is_null());
    }

    /// Answers every Landscape set with a PNG of `size`.
    struct GridLandscapes((u32, u32));

    impl SelectionDetector for GridLandscapes {
        fn detect(
            &self,
            _: AutoSelection,
            _: &Value,
            _: &DynamicImage,
            _: bool,
        ) -> Result<String, String> {
            Err("one part at a time".into())
        }
        fn detect_landscape(
            &self,
            sets: &[LandscapeParts],
            _: &DynamicImage,
            _: bool,
        ) -> Result<Vec<String>, String> {
            let mut png = std::io::Cursor::new(Vec::new());
            image::GrayImage::from_fn(self.0.0, self.0.1, |x, y| {
                image::Luma([(x * 80 + y * 40) as u8])
            })
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
            let url = format!(
                "data:image/png;base64,{}",
                base64::Engine::encode(&base64::engine::general_purpose::STANDARD, png.get_ref())
            );
            Ok(sets.iter().map(|_| url.clone()).collect())
        }
    }

    /// An export's Landscape detection at the network's grid records the
    /// photo's size it covers, as the editor's does; one at the photo's size
    /// records none, and a size left from an earlier bitmap goes.
    #[test]
    fn landscape_grids_detected_at_export_record_the_photo_size() {
        for (size, recorded) in [
            ((3, 2), Some(json!({"width": 6, "height": 4}))),
            ((6, 4), None),
        ] {
            let mut adjustments = json!({"masks": [{"id": "a", "name": "Land", "visible": true,
                "subMasks": [{"id": "w", "type": "ai-landscape", "visible": true, "parameters": {
                    "parts": ["water"], "maskDataBase64": null, "detectPending": true,
                    "bitmapGridOf": {"width": 60, "height": 40}}}]}]});
            let mut stored = adjustments.clone();
            let found = detect_missing(
                &mut adjustments,
                &Arc::new(DynamicImage::new_rgb8(6, 4)),
                false,
                Some(&GridLandscapes(size)),
                &mut || {},
            )
            .unwrap();
            let parameters = &adjustments["masks"][0]["subMasks"][0]["parameters"];
            assert!(parameters["maskDataBase64"].is_string());
            assert_eq!(
                parameters.get("bitmapGridOf").cloned(),
                recorded,
                "{size:?}"
            );
            // Kept in the saved edit with its bitmap, as the export rendered it.
            assert!(keep_detected_in(&mut stored, &json!({}), &found));
            let kept = &stored["masks"][0]["subMasks"][0]["parameters"];
            assert_eq!(kept["maskDataBase64"], parameters["maskDataBase64"]);
            assert_eq!(kept.get("bitmapGridOf").cloned(), recorded, "{size:?}");
            // The saved edit renders the mask the export rendered: over the
            // whole photo, not in its corner.
            let geometry = crate::mask_generation::PhotoGeometry::default().with_decoded_size(6, 4);
            let render = |p: &Value| {
                crate::mask_generation::ai_bitmap_in_frame(
                    p,
                    p["maskDataBase64"].as_str().unwrap(),
                    6,
                    4,
                    1.0,
                    (0.0, 0.0),
                    &geometry,
                )
                .unwrap()
            };
            let shown = render(parameters);
            assert_eq!(shown, render(kept));
            assert!(shown.get_pixel(5, 3)[0] > 0, "{size:?}");
        }
    }
}
