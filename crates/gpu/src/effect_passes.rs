//! Glow and halation in a pass of their own under a compile limit
//! (G76/EFX).
//!
//! Each of the two effects carries the tone stage for the blurred photo, so
//! on AMD's Windows Vulkan driver a display variant with both grew from the
//! Basic-panel superset's 66.1k inlined expressions to 88.0k (with grain
//! 117.4k), beyond what a Radeon 680M compiles within the memory bar (about
//! 77k: 4.4 GB at 66.1k, 5.1 GB at 88.0k, 5.95 GB at 117.4k). There the
//! display source splits each effect (`display_variant_ir`
//! `with_effect_terms`): a small terms pass computes, per pixel, what the
//! effect derives from the blurred photo (23.1k with both), and the main
//! pass reads it from two Rgba32Float tiles and applies it to the pixel's
//! colour (66.2k with both). The operations and their order are those of
//! the one pass.
//!
//! Only under a compile limit: elsewhere display variants compile as
//! before. Exports that render with display variants there (WCC7, WCC10)
//! take the same passes.

use super::*;

#[cfg(test)]
thread_local! {
    /// Tests: whether processors made on this thread render with effect
    /// passes, whatever the compile limit.
    pub(super) static FORCED: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
}

/// The terms pass's variants and the tiles it writes, of one processor.
pub(super) struct EffectPasses {
    /// The processor's display shader (`DisplayShader::with_effect_passes`).
    display: Arc<DisplayShader>,
    bgl: wgpu::BindGroupLayout,
    layout: wgpu::PipelineLayout,
    /// By the features the pass's code depends on and whether it reads the
    /// layers' sums.
    pipelines:
        Mutex<std::collections::HashMap<(usize, bool), Result<wgpu::ComputePipeline, String>>>,
    /// Glow's and halation's terms, tile-sized.
    tiles: std::sync::OnceLock<[HighPrecisionTile; 2]>,
}

/// The terms pass's outputs and the main pass's inputs (the display
/// source's `glow_terms_out`, `halation_terms_out`, `glow_terms`,
/// `halation_terms`).
pub(super) const TERMS_OUTPUT_BINDINGS: [u32; 2] = [29, 30];
pub(super) const TERMS_INPUT_BINDINGS: [u32; 2] = [31, 32];

/// The feature bits the terms pass's code depends on: layers (their
/// sliders' sums per pixel), the tone stage's switches and the two effects.
/// Every other bit leaves its module as it is
/// (`the_terms_pass_depends_only_on_its_features`), so edits that change
/// only those keep the compiled pass.
pub(super) const TERMS_FEATURES: usize = 8
    | TONE_PREVIEW_FEATURE
    | WHITES_V12_FEATURE
    | SHADOWS_V13_FEATURE
    | BLACKS_V16_FEATURE
    | TONE_V5_FEATURE
    | GLOW_FEATURE
    | HALATION_FEATURE;

/// The layout entries of the terms the main pass reads.
pub(super) fn terms_input_entries() -> [wgpu::BindGroupLayoutEntry; 2] {
    TERMS_INPUT_BINDINGS.map(|binding| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    })
}

/// The pipeline constants of the terms pass for a frame of `features`.
pub(super) fn terms_constants(features: usize, layer_sums: bool) -> Vec<(&'static str, f64)> {
    let features = features & TERMS_FEATURES;
    let mut constants = variant_constants(features, true, true, layer_sums, false);
    constants.extend(display_effect_constants(features));
    constants.push(("EFFECT_TERMS_PASS", 1.0));
    constants
}

impl EffectPasses {
    /// `main_entries`: the 8-bit main pass's layout entries without the
    /// terms it reads; the pass takes the same resources and writes the
    /// terms.
    pub(super) fn new(
        device: &wgpu::Device,
        main_entries: &[wgpu::BindGroupLayoutEntry],
        display: Arc<DisplayShader>,
    ) -> Self {
        let mut entries = main_entries.to_vec();
        entries.extend(
            TERMS_OUTPUT_BINDINGS.map(|binding| wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::StorageTexture {
                    access: wgpu::StorageTextureAccess::WriteOnly,
                    format: wgpu::TextureFormat::Rgba32Float,
                    view_dimension: wgpu::TextureViewDimension::D2,
                },
                count: None,
            }),
        );
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Effect Terms BGL"),
            entries: &entries,
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Effect Terms Pipeline Layout"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        Self {
            display,
            bgl,
            layout,
            pipelines: Mutex::new(std::collections::HashMap::new()),
            tiles: std::sync::OnceLock::new(),
        }
    }

    pub(super) fn bind_group_layout(&self) -> &wgpu::BindGroupLayout {
        &self.bgl
    }

    /// Whether a frame of `features` has glow or halation, so its main
    /// pass reads terms.
    pub(super) fn wanted(features: usize) -> bool {
        features & (GLOW_FEATURE | HALATION_FEATURE) != 0
    }

    /// The terms pass for a frame of `features`, compiled on first use: a
    /// small compile (23.1k inlined expressions with both effects).
    pub(super) fn pipeline(
        &self,
        device: &wgpu::Device,
        features: usize,
        layer_sums: bool,
    ) -> Result<wgpu::ComputePipeline, String> {
        let key = (features & TERMS_FEATURES, layer_sums);
        let mut pipelines = self.pipelines.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(pipeline) = pipelines.get(&key) {
            return pipeline.clone();
        }
        let label = format!(
            "Effect terms {}{}",
            key.0,
            if layer_sums { " (layer sums)" } else { "" }
        );
        let created = self
            .display
            .variant_module(&terms_constants(features, layer_sums))
            .and_then(|module| {
                let shader = validated(
                    device,
                    &format!("shader '{label}'"),
                    CompileWeight::Small,
                    || {
                        device.create_shader_module(wgpu::ShaderModuleDescriptor {
                            label: Some(&label),
                            source: wgpu::ShaderSource::Naga(std::borrow::Cow::Owned(module)),
                        })
                    },
                )?;
                create_compute_pipeline(
                    device,
                    &wgpu::ComputePipelineDescriptor {
                        label: Some(&label),
                        layout: Some(&self.layout),
                        module: &shader,
                        entry_point: Some("main"),
                        compilation_options: Default::default(),
                        cache: None,
                    },
                    CompileWeight::Small,
                )
            });
        pipelines.insert(key, created.clone());
        created
    }

    /// The tiles the pass writes and the main pass reads.
    pub(super) fn tiles(&self, processor: &GpuProcessor) -> &[HighPrecisionTile; 2] {
        self.tiles.get_or_init(|| {
            [
                processor.float_tile("Glow Terms Tile"),
                processor.float_tile("Halation Terms Tile"),
            ]
        })
    }
}
