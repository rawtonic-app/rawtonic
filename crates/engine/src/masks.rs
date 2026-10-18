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
    /// (`mask_generation::generate_layer_mask`). Absent (every layer before
    /// 2026-10-18): the 8-bit mask, unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask_version: Option<u32>,
    /// Refine edge: radius in pixels of the full-resolution photo within
    /// which the mask's edge follows the photo's (`maskVersion` 2 only).
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
                        && sm.parameters["lines"]
                            .as_array()
                            .is_some_and(|lines| lines.iter().any(|l| l["autoMask"] == true)))
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
