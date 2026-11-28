//! Glow, halation and grain in passes of their own (G76/EFX), for display
//! variants on every device since G76/FEW1.
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
//! Built for a compile limit, then made the display path everywhere: with
//! the main pass split at the tone stage (`tone_split`) a fresh NVIDIA
//! driver cache (RTX 3080, Linux) compiles the RAW default display variant
//! in 2.9 s of pieces instead of 4.7 s in one, and the Basic-panel
//! superset in pieces of at most 3.5 s instead of 10.1 s (compile_probe;
//! notes: benchmark-results/few1-2026-11-16): the first frame came 4 s
//! sooner and no first Basic-panel edit waited more than 1.3 s for its
//! variant instead of 8.5 s. Exports without a compile limit keep their own
//! export variants, which hold every effect in one pass (`export_variant`),
//! and their bytes. Exports that render with display variants under a
//! compile limit (WCC7, WCC10) take the same passes; one main pass serves
//! the 8-bit and the float outputs, so no deferred display variant
//! compiles.

use super::*;

#[cfg(test)]
thread_local! {
    /// Tests: whether processors made on this thread render with effect
    /// passes, whatever the compile limit.
    pub(super) static FORCED: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
thread_local! {
    /// Tests: the terms' block shift of drag frames rendered on this thread,
    /// whatever the frame (`terms_block_shift`).
    pub(super) static FORCED_TERMS_SHIFT: std::cell::Cell<Option<u32>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
thread_local! {
    /// Tests: the grain field's block shift of drag frames rendered on this
    /// thread, whatever the frame (`grain_block_shift`).
    pub(super) static FORCED_GRAIN_SHIFT: std::cell::Cell<Option<u32>> =
        const { std::cell::Cell::new(None) };
}

/// Whether processors made now render display variants with effect
/// passes: always, but for tests that compare.
pub(super) fn here() -> bool {
    #[cfg(test)]
    if let Some(forced) = FORCED.with(std::cell::Cell::get).or_else(|| {
        // `RAWTONIC_TEST_EFFECT_PASSES=1`: the whole test suite through the
        // passes.
        std::env::var_os("RAWTONIC_TEST_EFFECT_PASSES").map(|value| value == "1")
    }) {
        return forced;
    }
    true
}

/// A pass's variants: each compiles once, without holding up the others
/// (a frame's finish pass must not wait for the one with grain that
/// compiles ahead).
type Pipelines<K> = Mutex<std::collections::HashMap<K, Arc<PipelineCell>>>;

/// One variant of a pass, and whether a compile not made ahead waits for it
/// (`Wanted`).
#[derive(Default)]
struct PipelineCell {
    pipeline: std::sync::OnceLock<Result<wgpu::ComputePipeline, String>>,
    wanted: Arc<Wanted>,
}

/// The variant's pipeline, compiled by the first caller. A frame that finds
/// it compiling ahead of use (`compile_ahead`) makes that compile one a
/// frame waits for: it no longer gives the small slot away (`small_slot`;
/// a drag's grain field pass waited behind other compiles, G76/WCC5 review).
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
    if let Some(pipeline) = cell.pipeline.get() {
        return pipeline.clone();
    }
    if !compiles_ahead_here() {
        cell.wanted.set();
    }
    let _wanted = WantedHere::new(Arc::clone(&cell.wanted));
    cell.pipeline.get_or_init(create).clone()
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
    /// The regions of a drag's terms pass and grain field pass, in their
    /// blocks (`block_regions`).
    terms_regions: wgpu::Buffer,
    field_regions: wgpu::Buffer,
    /// The finish pass by output (8-bit, deferred): layouts, and variants
    /// by whether they hold grain and by output.
    finish_bgls: [wgpu::BindGroupLayout; 2],
    finish_layouts: [wgpu::PipelineLayout; 2],
    finish_pipelines: Pipelines<(bool, bool, bool)>,
    /// A drag's grain field pass (`GRAIN_FIELD_PASS`): the 8-bit finish
    /// pass's layout with the field it writes.
    field_layout: wgpu::PipelineLayout,
    field_bgl: wgpu::BindGroupLayout,
    field_pipelines: Pipelines<()>,
    /// The grain of a drag's 2 x 2 blocks, a quarter of a tile.
    field_tile: std::sync::OnceLock<HighPrecisionTile>,
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
/// A drag's film grain once per 2 x 2 pixels (G76/GRN3): the grain field
/// pass's output and the finish pass's input (`grain_field_out`,
/// `grain_field`).
pub(super) const GRAIN_FIELD_OUTPUT_BINDING: u32 = 40;
pub(super) const GRAIN_FIELD_BINDING: u32 = 41;

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
    | SHADOWS_V20_FEATURE
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

/// The size of a drag's term blocks (G76/GRN4), as a shift: 4 x 4 pixels
/// (2) when the frame's short side is at least 1080 pixels and no mask
/// layer changes what a term depends on, else 2 x 2 (1).
///
/// A term is the blurred photo through the pixel's Exposure, Brightness
/// and Whites, used where its Glow or Halation amount is above zero. The
/// blurs scale with the frame (`scale`, short side / 1080): halation's, the
/// smaller, has a sigma of 4 px at scale 1 (radius 8, blur.wgsl's sigma =
/// radius / 2), so from there a 4 x 4 block is no coarser than its sigma,
/// as a 2 x 2 block is at half that size. A mask layer with one of those
/// five sliders can change them from one pixel to the next (a hard mask
/// edge); such frames keep GRN3's 2 x 2 blocks. On a Radeon 680M the terms
/// pass is mostly arithmetic and scales with the blocks: 2 x 2 left
/// 1.56 ms of glow's 2.12 ms at 2048 x 1366 (notes:
/// benchmark-results/grn4-2026-11-21).
pub(super) fn terms_block_shift(width: u32, height: u32, adjustments: &AllAdjustments) -> u32 {
    #[cfg(test)]
    if let Some(shift) = FORCED_TERMS_SHIFT.with(std::cell::Cell::get) {
        return shift;
    }
    let local_inputs = adjustments
        .mask_adjustments
        .iter()
        .take(adjustments.mask_count as usize)
        .any(|m| {
            m.exposure != 0.0
                || m.brightness != 0.0
                || m.whites != 0.0
                || m.glow_amount != 0.0
                || m.halation_amount != 0.0
        });
    if width.min(height) >= 1080 && !local_inputs {
        2
    } else {
        1
    }
}

/// The size of the film grain blocks of a drag of another control than a
/// grain control (`DragEffects::half_grain`), as a shift: 4 x 4 pixels (2)
/// when the frame's short side is below 1080 pixels, else 2 x 2 (1)
/// (G76/GRN6). With grain on, a drag of another control renders at the
/// size without grain (GRN1): the default fit view of a Radeon 680M laptop
/// at 1024 x 682, native DPI at 1608 x 1072. There the grain field pass
/// took 0.54 ms of a 0.83 ms grain drag frame at 2 x 2 (notes:
/// benchmark-results/grn5-2026-11-21 §4), and all three effects together
/// were over the +1 ms drag budget. The cells are coarser while such a
/// drag lasts (coordinator decision 2026-11-21); a drag of Grain Amount,
/// Size or Roughness keeps every pixel's grain, and the settled frame and
/// exports are exact.
pub(super) fn grain_block_shift(width: u32, height: u32) -> u32 {
    #[cfg(test)]
    if let Some(shift) = FORCED_GRAIN_SHIFT.with(std::cell::Cell::get) {
        return shift;
    }
    if width.min(height) < 1080 { 2 } else { 1 }
}

/// A drag's pass over blocks (G76/GRN3, GRN4): the tile-local `regions`
/// (x, y, width, height) as the blocks of `1 << shift` x `1 << shift`
/// pixels of absolute coordinates that cover them, at the blocks' index in
/// the tile (the display source's `terms_index`, `block_index`), for the
/// tile at `offset`. Each block's term (or grain) is computed once, at its
/// first pixel in the tile (`terms_pixel`), and every pixel of the block
/// reads it.
pub(super) fn block_regions(regions: &[[u32; 4]], offset: [u32; 2], shift: u32) -> Vec<[u32; 4]> {
    regions
        .iter()
        .map(|&[x, y, width, height]| {
            let block =
                |at: u32, axis: usize| ((at + offset[axis]) >> shift) - (offset[axis] >> shift);
            let (x0, y0) = (block(x, 0), block(y, 1));
            if width == 0 || height == 0 {
                return [x0, y0, 0, 0];
            }
            [
                x0,
                y0,
                block(x + width - 1, 0) - x0 + 1,
                block(y + height - 1, 1) - y0 + 1,
            ]
        })
        .collect()
}

/// `region` (absolute coordinates) grown to whole blocks of `1 << shift`
/// pixels: the pixels a drag's changed pixels change (G76/GRN3).
pub(super) fn whole_blocks(region: Roi, shift: u32) -> Roi {
    let size = 1 << shift;
    let (x, y) = (region.x & !(size - 1), region.y & !(size - 1));
    Roi {
        x,
        y,
        width: (region.x + region.width).next_multiple_of(size) - x,
        height: (region.y + region.height).next_multiple_of(size) - y,
    }
}

/// A tile-local dispatch region (x, y, width, height) of the tile at
/// `offset`, started at the first pixel in the tile of the block its first
/// pixel lies in (`terms_pixel`), so the main pass renders every pixel the
/// passes over the blocks read.
/// Blocks of `1 << shift` pixels; smaller blocks nest in them.
pub(super) fn with_block_starts(region: [u32; 4], offset: [u32; 2], shift: u32) -> [u32; 4] {
    let start = |at: u32, o: u32| {
        let block = (at + o) >> shift;
        (block << shift).max(o) - o
    };
    let (x, y) = (start(region[0], offset[0]), start(region[1], offset[1]));
    [x, y, region[2] + region[0] - x, region[3] + region[1] - y]
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
            entries.push(float_texture_entry(GRAIN_FIELD_BINDING));
            entries
        });
        let mut field_entries = finish_bgls[0].clone();
        field_entries.push(output_entry(
            GRAIN_FIELD_OUTPUT_BINDING,
            wgpu::TextureFormat::Rgba32Float,
        ));
        let field_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Grain Field BGL"),
            entries: &field_entries,
        });
        let field_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Grain Field Pipeline Layout"),
            bind_group_layouts: &[Some(&field_bgl)],
            immediate_size: 0,
        });
        let finish_bgls = finish_bgls.map(|entries| {
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
        let [terms_regions, field_regions] =
            ["Effect Terms Regions Buffer", "Grain Field Regions Buffer"].map(|label| {
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(label),
                    size: ((MAX_RENDER_REGIONS + 1) * 16) as u64,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })
            });
        Self {
            display,
            float_display,
            terms_bgl,
            terms_layout,
            terms_pipelines: Mutex::new(std::collections::HashMap::new()),
            terms_tiles: std::sync::OnceLock::new(),
            terms_regions,
            field_regions,
            finish_bgls,
            finish_layouts,
            finish_pipelines: Mutex::new(std::collections::HashMap::new()),
            field_layout,
            field_bgl,
            field_pipelines: Mutex::new(std::collections::HashMap::new()),
            field_tile: std::sync::OnceLock::new(),
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
                    processor.float_tile("Split Before Tone Tile", TILE_SIZE),
                    processor.float_tile("Split Tone Tile", TILE_SIZE),
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
                processor.float_tile("Glow Terms Tile", TILE_SIZE),
                processor.float_tile("Halation Terms Tile", TILE_SIZE),
            ]
        })
    }

    /// The render regions of a drag's terms pass (`block_regions`).
    pub(super) fn terms_regions(&self) -> &wgpu::Buffer {
        &self.terms_regions
    }

    /// The render regions of a drag's grain field pass, in its blocks
    /// (`grain_block_shift`).
    pub(super) fn field_regions(&self) -> &wgpu::Buffer {
        &self.field_regions
    }

    /// The finish pass's layout into the 8-bit tile or the deferred one.
    pub(super) fn finish_bind_group_layout(&self, deferred: bool) -> &wgpu::BindGroupLayout {
        &self.finish_bgls[usize::from(deferred)]
    }

    /// The finish pass, compiled on first use: a small compile (35.9k
    /// inlined expressions with grain, 6.5k without). `from_field`: a
    /// drag's, which applies the film grain of the grain field pass.
    pub(super) fn finish_pipeline(
        &self,
        device: &wgpu::Device,
        grain: bool,
        deferred: bool,
        from_field: bool,
    ) -> Result<wgpu::ComputePipeline, String> {
        pipeline_of(
            &self.finish_pipelines,
            (grain, deferred, from_field),
            || {
                let label = format!(
                    "Finish pass{}{}{}",
                    if grain { " with grain" } else { "" },
                    if from_field {
                        " from the grain field"
                    } else {
                        ""
                    },
                    if deferred { " (deferred)" } else { "" }
                );
                let mut constants = finish_constants(grain, deferred);
                if from_field {
                    constants.push(("GRAIN_FROM_FIELD", 1.0));
                }
                let display = if deferred {
                    &self.float_display
                } else {
                    &self.display
                };
                small_pipeline(
                    device,
                    display,
                    &constants,
                    &self.finish_layouts[usize::from(deferred)],
                    &label,
                )
            },
        )
    }

    /// A drag's grain field pass (G76/GRN3, GRN6): the film grain of each
    /// 2 x 2 or 4 x 4 block of the stored colour, into the field tile.
    pub(super) fn field_pipeline(
        &self,
        device: &wgpu::Device,
    ) -> Result<wgpu::ComputePipeline, String> {
        pipeline_of(&self.field_pipelines, (), || {
            let mut constants = finish_constants(true, false);
            constants.push(("GRAIN_FIELD_PASS", 1.0));
            small_pipeline(
                device,
                &self.display,
                &constants,
                &self.field_layout,
                "Grain field pass",
            )
        })
    }

    pub(super) fn field_bind_group_layout(&self) -> &wgpu::BindGroupLayout {
        &self.field_bgl
    }

    /// The tile of a drag's grain field: one texel per block (2 x 2 at the
    /// smallest).
    pub(super) fn field_tile(&self, processor: &GpuProcessor) -> &HighPrecisionTile {
        self.field_tile
            .get_or_init(|| processor.float_tile("Grain Field Tile", TILE_SIZE / 2 + 1))
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
            // Ahead of use: a compile a frame waits for (its detail-cache
            // pass, a drag's grain field pass) takes the small slot first.
            let _ahead = AheadCompile::new();
            for layer_sums in [false, true] {
                if let Err(error) = passes.terms_pipeline(&device, key, layer_sums) {
                    log::error!("Effect terms pass {key} unavailable: {error}");
                }
            }
            if let Err(error) = passes.finish_pipeline(&device, true, false, false) {
                log::error!("Finish pass with grain unavailable: {error}");
            }
            // A drag's with film grain (G76/GRN3).
            if let Err(error) = passes
                .finish_pipeline(&device, true, false, true)
                .and_then(|_| passes.field_pipeline(&device))
            {
                log::error!("Drag passes of film grain unavailable: {error}");
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
            .get_or_init(|| processor.float_tile("Main Pass Colour Tile", TILE_SIZE))
    }

    /// The texture a main pass stores its view values in: the tile when
    /// the frame shows a view that needs them (`wanted`), else 1 x 1.
    pub(super) fn views(&self, processor: &GpuProcessor, wanted: bool) -> &HighPrecisionTile {
        if wanted {
            self.views_tile
                .get_or_init(|| processor.float_tile("Main Pass Views Tile", TILE_SIZE))
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

    /// A frame that comes to need a pass compiling ahead of use
    /// (`compile_ahead`), which waits for the small slot behind another
    /// compile, makes it a compile a frame waits for (`pipeline_of`): it
    /// stops giving the slot away (G76/WCC5 review, WCC6).
    #[test]
    fn a_frame_promotes_the_pass_it_waits_for() {
        use std::time::{Duration, Instant};
        let _alone = SMALL_SLOT_TESTS.lock();
        let waiting = || SMALL_SLOT.lock().unwrap().waiting;
        let until = |what: &str, done: &dyn Fn() -> bool| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !done() {
                assert!(Instant::now() < deadline, "{what}");
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        let pipelines: Arc<Pipelines<()>> = Arc::default();
        let held = small_slot();
        // Another compile waits for the slot.
        let other = std::thread::spawn(|| drop(small_slot()));
        until("the other compile waits", &|| waiting() == 1);
        // The pass compiles ahead and waits behind it.
        let (sender, inside) = std::sync::mpsc::channel();
        let ahead = {
            let pipelines = Arc::clone(&pipelines);
            std::thread::spawn(move || {
                let _ahead = AheadCompile::new();
                pipeline_of(&pipelines, (), || {
                    sender.send(()).unwrap();
                    let _slot = small_slot();
                    Err("compiled ahead".to_string())
                })
            })
        };
        inside.recv_timeout(Duration::from_secs(5)).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(waiting(), 1, "a compile made ahead does not count");
        // A frame needs the same pass.
        let frame = {
            let pipelines = Arc::clone(&pipelines);
            std::thread::spawn(move || {
                pipeline_of(&pipelines, (), || Err("compiled again".to_string()))
            })
        };
        until("the frame makes the pass one it waits for", &|| {
            waiting() == 2
        });
        drop(held);
        other.join().unwrap();
        assert_eq!(
            ahead.join().unwrap().err().as_deref(),
            Some("compiled ahead")
        );
        assert_eq!(
            frame.join().unwrap().err().as_deref(),
            Some("compiled ahead")
        );
        assert_eq!(waiting(), 0);
    }

    const SHADER: &str = include_str!("shaders/shader.wgsl");

    /// The terms pass compiles once per kind of tone stage
    /// (`TERMS_FEATURES`): no other feature bit changes its module, with
    /// and without the layers' sums.
    #[test]
    fn block_regions_cover_the_blocks_of_every_pixel() {
        // Tile pixel p of a tile at `offset` reads the block at
        // ((p + offset) >> shift) - (offset >> shift); a region's blocks
        // must hold every such index of its pixels, and no other.
        for (shift, offset) in [1, 2].into_iter().flat_map(|shift| {
            [[0, 0], [1, 0], [0, 1], [7, 13], [128, 255], [2, 3]].map(|offset| (shift, offset))
        }) {
            for region in [
                [0, 0, 1, 1],
                [0, 0, 9, 4],
                [3, 5, 10, 7],
                [1, 2, 2, 1],
                [4, 4, 0, 3],
            ] {
                let [half] = block_regions(&[region], offset, shift)[..] else {
                    panic!("one region");
                };
                let [x, y, w, h] = region;
                if w == 0 || h == 0 {
                    assert_eq!(half[2..], [0, 0]);
                    continue;
                }
                for axis in 0..2 {
                    let (start, len, block_start, block_len) = if axis == 0 {
                        (x, w, half[0], half[2])
                    } else {
                        (y, h, half[1], half[3])
                    };
                    let blocks: std::collections::BTreeSet<u32> = (start..start + len)
                        .map(|p| ((p + offset[axis]) >> shift) - (offset[axis] >> shift))
                        .collect();
                    let expected: std::collections::BTreeSet<u32> =
                        (block_start..block_start + block_len).collect();
                    assert_eq!(blocks, expected, "{region:?} at {offset:?}, axis {axis}");
                    // The display source's `terms_pixel`: the block's first
                    // pixel in the tile, whose block it is.
                    let o = offset[axis];
                    for b in expected {
                        let first = ((b + (o >> shift)) << shift).max(o) - o;
                        assert!(first < start + len);
                        assert_eq!(((first + o) >> shift) - (o >> shift), b);
                        // `with_block_starts` renders that first pixel.
                        let started = with_block_starts(region, offset, shift);
                        let begin = if axis == 0 { started[0] } else { started[1] };
                        assert!(begin <= first);
                    }
                }
            }
        }
    }

    #[test]
    fn drags_of_other_controls_take_grain_per_4x4_below_1080_px() {
        // G76/GRN6: the default fit view and native DPI with grain on
        // (1024 x 682, 1608 x 1072) take 4 x 4 grain blocks; frames whose
        // short side is 1080 or more keep 2 x 2.
        assert_eq!(grain_block_shift(1024, 682), 2);
        assert_eq!(grain_block_shift(1608, 1072), 2);
        assert_eq!(grain_block_shift(682, 1024), 2);
        assert_eq!(grain_block_shift(1920, 1080), 1);
        assert_eq!(grain_block_shift(2048, 1366), 1);
    }

    #[test]
    fn drag_frames_render_whole_blocks_and_their_first_pixels() {
        let roi = |x, y, width, height| Roi {
            x,
            y,
            width,
            height,
        };
        assert_eq!(whole_blocks(roi(3, 4, 2, 1), 1), roi(2, 4, 4, 2));
        assert_eq!(whole_blocks(roi(2, 4, 2, 2), 1), roi(2, 4, 2, 2));
        // 4 x 4 blocks (G76/GRN4).
        assert_eq!(whole_blocks(roi(3, 5, 2, 1), 2), roi(0, 4, 8, 4));
        assert_eq!(whole_blocks(roi(4, 8, 4, 4), 2), roi(4, 8, 4, 4));
        // A region from tile pixel 4 of a tile at 7: absolute 11, its
        // block's first pixel absolute 10, tile pixel 3.
        assert_eq!(with_block_starts([4, 6, 5, 2], [7, 0], 1), [3, 6, 6, 2]);
        // At the tile's start the block's first pixel is outside the tile:
        // the passes take the tile's first pixel (`terms_pixel`).
        assert_eq!(with_block_starts([0, 0, 5, 2], [7, 7], 1), [0, 0, 5, 2]);
        assert_eq!(with_block_starts([2, 1, 5, 2], [0, 0], 1), [2, 0, 5, 3]);
        // 4 x 4: absolute 11's block starts at absolute 8, tile pixel 1.
        assert_eq!(with_block_starts([4, 6, 5, 2], [7, 0], 2), [1, 4, 8, 4]);
        let mut adjustments = AllAdjustments::default();
        assert_eq!(terms_block_shift(2048, 1366, &adjustments), 2);
        assert_eq!(terms_block_shift(1608, 1072, &adjustments), 1);
        // A mask layer that changes a term's inputs keeps 2 x 2; one with
        // other sliders, or beyond the layer count, does not.
        adjustments.mask_count = 1;
        adjustments.mask_adjustments[0].saturation = 20.0;
        adjustments.mask_adjustments[1].exposure = 1.0;
        assert_eq!(terms_block_shift(2048, 1366, &adjustments), 2);
        let sliders: [fn(&mut crate::image_processing::MaskAdjustments); 5] = [
            |m| m.exposure = -0.5,
            |m| m.brightness = 10.0,
            |m| m.whites = -20.0,
            |m| m.glow_amount = 30.0,
            |m| m.halation_amount = 30.0,
        ];
        for set in sliders {
            let mut local = adjustments;
            set(&mut local.mask_adjustments[0]);
            assert_eq!(terms_block_shift(2048, 1366, &local), 1);
        }
    }

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
                for bit in (0..31).map(|bit| 1usize << bit) {
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
