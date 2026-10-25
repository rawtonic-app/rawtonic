use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Manager};

use crate::app_state::AppState;

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SortCriteria {
    pub key: String,
    pub order: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct FilterCriteria {
    pub rating: i8,
    pub raw_status: String,
    #[serde(default)]
    pub edited_status: Option<String>,
    #[serde(default)]
    pub colors: Vec<String>,
}

impl Default for FilterCriteria {
    fn default() -> Self {
        Self {
            rating: 0,
            raw_status: "all".to_string(),
            edited_status: Some("all".to_string()),
            colors: Vec::new(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct FolderTreeSort {
    pub key: String,
    pub order: String,
}

impl Default for FolderTreeSort {
    fn default() -> Self {
        Self {
            key: "name".to_string(),
            order: "asc".to_string(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct LastFolderState {
    #[serde(default)]
    pub current_folder_path: Option<String>,
    #[serde(default)]
    pub expanded_folders: Vec<String>,
    #[serde(default)]
    pub active_album_id: Option<String>,
    #[serde(default)]
    pub expanded_album_groups: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct MyLens {
    pub maker: String,
    pub model: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct CustomAspectRatio {
    pub width: f64,
    pub height: f64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum PasteMode {
    Merge,
    Replace,
}

pub fn all_available_adjustments() -> HashSet<String> {
    [
        "exposure",
        "brightness",
        "contrast",
        "curves",
        "pointCurves",
        "parametricCurve",
        "curveMode",
        "highlights",
        "shadows",
        "whites",
        "blacks",
        "toneMapper",
        "temperature",
        "tint",
        "saturation",
        "vibrance",
        "hsl",
        "hue",
        "colorGrading",
        "colorCalibration",
        "clarity",
        "structure",
        "dehaze",
        "sharpness",
        "sharpnessThreshold",
        "centré",
        "lumaNoiseReduction",
        "colorNoiseReduction",
        "chromaticAberrationRedCyan",
        "chromaticAberrationBlueYellow",
        "vignetteAmount",
        "vignetteFeather",
        "vignetteMidpoint",
        "vignetteRoundness",
        "grainAmount",
        "grainRoughness",
        "grainSize",
        "lutIntensity",
        "lutName",
        "lutPath",
        "lutSize",
        "lutData",
        "lutIsSceneReferred",
        "glowAmount",
        "halationAmount",
        "flareAmount",
        "crop",
        "aspectRatio",
        "rotation",
        "flipHorizontal",
        "flipVertical",
        "orientationSteps",
        "transformDistortion",
        "transformVertical",
        "transformHorizontal",
        "transformRotate",
        "transformAspect",
        "transformScale",
        "transformXOffset",
        "transformYOffset",
        "masks",
        "lensCorrectionMode",
        "lensMaker",
        "lensModel",
        "lensDistortionAmount",
        "lensVignetteAmount",
        "lensTcaAmount",
        "lensDistortionEnabled",
        "lensTcaEnabled",
        "lensVignetteEnabled",
        "guidedPerspective",
        "diffractionCorrection",
        "diffractionVersion",
        "autoChromaticAberration",
        "cameraCaVersion",
        "autoCaVersion",
        // Rawtonic's own tools. Keep this list equal to the frontend's
        // ADJUSTMENT_GROUPS (src/utils/adjustments.ts); a vitest compares them.
        "baseCurve",
        "cameraProfile",
        "levels",
        "regionCurve",
        "whiteBalance",
        "wbTemperature",
        "wbTint",
        "colorRanges",
        "blackAndWhite",
        "grayMixer",
        "sharpnessFalloff",
        "sharpenRadius",
        "sharpenDetail",
        "sharpenMasking",
        "sharpenVersion",
        "lumaNoiseDetail",
        "lumaNoiseContrast",
        "colorNoiseDetail",
        "colorNoiseSmoothness",
        "colorNoiseVersion",
        "lumaNoiseVersion",
        "toneVersion",
        "clarityVersion",
        "aiDenoise",
        "aiDenoiseVersion",
        "defringePurpleAmount",
        "defringePurpleHueLo",
        "defringePurpleHueHi",
        "defringeGreenAmount",
        "defringeGreenHueLo",
        "defringeGreenHueHi",
        "moireAmount",
        "moirePattern",
        "fringeVersion",
        "defringeVersion",
        "vignetteStyle",
        "vignetteHighlights",
        "grainStructure",
        "grainVersion",
        "relightLights",
        "relightDepthAmount",
        "relightSkinReflections",
        "relightOriginalLight",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

pub fn default_included_adjustments() -> HashSet<String> {
    let mut defaults = all_available_adjustments();

    let off_by_default = [
        "crop",
        "aspectRatio",
        "rotation",
        "flipHorizontal",
        "flipVertical",
        "orientationSteps",
        "transformDistortion",
        "transformVertical",
        "transformHorizontal",
        "transformRotate",
        "transformAspect",
        "transformScale",
        "transformXOffset",
        "transformYOffset",
        "masks",
        "lensCorrectionMode",
        "lensMaker",
        "lensModel",
        "lensDistortionAmount",
        "lensVignetteAmount",
        "lensTcaAmount",
        "lensDistortionEnabled",
        "lensTcaEnabled",
        "lensVignetteEnabled",
        "guidedPerspective",
        "diffractionCorrection",
        "diffractionVersion",
        "autoChromaticAberration",
        "cameraCaVersion",
        "autoCaVersion",
    ];

    for item in off_by_default.iter() {
        defaults.remove(*item);
    }

    defaults
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CopyPasteSettings {
    pub mode: PasteMode,
    #[serde(default = "default_included_adjustments")]
    pub included_adjustments: HashSet<String>,
    #[serde(default)]
    pub known_adjustments: HashSet<String>,
    #[serde(default)]
    pub auto_sync: bool,
}

impl Default for CopyPasteSettings {
    fn default() -> Self {
        Self {
            mode: PasteMode::Merge,
            included_adjustments: default_included_adjustments(),
            known_adjustments: all_available_adjustments(),
            auto_sync: false,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ExportPreset {
    pub id: String,
    pub name: String,
    pub file_format: String,
    pub jpeg_quality: u8,
    pub enable_resize: bool,
    pub resize_mode: String,
    /// Pixels, or megapixels/percent for those modes (fractional megapixels).
    pub resize_value: f64,
    /// The box's other side for the Dimensions mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resize_value2: Option<f64>,
    pub dont_enlarge: bool,
    pub keep_metadata: bool,
    pub strip_gps: bool,
    #[serde(default)]
    pub keep_creator_contact: bool,
    pub filename_template: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence_start: Option<u32>,
    pub enable_watermark: bool,
    pub watermark_path: Option<String>,
    pub watermark_anchor: Option<String>,
    pub watermark_scale: u32,
    pub watermark_spacing: u32,
    pub watermark_opacity: u32,
    /// The Studio's text watermark (kind, text, font, colour, shadow).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watermark_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watermark_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watermark_font: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watermark_color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watermark_shadow: Option<bool>,
    #[serde(default)]
    pub export_masks: Option<bool>,
    #[serde(default)]
    pub preserve_folders: Option<bool>,
    #[serde(default)]
    pub enable_pad: Option<bool>,
    #[serde(default)]
    pub pad_ratio_width: Option<f32>,
    #[serde(default)]
    pub pad_ratio_height: Option<f32>,
    #[serde(default)]
    pub pad_color: Option<String>,
    #[serde(default)]
    pub enable_border: Option<bool>,
    #[serde(default)]
    pub border_basis: Option<String>,
    #[serde(default)]
    pub border_horizontal_percent: Option<f32>,
    #[serde(default)]
    pub border_vertical_percent: Option<f32>,
    #[serde(default)]
    pub border_color: Option<String>,
    #[serde(default)]
    pub last_export_path: Option<String>,
    #[serde(default)]
    pub destination_type: Option<String>,
    #[serde(default)]
    pub subfolder: Option<String>,
    /// 8 or 16; the Studio's recipes keep it (it was dropped on save before).
    #[serde(default)]
    pub tiff_bit_depth: Option<u8>,
    #[serde(default)]
    pub preserve_timestamps: Option<bool>,
    /// The Studio's output sharpening (mode, amount, radius, threshold,
    /// print distance and resolution), kept as the form wrote it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_sharpening: Option<serde_json::Value>,
    /// Output colour space ("srgb", "displayP3", "adobeRgb", "proPhoto").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_space: Option<String>,
}

pub fn default_export_presets() -> Vec<ExportPreset> {
    vec![
        ExportPreset {
            id: "default-hq".to_string(),
            name: "High Quality".to_string(),
            file_format: "jpeg".to_string(),
            jpeg_quality: 95,
            enable_resize: false,
            resize_mode: "longEdge".to_string(),
            resize_value: 2048.0,
            resize_value2: None,
            dont_enlarge: true,
            keep_metadata: true,
            strip_gps: false,
            keep_creator_contact: false,
            filename_template: "{original_filename}".to_string(),
            sequence_start: None,
            enable_watermark: false,
            watermark_path: None,
            watermark_anchor: Some("bottomRight".to_string()),
            watermark_scale: 10,
            watermark_spacing: 5,
            watermark_opacity: 75,
            export_masks: Some(false),
            preserve_folders: Some(false),
            enable_pad: Some(false),
            pad_ratio_width: Some(1.0),
            pad_ratio_height: Some(1.0),
            pad_color: Some("#ffffff".to_string()),
            enable_border: Some(false),
            border_basis: Some("longEdge".to_string()),
            border_horizontal_percent: Some(2.0),
            border_vertical_percent: Some(2.0),
            border_color: Some("#ffffff".to_string()),
            last_export_path: None,
            destination_type: Some("customFolder".to_string()),
            subfolder: Some("".to_string()),
            tiff_bit_depth: None,
            preserve_timestamps: None,
            output_sharpening: None,
            color_space: None,
            watermark_kind: None,
            watermark_text: None,
            watermark_font: None,
            watermark_color: None,
            watermark_shadow: None,
        },
        ExportPreset {
            id: "default-fast".to_string(),
            name: "Fast (Web)".to_string(),
            file_format: "jpeg".to_string(),
            jpeg_quality: 80,
            enable_resize: true,
            resize_mode: "width".to_string(),
            resize_value: 2048.0,
            resize_value2: None,
            dont_enlarge: true,
            keep_metadata: false,
            strip_gps: true,
            keep_creator_contact: false,
            filename_template: "{original_filename}_web".to_string(),
            sequence_start: None,
            enable_watermark: false,
            watermark_path: None,
            watermark_anchor: Some("bottomRight".to_string()),
            watermark_scale: 10,
            watermark_spacing: 5,
            watermark_opacity: 75,
            export_masks: Some(false),
            preserve_folders: Some(false),
            enable_pad: Some(false),
            pad_ratio_width: Some(1.0),
            pad_ratio_height: Some(1.0),
            pad_color: Some("#ffffff".to_string()),
            enable_border: Some(false),
            border_basis: Some("longEdge".to_string()),
            border_horizontal_percent: Some(2.0),
            border_vertical_percent: Some(2.0),
            border_color: Some("#ffffff".to_string()),
            last_export_path: None,
            destination_type: Some("customFolder".to_string()),
            subfolder: Some("".to_string()),
            tiff_bit_depth: None,
            preserve_timestamps: None,
            output_sharpening: None,
            color_space: None,
            watermark_kind: None,
            watermark_text: None,
            watermark_font: None,
            watermark_color: None,
            watermark_shadow: None,
        },
    ]
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct AdjustmentLayout {
    pub section_order: Vec<String>,
    pub hidden_sections: Vec<String>,
    pub open_sections: BTreeMap<String, bool>,
    pub tool_order: HashMap<String, Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hidden_tools: Option<Vec<String>>,
    pub collapsed_tools: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceState {
    pub left_panel_width: u32,
    pub right_panel_width: u32,
    pub left_top_height: u32,
    pub right_top_height: u32,
    pub panel_layout: HashMap<String, Vec<String>>,
    pub active_panels: HashMap<String, Option<String>>,
    pub panel_switcher_placement: HashMap<String, String>,
}

impl Default for WorkspaceState {
    fn default() -> Self {
        let mut panel_layout = HashMap::new();
        let left_top = vec![
            "metadata".to_string(),
            "folderTree".to_string(),
            "export".to_string(),
        ];

        panel_layout.insert("leftTop".to_string(), left_top);
        panel_layout.insert("leftBottom".to_string(), vec![]);

        panel_layout.insert(
            "rightTop".to_string(),
            vec![
                "adjustments".to_string(),
                "crop".to_string(),
                "masks".to_string(),
                "ai".to_string(),
                "presets".to_string(),
            ],
        );
        panel_layout.insert("rightBottom".to_string(), vec![]);

        let mut active_panels = HashMap::new();
        active_panels.insert("leftTop".to_string(), Some("folderTree".to_string()));
        active_panels.insert("leftBottom".to_string(), None);
        active_panels.insert("rightTop".to_string(), Some("adjustments".to_string()));
        active_panels.insert("rightBottom".to_string(), None);

        let mut panel_switcher_placement = HashMap::new();
        panel_switcher_placement.insert("leftTop".to_string(), "bottom".to_string());
        panel_switcher_placement.insert("leftBottom".to_string(), "bottom".to_string());
        panel_switcher_placement.insert("rightTop".to_string(), "right".to_string());
        panel_switcher_placement.insert("rightBottom".to_string(), "right".to_string());

        Self {
            left_panel_width: 320,
            right_panel_width: 320,
            left_top_height: 450,
            right_top_height: 450,
            panel_layout,
            active_panels,
            panel_switcher_placement,
        }
    }
}

pub fn default_linear_raw_mode() -> String {
    "auto".to_string()
}

pub fn default_tagging_shortcuts_option() -> Option<Vec<String>> {
    Some(vec![
        "portrait".to_string(),
        "landscape".to_string(),
        "architecture".to_string(),
        "travel".to_string(),
        "street".to_string(),
        "family".to_string(),
        "nature".to_string(),
        "food".to_string(),
        "event".to_string(),
    ])
}

pub fn default_open_tree_sections() -> Vec<String> {
    vec!["current".to_string()]
}

/// Remembered brush settings (percent, size in image pixels).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct BrushDefaults {
    pub size: Option<f64>,
    pub hardness: Option<f64>,
    pub opacity: Option<f64>,
    pub flow: Option<f64>,
}

/// Open tools and tool order in the Library and Develop tool columns.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ToolLayout {
    pub open: BTreeMap<String, bool>,
    pub order: BTreeMap<String, Vec<String>>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    #[serde(default)]
    pub check_for_updates: Option<bool>,
    #[serde(default)]
    pub update_prereleases: Option<bool>,
    #[serde(default)]
    pub last_update_check: Option<u64>,
    #[serde(default)]
    pub dismissed_update_version: Option<String>,
    /// Offer to send a crash report after Rawtonic quit unexpectedly
    /// (missing: yes; false: Never, and no crash records are kept).
    #[serde(default)]
    pub crash_reports: Option<bool>,
    /// Send anonymous weekly usage statistics (ADR 0017; missing: off).
    #[serde(default)]
    pub usage_stats: Option<bool>,
    /// Rawtonic's own presets and styles in the presets panel and the tools'
    /// preset menus (missing: shown).
    #[serde(default)]
    pub show_builtin_presets: Option<bool>,
    /// Develop: a rating or flag key steps to the next photograph (missing:
    /// off).
    #[serde(default)]
    pub auto_advance: Option<bool>,
    pub last_root_path: Option<String>,
    #[serde(default)]
    pub root_folders: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recent_libraries: Option<Vec<String>>,
    #[serde(default)]
    pub pinned_folders: Vec<String>,
    pub editor_preview_resolution: Option<u32>,
    #[serde(default)]
    pub small_thumbnail_resolution: Option<u32>,
    #[serde(default)]
    pub medium_thumbnail_resolution: Option<u32>,
    #[serde(default)]
    pub enable_zoom_hifi: Option<bool>,
    #[serde(default)]
    pub use_full_dpi_rendering: Option<bool>,
    #[serde(default)]
    pub high_res_zoom_multiplier: Option<f32>,
    #[serde(default)]
    pub enable_live_previews: Option<bool>,
    #[serde(default)]
    pub live_preview_quality: Option<String>,
    /// Capture One-style mask visibility: "always", "drawing", "grayscale" or "never".
    #[serde(default)]
    pub mask_display: Option<String>,
    /// The visible mode the mask toggle restores after "never" or grayscale.
    #[serde(default)]
    pub mask_display_restore: Option<String>,
    /// Mask overlay colour as "#rrggbb" (Capture One's Mask Color; default red).
    #[serde(default)]
    pub mask_color: Option<String>,
    /// Size of the interface's panels and bars: 1.0 (default), 1.12 or 1.25.
    #[serde(default)]
    pub ui_scale: Option<f64>,
    /// Mask overlay opacity in percent, 0–100 (default 50, as in Capture One).
    #[serde(default)]
    pub mask_opacity: Option<u32>,
    /// Capture One's "Draw Mask Inside": new radial masks cover the inside
    /// (default) or, when false, the outside.
    #[serde(default)]
    pub radial_draw_inside: Option<bool>,
    /// Crop grid: "always", "drag" (only while the frame is dragged, as in
    /// Capture One; default) or "off".
    #[serde(default)]
    pub crop_grid: Option<String>,
    /// Darkening outside the crop frame in percent (Lightroom's crop
    /// overlay; default 50).
    #[serde(default)]
    pub crop_shade: Option<u32>,
    /// Tool columns: which tools are open (key "<column>/<tool>") and the
    /// user's tool order per column (missing = default order).
    #[serde(default)]
    pub tool_layout: Option<ToolLayout>,
    /// The brush's last size, hardness, opacity and flow (Capture One keeps
    /// them across photos and launches).
    #[serde(default)]
    pub brush_defaults: Option<BrushDefaults>,
    pub sort_criteria: Option<SortCriteria>,
    pub filter_criteria: Option<FilterCriteria>,
    pub theme: Option<String>,
    #[serde(default)]
    pub font_family: Option<String>,
    pub decorations: Option<bool>,
    #[serde(alias = "comfyuiAddress")]
    pub ai_connector_address: Option<String>,
    pub last_folder_state: Option<LastFolderState>,
    pub ui_visibility: Option<Value>,
    pub enable_ai_tagging: Option<bool>,
    pub tagging_thread_count: Option<u32>,
    #[serde(default = "default_tagging_shortcuts_option")]
    pub tagging_shortcuts: Option<Vec<String>>,
    #[serde(default)]
    pub custom_ai_tags: Option<Vec<String>>,
    #[serde(default)]
    pub ai_tag_count: Option<u32>,
    pub thumbnail_size: Option<String>,
    pub thumbnail_aspect_ratio: Option<String>,
    pub ai_provider: Option<String>,
    #[serde(default = "default_open_tree_sections")]
    pub open_tree_sections: Vec<String>,
    #[serde(default)]
    pub copy_paste_settings: CopyPasteSettings,
    #[serde(default)]
    pub raw_highlight_compression: Option<f32>,
    #[serde(default)]
    pub processing_backend: Option<String>,
    #[serde(default)]
    pub linux_gpu_optimization: Option<bool>,
    /// AI models on a GPU execution provider when one works (None = on).
    #[serde(default)]
    pub ai_gpu: Option<bool>,
    #[serde(default)]
    pub linux_gpu_optimization_migrated_v1: Option<bool>,
    #[serde(default)]
    pub library_view_mode: Option<String>,
    #[serde(default = "default_export_presets")]
    pub export_presets: Vec<ExportPreset>,
    #[serde(default)]
    pub import_metadata_presets: Vec<rawtonic_ingest::metadata::MetadataPreset>,
    #[serde(default)]
    pub my_lenses: Option<Vec<MyLens>>,
    #[serde(default)]
    pub enable_folder_image_counts: Option<bool>,
    #[serde(default)]
    pub display_edit_icon: Option<bool>,
    #[serde(default = "default_linear_raw_mode")]
    pub linear_raw_mode: String,
    #[serde(default)]
    pub enable_xmp_sync: Option<bool>,
    #[serde(default)]
    pub create_xmp_if_missing: Option<bool>,
    #[serde(default)]
    pub is_waveform_visible: Option<bool>,
    #[serde(default)]
    pub waveform_height: Option<u32>,
    #[serde(default)]
    pub active_waveform_channel: Option<String>,
    #[serde(default)]
    pub use_wgpu_renderer: Option<bool>,
    /// Legacy "neutral grey canvas" switch, replaced by `viewer_background`
    /// (true → "medium", see `migrate_viewer_background`); read, never written.
    #[serde(default, skip_serializing)]
    pub editor_neutral_grey_bg: Option<bool>,
    /// Develop viewer background around the photograph: black, veryDark,
    /// dark, medium, light or white (src/utils/viewerDisplay.ts); None: dark.
    #[serde(default)]
    pub viewer_background: Option<String>,
    /// Fit leaves a proof margin around the photograph (X).
    #[serde(default)]
    pub viewer_proof_margin: Option<bool>,
    /// Proof margin size in % of the viewer's shorter side; None: 6.
    #[serde(default)]
    pub viewer_proof_margin_percent: Option<f32>,
    #[serde(default)]
    pub canvas_input_mode: Option<String>,
    #[serde(default)]
    pub zoom_speed_multiplier: Option<f32>,
    #[serde(default)]
    pub zoom_photo_to_pixel_click: Option<bool>,
    #[serde(default)]
    pub keybinds: HashMap<String, Vec<String>>,
    #[serde(default)]
    pub thumbnail_worker_threads: Option<u32>,
    #[serde(default)]
    pub image_cache_size: Option<u32>,
    #[serde(default)]
    pub thumbnail_cache_limit_gb: Option<u32>,
    #[serde(default)]
    pub ai_cache_limit_gb: Option<u32>,
    #[serde(default)]
    pub tonemapper_override_enabled: Option<bool>,
    #[serde(default)]
    pub default_raw_tonemapper: Option<String>,
    #[serde(default)]
    pub default_non_raw_tonemapper: Option<String>,
    #[serde(default)]
    pub enable_focus_mode: Option<bool>,
    #[serde(default)]
    pub enable_tool_focus_mode: Option<bool>,
    #[serde(default)]
    pub folder_icons: Option<HashMap<String, String>>,
    #[serde(default)]
    pub raw_preprocessing_color_nr: Option<f32>,
    #[serde(default)]
    pub raw_preprocessing_sharpening: Option<f32>,
    #[serde(default)]
    pub apply_preprocessing_to_non_raws: Option<bool>,
    #[serde(default)]
    pub use_apple_raw9: Option<bool>,
    #[serde(default)]
    pub exif_overlay: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub folder_tree_sort: Option<FolderTreeSort>,
    #[serde(default)]
    pub library_display_mode: Option<String>,
    #[serde(default)]
    pub grouping: Option<String>,
    #[serde(default)]
    pub require_matching_exif: Option<bool>,
    #[serde(default)]
    pub group_edited_files: Option<bool>,
    #[serde(default, skip_serializing)] // legacy
    #[allow(dead_code)]
    pub group_associated_files: Option<bool>,
    #[serde(default, skip_serializing)] // legacy
    #[allow(dead_code)]
    pub group_preferred_type: Option<String>,
    #[serde(default)]
    pub always_decode_raw_thumbnails: Option<bool>,
    /// Long edge of Standard previews (preview_proxy.rs) in pixels at least;
    /// None or 0: sized to the Develop view.
    #[serde(default)]
    pub standard_preview_size: Option<u32>,
    /// Days after their last use when 1:1 previews are discarded; None: 30
    /// (Lightroom's default), 0: never.
    #[serde(default)]
    pub discard_full_previews_days: Option<u32>,
    /// Most the Standard previews of one previews folder may take, in GB;
    /// None: 10, 0: no limit (preview_proxy::enforce_standard_limit).
    #[serde(default)]
    pub standard_previews_limit_gb: Option<u32>,
    #[serde(default)]
    pub custom_aspect_ratios: Vec<CustomAspectRatio>,
    #[serde(default)]
    pub adjustment_layout: AdjustmentLayout,
    #[serde(default)]
    pub workspace: WorkspaceState,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            check_for_updates: None,
            update_prereleases: None,
            last_update_check: None,
            dismissed_update_version: None,
            crash_reports: None,
            show_builtin_presets: None,
            auto_advance: None,
            usage_stats: None,
            last_root_path: None,
            root_folders: Vec::new(),
            recent_libraries: None,
            pinned_folders: Vec::new(),
            small_thumbnail_resolution: Some(480),
            medium_thumbnail_resolution: Some(1280),
            editor_preview_resolution: Some(1920),
            enable_zoom_hifi: Some(true),
            use_full_dpi_rendering: Some(false),
            enable_live_previews: Some(true),
            live_preview_quality: Some("high".to_string()),
            mask_display: None,
            mask_display_restore: None,
            mask_color: None,
            ui_scale: None,
            mask_opacity: None,
            radial_draw_inside: None,
            crop_grid: None,
            crop_shade: None,
            tool_layout: None,
            brush_defaults: None,
            sort_criteria: None,
            filter_criteria: None,
            theme: Some("dark".to_string()),
            font_family: None,
            decorations: Some(false),
            ai_connector_address: None,
            last_folder_state: None,
            ui_visibility: None,
            enable_ai_tagging: Some(false),
            tagging_thread_count: Some(3),
            tagging_shortcuts: default_tagging_shortcuts_option(),
            custom_ai_tags: Some(Vec::new()),
            ai_tag_count: Some(10),
            thumbnail_size: Some("medium".to_string()),
            thumbnail_aspect_ratio: Some("contain".to_string()),
            ai_provider: Some("cpu".to_string()),
            open_tree_sections: default_open_tree_sections(),
            copy_paste_settings: CopyPasteSettings::default(),
            raw_highlight_compression: Some(2.5),
            processing_backend: Some("auto".to_string()),
            linux_gpu_optimization: Some(false),
            ai_gpu: None,
            linux_gpu_optimization_migrated_v1: Some(true),
            library_view_mode: Some("flat".to_string()),
            export_presets: default_export_presets(),
            import_metadata_presets: Vec::new(),
            my_lenses: Some(Vec::new()),
            high_res_zoom_multiplier: Some(1.0),
            enable_folder_image_counts: Some(false),
            display_edit_icon: Some(true),
            linear_raw_mode: default_linear_raw_mode(),
            enable_xmp_sync: Some(true),
            create_xmp_if_missing: Some(false),
            is_waveform_visible: Some(false),
            waveform_height: Some(220),
            active_waveform_channel: Some("luma".to_string()),
            #[cfg(target_os = "linux")]
            use_wgpu_renderer: Some(false),
            #[cfg(not(target_os = "linux"))]
            use_wgpu_renderer: Some(true),
            editor_neutral_grey_bg: None,
            viewer_background: None,
            viewer_proof_margin: Some(false),
            viewer_proof_margin_percent: None,
            canvas_input_mode: Some("mouse".to_string()),
            zoom_speed_multiplier: Some(1.0),
            zoom_photo_to_pixel_click: Some(false),
            keybinds: HashMap::new(),
            thumbnail_worker_threads: Some(4),
            image_cache_size: Some(5),
            thumbnail_cache_limit_gb: Some(2),
            ai_cache_limit_gb: Some(5),
            tonemapper_override_enabled: Some(false),
            default_raw_tonemapper: Some("agx".to_string()),
            default_non_raw_tonemapper: Some("basic".to_string()),
            enable_focus_mode: Some(false),
            enable_tool_focus_mode: Some(false),
            folder_icons: Some(HashMap::new()),
            raw_preprocessing_color_nr: Some(0.5),
            raw_preprocessing_sharpening: Some(0.35),
            apply_preprocessing_to_non_raws: Some(false),
            use_apple_raw9: Some(false),
            exif_overlay: Some("off".to_string()),
            language: Some("en".to_string()),
            folder_tree_sort: Some(FolderTreeSort::default()),
            library_display_mode: Some("grid".to_string()),
            grouping: Some("off".to_string()),
            require_matching_exif: Some(false),
            group_edited_files: Some(true),
            group_associated_files: Some(false),
            group_preferred_type: Some("raw".to_string()),
            always_decode_raw_thumbnails: Some(false),
            standard_preview_size: None,
            discard_full_previews_days: None,
            standard_previews_limit_gb: None,
            custom_aspect_ratios: Vec::new(),
            adjustment_layout: AdjustmentLayout::default(),
            workspace: WorkspaceState::default(),
        }
    }
}

pub fn get_settings_path(app_handle: &AppHandle) -> Result<PathBuf, String> {
    let settings_dir = app_handle
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?;

    if !settings_dir.exists() {
        fs::create_dir_all(&settings_dir).map_err(|e| e.to_string())?;
    }

    Ok(settings_dir.join("settings.json"))
}

/// The neutral grey canvas switch became the viewer background: a switched-on
/// canvas is the Medium background. Returns whether the settings changed.
fn migrate_viewer_background(settings: &mut AppSettings) -> bool {
    match settings.editor_neutral_grey_bg.take() {
        Some(true) if settings.viewer_background.is_none() => {
            settings.viewer_background = Some("medium".to_string());
            true
        }
        Some(_) => true,
        None => false,
    }
}

/// A settings file that did not parse and could not be kept aside: nothing
/// may replace it in this run (the app then works from fallback settings).
static UNKEPT_SETTINGS: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

/// Refuses to write settings over `path` when it is an unreadable file that
/// could not be kept.
fn may_replace_settings(path: &Path) -> Result<(), String> {
    let unkept = UNKEPT_SETTINGS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if unkept.as_deref() == Some(path) {
        return Err(format!(
            "Settings not saved: {} could not be read or kept aside",
            path.display()
        ));
    }
    Ok(())
}

/// The settings in `path` (defaults when there is none). A file that no longer
/// parses (a hand edit, a value another version wrote) is kept beside it as
/// `settings.unreadable-<digest>.json` before the defaults are used and saved
/// over it; when it cannot be kept, loading fails and no save replaces it.
fn read_settings_file(path: &Path) -> Result<AppSettings, String> {
    let protect = || {
        *UNKEPT_SETTINGS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(path.to_path_buf());
    };
    let content = match fs::read(path) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(AppSettings::default()),
        Err(e) => {
            protect();
            return Err(format!("Settings file could not be read: {e}"));
        }
    };
    let error = match serde_json::from_slice(&content) {
        Ok(settings) => return Ok(settings),
        Err(error) => error,
    };
    // Named by its content: loading the same file again finds it kept.
    let digest = rawtonic_metadata::digest(&content);
    let kept = path.with_file_name(format!("settings.unreadable-{}.json", &digest[..16]));
    let already_kept = fs::read(&kept).is_ok_and(|existing| existing == content);
    if !already_kept && let Err(e) = crate::session_bridge::write_new_output(&kept, &content) {
        protect();
        return Err(format!(
            "Settings file unreadable ({error}) and could not be kept: {e}"
        ));
    }
    log::warn!(
        "Settings file unreadable ({error}); kept as {} and starting from defaults",
        kept.display()
    );
    Ok(AppSettings::default())
}

static SETTINGS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[tauri::command]
pub fn load_settings(app_handle: AppHandle) -> Result<AppSettings, String> {
    let _guard = SETTINGS_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let path = get_settings_path(&app_handle)?;
    let mut settings = read_settings_file(&path)?;

    if settings.ai_provider.as_deref() == Some("cloud") {
        settings.ai_provider = Some("cpu".into());
    }
    let all_current_keys = all_available_adjustments();
    let default_included = default_included_adjustments();
    let mut settings_modified = migrate_viewer_background(&mut settings);

    if settings.root_folders.is_empty()
        && let Some(last) = &settings.last_root_path
    {
        settings.root_folders.push(last.clone());
        settings_modified = true;
    }

    #[cfg(target_os = "linux")]
    if !settings.linux_gpu_optimization_migrated_v1.unwrap_or(false) {
        if settings.linux_gpu_optimization == Some(true) {
            settings.linux_gpu_optimization = Some(false);
        }
        settings.linux_gpu_optimization_migrated_v1 = Some(true);
        settings_modified = true;
    }

    let is_first_migration = settings.copy_paste_settings.known_adjustments.is_empty();

    if is_first_migration {
        settings.copy_paste_settings.included_adjustments = default_included;
        settings.copy_paste_settings.known_adjustments = all_current_keys.clone();
        settings_modified = true;
    } else {
        let new_features: Vec<String> = all_current_keys
            .difference(&settings.copy_paste_settings.known_adjustments)
            .cloned()
            .collect();

        if !new_features.is_empty() {
            for feature in new_features {
                if default_included.contains(&feature) {
                    settings
                        .copy_paste_settings
                        .included_adjustments
                        .insert(feature.clone());
                }
                settings
                    .copy_paste_settings
                    .known_adjustments
                    .insert(feature);
            }
            settings_modified = true;
        }
    }

    if settings_modified && let Ok(json_string) = serde_json::to_string_pretty(&settings) {
        let _ = rawtonic_metadata::atomic_write(&path, json_string);
    }

    Ok(settings)
}

#[tauri::command]
pub fn save_settings(mut settings: AppSettings, app_handle: AppHandle) -> Result<(), String> {
    let _guard = SETTINGS_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let path = get_settings_path(&app_handle)?;
    if settings.ai_provider.as_deref() == Some("cloud") {
        settings.ai_provider = Some("cpu".into());
    }
    write_settings_file(&path, &mut settings)?;

    let state = app_handle.state::<AppState>();
    let cache_size = settings.image_cache_size.unwrap_or(5) as usize;
    state
        .decoded_image_cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .set_capacity(cache_size);
    Ok(())
}

/// Frontend snapshots must never roll back the backend's throttle timestamp,
/// nor the usage-statistics choice, which only `persist_usage_stats` changes.
fn write_settings_file(path: &Path, settings: &mut AppSettings) -> Result<(), String> {
    let saved = read_settings_file(path)?;
    settings.last_update_check = saved.last_update_check;
    settings.usage_stats = saved.usage_stats;
    persist_settings_file(path, settings)
}

fn persist_settings_file(path: &Path, settings: &AppSettings) -> Result<(), String> {
    let json = serde_json::to_vec_pretty(settings).map_err(|e| e.to_string())?;
    may_replace_settings(path)?;
    rawtonic_metadata::atomic_write(path, json).map_err(|e| e.to_string())
}

/// Saves the usage-statistics choice on its own (ADR 0017): the switch
/// reports a failed save instead of showing a choice that is not kept.
pub(crate) fn persist_usage_stats(app: &AppHandle, on: bool) -> Result<(), String> {
    let _guard = SETTINGS_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    persist_usage_stats_at(&get_settings_path(app)?, on)
}

fn persist_usage_stats_at(path: &Path, on: bool) -> Result<(), String> {
    let mut settings = read_settings_file(path)?;
    settings.usage_stats = Some(on);
    persist_settings_file(path, &settings)
}

/// Reserve an attempt before any network access; failed attempts are throttled too.
pub(crate) fn reserve_update_check(
    app: &AppHandle,
    manual: bool,
    now: u64,
) -> Result<Option<AppSettings>, String> {
    let _guard = SETTINGS_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let path = get_settings_path(app)?;
    reserve_update_check_at(&path, manual, now)
}

fn reserve_update_check_at(
    path: &Path,
    manual: bool,
    now: u64,
) -> Result<Option<AppSettings>, String> {
    let mut settings = read_settings_file(path)?;
    crate::update_check::core::when_due(
        settings.check_for_updates.unwrap_or(true),
        settings.last_update_check,
        now,
        manual,
        || {
            settings.last_update_check = Some(now);
            persist_settings_file(path, &settings)?;
            Ok(settings)
        },
    )
    .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_libraries_round_trip_and_distinguish_clear_from_legacy_settings() {
        let settings = AppSettings::default();
        let legacy = serde_json::to_value(&settings).unwrap();
        assert!(legacy.get("recentLibraries").is_none());
        let restored: AppSettings = serde_json::from_value(legacy).unwrap();
        assert!(restored.recent_libraries.is_none());
        let mut settings = restored;
        settings.recent_libraries =
            Some(vec!["/scratch/Session".into(), "/scratch/Catalog".into()]);
        let saved = serde_json::to_vec(&settings).unwrap();
        let loaded: AppSettings = serde_json::from_slice(&saved).unwrap();
        assert_eq!(loaded.recent_libraries, settings.recent_libraries);
        settings.recent_libraries = Some(Vec::new());
        let cleared: AppSettings =
            serde_json::from_slice(&serde_json::to_vec(&settings).unwrap()).unwrap();
        assert_eq!(cleared.recent_libraries, Some(Vec::new()));
    }

    #[test]
    fn an_unreadable_settings_file_is_kept_before_defaults_replace_it() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("settings.json");
        // A field of the wrong type fails the whole file; the app's first
        // settings save (its workspace layout at start-up) then writes the
        // defaults over it.
        let content = r#"{"rootFolders":"/Photos","lastRootPath":"/Photos"}"#;
        fs::write(&path, content).unwrap();
        let settings = read_settings_file(&path).unwrap();
        assert!(settings.root_folders.is_empty());
        let kept: Vec<_> = fs::read_dir(temp.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|p| p != &path)
            .collect();
        assert_eq!(kept.len(), 1, "{kept:?}");
        assert!(
            kept[0]
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("settings.unreadable-")
        );
        assert_eq!(fs::read_to_string(&kept[0]).unwrap(), content);
        // Loaded again before anything was saved (start-up loads twice).
        assert!(read_settings_file(&path).is_ok());
        assert!(may_replace_settings(&path).is_ok());
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 2);

        fs::write(&path, r#"{"lastRootPath":"/Photos"}"#).unwrap();
        assert_eq!(
            read_settings_file(&path).unwrap().last_root_path.as_deref(),
            Some("/Photos")
        );
        assert!(read_settings_file(&temp.path().join("none.json")).is_ok());

        // Not even UTF-8: kept byte for byte as well.
        let garbled = temp.path().join("garbled").join("settings.json");
        fs::create_dir(garbled.parent().unwrap()).unwrap();
        fs::write(&garbled, [0xff, 0xfe, b'{']).unwrap();
        assert!(read_settings_file(&garbled).is_ok());
        let kept: Vec<_> = fs::read_dir(garbled.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|p| p != &garbled)
            .collect();
        assert_eq!(fs::read(&kept[0]).unwrap(), [0xff, 0xfe, b'{']);
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_settings_file_that_cannot_be_kept_is_never_replaced() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("settings.json");
        fs::write(&path, r#"{"rootFolders":"/Photos"}"#).unwrap();
        assert!(may_replace_settings(&path).is_ok());
        // No room beside it for the kept copy.
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o555)).unwrap();
        let read = read_settings_file(&path);
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(read.is_err());
        assert!(may_replace_settings(&path).is_err());
        assert!(may_replace_settings(&temp.path().join("other.json")).is_ok());

        // Nor one in a folder that cannot be searched.
        let closed = temp.path().join("closed");
        fs::create_dir(&closed).unwrap();
        fs::write(closed.join("settings.json"), "{}").unwrap();
        fs::set_permissions(&closed, fs::Permissions::from_mode(0o000)).unwrap();
        let read = read_settings_file(&closed.join("settings.json"));
        fs::set_permissions(&closed, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(read.is_err());
        assert!(may_replace_settings(&closed.join("settings.json")).is_err());

        // A file that cannot be read at all is not replaced either.
        let locked = temp.path().join("locked.json");
        fs::write(&locked, "{}").unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let read = read_settings_file(&locked);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read.is_err());
        assert!(may_replace_settings(&locked).is_err());
    }

    #[test]
    fn neutral_grey_canvas_becomes_the_medium_viewer_background() {
        let mut settings: AppSettings =
            serde_json::from_value(serde_json::json!({ "editorNeutralGreyBg": true })).unwrap();
        assert!(migrate_viewer_background(&mut settings));
        assert_eq!(settings.viewer_background.as_deref(), Some("medium"));
        let saved = serde_json::to_value(&settings).unwrap();
        assert_eq!(saved["viewerBackground"], "medium");
        assert!(saved.get("editorNeutralGreyBg").is_none());

        // A chosen background stays; the switched-off canvas leaves the default.
        let mut chosen: AppSettings = serde_json::from_value(
            serde_json::json!({ "editorNeutralGreyBg": true, "viewerBackground": "white" }),
        )
        .unwrap();
        migrate_viewer_background(&mut chosen);
        assert_eq!(chosen.viewer_background.as_deref(), Some("white"));
        let mut off: AppSettings =
            serde_json::from_value(serde_json::json!({ "editorNeutralGreyBg": false })).unwrap();
        migrate_viewer_background(&mut off);
        assert_eq!(off.viewer_background, None);
        assert!(!migrate_viewer_background(&mut off));
    }

    #[test]
    fn viewer_settings_survive_a_save() {
        let settings: AppSettings = serde_json::from_value(serde_json::json!({
            "viewerBackground": "veryDark", "viewerProofMargin": true, "viewerProofMarginPercent": 10.0
        }))
        .unwrap();
        let saved = serde_json::to_value(&settings).unwrap();
        assert_eq!(saved["viewerBackground"], "veryDark");
        assert_eq!(saved["viewerProofMargin"], true);
        assert_eq!(saved["viewerProofMarginPercent"], 10.0);
    }

    #[test]
    fn export_recipes_keep_their_tiff_depth_colour_space_and_destination_through_a_save() {
        let json = serde_json::json!({
            "id": "print", "name": "Print", "fileFormat": "tiff", "jpegQuality": 90,
            "enableResize": false, "resizeMode": "longEdge", "resizeValue": 3000, "dontEnlarge": true,
            "keepMetadata": true, "stripGps": false, "filenameTemplate": "{original_filename}",
            "enableWatermark": false, "watermarkPath": null, "watermarkAnchor": "bottomRight",
            "watermarkScale": 10, "watermarkSpacing": 5, "watermarkOpacity": 75,
            "tiffBitDepth": 8, "preserveTimestamps": false, "colorSpace": "adobeRgb",
            "destinationType": "sessionOutput", "lastExportPath": "/print"
        });
        let preset: ExportPreset = serde_json::from_value(json).unwrap();
        let back = serde_json::to_value(&preset).unwrap();
        assert_eq!(back["tiffBitDepth"], 8);
        assert_eq!(back["colorSpace"], "adobeRgb");
        assert_eq!(back["preserveTimestamps"], false);
        assert_eq!(back["destinationType"], "sessionOutput");
        assert_eq!(back["lastExportPath"], "/print");
    }

    #[test]
    fn export_recipes_keep_studio_export_settings_through_a_save() {
        let json = serde_json::json!({
            "id": "web", "name": "Web", "fileFormat": "webp", "jpegQuality": 80,
            "enableResize": true, "resizeMode": "megapixels", "resizeValue": 2.5, "dontEnlarge": false,
            "keepMetadata": true, "stripGps": true, "filenameTemplate": "{original_filename}",
            "enableWatermark": true, "watermarkPath": "/logo.png", "watermarkAnchor": "topLeft",
            "watermarkScale": 12, "watermarkSpacing": 3, "watermarkOpacity": 60,
            "exportMasks": true, "preserveFolders": true, "subfolder": "Web",
            "enableBorder": true, "borderBasis": "eachEdge", "borderHorizontalPercent": 3.5,
            "borderVerticalPercent": 1, "borderColor": "#000000",
            "enablePad": true, "padRatioWidth": 4, "padRatioHeight": 5, "padColor": "#eeeeee"
        });
        let preset: ExportPreset = serde_json::from_value(json.clone()).unwrap();
        let back = serde_json::to_value(&preset).unwrap();
        for key in json.as_object().unwrap().keys() {
            let (want, got) = (&json[key], &back[key]);
            match (want.as_f64(), got.as_f64()) {
                (Some(w), Some(g)) => assert!((w - g).abs() < 1e-6, "{key}: {want} vs {got}"),
                _ => assert_eq!(want, got, "{key}"),
            }
        }
    }

    #[test]
    fn attempt_is_persisted_before_fetch_and_failure_does_not_retry_at_next_launch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let initial = AppSettings {
            theme: Some("light".into()),
            ..Default::default()
        };
        rawtonic_metadata::atomic_write(&path, serde_json::to_vec(&initial).unwrap()).unwrap();
        assert!(
            reserve_update_check_at(&path, false, 100)
                .unwrap()
                .is_some()
        );
        let saved = read_settings_file(&path).unwrap();
        assert_eq!(saved.last_update_check, Some(100));
        assert_eq!(saved.theme.as_deref(), Some("light"));
        // No successful response is needed for the reservation to throttle a relaunch.
        assert!(
            reserve_update_check_at(&path, false, 101)
                .unwrap()
                .is_none()
        );
        assert!(
            reserve_update_check_at(&path, false, 86500)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn saving_an_old_frontend_snapshot_keeps_the_last_attempt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let mut stale = AppSettings::default();
        reserve_update_check_at(&path, false, 100).unwrap().unwrap();
        stale.check_for_updates = Some(false);
        write_settings_file(&path, &mut stale).unwrap();
        let saved = read_settings_file(&path).unwrap();
        assert_eq!(saved.last_update_check, Some(100));
        assert_eq!(saved.check_for_updates, Some(false));
    }

    #[test]
    fn the_usage_statistics_choice_is_saved_on_its_own_or_reports_why_not() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let mut initial = AppSettings {
            crash_reports: Some(false),
            ..Default::default()
        };
        write_settings_file(&path, &mut initial).unwrap();
        persist_usage_stats_at(&path, true).unwrap();
        let saved = read_settings_file(&path).unwrap();
        assert_eq!(saved.usage_stats, Some(true));
        assert_eq!(saved.crash_reports, Some(false));
        // A stale frontend snapshot cannot change it back.
        persist_usage_stats_at(&path, false).unwrap();
        let mut stale = AppSettings {
            usage_stats: Some(true),
            ..Default::default()
        };
        write_settings_file(&path, &mut stale).unwrap();
        assert_eq!(read_settings_file(&path).unwrap().usage_stats, Some(false));
        persist_usage_stats_at(&path, true).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
            let refused = persist_usage_stats_at(&path, false);
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
            // Root may write anyway; otherwise the failure is reported.
            let saved = read_settings_file(&path).unwrap();
            assert_eq!(refused.is_err(), saved.usage_stats == Some(true));
        }
    }

    #[test]
    fn opting_out_reserves_no_attempt_but_check_now_can_run() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let initial = AppSettings {
            check_for_updates: Some(false),
            ..Default::default()
        };
        let original = serde_json::to_vec(&initial).unwrap();
        rawtonic_metadata::atomic_write(&path, &original).unwrap();
        assert!(
            reserve_update_check_at(&path, false, 100)
                .unwrap()
                .is_none()
        );
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert!(reserve_update_check_at(&path, true, 100).unwrap().is_some());
    }
}
