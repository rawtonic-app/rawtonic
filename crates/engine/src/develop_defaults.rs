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

/// The camera a photograph was taken with: one name for all its files
/// (`canonical_names`: its RAWs, their DNG conversions and its JPEG/HEIF
/// name it alike), and the names its file gave before there was one
/// (`file_make`/`file_model`, only when they differ), which a camera
/// default saved under them is still found by first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CameraIdentity {
    pub make: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial: Option<String>,
    /// The make as the file names it (a JPEG's EXIF, a RAW's decoder),
    /// none when it is `make`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_make: Option<String>,
    /// The model as the file names it, none when it is `model`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_model: Option<String>,
    /// The other names files of this camera carry (rawler's catalog, folded
    /// to lower case): a camera default saved under one of them is this
    /// camera's (`CameraDefaults::is_for`; the editor matches by them).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub also_named: Vec<CameraName>,
}

impl CameraIdentity {
    /// The names the photo's file gives its camera.
    fn file_names(&self) -> (&str, &str) {
        (
            self.file_make.as_deref().unwrap_or(&self.make),
            self.file_model.as_deref().unwrap_or(&self.model),
        )
    }
}

/// One camera's make and model for all its files: the clean names of
/// rawler's camera catalog for any make and model the catalog knows (the
/// names a camera writes into its RAW's TIFF/EXIF tags, which its JPEG and
/// HEIF and its DNG conversions carry too; regional names stay their own),
/// the names as given otherwise (a camera without RAW support, a phone).
/// Exact names only, ignoring case and surrounding spaces, so two bodies
/// with similar names never meet; a camera added to the catalog later
/// keeps its identity, as its files' names map to the same clean names.
pub fn canonical_names(make: &str, model: &str) -> (String, String) {
    catalog_names()
        .one
        .get(&folded(make, model))
        .cloned()
        .unwrap_or_else(|| (make.trim().to_string(), model.trim().to_string()))
}

/// Every other make and model the catalog gives the camera named `make` /
/// `model` (`canonical_names`), so the editor finds an entry saved under
/// any of them as the engine does (`CameraIdentity::also_named`).
fn other_names(make: &str, model: &str) -> Vec<CameraName> {
    catalog_names()
        .all
        .get(&folded(make, model))
        .map(|names| {
            names
                .iter()
                .filter(|name| !(same(&name.make, make) && same(&name.model, model)))
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

fn folded(make: &str, model: &str) -> (String, String) {
    (make.trim().to_lowercase(), model.trim().to_lowercase())
}

struct CatalogNames {
    /// A file's make and model (folded) → the camera's one name.
    one: HashMap<(String, String), (String, String)>,
    /// The camera's one name (folded) → the file names giving it.
    all: HashMap<(String, String), Vec<CameraName>>,
}

fn catalog_names() -> &'static CatalogNames {
    static CATALOG: OnceLock<CatalogNames> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let mut one = HashMap::new();
        // The decoder looks a file's names up with mode "" first, then by
        // its own mode: the "" entry names a file as the decoder does; a
        // name only some modes list takes the first mode's in sorted order,
        // the same every run.
        let mut cameras: Vec<_> = rawler::global_loader().get_cameras().iter().collect();
        cameras.sort_by(|a, b| a.0.cmp(b.0));
        for ((make, model, _mode), camera) in cameras {
            if camera.clean_make.trim().is_empty() && camera.clean_model.trim().is_empty() {
                continue;
            }
            one.entry(folded(make, model)).or_insert_with(|| {
                (
                    camera.clean_make.trim().to_string(),
                    camera.clean_model.trim().to_string(),
                )
            });
        }
        let mut all: HashMap<(String, String), Vec<CameraName>> = HashMap::new();
        for ((make, model), (one_make, one_model)) in &one {
            let names = all.entry(folded(one_make, one_model)).or_default();
            names.push(CameraName {
                make: make.clone(),
                model: model.clone(),
            });
        }
        for names in all.values_mut() {
            names.sort_by(|a, b| (&a.make, &a.model).cmp(&(&b.make, &b.model)));
        }
        CatalogNames { one, all }
    })
}

/// A digest of the decoder's camera naming data (every catalogue entry's
/// file names, mode and clean names): the camera names a file gives through
/// the decoder change with it, so names kept from another catalogue
/// (`camera_store`) are not used.
pub(crate) fn naming_fingerprint() -> &'static str {
    static DIGEST: OnceLock<String> = OnceLock::new();
    DIGEST.get_or_init(|| {
        let mut cameras: Vec<_> = rawler::global_loader()
            .get_cameras()
            .iter()
            .map(|((make, model, mode), camera)| {
                [
                    make.as_str(),
                    model.as_str(),
                    mode.as_str(),
                    camera.clean_make.as_str(),
                    camera.clean_model.as_str(),
                ]
                .join("\u{1f}")
            })
            .collect();
        cameras.sort_unstable();
        rawtonic_fs::sha256_hex(cameras.join("\u{1e}"))
    })
}

/// A camera's make and model as a file or a settings entry names it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CameraName {
    pub make: String,
    pub model: String,
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
    /// Its camera is `camera`: the same one by their names for all files
    /// (`canonical_names`, so a default saved under names a file gave
    /// before, e.g. an uncatalogued DNG's tags, still applies).
    fn is_for(&self, camera: &CameraIdentity) -> bool {
        let (make, model) = canonical_names(&self.make, &self.model);
        same(&make, &camera.make) && same(&model, &camera.model)
    }

    /// Saved under the very names the photo's file gives its camera.
    fn is_named_as(&self, camera: &CameraIdentity) -> bool {
        let (make, model) = camera.file_names();
        same(&self.make, make) && same(&self.model, model)
    }

    fn level_matches(&self, serial: Option<&str>) -> bool {
        match serial {
            Some(serial) => self
                .serial
                .as_deref()
                .is_some_and(|s| s.trim() == serial.trim()),
            None => self.serial.is_none(),
        }
    }
}

/// The camera's (`serial` some: its body's) entry: the one saved under
/// the names the photo's file gives first (two entries saved from its
/// RAW and its JPEG before they shared an identity each keep theirs),
/// else the first saved for the camera under any of its names.
fn entry_for<'a>(
    cameras: &'a [CameraDefaults],
    camera: &CameraIdentity,
    serial: Option<&str>,
) -> Option<&'a CameraDefaults> {
    let mut matching = cameras
        .iter()
        .filter(|entry| entry.level_matches(serial) && entry.is_for(camera));
    let first = matching.next()?;
    if first.is_named_as(camera) {
        return Some(first);
    }
    Some(
        matching
            .find(|entry| entry.is_named_as(camera))
            .unwrap_or(first),
    )
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
            && let Some(body) = entry_for(&defaults.cameras, camera, Some(serial))
        {
            chain.push((BODY, &body.settings, body.inherit));
        }
        if let Some(model) = entry_for(&defaults.cameras, camera, None) {
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
    // A level without its own tone version holds Shadows as the amount it
    // renders (version 20's slider value, as carried settings do; every
    // default saved before version 21 is that already).
    let mut shadows_amount = false;
    for (level, settings, _) in chain.into_iter().rev() {
        resolved.levels.push(level.to_string());
        for (key, value) in settings {
            if !NEVER_DEFAULTED.contains(&key.as_str()) {
                resolved.settings.insert(key.clone(), value.clone());
            }
        }
        if settings.contains_key("shadows") {
            shadows_amount = !settings.contains_key(crate::image_processing::TONE_VERSION_KEY);
        }
    }
    if shadows_amount {
        // As the new edit's tone version (or the one a default names) renders it.
        let version = resolved
            .settings
            .get(crate::image_processing::TONE_VERSION_KEY)
            .cloned()
            .unwrap_or(serde_json::json!(crate::image_processing::TONE_VERSION));
        let tone = crate::recipe_versions::ToneVersion::of(&serde_json::json!({
            crate::image_processing::TONE_VERSION_KEY: version.clone()
        }));
        // Stated with the value, so the record of the defaults (the photo's
        // Before) reads it in the response it was expressed for.
        resolved.settings.insert(
            crate::image_processing::TONE_VERSION_KEY.to_string(),
            version,
        );
        if let Some(value) = resolved.settings.get_mut("shadows")
            && let Some(amount) = value.as_f64()
        {
            let expressed = tone.shadows_value(amount);
            if expressed != amount {
                *value = serde_json::json!(expressed);
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

/// The camera of the photograph at `path` (`CameraIdentity`): from a
/// RAW's make and model as the decoder names them (rawler's clean names
/// for a catalogued camera, an uncatalogued DNG's tags) and its serial
/// number, other files' from their EXIF. The file's names are read once
/// per file state (size, modification time, file id) and kept in the app's
/// cache folder (`camera_store`); the identity is made from them each time.
pub fn camera_of(path: &Path) -> Option<CameraIdentity> {
    use crate::camera_store::{self, Stamp};
    type Cache = Mutex<HashMap<PathBuf, (Stamp, Option<CameraIdentity>)>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let stamp = Stamp::of(path)?;
    if let Some((s, camera)) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(path)
        && *s == stamp
    {
        return camera.clone();
    }
    let names = camera_store::get(path, &stamp).or_else(|| {
        let names = file_names_of(path)?;
        // Only names read: a file that could not be read now is read again.
        camera_store::put(path, stamp, names.clone());
        Some(names)
    });
    let camera = names.and_then(|n| identity(&n.make, &n.model, n.serial.as_deref()));
    let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
    if cache.len() > 4096 {
        cache.clear();
    }
    cache.insert(path.to_path_buf(), (stamp, camera.clone()));
    camera
}

fn file_names_of(path: &Path) -> Option<crate::camera_store::FileNames> {
    if crate::formats::is_raw_file(path) {
        // Mapped without read-ahead: only the tags are read.
        rawler::rawsource::RawSource::new_lazy(path)
            .ok()
            .and_then(|source| raw_names(&source))
    } else {
        exif_names(path)
    }
}

/// A RAW's camera from bytes already read.
pub fn camera_of_raw(source: &rawler::rawsource::RawSource) -> Option<CameraIdentity> {
    raw_names(source).and_then(|n| identity(&n.make, &n.model, n.serial.as_deref()))
}

fn raw_names(source: &rawler::rawsource::RawSource) -> Option<crate::camera_store::FileNames> {
    let decoder = rawler::get_decoder(source).ok()?;
    let metadata = decoder
        .raw_metadata(source, &rawler::decoders::RawDecodeParams::default())
        .ok()?;
    Some(crate::camera_store::FileNames {
        make: metadata.make,
        model: metadata.model,
        serial: metadata.exif.serial_number,
    })
}

#[cfg(test)]
fn camera_of_exif(path: &Path) -> Option<CameraIdentity> {
    exif_names(path).and_then(|n| identity(&n.make, &n.model, n.serial.as_deref()))
}

fn exif_names(path: &Path) -> Option<crate::camera_store::FileNames> {
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
    Some(crate::camera_store::FileNames {
        make: text(exif::Tag::Make)?,
        model: text(exif::Tag::Model)?,
        serial: text(exif::Tag::BodySerialNumber),
    })
}

fn identity(make: &str, model: &str, serial: Option<&str>) -> Option<CameraIdentity> {
    let (make, model) = (make.trim(), model.trim());
    if make.is_empty() && model.is_empty() {
        return None;
    }
    let (one_make, one_model) = canonical_names(make, model);
    let differs = |file: &str, one: &str| (file != one).then(|| file.to_string());
    Some(CameraIdentity {
        also_named: other_names(&one_make, &one_model),
        file_make: differs(make, &one_make),
        file_model: differs(model, &one_model),
        make: one_make,
        model: one_model,
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
            ..Default::default()
        }
    }

    /// A default without its own tone version holds Shadows as the amount
    /// (every default saved before tone version 21): a new edit gets the
    /// slider value that renders it, so a saved +50 still renders +50's lift.
    #[test]
    fn a_keyless_defaults_shadows_render_as_they_were_saved() {
        let global = |settings_value: Value| DevelopDefaults {
            raw_only: false,
            global: Some(DefaultSettings {
                name: None,
                settings: settings(settings_value),
            }),
            cameras: vec![],
        };
        let shadows = |defaults: &DevelopDefaults| {
            resolve(defaults, true, None).unwrap().settings["shadows"].as_f64()
        };
        assert_eq!(shadows(&global(json!({"shadows": 61}))), Some(50.0));
        // Stated with it, for the record of the defaults (the photo's Before).
        let resolved = resolve(&global(json!({"shadows": 61})), true, None).unwrap();
        assert_eq!(
            resolved.settings["toneVersion"],
            json!(crate::image_processing::TONE_VERSION)
        );
        assert_eq!(
            shadows(&global(json!({"shadows": 50}))).map(|v| (v * 1e3).round()),
            Some(38_554.0)
        );
        assert_eq!(shadows(&global(json!({"shadows": -40}))), Some(-40.0));
        // A default that names its tone version keeps its values.
        assert_eq!(
            shadows(&global(json!({"shadows": 50, "toneVersion": 20}))),
            Some(50.0)
        );
        assert_eq!(
            shadows(&global(json!({"shadows": 61, "toneVersion": 21}))),
            Some(61.0)
        );
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
            ..Default::default()
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

    fn model_default(make: &str, model: &str, exposure: f64) -> CameraDefaults {
        CameraDefaults {
            make: make.into(),
            model: model.into(),
            serial: None,
            name: None,
            settings: settings(json!({ "exposure": exposure })),
            inherit: false,
        }
    }

    fn exposure_for(
        defaults: &DevelopDefaults,
        is_raw: bool,
        camera: &CameraIdentity,
    ) -> Option<f64> {
        resolve(defaults, is_raw, Some(camera)).and_then(|r| r.settings["exposure"].as_f64())
    }

    /// RWL1: adding a camera to rawler's catalog turns its DNGs' names from
    /// their tags ("NIKON CORPORATION" / "NIKON D810A") into the catalog's
    /// clean names; a camera default saved before keeps applying.
    #[test]
    fn a_dng_of_a_newly_catalogued_camera_keeps_its_saved_default() {
        let defaults = DevelopDefaults {
            raw_only: true,
            global: None,
            cameras: vec![model_default("NIKON CORPORATION", "NIKON D810A", 0.7)],
        };
        let dng = identity("Nikon", "D810A", None).unwrap();
        assert_eq!(exposure_for(&defaults, true, &dng), Some(0.7));
    }

    /// One body is one camera whichever file names it: a JPEG's EXIF names
    /// and its RAW's clean names give the same identity, so a default set
    /// from either applies to both when "RAW files only" is off.
    #[test]
    fn a_jpeg_and_a_raw_of_one_body_are_one_camera() {
        let jpeg = identity("Canon", "Canon EOS R5", Some("012345")).unwrap();
        let raw = identity("Canon", "EOS R5", Some("012345")).unwrap();
        assert_eq!((&jpeg.make, &jpeg.model), (&raw.make, &raw.model));
        assert_eq!(jpeg.file_model.as_deref(), Some("Canon EOS R5"));
        assert_eq!(raw.file_model, None);
        // The editor finds an entry saved under the JPEG's names by them.
        assert!(
            raw.also_named
                .iter()
                .any(|name| name.model == "canon eos r5"),
            "{raw:?}"
        );
        assert_eq!(raw.also_named, jpeg.also_named);
        let nikon = identity("NIKON CORPORATION", "NIKON Z 6_2", None).unwrap();
        assert_eq!(
            (nikon.make.as_str(), nikon.model.as_str()),
            ("Nikon", "Z 6 2")
        );

        let from_jpeg = DevelopDefaults {
            raw_only: false,
            global: None,
            cameras: vec![model_default("Canon", "Canon EOS R5", 0.4)],
        };
        assert_eq!(exposure_for(&from_jpeg, true, &raw), Some(0.4));
        assert_eq!(exposure_for(&from_jpeg, false, &jpeg), Some(0.4));
        // "RAW files only" keeps the JPEG neutral.
        let raw_only = DevelopDefaults {
            raw_only: true,
            ..from_jpeg
        };
        assert_eq!(exposure_for(&raw_only, false, &jpeg), None);
        assert_eq!(exposure_for(&raw_only, true, &raw), Some(0.4));
    }

    /// Exact names only: bodies whose names start alike stay apart, and a
    /// camera the catalog does not know keeps its own names.
    #[test]
    fn two_bodies_with_similar_names_stay_apart() {
        let defaults = DevelopDefaults {
            raw_only: false,
            global: None,
            cameras: vec![
                model_default("Canon", "EOS R5", 0.1),
                model_default("NIKON CORPORATION", "NIKON Z 6", 0.2),
            ],
        };
        for (make, model) in [
            ("Canon", "Canon EOS R5 C"),
            ("Canon", "EOS R5 C"),
            ("Canon", "Canon EOS R6"),
            ("NIKON CORPORATION", "NIKON Z 6_2"),
            ("Nikon", "Z 6 2"),
            ("Apple", "iPhone 15 Pro"),
        ] {
            let camera = identity(make, model, None).unwrap();
            assert_eq!(
                exposure_for(&defaults, true, &camera),
                None,
                "{make} {model}"
            );
        }
        assert_eq!(
            exposure_for(
                &defaults,
                true,
                &identity("NIKON CORPORATION", "NIKON Z 6", None).unwrap()
            ),
            Some(0.2)
        );
        let phone = identity("Apple", "iPhone 15 Pro", None).unwrap();
        assert_eq!(
            (phone.make.as_str(), phone.model.as_str(), phone.file_model),
            ("Apple", "iPhone 15 Pro", None)
        );
    }

    /// Defaults saved for one body from its JPEG (EXIF names) and from its
    /// RAW (clean names) before they shared an identity: each file keeps
    /// the one saved from its kind.
    #[test]
    fn entries_saved_from_a_jpeg_and_a_raw_of_one_body_each_keep_their_files() {
        let defaults = DevelopDefaults {
            raw_only: false,
            global: None,
            cameras: vec![
                model_default("Canon", "Canon EOS R5", 0.4),
                model_default("Canon", "EOS R5", 0.9),
                model_default("NIKON CORPORATION", "NIKON D810A", 0.7),
            ],
        };
        let jpeg = identity("Canon", "Canon EOS R5", None).unwrap();
        let raw = identity("Canon", "EOS R5", None).unwrap();
        assert_eq!(exposure_for(&defaults, false, &jpeg), Some(0.4));
        assert_eq!(exposure_for(&defaults, true, &raw), Some(0.9));
        // A DNG converted from the R5 (its tags are the EXIF names; the
        // decoder gives the clean ones) is the RAW.
        let dng = identity("Canon", "EOS R5", None).unwrap();
        assert_eq!(exposure_for(&defaults, true, &dng), Some(0.9));
    }

    /// Every catalogued camera's clean names are their own identity, and
    /// the names a camera's files carry map to them (ignoring case, as
    /// identities are compared: the catalog spells a few makes both ways).
    #[test]
    fn the_catalogs_clean_names_are_their_own_identity() {
        let folded = |(make, model): (String, String)| (make.to_lowercase(), model.to_lowercase());
        let mut wrong = Vec::new();
        for ((make, model, _), camera) in rawler::global_loader().get_cameras() {
            let clean = folded((
                camera.clean_make.trim().to_string(),
                camera.clean_model.trim().to_string(),
            ));
            for (from, to) in [
                (
                    (camera.clean_make.as_str(), camera.clean_model.as_str()),
                    canonical_names(&camera.clean_make, &camera.clean_model),
                ),
                (
                    (make.as_str(), model.as_str()),
                    canonical_names(make, model),
                ),
            ] {
                if folded(to.clone()) != clean {
                    wrong.push(format!("{from:?} -> {to:?}, not {clean:?}"));
                }
            }
            // The editor's list of the camera's names holds every file name
            // the engine maps to it.
            let one = canonical_names(make, model);
            let camera = identity(&one.0, &one.1, None).unwrap();
            if !(same(make, &camera.make) && same(model, &camera.model))
                && !camera
                    .also_named
                    .iter()
                    .any(|n| same(&n.make, make) && same(&n.model, model))
            {
                wrong.push(format!("{make} / {model} not among {camera:?}'s names"));
            }
        }
        wrong.sort();
        wrong.dedup();
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// The broad check of a catalog sync: every photo under
    /// `RAWTONIC_RAW_FIXTURES` whose camera's identity is not the names its
    /// file gives (a JPEG's EXIF names, an uncatalogued DNG's tags), one
    /// line each, and how many files and cameras were read.
    /// `RAWTONIC_RAW_FIXTURES=… cargo test -p rawtonic-engine -- --ignored --nocapture identities_of_the_fixtures`
    #[test]
    #[ignore = "needs RAWTONIC_RAW_FIXTURES"]
    fn identities_of_the_fixtures() {
        let Some(root) = std::env::var_os("RAWTONIC_RAW_FIXTURES") else {
            println!("SKIP: RAWTONIC_RAW_FIXTURES not set");
            return;
        };
        let mut stack = vec![PathBuf::from(root)];
        let (mut files, mut read) = (0, 0);
        let mut cameras = std::collections::BTreeSet::new();
        let mut changed = std::collections::BTreeSet::new();
        let mut apart = std::collections::BTreeSet::new();
        let mut exif_checked = 0;
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                let known = crate::formats::is_raw_file(&path)
                    || ["jpg", "jpeg", "heic", "heif", "hif", "tif", "tiff"]
                        .iter()
                        .any(|ext| {
                            path.extension()
                                .is_some_and(|e| e.eq_ignore_ascii_case(ext))
                        });
                if !known {
                    continue;
                }
                files += 1;
                let Some(camera) = camera_of(&path) else {
                    continue;
                };
                read += 1;
                let (make, model) = camera.file_names();
                let line = format!("{make} / {model}\t-> {} / {}", camera.make, camera.model);
                cameras.insert((camera.make.clone(), camera.model.clone()));
                if camera.file_make.is_some() || camera.file_model.is_some() {
                    changed.insert(line);
                }
                // A RAW's own EXIF Make/Model tags (what its camera writes
                // into its JPEG/HEIF too), where the EXIF reader reads them:
                // they must give the RAW's identity.
                if crate::formats::is_raw_file(&path)
                    && let Some(exif) = camera_of_exif(&path)
                {
                    exif_checked += 1;
                    if (exif.make.to_lowercase(), exif.model.to_lowercase())
                        != (camera.make.to_lowercase(), camera.model.to_lowercase())
                    {
                        let (make, model) = exif.file_names();
                        apart.insert(format!(
                            "{make} / {model}\t-> {} / {}\tRAW {} / {}",
                            exif.make, exif.model, camera.make, camera.model
                        ));
                    }
                }
            }
        }
        for line in &changed {
            println!("CHANGED\t{line}");
        }
        for line in &apart {
            println!("EXIF-APART\t{line}");
        }
        println!(
            "files {files}, camera read {read}, cameras {}, changed names {}, RAWs with EXIF names {exif_checked}, EXIF names of another identity {}",
            cameras.len(),
            changed.len(),
            apart.len()
        );
    }
}
