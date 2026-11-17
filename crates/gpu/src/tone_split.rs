//! The main pass split at the tone stage under a compile limit (G76/COLD4).
//!
//! On AMD's Windows Vulkan driver a compile's time and memory grow far
//! faster than the shader's size: the Basic-panel superset (70.5k inlined
//! expressions) took 38.6 s and 4.4 GB on a Radeon 680M, so the first edit
//! on a fresh computer waited for it. There each main pass of the effect
//! passes (`effect_passes`) runs after two parts of `main` from the same
//! display source (`display_variant_ir::with_tone_split`):
//!
//! - the part before the tone stage (25.9k: 2.6 s / 449 MB) stores the
//!   colour and the sharpening view's mask in an Rgba32Float tile;
//! - the tone stage and Highlights (13.9k: 3.2 s / 732 MB) read it and
//!   store the colour after them in a second tile;
//! - the main pass (33.3k: 4.5 s / 543 MB) reads that and does the rest
//!   (compile_probe round 21, WR21).
//!
//! The operations and their order are those of one pass, and the values
//! between the passes are stored as 32-bit floats. The parts compile beside
//! their main pass, each on its own thread as a small compile, and are kept
//! by their folded module:
//! a main pass whose parts did not change (an edit after the tone stage)
//! finds them compiled. Wherever display variants render with the effect
//! passes (every device since G76/FEW1): on a fresh NVIDIA driver cache
//! (RTX 3080) the pieces of the RAW default variant compile in 1.3, 0.02 and
//! 1.6 s instead of 4.7 s in one, and display frames at 2560 x 1440 took
//! 0.7 ms less than in one pass (`tone_split_drag_render_time`).

use super::*;

#[cfg(test)]
thread_local! {
    /// Tests: whether processors made on this thread with the effect
    /// passes split their main passes (default: they do).
    pub(super) static FORCED: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
}

/// Whether processors made now with the effect passes split their main
/// passes at the tone stage: always, but for tests that compare.
pub(super) fn here() -> bool {
    #[cfg(test)]
    if let Some(forced) = FORCED.with(std::cell::Cell::get) {
        return forced;
    }
    true
}

/// The tiles the parts write (the part before the tone stage, the tone
/// part) and the tone part and the main pass read (the display source's
/// `split_out1`, `split_out2`, `split_in1`, `split_in2`).
pub(super) const OUTPUT_BINDINGS: [u32; 2] = [36, 38];
pub(super) const INPUT_BINDINGS: [u32; 2] = [37, 39];

/// The constant a main pass of the split has.
pub(super) const MAIN_CONSTANT: (&str, f64) = ("SPLIT_LOAD2", 1.0);

/// The parts' constants, in their order in a frame.
const PART_CONSTANTS: [&[(&str, f64)]; 2] = [
    &[("SPLIT_STORE1", 1.0)],
    &[("SPLIT_LOAD1", 1.0), ("SPLIT_STORE2", 1.0)],
];
const PART_NAMES: [&str; 2] = ["before the tone stage", "tone stage"];

type Part = Arc<std::sync::OnceLock<Result<wgpu::ComputePipeline, String>>>;

/// The parts' pipelines of one display shader.
#[derive(Default)]
pub(crate) struct ToneSplitParts {
    /// By pipeline layout and folded module: each compiles once.
    parts: Mutex<std::collections::HashMap<(wgpu::PipelineLayout, u64), Part>>,
    /// The parts each main pass runs after.
    of_main: Mutex<std::collections::HashMap<wgpu::ComputePipeline, [wgpu::ComputePipeline; 2]>>,
}

/// The layout entries the split adds to the main pass's.
pub(super) fn layout_entries() -> [wgpu::BindGroupLayoutEntry; 4] {
    [
        effect_passes::output_entry(OUTPUT_BINDINGS[0], wgpu::TextureFormat::Rgba32Float),
        effect_passes::float_texture_entry(INPUT_BINDINGS[0]),
        effect_passes::output_entry(OUTPUT_BINDINGS[1], wgpu::TextureFormat::Rgba32Float),
        effect_passes::float_texture_entry(INPUT_BINDINGS[1]),
    ]
}

/// A hash of a module's whole content.
fn module_key(module: &naga::Module) -> u64 {
    use std::hash::Hasher as _;
    struct Hashing(std::collections::hash_map::DefaultHasher);
    impl std::fmt::Write for Hashing {
        fn write_str(&mut self, text: &str) -> std::fmt::Result {
            self.0.write(text.as_bytes());
            Ok(())
        }
    }
    let mut hashing = Hashing(std::collections::hash_map::DefaultHasher::new());
    let _ = std::fmt::Write::write_fmt(&mut hashing, format_args!("{module:?}"));
    hashing.0.finish()
}

/// The thread-local state a compile reads, carried to the thread that
/// compiles the parts beside their main pass.
struct CompileContext {
    helper_cache: Option<wgpu::PipelineCache>,
    background: bool,
    small: bool,
}

impl CompileContext {
    fn here() -> Self {
        Self {
            helper_cache: idle_compile_child::HELPER_CACHE.with(|cache| cache.borrow().clone()),
            background: idle_compiles::BACKGROUND_COMPILE.with(std::cell::Cell::get),
            small: SMALL_VARIANT.with(std::cell::Cell::get),
        }
    }

    fn enter(self) {
        idle_compile_child::HELPER_CACHE.with(|cache| *cache.borrow_mut() = self.helper_cache);
        idle_compiles::BACKGROUND_COMPILE.with(|background| background.set(self.background));
        SMALL_VARIANT.with(|small| small.set(self.small));
    }
}

impl ToneSplitParts {
    /// A main pass of `display` with `constants` (`compile_main`), with its
    /// parts compiled beside it on another thread: `Err` when either fails
    /// (a main pass has no stand-in).
    pub(super) fn main_pass(
        &self,
        device: &wgpu::Device,
        display: &DisplayShader,
        layout: &wgpu::PipelineLayout,
        label: &str,
        constants: &[(&str, f64)],
        compile_main: impl FnOnce(&[(&str, f64)]) -> Result<wgpu::ComputePipeline, String>,
    ) -> Result<wgpu::ComputePipeline, String> {
        let main_constants = [constants, &[MAIN_CONSTANT][..]].concat();
        let (main, parts) = std::thread::scope(|scope| {
            // Each part on a thread of its own: in a helper process (no
            // compile slot) all three compile at once; elsewhere the small
            // compiles' turns order the parts.
            let parts = [0, 1].map(|index| {
                let context = CompileContext::here();
                let constants = [constants, PART_CONSTANTS[index]].concat();
                std::thread::Builder::new()
                    .name("pipeline-compile".into())
                    .spawn_scoped(scope, move || {
                        context.enter();
                        self.part(device, display, layout, label, index, &constants)
                    })
            });
            let main = compile_main(&main_constants);
            let parts = parts.map(|part| match part {
                Ok(handle) => handle
                    .join()
                    .unwrap_or_else(|_| Err("the part's compile panicked".into())),
                Err(error) => Err(format!("no thread for the part: {error}")),
            });
            (main, parts)
        });
        let [before, tone] = parts;
        let parts = before
            .and_then(|before| tone.map(|tone| [before, tone]))
            .map_err(|error| format!("{label}: {error}"));
        let main = main?;
        let parts = parts?;
        self.of_main
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(main.clone(), parts);
        Ok(main)
    }

    /// The parts `main` runs after, if it is a main pass of the split.
    pub(super) fn of(&self, main: &wgpu::ComputePipeline) -> Option<[wgpu::ComputePipeline; 2]> {
        self.of_main
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(main)
            .cloned()
    }

    /// Tests: how many main passes of the split were made.
    #[cfg(test)]
    pub(super) fn main_passes(&self) -> usize {
        self.of_main.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    fn part(
        &self,
        device: &wgpu::Device,
        display: &DisplayShader,
        layout: &wgpu::PipelineLayout,
        main_label: &str,
        index: usize,
        constants: &[(&str, f64)],
    ) -> Result<wgpu::ComputePipeline, String> {
        let module = display.variant_module(constants)?;
        let cell = Arc::clone(
            self.parts
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .entry((layout.clone(), module_key(&module)))
                .or_default(),
        );
        cell.get_or_init(|| {
            let label = format!("{main_label} ({})", PART_NAMES[index]);
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
                    layout: Some(layout),
                    module: &shader,
                    entry_point: Some("main"),
                    compilation_options: Default::default(),
                    cache: None,
                },
                CompileWeight::Small,
            )
        })
        .clone()
    }
}
