//! The user's default develop settings (Preferences → Develop defaults): one
//! global default, per-camera defaults by make and model and per camera body
//! by serial number, applied to photographs without an edit
//! (`image_processing::is_new_recipe`) as the settings their new edit starts
//! with.
//!
//! The app sets them (`set_current`) whenever its settings change; every
//! new edit starts through `camera_crop::apply_default` (renders of
//! never-edited photos, exports, thumbnails, batch edits of never-opened
//! photos) and the editor asks for the same settings (`for_photo`), so a
//! photograph renders the same whether it was opened or not. Its first save
//! writes the settings into its recipe together with `KEY`, the levels they
//! came from, so an edited photograph never changes when the defaults
//! change later; one never edited follows them. A recipe that carries `KEY`
//! without any other edit (Reset to neutral writes `[]`) is still new but
//! has its defaults decided: it gets none.
//!
//! Nothing is applied while no default is set (`DevelopDefaults::is_empty`),
//! so recipes and exports stay byte-identical until the user sets one.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The recipe key recording which defaults a new edit started with: the
/// levels applied, most general first (`"global"`, `"camera"`, `"body"`);
/// `[]` for none (a Reset to neutral, or no default for the photo).
pub const KEY: &str = "developDefaults";

/// The recipe key keeping the settings those levels gave the new edit, so
/// the photo's Before shows the defaults its edit began with, never later
/// ones (written with `KEY` when any level applied).
pub const SETTINGS_KEY: &str = "developDefaultsSettings";

/// Level names written under `KEY`.
pub const GLOBAL: &str = "global";
pub const CAMERA: &str = "camera";
pub const BODY: &str = "body";

/// Keys a default never sets, whatever the settings file holds: the
/// photograph's own frame and render pipeline, its layers and generated
/// patches, and the record itself. The editor leaves out crop, transform,
/// lens and every other per-photo key when it saves a default
/// (`src/utils/developDefaults.ts`); these are the ones that would break a
/// new edit's invariants if a hand-edited file carried them.
const NEVER_DEFAULTED: &[&str] = &[
    KEY,
    SETTINGS_KEY,
    "renderVersion",
    "sourceFrameVersion",
    crate::recipe_frame::KEY,
    "lensProfileFor",
    "colorLabel",
    "masks",
    "aiPatches",
    "crop",
    "aspectRatio",
    "rotation",
    "orientationSteps",
    "flipHorizontal",
    "flipVertical",
    "lensCorrectionMode",
    "lensDistortionParams",
];

fn yes() -> bool {
    true
}

/// One set of default settings: recipe keys at their values (top-level keys
/// replace the new edit's), with the name it was made from for the
/// Preferences list.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DefaultSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default)]
    pub settings: Map<String, Value>,
}

/// Defaults for a camera model (`serial` none) or one body of it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CameraDefaults {
    pub make: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default)]
    pub settings: Map<String, Value>,
    /// Its settings join the next general level's (a body's the model's, a
    /// model's the global default's) instead of replacing them: true for a
    /// tool saved as default (Capture One's per-tool camera defaults), false
    /// for a preset or a photo's settings set as the camera's default
    /// (Lightroom's Raw Defaults: exactly that preset).
    #[serde(default = "yes")]
    pub inherit: bool,
}

/// The user's default develop settings (`AppSettings::develop_defaults`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DevelopDefaults {
    /// Only RAW files get defaults; JPEG, HEIF and TIFF from the camera
    /// start neutral (on by default, as both references' raw defaults).
    #[serde(default = "yes")]
    pub raw_only: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub global: Option<DefaultSettings>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cameras: Vec<CameraDefaults>,
}

impl Default for DevelopDefaults {
    fn default() -> Self {
        Self {
            raw_only: true,
            global: None,
            cameras: Vec::new(),
        }
    }
}

impl DevelopDefaults {
    /// No default set: nothing is applied anywhere.
    pub fn is_empty(&self) -> bool {
        self.global.is_none() && self.cameras.is_empty()
    }
}

/// The camera a photograph was taken with, as its file names it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CameraIdentity {
    pub make: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial: Option<String>,
}

/// The defaults one photograph gets.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Resolved {
    /// The levels applied, most general first (`KEY`'s value).
    pub levels: Vec<String>,
    /// The settings, the more specific level's winning key by key.
    pub settings: Map<String, Value>,
}

fn same(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

impl CameraDefaults {
    fn is_for(&self, camera: &CameraIdentity) -> bool {
        same(&self.make, &camera.make) && same(&self.model, &camera.model)
    }
}

/// The defaults for a photograph (`is_raw`, taken with `camera` when its
/// file names one): its body's (the serial number's), its model's and the
/// global default, the more specific winning (a level that does not
/// `inherit` ends the chain). None when none applies.
pub fn resolve(
    defaults: &DevelopDefaults,
    is_raw: bool,
    camera: Option<&CameraIdentity>,
) -> Option<Resolved> {
    if defaults.raw_only && !is_raw {
        return None;
    }
    // Most specific first.
    let mut chain: Vec<(&'static str, &Map<String, Value>, bool)> = Vec::new();
    if let Some(camera) = camera {
        if let Some(serial) = camera.serial.as_deref().filter(|s| !s.trim().is_empty())
            && let Some(body) = defaults.cameras.iter().find(|entry| {
                entry.is_for(camera)
                    && entry
                        .serial
                        .as_deref()
                        .is_some_and(|s| s.trim() == serial.trim())
            })
        {
            chain.push((BODY, &body.settings, body.inherit));
        }
        if let Some(model) = defaults
            .cameras
            .iter()
            .find(|entry| entry.is_for(camera) && entry.serial.is_none())
        {
            chain.push((CAMERA, &model.settings, model.inherit));
        }
    }
    if let Some(global) = &defaults.global {
        chain.push((GLOBAL, &global.settings, false));
    }
    let end = chain
        .iter()
        .position(|(_, _, inherit)| !inherit)
        .map_or(chain.len(), |at| at + 1);
    chain.truncate(end);
    if chain.is_empty() {
        return None;
    }
    let mut resolved = Resolved::default();
    for (level, settings, _) in chain.into_iter().rev() {
        resolved.levels.push(level.to_string());
        for (key, value) in settings {
            if !NEVER_DEFAULTED.contains(&key.as_str()) {
                resolved.settings.insert(key.clone(), value.clone());
            }
        }
    }
    Some(resolved)
}

fn registry() -> &'static RwLock<Arc<DevelopDefaults>> {
    static CURRENT: OnceLock<RwLock<Arc<DevelopDefaults>>> = OnceLock::new();
    CURRENT.get_or_init(Default::default)
}

/// Sets the defaults new edits start with (the app, on every settings load
/// and change).
pub fn set_current(defaults: DevelopDefaults) {
    let mut current = registry().write().unwrap_or_else(|e| e.into_inner());
    if **current != defaults {
        *current = Arc::new(defaults);
    }
}

/// The defaults new edits start with now.
pub fn current() -> Arc<DevelopDefaults> {
    registry().read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// The current defaults as bytes for the keys of caches of never-edited
/// renders (thumbnails), None while none is set.
pub fn fingerprint() -> Option<Vec<u8>> {
    let defaults = current();
    if defaults.is_empty() {
        return None;
    }
    serde_json::to_vec(&*defaults).ok()
}

/// Whether a recipe follows the defaults: new (`is_new_recipe`) and without
/// its defaults decided (`KEY`).
pub fn follows_defaults(recipe: &Value) -> bool {
    crate::image_processing::is_new_recipe(recipe) && recipe.get(KEY).is_none()
}

/// The camera of the photograph at `path`: a RAW's make and model as the
/// decoder names them (rawler's clean names) and its serial number, other
/// files' from their EXIF. Read once per file size and modification time.
pub fn camera_of(path: &Path) -> Option<CameraIdentity> {
    type Cache = Mutex<HashMap<PathBuf, (u64, std::time::SystemTime, Option<CameraIdentity>)>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let (len, modified) = std::fs::metadata(path)
        .ok()
        .and_then(|m| Some((m.len(), m.modified().ok()?)))?;
    if let Some((l, m, camera)) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(path)
        && *l == len
        && *m == modified
    {
        return camera.clone();
    }
    let camera = if crate::formats::is_raw_file(path) {
        // Mapped without read-ahead: only the tags are read.
        rawler::rawsource::RawSource::new_lazy(path)
            .ok()
            .and_then(|source| camera_of_raw(&source))
    } else {
        camera_of_exif(path)
    };
    let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
    if cache.len() > 4096 {
        cache.clear();
    }
    cache.insert(path.to_path_buf(), (len, modified, camera.clone()));
    camera
}

/// A RAW's camera from bytes already read.
pub fn camera_of_raw(source: &rawler::rawsource::RawSource) -> Option<CameraIdentity> {
    let decoder = rawler::get_decoder(source).ok()?;
    let metadata = decoder
        .raw_metadata(source, &rawler::decoders::RawDecodeParams::default())
        .ok()?;
    identity(
        &metadata.make,
        &metadata.model,
        metadata.exif.serial_number.as_deref(),
    )
}

fn camera_of_exif(path: &Path) -> Option<CameraIdentity> {
    let file = std::fs::File::open(path).ok()?;
    let exif = exif::Reader::new()
        .read_from_container(&mut std::io::BufReader::new(file))
        .ok()?;
    let text = |tag: exif::Tag| {
        exif.get_field(tag, exif::In::PRIMARY)
            .and_then(|field| match &field.value {
                exif::Value::Ascii(parts) => parts.first().map(|bytes| {
                    String::from_utf8_lossy(bytes)
                        .trim_matches(char::from(0))
                        .trim()
                        .to_string()
                }),
                _ => None,
            })
    };
    identity(
        &text(exif::Tag::Make)?,
        &text(exif::Tag::Model)?,
        text(exif::Tag::BodySerialNumber).as_deref(),
    )
}

/// The camera named by a photograph's cached EXIF values ("Make", "Model",
/// "SerialNumber"), for an original that must not be read.
pub fn camera_of_exif_map(
    exif: &std::collections::HashMap<String, String>,
) -> Option<CameraIdentity> {
    let get = |key: &str| exif.get(key).map(String::as_str);
    identity(
        get("Make").unwrap_or_default(),
        get("Model").unwrap_or_default(),
        get("SerialNumber"),
    )
}

fn identity(make: &str, model: &str, serial: Option<&str>) -> Option<CameraIdentity> {
    let (make, model) = (make.trim(), model.trim());
    if make.is_empty() && model.is_empty() {
        return None;
    }
    Some(CameraIdentity {
        make: make.to_string(),
        model: model.to_string(),
        serial: serial
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    })
}

/// The current defaults for the photograph at `path` (its file's camera
/// read only when a default is set), None when none applies.
pub fn for_path(path: &Path) -> Option<Resolved> {
    let defaults = current();
    if defaults.is_empty() {
        return None;
    }
    resolve(
        &defaults,
        crate::formats::is_raw_file(path),
        camera_of(path).as_ref(),
    )
}

/// Puts `resolved` onto a recipe that has just started as a new edit:
/// its settings over the new edit's, `KEY` with its levels and
/// `SETTINGS_KEY` with the settings (the photo's Before).
pub fn apply_resolved(recipe: &mut Value, resolved: &Resolved) {
    if let Some(map) = recipe.as_object_mut() {
        for (key, value) in &resolved.settings {
            map.insert(key.clone(), value.clone());
        }
        map.insert(KEY.to_string(), serde_json::json!(resolved.levels));
        map.insert(
            SETTINGS_KEY.to_string(),
            Value::Object(resolved.settings.clone()),
        );
    }
}

/// The current defaults onto a recipe that was new (`follows_defaults`)
/// before it started as a new edit, for the photograph whose camera
/// `camera` reads (only when a default is set). Called by
/// `camera_crop::apply_default*` right after the new edit's versions.
pub(crate) fn apply_current(
    recipe: &mut Value,
    is_raw: bool,
    camera: impl FnOnce() -> Option<CameraIdentity>,
) {
    apply_with(&current(), recipe, is_raw, camera);
}

/// `apply_current` with `defaults`.
fn apply_with(
    defaults: &DevelopDefaults,
    recipe: &mut Value,
    is_raw: bool,
    camera: impl FnOnce() -> Option<CameraIdentity>,
) {
    if defaults.is_empty() {
        return;
    }
    if let Some(resolved) = resolve(defaults, is_raw, camera().as_ref()) {
        apply_resolved(recipe, &resolved);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn settings(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    fn z9(serial: Option<&str>) -> CameraIdentity {
        CameraIdentity {
            make: "Nikon".into(),
            model: "Z 9".into(),
            serial: serial.map(str::to_string),
        }
    }

    fn defaults() -> DevelopDefaults {
        DevelopDefaults {
            raw_only: true,
            global: Some(DefaultSettings {
                name: Some("House look".into()),
                settings: settings(json!({"contrast": 10, "vibrance": 5})),
            }),
            cameras: vec![
                CameraDefaults {
                    make: "NIKON".into(),
                    model: "z 9".into(),
                    serial: None,
                    name: None,
                    settings: settings(json!({"contrast": 20, "sharpness": 30})),
                    inherit: true,
                },
                CameraDefaults {
                    make: "Nikon".into(),
                    model: "Z 9".into(),
                    serial: Some("3001234".into()),
                    name: None,
                    settings: settings(json!({"contrast": 30})),
                    inherit: true,
                },
            ],
        }
    }

    #[test]
    fn serial_wins_over_model_and_model_over_global_key_by_key() {
        let d = defaults();
        let body = resolve(&d, true, Some(&z9(Some("3001234")))).unwrap();
        assert_eq!(body.levels, ["global", "camera", "body"]);
        assert_eq!(
            Value::Object(body.settings),
            json!({"contrast": 30, "vibrance": 5, "sharpness": 30})
        );
        let model = resolve(&d, true, Some(&z9(Some("999")))).unwrap();
        assert_eq!(model.levels, ["global", "camera"]);
        assert_eq!(model.settings["contrast"], json!(20));
        let other = CameraIdentity {
            make: "Canon".into(),
            model: "EOS R5".into(),
            serial: None,
        };
        let global = resolve(&d, true, Some(&other)).unwrap();
        assert_eq!(global.levels, ["global"]);
        assert_eq!(resolve(&d, true, None).unwrap().levels, ["global"]);
    }

    #[test]
    fn a_level_that_does_not_inherit_replaces_the_more_general_ones() {
        let mut d = defaults();
        d.cameras[0].inherit = false;
        let model = resolve(&d, true, Some(&z9(None))).unwrap();
        assert_eq!(model.levels, ["camera"]);
        assert_eq!(
            Value::Object(model.settings),
            json!({"contrast": 20, "sharpness": 30})
        );
        // The body still joins the model it inherits from.
        let body = resolve(&d, true, Some(&z9(Some("3001234")))).unwrap();
        assert_eq!(body.levels, ["camera", "body"]);
    }

    #[test]
    fn raw_only_keeps_other_files_neutral() {
        let mut d = defaults();
        assert!(resolve(&d, false, Some(&z9(None))).is_none());
        d.raw_only = false;
        assert_eq!(
            resolve(&d, false, Some(&z9(None))).unwrap().levels,
            ["global", "camera"]
        );
        assert!(resolve(&DevelopDefaults::default(), true, Some(&z9(None))).is_none());
    }

    #[test]
    fn per_photo_keys_never_come_from_a_default() {
        let d = DevelopDefaults {
            global: Some(DefaultSettings {
                name: None,
                settings: settings(json!({
                    "exposure": 0.5, "crop": {"x": 0}, "masks": [], "renderVersion": 1,
                    "developDefaults": ["x"], "developDefaultsSettings": {}, "aiPatches": []
                })),
            }),
            ..Default::default()
        };
        let resolved = resolve(&d, true, None).unwrap();
        assert_eq!(Value::Object(resolved.settings), json!({"exposure": 0.5}));
    }

    #[test]
    fn the_record_keeps_a_new_recipe_new_but_decided() {
        assert!(follows_defaults(&Value::Null));
        assert!(follows_defaults(&json!({"sourceFrameVersion": 2})));
        let neutral = json!({"developDefaults": [], "sourceFrameVersion": 2});
        assert!(crate::image_processing::is_new_recipe(&neutral));
        assert!(!follows_defaults(&neutral));
        let mut recipe = json!({});
        apply_resolved(
            &mut recipe,
            &Resolved {
                levels: vec!["global".into()],
                settings: settings(json!({"contrast": 10})),
            },
        );
        assert_eq!(
            recipe,
            json!({
                "contrast": 10,
                "developDefaults": ["global"],
                "developDefaultsSettings": {"contrast": 10}
            })
        );
        assert!(!crate::image_processing::is_new_recipe(&recipe));
    }

    #[test]
    fn a_new_edit_takes_the_defaults_over_its_versions_and_reads_no_camera_without_one() {
        let mut recipe = json!({});
        crate::image_processing::start_new_edit(&mut recipe, true);
        let before = recipe.clone();
        apply_with(&DevelopDefaults::default(), &mut recipe, true, || {
            panic!("no camera is read while no default is set")
        });
        assert_eq!(recipe, before);
        let mut d = defaults();
        d.global
            .as_mut()
            .unwrap()
            .settings
            .insert("toneVersion".into(), json!(17));
        apply_with(&d, &mut recipe, true, || Some(z9(None)));
        assert_eq!(recipe["contrast"], json!(20));
        assert_eq!(recipe["toneVersion"], json!(17));
        assert_eq!(recipe["developDefaults"], json!(["global", "camera"]));
        assert_eq!(recipe["renderVersion"], before["renderVersion"]);
    }

    #[test]
    fn the_settings_file_reads_with_its_defaults() {
        let d: DevelopDefaults = serde_json::from_value(json!({
            "cameras": [{"make": "Nikon", "model": "Z 9", "settings": {"contrast": 5}}]
        }))
        .unwrap();
        assert!(d.raw_only);
        assert!(d.cameras[0].inherit);
        assert!(!d.is_empty());
        assert!(DevelopDefaults::default().is_empty());
    }
}
