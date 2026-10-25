//! User presets: storage, import and export.
//! Part of `file_management`, split by concern (ADR 0009).
use crate::preset_converter;
use regex::regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use tauri::AppHandle;
use tauri::Manager;
use tauri::path::BaseDirectory;
use uuid::Uuid;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Preset {
    pub id: String,
    pub name: String,
    pub adjustments: Value,
    #[serde(rename = "includeMasks", skip_serializing_if = "Option::is_none")]
    pub include_masks: Option<bool>,
    #[serde(
        rename = "includeCropTransform",
        skip_serializing_if = "Option::is_none"
    )]
    pub include_crop_transform: Option<bool>,
    #[serde(rename = "presetType", skip_serializing_if = "Option::is_none")]
    pub preset_type: Option<String>,
    /// A preset of one tool's menu: that tool's parameters, kept on update.
    #[serde(rename = "toolKeys", skip_serializing_if = "Option::is_none")]
    pub tool_keys: Option<Vec<String>>,
}

#[derive(Serialize)]
pub(crate) struct ExportPresetFile<'a> {
    pub(crate) creator: &'a str,
    pub(crate) presets: &'a [PresetItem],
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PresetFolder {
    /// Tool menus create these folders; hide them while empty, unlike user folders.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub automatic: Option<bool>,
    pub id: String,
    pub name: String,
    pub children: Vec<Preset>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub enum PresetItem {
    Preset(Preset),
    Folder(PresetFolder),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PresetFile {
    pub presets: Vec<PresetItem>,
}

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PresetImportFailure {
    pub file_name: String,
    pub error: String,
}

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PresetImportResult {
    pub presets: Vec<PresetItem>,
    pub failures: Vec<PresetImportFailure>,
    /// Lightroom presets and Capture One styles whose settings were only
    /// partly translated, so the photographer knows what the import left out.
    pub untranslated: Vec<PresetImportUntranslated>,
}

/// Settings a converted preset file set that Rawtonic did not translate:
/// codes from `preset_converter::untranslated_settings` and
/// `costyle::untranslated_settings`, and the Lightroom profile's name.
#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PresetImportUntranslated {
    pub file_name: String,
    pub settings: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
}

pub(crate) fn get_presets_path(app_handle: &AppHandle) -> Result<std::path::PathBuf, String> {
    let presets_dir = app_handle
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("presets");

    if !presets_dir.exists() {
        fs::create_dir_all(&presets_dir).map_err(|e| e.to_string())?;
    }

    Ok(presets_dir.join("presets.json"))
}

#[tauri::command]
pub fn load_presets(app_handle: AppHandle) -> Result<Vec<PresetItem>, String> {
    let path = get_presets_path(&app_handle)?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(path).map_err(|e| e.to_string())?;
    serde_json::from_str(&content).map_err(|e| e.to_string())
}

/// One of Rawtonic's own presets or styles, shipped read-only with the app
/// (`resources/presets/builtin.json`). Applying one copies its values into
/// the recipe; recipes never refer to it, so a later `version` of a built-in
/// leaves photos developed with an earlier one as they are.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BuiltinPreset {
    pub id: String,
    pub version: u32,
    /// English name; the interface shows its translation by `id`.
    pub name: String,
    /// "tool": the settings of one tool; "style": a whole look.
    pub preset_type: String,
    pub adjustments: Value,
}

#[derive(Deserialize)]
struct BuiltinPresetFile {
    format: u32,
    presets: Vec<BuiltinPreset>,
}

/// The built-in presets file's format this build reads.
const BUILTIN_PRESETS_FORMAT: u32 = 1;

pub(crate) fn parse_builtin_presets(content: &str) -> Result<Vec<BuiltinPreset>, String> {
    let file: BuiltinPresetFile =
        serde_json::from_str(content).map_err(|e| format!("Built-in presets unreadable: {e}"))?;
    if file.format != BUILTIN_PRESETS_FORMAT {
        return Err(format!("Built-in presets have format {}", file.format));
    }
    let mut ids = HashSet::new();
    for preset in &file.presets {
        if !ids.insert(preset.id.as_str()) {
            return Err(format!("Built-in preset {} is listed twice", preset.id));
        }
        if preset.preset_type != "tool" && preset.preset_type != "style" {
            return Err(format!("Built-in preset {} has no known type", preset.id));
        }
        if !preset
            .adjustments
            .as_object()
            .is_some_and(|a| !a.is_empty())
        {
            return Err(format!("Built-in preset {} sets nothing", preset.id));
        }
    }
    Ok(file.presets)
}

/// Rawtonic's own presets and styles from the app's resources, apart from
/// the user's presets (a missing or broken presets.json leaves them).
#[tauri::command]
pub fn load_builtin_presets(app_handle: AppHandle) -> Result<Vec<BuiltinPreset>, String> {
    let path = app_handle
        .path()
        .resolve("resources/presets/builtin.json", BaseDirectory::Resource)
        .map_err(|e| e.to_string())?;
    let content =
        fs::read_to_string(&path).map_err(|e| format!("Built-in presets missing: {e}"))?;
    parse_builtin_presets(&content)
}

#[tauri::command]
pub fn save_presets(presets: Vec<PresetItem>, app_handle: AppHandle) -> Result<(), String> {
    let path = get_presets_path(&app_handle)?;
    let json_string = serde_json::to_string_pretty(&presets).map_err(|e| e.to_string())?;
    rawtonic_metadata::atomic_write(&path, json_string).map_err(|e| e.to_string())
}

pub(crate) fn preset_file_display_name(file_path: &str) -> String {
    Path::new(file_path)
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| file_path.to_string())
}

pub(crate) fn collect_top_level_preset_names(items: &[PresetItem]) -> HashSet<String> {
    items
        .iter()
        .map(|item| match item {
            PresetItem::Preset(p) => p.name.clone(),
            PresetItem::Folder(f) => f.name.clone(),
        })
        .collect()
}

pub(crate) fn parse_preset_file(file_path: &str) -> Result<Vec<PresetItem>, String> {
    parse_preset_file_reporting(file_path).map(|(items, _)| items)
}

/// Parses a preset file and, for Lightroom presets and Capture One styles,
/// what they set that the translation left out (`None` when nothing).
pub(crate) fn parse_preset_file_reporting(
    file_path: &str,
) -> Result<(Vec<PresetItem>, Option<PresetImportUntranslated>), String> {
    let report = |settings: Vec<&'static str>, profile: Option<String>| {
        (!settings.is_empty()).then(|| PresetImportUntranslated {
            file_name: preset_file_display_name(file_path),
            settings,
            profile,
        })
    };
    let lower_path = file_path.to_lowercase();
    if lower_path.ends_with(".costyle") || lower_path.ends_with(".cos") {
        let content = fs::read_to_string(file_path)
            .map_err(|e| format!("Failed to read Capture One style: {}", e))?;
        let stem = Path::new(file_path).file_stem().map_or_else(
            || "Capture One style".to_string(),
            |s| s.to_string_lossy().to_string(),
        );
        let preset = crate::costyle::convert_costyle_to_preset(&content, &stem)?;
        let untranslated = report(crate::costyle::untranslated_settings(&content), None);
        return Ok((vec![PresetItem::Preset(preset)], untranslated));
    }
    let is_legacy = lower_path.ends_with(".xmp") || lower_path.ends_with(".lrtemplate");

    if !is_legacy {
        let content = fs::read_to_string(file_path)
            .map_err(|e| format!("Failed to read preset file: {}", e))?;
        let preset_file: PresetFile = serde_json::from_str(&content)
            .map_err(|e| format!("Failed to parse preset file: {}", e))?;
        return Ok((preset_file.presets, None));
    }

    let content = fs::read_to_string(file_path)
        .map_err(|e| format!("Failed to read legacy preset file: {}", e))?;

    let xmp_content = if lower_path.ends_with(".lrtemplate") {
        if let Some(caps) = regex!(r#"(?s)s.xmp = "(.*)""#).captures(&content) {
            caps.get(1)
                .map(|m| m.as_str().replace(r#"\""#, r#"""#))
                .unwrap_or(content)
        } else {
            content
        }
    } else {
        content
    };

    let converted_preset = preset_converter::convert_xmp_to_preset(&xmp_content)?;
    let (settings, profile) = preset_converter::untranslated_settings(&xmp_content);
    Ok((
        vec![PresetItem::Preset(converted_preset)],
        report(settings, profile),
    ))
}

pub(crate) fn merge_imported_items(
    target: &mut Vec<PresetItem>,
    taken_names: &mut HashSet<String>,
    imported: Vec<PresetItem>,
) {
    for mut imported_item in imported {
        let original_name = match &mut imported_item {
            PresetItem::Preset(p) => {
                p.id = Uuid::new_v4().to_string();
                p.name.clone()
            }
            PresetItem::Folder(f) => {
                f.id = Uuid::new_v4().to_string();
                for child in &mut f.children {
                    child.id = Uuid::new_v4().to_string();
                }
                f.name.clone()
            }
        };

        let mut new_name = original_name.clone();
        let mut counter = 1;
        while taken_names.contains(&new_name) {
            new_name = format!("{} ({})", original_name, counter);
            counter += 1;
        }

        match &mut imported_item {
            PresetItem::Preset(p) => p.name = new_name.clone(),
            PresetItem::Folder(f) => f.name = new_name.clone(),
        }

        taken_names.insert(new_name);
        target.push(imported_item);
    }
}

pub(crate) fn import_preset_file_into_library(
    file_path: &str,
    app_handle: AppHandle,
) -> Result<Vec<PresetItem>, String> {
    let imported = parse_preset_file(file_path)?;

    let mut current_presets = load_presets(app_handle.clone())?;
    let mut taken_names = collect_top_level_preset_names(&current_presets);
    merge_imported_items(&mut current_presets, &mut taken_names, imported);

    save_presets(current_presets.clone(), app_handle)?;
    Ok(current_presets)
}

#[tauri::command]
pub fn handle_import_presets_from_file(
    file_path: String,
    app_handle: AppHandle,
) -> Result<Vec<PresetItem>, String> {
    import_preset_file_into_library(&file_path, app_handle)
}

#[tauri::command]
pub fn handle_import_legacy_presets_from_file(
    file_path: String,
    app_handle: AppHandle,
) -> Result<Vec<PresetItem>, String> {
    import_preset_file_into_library(&file_path, app_handle)
}

#[tauri::command]
pub fn handle_import_presets_from_files(
    file_paths: Vec<String>,
    app_handle: AppHandle,
) -> Result<PresetImportResult, String> {
    let mut current_presets = load_presets(app_handle.clone())?;
    let mut taken_names = collect_top_level_preset_names(&current_presets);

    let mut failures: Vec<PresetImportFailure> = Vec::new();
    let mut untranslated: Vec<PresetImportUntranslated> = Vec::new();
    let mut library_changed = false;

    for file_path in &file_paths {
        match parse_preset_file_reporting(file_path) {
            Ok((imported, report)) => {
                untranslated.extend(report);
                library_changed |= !imported.is_empty();
                merge_imported_items(&mut current_presets, &mut taken_names, imported);
            }
            Err(error) => failures.push(PresetImportFailure {
                file_name: preset_file_display_name(file_path),
                error,
            }),
        }
    }

    if library_changed {
        save_presets(current_presets.clone(), app_handle)?;
    }

    Ok(PresetImportResult {
        presets: current_presets,
        failures,
        untranslated,
    })
}

#[tauri::command]
pub fn handle_export_presets_to_file(
    presets_to_export: Vec<PresetItem>,
    file_path: String,
) -> Result<(), String> {
    let preset_file = ExportPresetFile {
        creator: "Anonymous",
        presets: &presets_to_export,
    };

    let json_string = serde_json::to_string_pretty(&preset_file)
        .map_err(|e| format!("Failed to serialize presets: {}", e))?;
    rawtonic_metadata::atomic_write(Path::new(&file_path), json_string)
        .map_err(|e| format!("Failed to write preset file: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_tool_folder_origin_survives_storage() {
        let items: Vec<PresetItem> = serde_json::from_str(
            r#"[{"folder":{"id":"g","name":"Grain","children":[],"automatic":true}},{"folder":{"id":"u","name":"User","children":[]}}]"#,
        ).unwrap();
        let saved = serde_json::to_value(items).unwrap();
        assert_eq!(saved[0]["folder"]["automatic"], true);
        assert!(saved[1]["folder"].get("automatic").is_none());
    }

    const SHIPPED: &str = include_str!("../../resources/presets/builtin.json");

    #[test]
    fn the_shipped_built_ins_load() {
        let presets = parse_builtin_presets(SHIPPED).unwrap();
        assert!(presets.iter().any(|p| p.preset_type == "style"));
        assert!(presets.iter().any(|p| p.preset_type == "tool"));
        assert!(presets.iter().all(|p| p.version >= 1));
    }

    #[test]
    fn a_tool_preset_keeps_its_parameters_when_saved() {
        let json = r#"[{"preset":{"id":"g","name":"Grain","adjustments":{"grainAmount":30},"presetType":"tool","toolKeys":["grainAmount","grainSize"]}}]"#;
        let items: Vec<PresetItem> = serde_json::from_str(json).unwrap();
        let saved = serde_json::to_value(&items).unwrap();
        assert_eq!(
            saved[0]["preset"]["toolKeys"],
            serde_json::json!(["grainAmount", "grainSize"])
        );
        // Presets without it are written as before.
        let plain: Vec<PresetItem> =
            serde_json::from_str(r#"[{"preset":{"id":"p","name":"P","adjustments":{}}}]"#).unwrap();
        assert!(
            serde_json::to_value(&plain).unwrap()[0]["preset"]
                .get("toolKeys")
                .is_none()
        );
    }

    #[test]
    fn a_broken_built_in_file_is_refused() {
        let one =
            r#"{"id":"a","version":1,"name":"A","presetType":"tool","adjustments":{"contrast":1}}"#;
        assert!(
            parse_builtin_presets(&format!(r#"{{"format":1,"presets":[{one},{one}]}}"#)).is_err()
        );
        assert!(parse_builtin_presets(&format!(r#"{{"format":2,"presets":[{one}]}}"#)).is_err());
        assert!(
            parse_builtin_presets(
                r#"{"format":1,"presets":[{"id":"a","version":1,"name":"A","presetType":"tool","adjustments":{}}]}"#
            )
            .is_err()
        );
        assert_eq!(
            parse_builtin_presets(&format!(r#"{{"format":1,"presets":[{one}]}}"#))
                .unwrap()
                .len(),
            1
        );
    }
}
