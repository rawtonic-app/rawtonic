//! Render-input snapshots (QA; notes: plans/architecture-cleanup-2026-11.md,
//! step 0.3). For one export, every input the GPU render consumes, recorded
//! on the export's own code path ([`FileExporter`](crate::file_export::FileExporter)
//! with [`with_render_inputs`](crate::file_export::FileExporter::with_render_inputs),
//! the CLI's `export --render-inputs`): the decode options, the decoded and
//! patched source pixels, the geometry, every mask bitmap, the uniform blocks,
//! the flags and the `Arc` resources (curve table, Relight surface and
//! patterns, LUT). Recording reads these values and changes none of them.
//!
//! A snapshot is a fast early warning under the end-to-end export hashes of
//! the preservation run, never a replacement for them: what the GPU does
//! with these inputs is not in it. Plain JSON with sorted keys and no paths;
//! hashes are SHA-256.

use std::sync::{Arc, Mutex};

use image::{DynamicImage, GenericImageView};
use rawtonic_fs::sha256_hex;
use serde_json::{Map, Value, json};

use crate::export_processing::ExportSource;
use crate::gpu_processing::RenderOutputPrecision;
use crate::image_processing::PreparedAdjustments;
use crate::lut::Lut;
use crate::mask_generation::MaskBitmap;
use crate::output_color_space::OutputColorSpace;
use rawtonic_engine::image_loader::{AiDenoiseSettings, BaseImageOptions};

/// `value` with the keys of every object sorted.
pub fn sorted(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            Value::Object(
                keys.into_iter()
                    .map(|key| (key.clone(), sorted(&map[key])))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.iter().map(sorted).collect()),
        other => other.clone(),
    }
}

/// SHA-256 of a snapshot's compact JSON with sorted keys.
pub fn snapshot_hash(snapshot: &Value) -> String {
    sha256_hex(sorted(snapshot).to_string().as_bytes())
}

fn image_entry(image: &DynamicImage) -> Value {
    let (width, height) = image.dimensions();
    json!({
        "size": [width, height],
        "color": format!("{:?}", image.color()),
        "sha256": sha256_hex(image.as_bytes()),
    })
}

/// Records one export's render inputs. Shared by the threads that prepare
/// the render (mask layers are generated side by side).
#[derive(Default)]
pub struct RenderInputs {
    entries: Mutex<Map<String, Value>>,
}

impl RenderInputs {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn set(&self, key: &str, value: Value) {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.to_string(), value);
    }

    /// Forgets what was recorded: called when an export starts, so a
    /// recorder reused for several exports holds only the last one's.
    pub fn clear(&self) {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    /// The inputs recorded so far, keys sorted.
    pub fn snapshot(&self) -> Value {
        sorted(&Value::Object(
            self.entries
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
        ))
    }

    /// The options the source was decoded with.
    pub fn decode(&self, is_raw: bool, base: &BaseImageOptions, ai: Option<AiDenoiseSettings>) {
        self.set(
            "decode",
            json!({
                "is_raw": is_raw,
                "highlight_compression": base.highlight_compression,
                "linear_mode": base.linear_mode,
                "color_nr_amount": base.color_nr_amount,
                "sharpening_amount": base.sharpening_amount,
                "apply_to_non_raws": base.apply_to_non_raws,
                "dead_test": format!("{:?}", base.dead_test),
                "frame": base.frame,
                "black": format!("{:?}", base.black),
                "highlights": format!("{:?}", base.highlights),
                "ai_denoise": ai.map(|ai| json!({
                    "version": ai.version,
                    "blend": ai.blend,
                    "noise_scale": ai.noise_scale,
                })),
            }),
        );
    }

    /// The decoded source, its measures and the source with AI patches,
    /// spots and other base retouching composited (`None`: none).
    pub fn source(&self, source: &ExportSource) {
        self.set(
            "source",
            json!({
                "decoded": image_entry(&source.pristine),
                "measures": source.measures.map(|m| format!("{m:?}")),
                "patched": source.patched.as_deref().map(image_entry),
            }),
        );
    }

    /// The geometry of the render's input: `path` names where the warp,
    /// edge-fringe pass and crop ran (`gpu_warp`, `gpu_fringe` or `cpu`),
    /// `window` the crop after the fringe pass on the GPU paths, the warp
    /// parameters and edge-fringe version as `recipe` (on a RAW: `is_raw`)
    /// asks for them,
    /// `transformed` the input pixels where they exist on the CPU.
    #[allow(clippy::too_many_arguments)]
    pub fn geometry(
        &self,
        path: &str,
        recipe: &Value,
        is_raw: bool,
        window: Option<([u32; 4], (f32, f32))>,
        size: (u32, u32),
        crop_offset: (f32, f32),
        color_noise: Option<String>,
        mask_geometry: &rawtonic_engine::mask_generation::PhotoGeometry,
        transformed: Option<&DynamicImage>,
    ) {
        self.set(
            "geometry",
            json!({
                "path": path,
                "warp": serde_json::to_value(
                    rawtonic_engine::image_processing::geometry_params_for(recipe, is_raw),
                )
                .unwrap_or(Value::Null),
                "edge_fringe_version": rawtonic_engine::edge_fringe::version_of(recipe),
                "window": window.map(|(window, offset)| json!({"rect": window, "offset": [offset.0, offset.1]})),
                "size": [size.0, size.1],
                "crop_offset": [crop_offset.0, crop_offset.1],
                "color_noise": color_noise,
                "mask_geometry": format!("{mask_geometry:?}"),
                "transformed": transformed.map(image_entry),
            }),
        );
    }

    /// Mask layer `index`'s bitmap (8-bit pixels and 16-bit coverage).
    pub fn mask_bitmap(&self, index: usize, mask: &MaskBitmap) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let masks = entries
            .entry("mask_bitmaps")
            .or_insert_with(|| Value::Object(Map::new()));
        if let Value::Object(masks) = masks {
            masks.insert(
                format!("{index:02}"),
                json!({
                    "size": [mask.width(), mask.height()],
                    "sha256": sha256_hex(mask.as_raw()),
                    "wide_sha256": mask.wide().map(|wide| sha256_hex(bytemuck::cast_slice(wide))),
                }),
            );
        }
    }

    /// What the GPU render receives: the uniform blocks (after the output
    /// colour space), the flags, the `Arc` resources and the output.
    pub fn render(
        &self,
        adjustments: &PreparedAdjustments,
        lut: Option<&Lut>,
        size: (u32, u32),
        precision: RenderOutputPrecision,
        color_space: OutputColorSpace,
    ) {
        let uniforms = &adjustments.uniforms;
        let masks: Vec<String> = uniforms
            .mask_adjustments
            .iter()
            .take(uniforms.mask_count as usize)
            .map(|mask| sha256_hex(bytemuck::bytes_of(mask)))
            .collect();
        self.set(
            "render",
            json!({
                "all_adjustments_sha256": sha256_hex(bytemuck::bytes_of(uniforms)),
                "global_sha256": sha256_hex(bytemuck::bytes_of(&uniforms.global)),
                "mask_count": uniforms.mask_count,
                "mask_uniforms_sha256": masks,
                "whole_blur_radius": adjustments.whole_blur_radius,
                "specialized_export": adjustments.specialized_export,
                "curve_table_sha256": adjustments.curve_table.as_deref()
                    .map(|table| sha256_hex(bytemuck::cast_slice(table))),
                "relight_surface_sha256": adjustments.relight_surface.as_deref()
                    .map(|surface| sha256_hex(format!("{surface:?}").as_bytes())),
                "relight_patterns": adjustments.relight_patterns.as_deref().map(|patterns| json!({
                    "size": [patterns.width(), patterns.height()],
                    "sha256": sha256_hex(patterns.as_raw()),
                })),
                "lut": lut.map(|lut| json!({
                    "size": lut.size,
                    "sha256": sha256_hex(bytemuck::cast_slice(&lut.data)),
                    "shaper_sha256": lut.shaper.as_ref()
                        .map(|shaper| sha256_hex(format!("{shaper:?}").as_bytes())),
                })),
                "output_size": [size.0, size.1],
                "precision": format!("{precision:?}"),
                "color_space": format!("{color_space:?}"),
            }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reused_recorder_holds_only_the_last_exports_inputs() {
        let inputs = RenderInputs::new();
        let mask = MaskBitmap::from_coverage(2, 1, &[0.0, 1.0]);
        inputs.mask_bitmap(0, &mask);
        inputs.mask_bitmap(1, &mask);
        assert_eq!(
            inputs.snapshot()["mask_bitmaps"].as_object().unwrap().len(),
            2
        );
        inputs.clear();
        inputs.mask_bitmap(0, &mask);
        assert_eq!(
            inputs.snapshot()["mask_bitmaps"].as_object().unwrap().len(),
            1
        );
        inputs.clear();
        assert_eq!(inputs.snapshot(), json!({}));
    }
}
