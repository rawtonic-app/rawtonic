//! The style last applied to a photograph (`appliedStyle` in its recipe),
//! so that the next style replaces it and applying it again removes it, on
//! every photo of a selection and after the app restarts. The editor applies
//! styles to the open photograph by the same rules (src/utils/appliedStyle.ts;
//! both check the cases in `applied_style_cases.json`).
//!
//! ```json
//! "appliedStyle": {"id": "…", "amount": 100,
//!   "changes": [{"path": ["exposure"], "from": 0, "to": 1}, {"path": ["hsl", "reds", "hue"], "to": 5}]}
//! ```
//!
//! `changes` are the settings the style sets, at the deepest level of
//! nested settings (one slider of HSL or grading, also inside a group the
//! recipe did not have yet); lists and the curves (points with their
//! `version`) count as one setting.
//! `from` is the value before the style, `to` the value it set (the editor
//! adds `full`, its value at full strength, when the Amount changes); a
//! missing one means the setting was not in the recipe. Taking the style
//! back sets each of them to `from`, but only where the recipe still holds
//! `to`: a setting the user changed after the style keeps the user's value
//! (and is no longer the style's), and the user's changes to other settings
//! stay. A group the style added and that ends up empty goes too. Settings
//! the app works out or rewrites per photo (`PHOTO_OWN_KEYS`: the lens
//! choice and its data, layers, relight lights) are the photo's own: a style
//! sets them, but they are not recorded. The renderer never reads the key;
//! recipes without it are unchanged.

use serde_json::{Map, Value};

/// The recipe key.
pub const APPLIED_STYLE: &str = "appliedStyle";

/// Settings the app works out or rewrites for each photo after a style set
/// them: the lens choice and its data (resolved per photo), layers (AI
/// detections, placed gradients, frames after a turn) and relight lights
/// (filled in on loading). They are the photo's own: a style sets them, but
/// its record leaves them out, so taking it back or replacing it leaves them
/// as they are (`PHOTO_OWN_KEYS` in src/utils/appliedStyle.ts, checked there).
pub const PHOTO_OWN_KEYS: [&str; 9] = [
    "lensCorrectionMode",
    "lensMaker",
    "lensModel",
    "cameraCaVersion",
    "cameraGeometryVersion",
    "lensDistortionParams",
    crate::lens_recipe::LENS_PROFILE_FOR,
    "masks",
    "relightLights",
];

/// Whether `current` still holds what a style set (`to`), exactly: numbers
/// compare by value, a missing setting and `null` are the same.
fn holds(current: Option<&Value>, to: Option<&Value>) -> bool {
    match (current, to) {
        (None | Some(Value::Null), None | Some(Value::Null)) => true,
        (Some(current), Some(to)) => contains(current, to),
        _ => false,
    }
}

fn contains(current: &Value, to: &Value) -> bool {
    match (current, to) {
        (Value::Number(a), Value::Number(b)) => a.as_f64() == b.as_f64(),
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| contains(a, b))
        }
        (Value::Object(a), Value::Object(b)) => a
            .keys()
            .chain(b.keys())
            .all(|key| holds(a.get(key), b.get(key))),
        (a, b) => a == b,
    }
}

fn get<'a>(recipe: &'a Value, path: &[String]) -> Option<&'a Value> {
    path.iter()
        .try_fold(recipe, |value, key| value.as_object()?.get(key))
}

/// Sets (or with `None` removes) the setting at `path`, making the groups
/// above it where missing; nothing when one of them is not a group. A group
/// a removal leaves empty goes too (the style added it).
fn set(recipe: &mut Value, path: &[String], value: Option<&Value>) {
    let (Some((key, rest)), Some(map)) = (path.split_first(), recipe.as_object_mut()) else {
        return;
    };
    if rest.is_empty() {
        match value {
            Some(value) => {
                map.insert(key.clone(), value.clone());
            }
            None => {
                map.remove(key);
            }
        }
        return;
    }
    if value.is_some() && !map.contains_key(key) {
        map.insert(key.clone(), Value::Object(Map::new()));
    }
    let Some(group) = map.get_mut(key).filter(|group| group.is_object()) else {
        return;
    };
    set(group, rest, value);
    if value.is_none() && group.as_object().is_some_and(Map::is_empty) {
        map.remove(key);
    }
}

/// The id of the style last applied to `recipe`, if any.
pub fn applied_style_id(recipe: &Value) -> Option<&str> {
    recipe.get(APPLIED_STYLE)?.get("id")?.as_str()
}

/// Takes the style last applied to `recipe` back (see the module): the
/// settings it set and the user left alone return to their values before it.
/// Returns the paths of the settings it set that the user changed since
/// (kept as the user set them). The `appliedStyle` key is removed.
pub fn revert(recipe: &mut Value) -> Vec<Vec<String>> {
    let Some(applied) = recipe
        .as_object_mut()
        .and_then(|map| map.remove(APPLIED_STYLE))
    else {
        return Vec::new();
    };
    let mut kept = Vec::new();
    for change in applied
        .get("changes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(path) = change
            .get("path")
            .and_then(Value::as_array)
            .and_then(|path| {
                path.iter()
                    .map(|key| key.as_str().map(str::to_owned))
                    .collect::<Option<Vec<_>>>()
            })
        else {
            continue;
        };
        if path.is_empty() {
            continue;
        }
        if holds(get(recipe, &path), change.get("to")) {
            set(recipe, &path, change.get("from"));
        } else {
            kept.push(path);
        }
    }
    kept
}

/// Whether a style's `value` for the setting at `path` is a group of
/// settings that goes setting by setting (into `own`, the photo's value):
/// not the curves, and not over a setting that is no group.
fn goes_by_setting(path: &[String], value: &Value, own: Option<&Value>) -> bool {
    // Curves go whole: their points with the version that says how they render.
    value.is_object()
        && !(path.len() == 1 && path[0] == "curves")
        && own.is_none_or(Value::is_object)
}

/// `own` (a photo's setting at `path`) with a style's `value` applied:
/// groups of settings join the photo's (a style that sets one HSL slider
/// leaves the others as they were), every other value replaces it.
pub fn merged(path: &mut Vec<String>, own: Option<&Value>, value: &Value) -> Value {
    if !goes_by_setting(path, value, own) {
        return value.clone();
    }
    let mut out = own.cloned().unwrap_or_else(|| Value::Object(Map::new()));
    for (key, value) in value.as_object().into_iter().flatten() {
        path.push(key.clone());
        let next = merged(path, out.get(key), value);
        path.pop();
        out[key.as_str()] = next;
    }
    out
}

fn walk(
    values: &Value,
    before: &Value,
    after: &Value,
    path: &mut Vec<String>,
    out: &mut Vec<Value>,
) {
    for (key, value) in values.as_object().into_iter().flatten() {
        if path.is_empty() && key == APPLIED_STYLE {
            continue;
        }
        path.push(key.clone());
        let (b, a) = (get(before, path), get(after, path));
        if goes_by_setting(path, value, b) && a.is_some_and(Value::is_object) {
            walk(value, before, after, path, out);
        } else if b.is_some() || a.is_some() {
            // Every setting the style sets, also one it leaves as it was: a
            // user change to it is the user's, and its Amount still moves it.
            let mut change = Map::new();
            change.insert(
                "path".into(),
                path.iter().cloned().map(Value::String).collect(),
            );
            if let Some(b) = b {
                change.insert("from".into(), b.clone());
            }
            if let Some(a) = a {
                change.insert("to".into(), a.clone());
            }
            out.push(Value::Object(change));
        }
        path.pop();
    }
}

/// Records in `after` that the style `id` (at `amount` percent) set its
/// settings (`values`, one by one inside groups) from their values in
/// `before` (the recipe with the previous style taken back) to those in
/// `after`. A style that changed nothing is still recorded, so that it shows
/// as applied and applying it again removes it.
pub fn record(before: &Value, after: &mut Value, values: &Value, id: &str, amount: f64) {
    let mut changes = Vec::new();
    // The photo's own settings: set, not recorded.
    let mut values = values.clone();
    if let Some(map) = values.as_object_mut() {
        map.retain(|key, _| !PHOTO_OWN_KEYS.contains(&key.as_str()));
    }
    walk(&values, before, after, &mut Vec::new(), &mut changes);
    if let Some(map) = after.as_object_mut() {
        map.insert(
            APPLIED_STYLE.into(),
            serde_json::json!({ "id": id, "amount": amount, "changes": changes }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The cases the editor checks too (src/utils/appliedStyle.test.ts).
    #[test]
    fn takes_a_style_back_as_the_editor_does() {
        let cases: Value = serde_json::from_str(include_str!("applied_style_cases.json")).unwrap();
        for case in cases.as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let mut recipe = case["recipe"].clone();
            let kept = revert(&mut recipe);
            assert_eq!(recipe, case["reverted"], "{name}");
            let kept: Vec<Value> = kept.into_iter().map(|path| json!(path)).collect();
            assert_eq!(Value::Array(kept), case["kept"], "{name}");
        }
    }

    #[test]
    fn records_every_setting_a_style_sets_and_takes_it_back() {
        let before = json!({"exposure": 0.0, "contrast": 10, "shadows": 0, "hsl": {"reds": {"hue": 0, "saturation": 0}}});
        let mut after = json!({"exposure": 1, "contrast": 10, "shadows": 0, "hsl": {"reds": {"hue": 5, "saturation": 0}}, "grainVersion": 2});
        let values = json!({"exposure": 1, "contrast": 10, "hsl": {"reds": {"hue": 5, "saturation": 0}}, "grainVersion": 2});
        record(&before, &mut after, &values, "warm", 100.0);
        assert_eq!(
            after[APPLIED_STYLE],
            json!({"id": "warm", "amount": 100.0, "changes": [
                {"path": ["contrast"], "from": 10, "to": 10},
                {"path": ["exposure"], "from": 0.0, "to": 1},
                {"path": ["grainVersion"], "to": 2},
                {"path": ["hsl", "reds", "hue"], "from": 0, "to": 5},
                {"path": ["hsl", "reds", "saturation"], "from": 0, "to": 0},
            ]})
        );
        assert_eq!(applied_style_id(&after), Some("warm"));
        // The user changes shadows (not the style's) and the red hue.
        after["shadows"] = json!(30);
        after["hsl"]["reds"]["hue"] = json!(9);
        let kept = revert(&mut after);
        assert_eq!(
            after,
            json!({"exposure": 0.0, "contrast": 10, "shadows": 30, "hsl": {"reds": {"hue": 9, "saturation": 0}}})
        );
        assert_eq!(
            kept,
            vec![vec!["hsl".to_owned(), "reds".to_owned(), "hue".to_owned()]]
        );
    }

    /// A style that sets some sliders of a group leaves the others; taken
    /// back, the group is as it was.
    #[test]
    fn a_partial_group_joins_the_photos_own() {
        let own = json!({"hsl": {"reds": {"hue": 0, "saturation": 4}, "blues": {"hue": 22}}, "masks": [{"id": "own"}]});
        let values = json!({"hsl": {"reds": {"hue": 5}}, "masks": []});
        let mut after = own.clone();
        for (key, value) in values.as_object().unwrap() {
            after[key.as_str()] = merged(&mut vec![key.clone()], own.get(key), value);
        }
        assert_eq!(
            after,
            json!({"hsl": {"reds": {"hue": 5, "saturation": 4}, "blues": {"hue": 22}}, "masks": []})
        );
        record(&own, &mut after, &values, "red", 100.0);
        assert_eq!(after[APPLIED_STYLE]["changes"].as_array().unwrap().len(), 1);
        revert(&mut after);
        assert_eq!(after, json!({"hsl": own["hsl"], "masks": []}));
    }

    /// A group the recipe did not have yet goes setting by setting too: a
    /// user change to one of them keeps only that one.
    #[test]
    fn a_group_the_style_adds_goes_setting_by_setting() {
        let before = json!({"exposure": 0});
        let mut after = json!({"exposure": 0, "colorGrading": {"shadows": {"hue": 30}, "highlights": {"hue": 200}}});
        let values = json!({"colorGrading": {"shadows": {"hue": 30}, "highlights": {"hue": 200}}});
        record(&before, &mut after, &values, "tone", 100.0);
        after["colorGrading"]["shadows"]["hue"] = json!(40);
        after["colorGrading"]["midtones"] = json!({"hue": 90});
        revert(&mut after);
        assert_eq!(
            after,
            json!({"exposure": 0, "colorGrading": {"shadows": {"hue": 40}, "midtones": {"hue": 90}}})
        );
        // Left alone, the group goes as a whole.
        let mut untouched = json!({"exposure": 0, "colorGrading": {"shadows": {"hue": 30}}});
        record(
            &before,
            &mut untouched,
            &json!({"colorGrading": {"shadows": {"hue": 30}}}),
            "tone",
            100.0,
        );
        revert(&mut untouched);
        assert_eq!(untouched, before);
    }

    /// Layers, relight lights and the lens a style sets are the photo's own:
    /// taking the style back leaves them.
    #[test]
    fn the_photos_own_settings_stay_when_a_style_is_taken_back() {
        let before = json!({"exposure": 0, "masks": [], "lensCorrectionMode": "manual"});
        let values = json!({"exposure": 1, "masks": [{"id": "sky"}], "relightLights": [{"x": 1}], "lensCorrectionMode": "auto"});
        let mut after = before.clone();
        for (key, value) in values.as_object().unwrap() {
            after[key.as_str()] = value.clone();
        }
        record(&before, &mut after, &values, "look", 100.0);
        assert_eq!(
            after[APPLIED_STYLE]["changes"],
            json!([{"path": ["exposure"], "from": 0, "to": 1}])
        );
        revert(&mut after);
        assert_eq!(
            after,
            json!({"exposure": 0, "masks": [{"id": "sky"}], "relightLights": [{"x": 1}], "lensCorrectionMode": "auto"})
        );
    }

    /// Curves go whole, with the version that says how their points render.
    #[test]
    fn curves_go_whole_with_their_version() {
        let own = json!({"curves": {"version": 3, "luma": [[0, 0], [255, 255]]}});
        let values = json!({"curves": {"luma": [[0, 0], [128, 150], [255, 255]]}});
        let after = merged(
            &mut vec!["curves".into()],
            own.get("curves"),
            &values["curves"],
        );
        assert_eq!(after, values["curves"]);
        let mut recipe = json!({"curves": after});
        record(&own, &mut recipe, &values, "c", 100.0);
        assert_eq!(
            recipe[APPLIED_STYLE]["changes"][0]["path"],
            json!(["curves"])
        );
        revert(&mut recipe);
        assert_eq!(recipe, own);
    }

    /// The renderer does not read the key: a recipe renders the same with it.
    #[test]
    fn the_renderer_ignores_the_applied_style() {
        let plain = json!({"exposure": 0.7, "contrast": 12, "renderVersion": 6});
        let mut styled = plain.clone();
        record(
            &json!({"exposure": 0}),
            &mut styled,
            &json!({"exposure": 0.7}),
            "s",
            100.0,
        );
        for is_raw in [true, false] {
            let a = crate::image_processing::get_all_adjustments_from_json(&plain, is_raw, None);
            let b = crate::image_processing::get_all_adjustments_from_json(&styled, is_raw, None);
            assert_eq!(
                bytemuck::bytes_of(&a.uniforms),
                bytemuck::bytes_of(&b.uniforms)
            );
            assert_eq!(format!("{a:?}"), format!("{b:?}"));
        }
    }
}
