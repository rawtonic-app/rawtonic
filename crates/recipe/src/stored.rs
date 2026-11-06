//! The recipe as saved.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::key::{Key, KeyValue};

/// The recipe key holding each section's on/off switch.
const SECTION_VISIBILITY: &str = "sectionVisibility";

/// A section of the develop panel with an on/off switch in
/// `sectionVisibility` (missing: on).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Section(&'static str);

impl Section {
    /// Exposure, contrast and the tone sliders.
    pub const BASIC: Self = Self("basic");
    /// Presence and detail (clarity, dehaze, centré, sharpening, …).
    pub const DETAILS: Self = Self("details");
    pub const SHARPENING: Self = Self("sharpening");
    pub const NOISE_REDUCTION: Self = Self("noiseReduction");
    pub const EFFECTS: Self = Self("effects");
    pub const COLOR: Self = Self("color");
    pub const WHITE_BALANCE: Self = Self("whiteBalance");
    pub const PRESENCE: Self = Self("presence");
    pub const COLOR_MIXER: Self = Self("colorMixer");
    pub const COLOR_GRADING: Self = Self("colorGrading");
    pub const CALIBRATION: Self = Self("calibration");
    pub const COLOR_EDITOR: Self = Self("colorEditor");

    /// The section's name in `sectionVisibility`.
    pub const fn name(self) -> &'static str {
        self.0
    }
}

/// A saved recipe, read only (see [`StoredRecipe`]): a view of the JSON
/// value as read from a sidecar, the catalog or the editor.
#[derive(Clone, Copy, Debug)]
pub struct StoredRef<'a>(&'a Value);

impl<'a> StoredRef<'a> {
    /// The recipe `json` (any JSON value: a recipe that is not an object
    /// holds no key).
    pub fn new(json: &'a Value) -> Self {
        Self(json)
    }

    /// The JSON value, unchanged.
    pub fn as_value(self) -> &'a Value {
        self.0
    }

    /// The value of `key` when the recipe holds it with the key's type.
    pub fn get<T: KeyValue>(self, key: &Key<T>) -> Option<T> {
        self.0.get(key.name()).and_then(T::from_json)
    }

    /// Borrow a nullable string key without allocating, including large resource payloads.
    pub fn get_str(self, key: &Key<Option<String>>) -> Option<&'a str> {
        self.0.get(key.name()).and_then(Value::as_str)
    }

    /// The value of `key`, or its default when the recipe does not hold it
    /// (or holds another type).
    pub fn get_or_default<T: KeyValue + Clone>(self, key: &Key<T>) -> T {
        self.get(key).unwrap_or(key.default())
    }

    /// Whether `section` is switched on (`sectionVisibility`, missing or
    /// not a boolean: on).
    pub fn section_visible(self, section: Section) -> bool {
        self.0
            .get(SECTION_VISIBILITY)
            .and_then(|sections| sections.get(section.name()))
            .and_then(Value::as_bool)
            .unwrap_or(true)
    }
}

/// A saved recipe, owned: the JSON value exactly as read — key presence,
/// explicit `null`s, unknown and nested fields — written back unchanged
/// unless a typed key is set or removed. Serialises as that value.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StoredRecipe(Value);

impl StoredRecipe {
    /// The recipe `json`, as read.
    pub fn from_value(json: Value) -> Self {
        Self(json)
    }

    /// The JSON value to save.
    pub fn into_value(self) -> Value {
        self.0
    }

    /// A read-only view.
    pub fn view(&self) -> StoredRef<'_> {
        StoredRef(&self.0)
    }

    /// See [`StoredRef::get`].
    pub fn get<T: KeyValue>(&self, key: &Key<T>) -> Option<T> {
        self.view().get(key)
    }

    /// See [`StoredRef::get_or_default`].
    pub fn get_or_default<T: KeyValue + Clone>(&self, key: &Key<T>) -> T {
        self.view().get_or_default(key)
    }

    /// Stores `value` under `key`, leaving every other key as it is. A
    /// recipe that is not an object (`null`: never edited) becomes one.
    pub fn set<T: KeyValue>(&mut self, key: &Key<T>, value: T) {
        self.object()
            .insert(key.name().to_string(), value.to_json());
    }

    /// Removes `key` (the recipe then means its default), returning what
    /// it held.
    pub fn remove<T: KeyValue>(&mut self, key: &Key<T>) -> Option<Value> {
        self.0.as_object_mut()?.remove(key.name())
    }

    fn object(&mut self) -> &mut Map<String, Value> {
        if !self.0.is_object() {
            self.0 = Value::Object(Map::new());
        }
        match &mut self.0 {
            Value::Object(map) => map,
            _ => unreachable!("made an object above"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tone;
    use serde_json::json;

    /// The codec fixtures of the engine's round-trip tests (step 0.3b).
    fn fixtures() -> Vec<(String, Value)> {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../engine/tests/fixtures/");
        let read = |name: &str| -> Value {
            serde_json::from_str(&std::fs::read_to_string(format!("{dir}{name}")).unwrap()).unwrap()
        };
        let mut out: Vec<(String, Value)> = read("recipe-codec.json")
            .as_object()
            .unwrap()
            .iter()
            .map(|(name, recipe)| (name.clone(), recipe.clone()))
            .collect();
        out.push(("new-edit".into(), read("new-edit-recipe.raw.json")));
        out
    }

    #[test]
    fn a_recipe_saved_without_an_edit_is_the_value_it_was_read_as() {
        for (name, recipe) in fixtures() {
            let text = serde_json::to_string(&recipe).unwrap();
            let stored: StoredRecipe = serde_json::from_str(&text).unwrap();
            assert_eq!(serde_json::to_string(&stored).unwrap(), text, "{name}");
            assert_eq!(stored.into_value(), recipe, "{name}");
        }
    }

    #[test]
    fn setting_a_key_changes_only_that_key() {
        for (name, recipe) in fixtures() {
            let mut stored = StoredRecipe::from_value(recipe.clone());
            stored.set(&tone::EXPOSURE, 1.25);
            let mut expected = match recipe {
                Value::Object(map) => map,
                _ => Map::new(),
            };
            expected.insert("exposure".into(), json!(1.25));
            assert_eq!(stored.into_value(), Value::Object(expected), "{name}");
        }
    }

    #[test]
    fn removing_a_key_changes_only_that_key() {
        let mut stored =
            StoredRecipe::from_value(json!({"exposure": null, "contrast": 20, "x": {"y": [1]}}));
        assert_eq!(stored.remove(&tone::EXPOSURE), Some(Value::Null));
        assert_eq!(
            stored.into_value(),
            json!({"contrast": 20, "x": {"y": [1]}})
        );
        let mut never_edited = StoredRecipe::from_value(Value::Null);
        assert_eq!(never_edited.remove(&tone::EXPOSURE), None);
        assert_eq!(never_edited.into_value(), Value::Null);
    }

    #[test]
    fn a_key_reads_as_the_renderer_read_it() {
        let recipe = json!({
            "exposure": 1,
            "contrast": "20",
            "highlights": null,
            "shadows": -12.5,
            "sectionVisibility": {"basic": false, "details": "no"}
        });
        let stored = StoredRef::new(&recipe);
        for key in tone::KEYS {
            assert_eq!(
                stored.get_or_default(key),
                recipe[key.name()].as_f64().unwrap_or(0.0),
                "{}",
                key.name()
            );
        }
        assert!(!stored.section_visible(Section::BASIC));
        assert!(stored.section_visible(Section::DETAILS));
        assert!(StoredRef::new(&Value::Null).section_visible(Section::BASIC));
        assert_eq!(StoredRef::new(&json!([1])).get(&tone::EXPOSURE), None);
    }
}
