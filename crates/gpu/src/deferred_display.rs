//! Display variants that render exports with float output under a compile
//! limit (G76/WCC10).
//!
//! On AMD's Windows Vulkan driver 8-bit exports render with the display
//! variant the editor compiled (`GpuProcessor::export_display_pipeline`,
//! WCC7). A pipeline's output format is part of what the driver compiles, so
//! 16-bit exports, exports of several outputs and exports of recipes whose
//! curve is sampled into a table (> 16 points) compiled their own export
//! variant there: the WGT16 edit's 16-bit export variant (87.7k inlined
//! expressions) took 116 s and 9.3 GB on a Radeon 680M, with Glow it was
//! refused (153k, over `EXPORT_INLINED_CEILING`), and a long curve took the
//! whole unfolded shader.
//!
//! These exports render with the display variant's code instead, compiled
//! with float output in deferred mode (`DEFER_OUTPUT`): the main pass
//! writes into a float tile, and a small encode pass per output writes it
//! in 8 or 16 bits and its colour space, as every deferred export does. The
//! WGT16 edit's variant is 65.8k inlined (Glow 76.7k), the size of its
//! 8-bit display variant, and compiled in 46.4 s / 3.76 GB (Glow 52.3 s /
//! 4.42 GB) on a Radeon 680M (probe round 18). WGT24 there: 16-bit TIFF
//! exports after editing 3.4–4.5 s. The idle plan compiles it when the photographer's
//! exports are 16-bit (`set_idle_deferred_exports`); a second processor
//! that compiles the same module (the exports') is answered by the driver's
//! pipeline cache (G76/WCC7 probe round 15: 140 ms).
//!
//! Only under a compile limit: elsewhere exports keep their own variants and
//! bytes.

use super::*;

/// The float-output display shader, its layout and the variants compiled
/// from it, shared by a processor and its idle worker.
pub(super) struct DeferredDisplay {
    /// shader.wgsl with float output (`high_precision_shader_source`).
    source: Arc<str>,
    /// Its display source, parsed on first use.
    display: std::sync::OnceLock<Arc<DisplayShader>>,
    /// The whole high-precision shader: what a variant compiles if its
    /// folded module cannot be made.
    shader: wgpu::ShaderModule,
    layout: wgpu::PipelineLayout,
    pipelines: VariantTable<std::sync::OnceLock<wgpu::ComputePipeline>>,
    failed: VariantTable<std::sync::atomic::AtomicBool>,
}

impl DeferredDisplay {
    pub(super) fn new(
        source: &str,
        shader: wgpu::ShaderModule,
        layout: wgpu::PipelineLayout,
    ) -> Arc<Self> {
        Arc::new(Self {
            source: source.into(),
            display: std::sync::OnceLock::new(),
            shader,
            layout,
            pipelines: variant_table(),
            failed: variant_table(),
        })
    }

    /// The compiled variant `features`, if it is.
    pub(super) fn get(&self, features: usize) -> Option<&wgpu::ComputePipeline> {
        self.pipelines[features].get()
    }

    /// Whether variant `features` failed to compile.
    pub(super) fn failed(&self, features: usize) -> bool {
        self.failed[features].load(std::sync::atomic::Ordering::Acquire)
    }

    /// Compiles variant `features` unless it is compiled: the display
    /// variant `features` (prepared layers, direct curves) with float output
    /// in deferred mode. `Err` when it fails (it is not tried again).
    pub(super) fn compile(
        &self,
        device: &wgpu::Device,
        features: usize,
    ) -> Result<&wgpu::ComputePipeline, String> {
        if let Some(pipeline) = self.get(features) {
            return Ok(pipeline);
        }
        if self.failed(features) {
            return Err(format!("Deferred display variant {features} failed before"));
        }
        let display = self
            .display
            .get_or_init(|| DisplayShader::new(&self.source));
        match create_preview_pipeline(
            device,
            &self.shader,
            Some(display),
            &self.layout,
            features,
            true,
            true,
            false,
            true,
        ) {
            Ok(pipeline) => Ok(self.pipelines[features].get_or_init(|| pipeline)),
            Err(error) => {
                self.failed[features].store(true, std::sync::atomic::Ordering::Release);
                Err(error)
            }
        }
    }
}

/// Whether the photographer keeps a 16-bit export recipe
/// (`set_idle_deferred_exports`).
static SIXTEEN_BIT_RECIPES: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
/// Whether an export rendered with a deferred display variant in this
/// session (`note_deferred_export`).
static DEFERRED_EXPORTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Tells the idle compiles whether the photographer's export recipes
/// include a 16-bit one. Then, or after a 16-bit export (or Edit With)
/// in this session, under a compile limit the variant those exports render
/// with compiles in idle time after the display variants, so an export
/// after editing finds it compiled: one more compile per edit (Radeon
/// 680M, probe round 18: the WGT16 edit's deferred variant 46.4 s / 3.76
/// GB, with Glow 52.3 s / 4.42 GB); without it the first such export
/// compiles it (coordinator decision, G76/WCC10).
pub fn set_idle_deferred_exports(sixteen_bit_recipes: bool) {
    SIXTEEN_BIT_RECIPES.store(sixteen_bit_recipes, std::sync::atomic::Ordering::Relaxed);
}

/// An export rendered with a deferred display variant: from now on the
/// idle plan compiles it.
pub(super) fn note_deferred_export() {
    DEFERRED_EXPORTED.store(true, std::sync::atomic::Ordering::Relaxed);
}

pub(super) fn idle_deferred_exports() -> bool {
    SIXTEEN_BIT_RECIPES.load(std::sync::atomic::Ordering::Relaxed)
        || DEFERRED_EXPORTED.load(std::sync::atomic::Ordering::Relaxed)
}
