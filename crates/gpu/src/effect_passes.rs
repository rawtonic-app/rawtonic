//! Glow, halation and grain in passes of their own under a compile limit
//! (G76/EFX).
//!
//! Glow and halation each carry the tone stage for the blurred photo, and
//! grain four versions of itself, so on AMD's Windows Vulkan driver a
//! display variant with them grew from the Basic-panel superset's 66.1k
//! inlined expressions to 117.4k, beyond what a Radeon 680M compiles within
//! the memory bar (about 77k). There the display source is split
//! (`display_variant_ir::display_source_with_effect_passes`) and a frame
//! renders in up to three passes:
//!
//! - the terms pass (with glow or halation): per pixel the blurred photo
//!   through exposure and the tone stage, into two Rgba32Float tiles;
//! - the main pass, a display variant: everything else, reading the terms.
//!   It holds no grain. A frame without grain finishes in it, as one pass
//!   does;
//! - the finish pass (with grain, and for exports that encode each output
//!   in a pass of its own): the main pass stores the colour before grain
//!   in an Rgba32Float tile, and this pass adds grain, the clipping
//!   warnings and views, the output encoding and the histogram's image,
//!   into the 8-bit tile or the deferred one.
//!
//! Radeon 680M, probe round 20: one shader with every effect 55.9 s /
//! 5954 MB; the main pass with glow and halation as terms 37.1 s / 4331 MB
//! (the Basic-panel superset alone: 36.7 s / 4329 MB), the terms pass 1.6 s
//! / 539 MB, the finish pass with grain 2.3 s / 458 MB. The operations and
//! their order are those of the one pass, and the values between the
//! passes are stored as 32-bit floats. (A main pass that always left the
//! finish to its pass compiled into 3761 MB, but the stored tile and its
//! read cost every frame 12 ms of 53 on that GPU, G76/WEFX.)
//!
//! Only under a compile limit: elsewhere display variants compile as
//! before. Exports that render with display variants there (WCC7, WCC10)
//! take the same passes; one main pass serves the 8-bit and the float
//! outputs, so no deferred display variant compiles.

use super::*;

#[cfg(test)]
thread_local! {
    /// Tests: whether processors made on this thread render with effect
    /// passes, whatever the compile limit.
    pub(super) static FORCED: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
}

/// Whether processors made now render with effect passes: under a compile
/// limit (`compile_limited`).
pub(super) fn here() -> bool {
    #[cfg(test)]
    if let Some(forced) = FORCED.with(std::cell::Cell::get).or_else(|| {
        // `RAWTONIC_TEST_EFFECT_PASSES=1`: the whole test suite through the
        // passes.
        std::env::var_os("RAWTONIC_TEST_EFFECT_PASSES").map(|value| value == "1")
    }) {
        return forced;
    }
    compile_limited()
}

/// A pass's variants: each compiles once, without holding up the others
/// (a frame's finish pass must not wait for the one with grain that
/// compiles ahead).
type Pipelines<K> = Mutex<
    std::collections::HashMap<K, Arc<std::sync::OnceLock<Result<wgpu::ComputePipeline, String>>>>,
>;

fn pipeline_of<K: std::hash::Hash + Eq>(
    pipelines: &Pipelines<K>,
    key: K,
    create: impl FnOnce() -> Result<wgpu::ComputePipeline, String>,
) -> Result<wgpu::ComputePipeline, String> {
    let cell = Arc::clone(
        pipelines
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(key)
            .or_default(),
    );
    cell.get_or_init(create).clone()
}

/// The passes' variants and the tiles between them, of one processor.
pub(super) struct EffectPasses {
    /// The 8-bit display source with the passes
    /// (`DisplayShader::with_effect_passes`): the terms pass and the finish
    /// pass into the 8-bit tile.
    display: Arc<DisplayShader>,
    /// The same of the high-precision source: the finish pass into the
    /// deferred tile.
    float_display: Arc<DisplayShader>,
    terms_bgl: wgpu::BindGroupLayout,
    terms_layout: wgpu::PipelineLayout,
    /// The terms pass by the features its code depends on and whether it
    /// reads the layers' sums.
    terms_pipelines: Pipelines<(usize, bool)>,
    /// Glow's and halation's terms, tile-sized.
    terms_tiles: std::sync::OnceLock<[HighPrecisionTile; 2]>,
    /// The finish pass by output (8-bit, deferred): layouts, and variants
    /// by whether they hold grain and by output.
    finish_bgls: [wgpu::BindGroupLayout; 2],
    finish_layouts: [wgpu::PipelineLayout; 2],
    finish_pipelines: Pipelines<(bool, bool)>,
    /// The colour a main pass stores for the finish pass.
    colour_tile: std::sync::OnceLock<HighPrecisionTile>,
    /// The view values a main pass stores (colour range view, sharpening
    /// view): made when a frame first shows one; a 1 x 1 texture otherwise,
    /// which the passes leave alone.
    views_tile: std::sync::OnceLock<HighPrecisionTile>,
    no_views: HighPrecisionTile,
    /// What a main pass that finishes its pixels itself has in the stored
    /// colour's place.
    no_colour: HighPrecisionTile,
    /// The terms features whose passes compile ahead (`compile_ahead`).
    ahead: Mutex<std::collections::HashSet<usize>>,
    /// The colours the parts of a split main pass store (`tone_split`).
    split_tiles: std::sync::OnceLock<[HighPrecisionTile; 2]>,
    /// 1 x 1 textures in the split's bindings a pass leaves alone: two to
    /// write, one to read.
    no_split_outputs: [HighPrecisionTile; 2],
    no_split_input: HighPrecisionTile,
}

/// The constants of a main pass: it reads the terms and leaves the finish
/// to its pass (`create_display_pipeline`).
pub(super) const MAIN_CONSTANTS: [(&str, f64); 3] = [
    ("EFFECT_TERMS", 1.0),
    ("FINISH_APART", 1.0),
    // Grain is the finish pass's alone.
    ("ENABLE_GRAIN", 0.0),
];

/// The constant only a main pass has (`MAIN_CONSTANTS`; every display
/// variant has `ENABLE_GRAIN`).
pub(super) const MAIN_PASS_CONSTANT: &str = "FINISH_APART";

/// The terms pass's outputs and the main pass's inputs (the display
/// source's `glow_terms_out`, `halation_terms_out`, `glow_terms`,
/// `halation_terms`).
pub(super) const TERMS_OUTPUT_BINDINGS: [u32; 2] = [29, 30];
pub(super) const TERMS_INPUT_BINDINGS: [u32; 2] = [31, 32];
/// The finish pass's inputs (shader.wgsl's `deferred_texture` and the
/// display source's `finish_views`) and the main pass's view output
/// (`finish_views_out`).
pub(super) const FINISH_COLOUR_BINDING: u32 = 25;
pub(super) const FINISH_VIEWS_BINDING: u32 = 33;
pub(super) const VIEWS_OUTPUT_BINDING: u32 = 34;
/// The colour a main pass stores for the finish pass (`finish_colour_out`).
pub(super) const COLOUR_OUTPUT_BINDING: u32 = 35;

/// The feature bits the terms pass's code depends on: layers (their
/// sliders' sums per pixel) and the tone stage's switches. Every other bit
/// leaves its module as it is
/// (`the_terms_pass_depends_only_on_its_features`), so edits that change
/// only those keep the compiled pass. It always holds both effects: a
/// pixel's amounts decide which run.
pub(super) const TERMS_FEATURES: usize = 8
    | TONE_PREVIEW_FEATURE
    | WHITES_V12_FEATURE
    | SHADOWS_V13_FEATURE
    | BLACKS_V16_FEATURE
    | SHADOWS_V19_FEATURE
    | TONE_V5_FEATURE;

pub(super) fn float_texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

pub(super) fn output_entry(
    binding: u32,
    format: wgpu::TextureFormat,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::StorageTexture {
            access: wgpu::StorageTextureAccess::WriteOnly,
            format,
            view_dimension: wgpu::TextureViewDimension::D2,
        },
        count: None,
    }
}

/// The layout entries a main pass adds to the main shader's: the terms it
/// reads, and the view values and the colour it stores for the finish pass.
pub(super) fn main_pass_entries() -> [wgpu::BindGroupLayoutEntry; 4] {
    [
        float_texture_entry(TERMS_INPUT_BINDINGS[0]),
        float_texture_entry(TERMS_INPUT_BINDINGS[1]),
        output_entry(VIEWS_OUTPUT_BINDING, wgpu::TextureFormat::Rgba32Float),
        output_entry(COLOUR_OUTPUT_BINDING, wgpu::TextureFormat::Rgba32Float),
    ]
}

/// The pipeline constants of the terms pass for a frame of `features`.
pub(super) fn terms_constants(features: usize, layer_sums: bool) -> Vec<(&'static str, f64)> {
    let features = (features & TERMS_FEATURES) | GLOW_FEATURE | HALATION_FEATURE;
    let mut constants = variant_constants(features, true, true, layer_sums, false);
    constants.extend(display_effect_constants(features));
    constants.push(("EFFECT_TERMS_PASS", 1.0));
    constants
}

/// The pipeline constants of the finish pass: with or without grain, into
/// the 8-bit tile or (`deferred`) as the colour `encode_deferred` encodes.
/// The sharpening view's two lines are always in: a frame without the view
/// asks for none.
pub(super) fn finish_constants(grain: bool, deferred: bool) -> Vec<(&'static str, f64)> {
    let mut constants = vec![
        ("FINISH_PASS", 1.0),
        ("ENABLE_EFFECTS", u8::from(grain) as f64),
        ("ENABLE_GRAIN", u8::from(grain) as f64),
        ("SHARPEN_VIEW", 1.0),
    ];
    if deferred {
        constants.push(("DEFER_OUTPUT", 1.0));
    }
    constants
}

impl EffectPasses {
    /// `main_entries`: the 8-bit main pass's layout entries without the
    /// passes' own; the terms pass takes the same resources and writes the
    /// terms.
    pub(super) fn new(
        device: &wgpu::Device,
        main_entries: &[wgpu::BindGroupLayoutEntry],
        display: Arc<DisplayShader>,
        float_display: Arc<DisplayShader>,
    ) -> Self {
        let mut entries = main_entries.to_vec();
        entries.extend(
            TERMS_OUTPUT_BINDINGS
                .map(|binding| output_entry(binding, wgpu::TextureFormat::Rgba32Float)),
        );
        let terms_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Effect Terms BGL"),
            entries: &entries,
        });
        let terms_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Effect Terms Pipeline Layout"),
            bind_group_layouts: &[Some(&terms_bgl)],
            immediate_size: 0,
        });
        // The finish pass: the input (its size), the output, the
        // adjustments, the regions, the stored colour, the film grain's
        // table, the histogram's image and the view values
        // (display_variant_ir `effect_passes_split_the_display_source`).
        let finish_bgls = [
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureFormat::Rgba32Float,
        ]
        .map(|format| {
            let mut entries: Vec<_> = main_entries
                .iter()
                .filter(|entry| matches!(entry.binding, 0 | 2 | 16 | 27))
                .copied()
                .collect();
            entries.push(output_entry(1, format));
            entries.push(output_entry(ANALYTICS_OUTPUT_BINDING, format));
            entries.push(float_texture_entry(FINISH_COLOUR_BINDING));
            entries.push(float_texture_entry(FINISH_VIEWS_BINDING));
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("Finish Pass BGL"),
                entries: &entries,
            })
        });
        let finish_layouts = finish_bgls.each_ref().map(|bgl| {
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Finish Pass Pipeline Layout"),
                bind_group_layouts: &[Some(bgl)],
                immediate_size: 0,
            })
        });
        let one_texel = |label| {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba32Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            HighPrecisionTile { texture, view }
        };
        let no_views = one_texel("No Finish Views");
        let no_colour = one_texel("No Finish Colour");
        let no_split_outputs = ["No Split Output 1", "No Split Output 2"].map(one_texel);
        let no_split_input = one_texel("No Split Input");
        Self {
            display,
            float_display,
            terms_bgl,
            terms_layout,
            terms_pipelines: Mutex::new(std::collections::HashMap::new()),
            terms_tiles: std::sync::OnceLock::new(),
            finish_bgls,
            finish_layouts,
            finish_pipelines: Mutex::new(std::collections::HashMap::new()),
            colour_tile: std::sync::OnceLock::new(),
            views_tile: std::sync::OnceLock::new(),
            no_views,
            no_colour,
            ahead: Mutex::new(std::collections::HashSet::new()),
            split_tiles: std::sync::OnceLock::new(),
            no_split_outputs,
            no_split_input,
        }
    }

    /// The bindings of the tone split (`tone_split`) for one of a frame's
    /// passes: `Some(0)` the part before the tone stage, `Some(1)` the tone
    /// part, `Some(2)` a main pass that reads what they stored, `None` a
    /// pass that reads none of it.
    pub(super) fn split_entries(
        &self,
        processor: &GpuProcessor,
        pass: Option<usize>,
    ) -> [wgpu::BindGroupEntry<'_>; 4] {
        let tiles = pass.map(|_| {
            self.split_tiles.get_or_init(|| {
                [
                    processor.float_tile("Split Before Tone Tile"),
                    processor.float_tile("Split Tone Tile"),
                ]
            })
        });
        let output = |index: usize| match tiles {
            Some(tiles) if pass == Some(index) => &tiles[index].view,
            _ => &self.no_split_outputs[index].view,
        };
        let input = |index: usize| match tiles {
            Some(tiles) if pass == Some(index + 1) => &tiles[index].view,
            _ => &self.no_split_input.view,
        };
        let entry = |binding, view| wgpu::BindGroupEntry {
            binding,
            resource: wgpu::BindingResource::TextureView(view),
        };
        [
            entry(tone_split::OUTPUT_BINDINGS[0], output(0)),
            entry(tone_split::INPUT_BINDINGS[0], input(0)),
            entry(tone_split::OUTPUT_BINDINGS[1], output(1)),
            entry(tone_split::INPUT_BINDINGS[1], input(1)),
        ]
    }

    pub(super) fn terms_bind_group_layout(&self) -> &wgpu::BindGroupLayout {
        &self.terms_bgl
    }

    /// Whether a frame of `features` has glow or halation, so its main
    /// pass reads terms.
    pub(super) fn terms_wanted(features: usize) -> bool {
        features & (GLOW_FEATURE | HALATION_FEATURE) != 0
    }

    /// The terms pass for a frame of `features`, compiled on first use: a
    /// small compile (23.1k inlined expressions).
    pub(super) fn terms_pipeline(
        &self,
        device: &wgpu::Device,
        features: usize,
        layer_sums: bool,
    ) -> Result<wgpu::ComputePipeline, String> {
        let key = (features & TERMS_FEATURES, layer_sums);
        pipeline_of(&self.terms_pipelines, key, || {
            let label = format!(
                "Effect terms {}{}",
                key.0,
                if layer_sums { " (layer sums)" } else { "" }
            );
            small_pipeline(
                device,
                &self.display,
                &terms_constants(features, layer_sums),
                &self.terms_layout,
                &label,
            )
        })
    }

    /// The tiles the terms pass writes and the main pass reads.
    pub(super) fn terms_tiles(&self, processor: &GpuProcessor) -> &[HighPrecisionTile; 2] {
        self.terms_tiles.get_or_init(|| {
            [
                processor.float_tile("Glow Terms Tile"),
                processor.float_tile("Halation Terms Tile"),
            ]
        })
    }

    /// The finish pass's layout into the 8-bit tile or the deferred one.
    pub(super) fn finish_bind_group_layout(&self, deferred: bool) -> &wgpu::BindGroupLayout {
        &self.finish_bgls[usize::from(deferred)]
    }

    /// The finish pass, compiled on first use: a small compile (35.9k
    /// inlined expressions with grain, 6.5k without).
    pub(super) fn finish_pipeline(
        &self,
        device: &wgpu::Device,
        grain: bool,
        deferred: bool,
    ) -> Result<wgpu::ComputePipeline, String> {
        pipeline_of(&self.finish_pipelines, (grain, deferred), || {
            let label = format!(
                "Finish pass{}{}",
                if grain { " with grain" } else { "" },
                if deferred { " (deferred)" } else { "" }
            );
            let display = if deferred {
                &self.float_display
            } else {
                &self.display
            };
            small_pipeline(
                device,
                display,
                &finish_constants(grain, deferred),
                &self.finish_layouts[usize::from(deferred)],
                &label,
            )
        })
    }

    /// After a frame on screen: the passes its photo's next effect edit
    /// takes compile off the render worker, once per kind of tone stage, so
    /// turning on Glow, Halation or Grain does not wait for them (Radeon
    /// 680M: 1.6 s and 2.3 s). Small compiles, as beside an idle helper.
    pub(super) fn compile_ahead(self: &Arc<Self>, device: &Arc<wgpu::Device>, features: usize) {
        // Tests end while such a thread would still compile (NVIDIA's
        // driver then crashed at process exit); they compile on first use.
        if cfg!(test) {
            return;
        }
        let key = features & TERMS_FEATURES;
        if !self
            .ahead
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key)
        {
            return;
        }
        let passes = Arc::clone(self);
        let device = Arc::clone(device);
        rawtonic_runtime::spawn_render_thread("pipeline-compile", move || {
            for layer_sums in [false, true] {
                if let Err(error) = passes.terms_pipeline(&device, key, layer_sums) {
                    log::error!("Effect terms pass {key} unavailable: {error}");
                }
            }
            if let Err(error) = passes.finish_pipeline(&device, true, false) {
                log::error!("Finish pass with grain unavailable: {error}");
            }
        });
    }

    /// A 1 x 1 texture in the stored colour's place.
    pub(super) fn no_colour(&self) -> &wgpu::TextureView {
        &self.no_colour.view
    }

    /// The tile a main pass stores its colour in.
    pub(super) fn colour_tile(&self, processor: &GpuProcessor) -> &HighPrecisionTile {
        self.colour_tile
            .get_or_init(|| processor.float_tile("Main Pass Colour Tile"))
    }

    /// The texture a main pass stores its view values in: the tile when
    /// the frame shows a view that needs them (`wanted`), else 1 x 1.
    pub(super) fn views(&self, processor: &GpuProcessor, wanted: bool) -> &HighPrecisionTile {
        if wanted {
            self.views_tile
                .get_or_init(|| processor.float_tile("Main Pass Views Tile"))
        } else {
            &self.no_views
        }
    }
}

/// A pass's pipeline from the folded module of `display` with `constants`.
/// A pass has no stand-in: without its module it fails.
fn small_pipeline(
    device: &wgpu::Device,
    display: &DisplayShader,
    constants: &[(&str, f64)],
    layout: &wgpu::PipelineLayout,
    label: &str,
) -> Result<wgpu::ComputePipeline, String> {
    let module = display.variant_module(constants)?;
    let shader = validated(
        device,
        &format!("shader '{label}'"),
        CompileWeight::Small,
        || {
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(label),
                source: wgpu::ShaderSource::Naga(std::borrow::Cow::Owned(module)),
            })
        },
    )?;
    create_compute_pipeline(
        device,
        &wgpu::ComputePipelineDescriptor {
            label: Some(label),
            layout: Some(layout),
            module: &shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        },
        CompileWeight::Small,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHADER: &str = include_str!("shaders/shader.wgsl");

    /// The terms pass compiles once per kind of tone stage
    /// (`TERMS_FEATURES`): no other feature bit changes its module, with
    /// and without the layers' sums.
    #[test]
    fn the_terms_pass_depends_only_on_its_features() {
        let shader = DisplayShader::with_effect_passes(SHADER);
        assert!(shader.effect_passes());
        // What the driver would compile: the functions the module keeps,
        // each with its expressions and statements.
        let module = |features: usize, layer_sums: bool| {
            // The constants as a frame of `features` would set them, without
            // `terms_constants`' mask.
            let features = features | GLOW_FEATURE | HALATION_FEATURE;
            let mut constants = variant_constants(features, true, true, layer_sums, false);
            constants.extend(display_effect_constants(features));
            constants.push(("EFFECT_TERMS_PASS", 1.0));
            let module = shader.variant_module(&constants).unwrap();
            module
                .functions
                .iter()
                .map(|(_, function)| function)
                .chain([&module.entry_points[0].function])
                .map(|function| {
                    format!(
                        "{:?} {:?} {:?}",
                        function.name, function.expressions, function.body
                    )
                })
                .collect::<Vec<_>>()
        };
        for layer_sums in [false, true] {
            for base in [0, TERMS_FEATURES] {
                let own = module(base, layer_sums);
                for bit in (0..26).map(|bit| 1usize << bit) {
                    if bit & TERMS_FEATURES == 0 {
                        let other = module(base | bit, layer_sums);
                        assert!(
                            other == own,
                            "feature {bit} on {base}: {} against {} functions",
                            other.len(),
                            own.len()
                        );
                    }
                }
            }
        }
        // Its own bits do change it.
        assert_ne!(module(0, false), module(TERMS_FEATURES, false));
        assert_ne!(module(8, false), module(8, true));
    }

    /// Only a main pass is marked as one: a display variant without the
    /// passes keeps the whole shader as its stand-in when its folded module
    /// cannot be made (`compile_variant_module`).
    #[test]
    fn only_main_passes_have_the_main_pass_constant() {
        assert!(
            MAIN_CONSTANTS
                .iter()
                .any(|(name, _)| *name == MAIN_PASS_CONSTANT)
        );
        let ordinary = [
            &variant_constants(ALL_PREVIEW_FEATURES, true, true, true, true)[..],
            &display_effect_constants(ALL_PREVIEW_FEATURES)[..],
        ]
        .concat();
        assert!(ordinary.iter().all(|(name, _)| *name != MAIN_PASS_CONSTANT));
    }

    #[test]
    fn the_finish_pass_holds_grain_only_when_asked() {
        let shader = DisplayShader::with_effect_passes(SHADER);
        let sizes = [false, true].map(|grain| {
            let module = shader
                .variant_module(&finish_constants(grain, false))
                .unwrap();
            crate::display_variant_ir::inlined_size(&module)
        });
        assert!(sizes[0] * 3 < sizes[1], "{sizes:?}");
        let float = DisplayShader::with_effect_passes(&high_precision_shader_source(SHADER));
        float.variant_module(&finish_constants(true, true)).unwrap();
    }
}
