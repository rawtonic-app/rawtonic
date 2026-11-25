// SPDX-License-Identifier: AGPL-3.0-or-later
//! Super Resolution, independent of Tauri: a rendered photo enlarged 2× by
//! an AI model (Lightroom's Enhance ▸ Super Resolution), run in tiles on
//! ONNX Runtime or, on macOS with the model pack's Core ML program, Core ML.
//! The export applies it to the render in its output encoding, before the
//! resize, border and output sharpening (`rawtonic_export::export_processing::Enlarger`).
//!
//! The model is Philip Hofmann's 2xBHI_small_compact_pretrain (CC BY 4.0), a
//! Compact network trained with L1 and MS-SSIM losses only: it enlarges
//! faithfully and invents no texture. `scripts/models/export_superres.py`
//! exports it with the four right-angle rotations of each tile averaged
//! inside the graph. Measured against Lightroom's Super Resolution on 34
//! crops of 17 RAWs (2× downsampled, enlarged, compared with the original):
//! PSNR 32.57 vs 32.66 dB, SSIM 0.8777 vs 0.8785, LPIPS 0.133 vs 0.129, all
//! differences within their 95 % intervals; 24 → 96 MP in about 6 s on an
//! M5 Pro's GPU (Lightroom 7 s). notes:
//! benchmark-results/srx-super-resolution-2026-11-08/REPORT.md.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Result, bail, ensure};
use image::{DynamicImage, ImageBuffer, Luma, Pixel, Primitive, Rgb, Rgba};
use ndarray::Array4;
use ort::value::Tensor;

/// The model's square input tile, in pixels.
pub const TILE: usize = 512;
/// Pixels of context on each side of a tile that are enlarged but not kept.
/// The network's 18 layers of 3 × 3 convolutions see 18 px around a pixel,
/// so the kept centres are what one run over the whole photo would give.
/// A 24 MP photo takes 9 × 13 tiles.
pub const MARGIN: usize = 24;
/// The enlargement.
pub const SCALE: usize = 2;
/// The kept part of a tile.
const STEP: usize = TILE - 2 * MARGIN;

const INPUT: &str = "image";
const OUTPUT: &str = "enlarged";

pub const ONNX_FILE: &str = "superres_compact_x2.onnx";
pub const ONNX_SHA256: &str = "088990646244e01a74c12460dda5a86547d967299d70b4e58afd4904abecd0d8";

/// Sequential four-rotation conversion, for the CPU provider only.
pub const CPU_ONNX_FILE: &str = "superres_compact_x2_cpu.onnx";
pub const CPU_ONNX_SHA256: &str =
    "40bf5c362bad13ca57c1e4493f66f9ab5f2ce7f57156e1d20a77aad7322cd7a1";

/// Super Resolution's Fast quality (`Quality::Fast`): the same weights on
/// the tile as it is, without the four-rotation average (a quarter of the
/// work), on every ONNX Runtime provider. On 34 crops of 17 RAWs PSNR
/// 0.07 dB below the average, LPIPS 0.001 better, no direction dependence,
/// 8-bit codes p99 ≤ 2.3 on full-resolution renders; nothing visible at
/// 100 % or 300 % (notes: benchmark-results/srr1-2026-11-25/REPORT.md).
/// The name is the one it was published under (pack-2026-11-25). Loaded
/// from the model folder when a Fast tile first needs it; without it a Fast
/// export fails with a clear error (never a Full result instead).
pub const SINGLE_ONNX_FILE: &str = "superres_compact_x2_cpu_single.onnx";
pub const SINGLE_ONNX_SHA256: &str =
    "3ad7b2ebfa053e930c5356db5d326b7587b458ae9fab4869f9e0534fbf563c1e";

/// Super Resolution's quality, the export recipe's choice
/// (`superResolutionQuality`), honoured on every backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quality {
    /// Each tile's four right-angle rotations enlarged and averaged.
    Full,
    /// Each tile enlarged once (`SINGLE_ONNX_FILE`, `CORE_ML_SINGLE_PACKAGE`).
    Fast,
}

/// The same graph with fp16 weights and compute, fp32 input and output
/// (`scripts/models/convert_superres_fp16.py`), for GPU providers only: on
/// an RTX 3080 (Linux) a 45 MP photo took 15.4 s with CUDA instead of
/// 22.2 s and 29.0 s with WebGPU instead of 40.4 s, at most 1 (CUDA) and 2
/// (WebGPU) 8-bit codes from the fp32 graph
/// (`docs/benchmark-results/srf16-superres-fp16-2026-11-20/REPORT.md`).
/// Packs before pack-2026-11-20 lack it.
pub const GPU_ONNX_FILE: &str = "superres_compact_x2_fp16.onnx";
pub const GPU_ONNX_SHA256: &str =
    "4ee4e846aebe963d9739cca170df56b6b13f0fb6f604483d284ac227d250289d";

/// The same graph as a Core ML program (fp16; largest difference to the
/// ONNX model 0.003, mean 0.0002 of the 0..1 range), macOS's fast path.
#[cfg(target_os = "macos")]
pub const CORE_ML_PACKAGE: &str = "superres_compact_x2.mlpackage";
/// The program's pack files: pack file, place in the package, SHA-256.
#[cfg(target_os = "macos")]
pub const CORE_ML_FILES: [(&str, &str, &str); 3] = [
    (
        "superres_coreml_Manifest.json",
        "Manifest.json",
        "935b1ef0c97fe51b4999919c28b9c91151c648c72571c1d5c3c49dd6e9b3b184",
    ),
    (
        "superres_coreml_model.mlmodel",
        "Data/com.apple.CoreML/model.mlmodel",
        "59668d6013f38bcd11e721b80379f2cb5d0e896b754095f08433d469988cb132",
    ),
    (
        "superres_coreml_weight.bin",
        "Data/com.apple.CoreML/weights/weight.bin",
        "1df839340b0df1bb953ec97b0e084008c3d44557483d6009fed732d635b3a5fe",
    ),
];
/// The Fast quality as a Core ML program (fp16; largest difference to the
/// PyTorch pass 0.0026, mean 0.0002; 11 ms per tile instead of 46 ms on an
/// M5 Pro). Same weights file as `CORE_ML_FILES`.
#[cfg(target_os = "macos")]
pub const CORE_ML_SINGLE_PACKAGE: &str = "superres_compact_x2_single.mlpackage";
#[cfg(target_os = "macos")]
pub const CORE_ML_SINGLE_FILES: [(&str, &str, &str); 3] = [
    (
        "superres_single_coreml_Manifest.json",
        "Manifest.json",
        "935b1ef0c97fe51b4999919c28b9c91151c648c72571c1d5c3c49dd6e9b3b184",
    ),
    (
        "superres_single_coreml_model.mlmodel",
        "Data/com.apple.CoreML/model.mlmodel",
        "181860c6811ef1539244bf964b8570c8ff85ca377e39bf7f993ebcba5d461db3",
    ),
    (
        "superres_single_coreml_weight.bin",
        "Data/com.apple.CoreML/weights/weight.bin",
        "1df839340b0df1bb953ec97b0e084008c3d44557483d6009fed732d635b3a5fe",
    ),
];
/// CPU and GPU: 50 ms per tile on an M5 Pro; with the Neural Engine 54 ms
/// (larger tiles were slower on both).
#[cfg(target_os = "macos")]
pub const CORE_ML_UNITS: rawtonic_denoise::coreml::ComputeUnits =
    rawtonic_denoise::coreml::ComputeUnits::CpuAndGpu;

/// The Super Resolution model on one backend.
pub struct Model {
    backend: Backend,
}

enum Backend {
    /// `model` is the original graph, on the provider ONNX Runtime picked
    /// (it can move from the CPU to a GPU later, when the idle GPU check
    /// finds the GPU faster). `cpu` is the sequential conversion, used
    /// while `model` runs on the CPU. When `model` is the fp16 graph
    /// (`GPU_ONNX_FILE`, on a GPU) it runs without ONNX Runtime's move to
    /// the CPU; once a run failed (`gpu_failed`) `fp32_cpu` takes over for
    /// good: the CPU conversion, else the original graph, loaded then.
    /// `single_cpu` and `single_gpu` are the Fast graph
    /// (`SINGLE_ONNX_FILE`) on the CPU and on the GPU provider the Full graph
    /// runs on, each loaded the first time a Fast tile needs it (kept once
    /// loaded, tried again while missing).
    Onnx {
        model: rawtonic_onnx::Model,
        cpu: Option<rawtonic_onnx::Model>,
        single_cpu: OnceLock<rawtonic_onnx::Model>,
        single_gpu: OnceLock<rawtonic_onnx::Model>,
        fp32_cpu: OnceLock<Option<rawtonic_onnx::Model>>,
        gpu_failed: AtomicBool,
    },
    /// The Full program and, once installed (`attach_core_ml_fast`), the
    /// Fast one.
    #[cfg(target_os = "macos")]
    CoreMl {
        full: rawtonic_denoise::coreml::CoreMlModel,
        fast: OnceLock<rawtonic_denoise::coreml::CoreMlModel>,
    },
}

impl Model {
    /// The ONNX model on ONNX Runtime: a GPU provider when one works, else
    /// the CPU. On a GPU the fp16 graph (`GPU_ONNX_FILE`) when it is
    /// installed and its check on that GPU passed; until that check has run
    /// (queued for when the app is idle) and with older packs, the original
    /// graph. Which graph runs is not part of the recipe: GPU providers'
    /// output already differs from the CPU's and between providers by about
    /// one 8-bit code, so the provider's output is not pinned by recipe
    /// version (coordinator decision CUDA1, 2026-11-20; the CHANGELOG states
    /// the measured change).
    pub fn onnx(path: &Path) -> Result<Self> {
        let cpu_path = path.with_file_name(CPU_ONNX_FILE);
        let gpu_path = path.with_file_name(GPU_ONNX_FILE);
        let cpu_only = cpu_only();
        if cpu_only && let Some(cpu) = Self::optional_cpu(&cpu_path) {
            return Ok(Self::onnx_backend(cpu, None));
        }
        if tries_gpu_graph(cpu_only, || verified(&gpu_path, GPU_ONNX_SHA256))
            && let Some(model) = rawtonic_onnx::Model::load_on_checked_gpu(
                &gpu_path,
                rawtonic_onnx::ModelKind::SuperResolution,
            )
        {
            return Ok(Self::onnx_backend(model, None));
        }
        let model = rawtonic_onnx::Model::load(path, rawtonic_onnx::ModelKind::SuperResolution)?;
        let cpu = if model.provider() == rawtonic_onnx::Provider::Cpu {
            Self::optional_cpu(&cpu_path)
        } else {
            None
        };
        Ok(Self::onnx_backend(model, cpu))
    }

    fn onnx_backend(model: rawtonic_onnx::Model, cpu: Option<rawtonic_onnx::Model>) -> Self {
        Self {
            backend: Backend::Onnx {
                model,
                cpu,
                single_cpu: OnceLock::new(),
                single_gpu: OnceLock::new(),
                fp32_cpu: OnceLock::new(),
                gpu_failed: AtomicBool::new(false),
            },
        }
    }

    /// Whether `Model::onnx` would now load the fp16 graph on a GPU while
    /// this model runs another graph: its check on that GPU passed after
    /// this model was loaded (the app's idle check). The app then loads the
    /// model again before the next photo. A cheap lookup; loads nothing.
    pub fn gpu_graph_ready(&self) -> bool {
        let model = match &self.backend {
            Backend::Onnx { model, .. } => model,
            #[cfg(target_os = "macos")]
            Backend::CoreMl { .. } => return false,
        };
        let gpu_path = model.path().with_file_name(GPU_ONNX_FILE);
        !is_gpu_graph(model)
            && tries_gpu_graph(cpu_only(), || verified(&gpu_path, GPU_ONNX_SHA256))
            && rawtonic_onnx::Model::passed_gpu_check(
                &gpu_path,
                rawtonic_onnx::ModelKind::SuperResolution,
            )
    }

    /// The fp32 graph on the CPU beside the fp16 graph's folder: the CPU
    /// conversion, else the original graph.
    fn fp32_on_cpu(gpu_path: &Path) -> Option<rawtonic_onnx::Model> {
        Self::optional_cpu(&gpu_path.with_file_name(CPU_ONNX_FILE)).or_else(|| {
            let original = gpu_path.with_file_name(ONNX_FILE);
            verified(&original, ONNX_SHA256)
                .then(|| Self::load_cpu_graph(&original).ok())
                .flatten()
        })
    }

    /// The ONNX graph that runs a tile now at `quality` (`Ok(None)` for
    /// Core ML); an error when a Fast tile needs `SINGLE_ONNX_FILE` and it
    /// is not installed.
    fn onnx_graph(&self, quality: Quality) -> Result<Option<&rawtonic_onnx::Model>> {
        let (model, cpu, single_cpu, single_gpu, fp32_cpu, gpu_failed) = match &self.backend {
            Backend::Onnx {
                model,
                cpu,
                single_cpu,
                single_gpu,
                fp32_cpu,
                gpu_failed,
            } => (model, cpu, single_cpu, single_gpu, fp32_cpu, gpu_failed),
            #[cfg(target_os = "macos")]
            Backend::CoreMl { .. } => return Ok(None),
        };
        let graph = graph_now(
            model,
            cpu.as_ref(),
            || {
                fp32_cpu
                    .get_or_init(|| Self::fp32_on_cpu(model.path()))
                    .as_ref()
            },
            is_gpu_graph(model),
            model.provider() == rawtonic_onnx::Provider::Cpu,
            gpu_failed.load(Ordering::Relaxed),
        );
        let provider = graph.provider();
        for_quality(
            graph,
            quality,
            provider == rawtonic_onnx::Provider::Cpu,
            || Self::single_graph(single_cpu, model.path(), rawtonic_onnx::Provider::Cpu),
            || Self::single_graph(single_gpu, model.path(), provider),
        )
        .map(Some)
    }

    /// The Fast graph beside `model_path` on `provider` (a GPU: that GPU when
    /// its check passed, else the CPU until it has), loaded on first use;
    /// `None` while it is not installed (or does not load).
    fn single_graph<'a>(
        slot: &'a OnceLock<rawtonic_onnx::Model>,
        model_path: &Path,
        provider: rawtonic_onnx::Provider,
    ) -> Option<&'a rawtonic_onnx::Model> {
        if let Some(graph) = slot.get() {
            return Some(graph);
        }
        let path = model_path.with_file_name(SINGLE_ONNX_FILE);
        if !verified(&path, SINGLE_ONNX_SHA256) {
            return None;
        }
        let loaded = if provider == rawtonic_onnx::Provider::Cpu {
            Self::load_cpu_graph(&path)
        } else {
            rawtonic_onnx::Model::load_on(
                &path,
                rawtonic_onnx::ModelKind::SuperResolution,
                &[provider, rawtonic_onnx::Provider::Cpu],
                &rawtonic_onnx::settings().plugin_dirs,
            )
        }
        .ok()?;
        // Another tile may have loaded it meanwhile: either copy will do.
        let _ = slot.set(loaded);
        slot.get()
    }

    /// Loads a known CPU conversion directly, without probing GPU providers
    /// or building a session for the original graph first.
    pub fn cpu(path: &Path) -> Result<Self> {
        Ok(Self::onnx_backend(Self::load_cpu_graph(path)?, None))
    }

    /// Adds the sequential CPU conversion to a model loaded on the CPU, so
    /// the CPU runs it, while the original graph stays free to move to a
    /// GPU in this session (the idle GPU check, rawtonic-onnx `check`).
    pub fn attach_cpu_graph(&mut self, path: &Path) -> Result<()> {
        if let Backend::Onnx { model, cpu, .. } = &mut self.backend
            && cpu.is_none()
            && model.provider() == rawtonic_onnx::Provider::Cpu
        {
            *cpu = Some(Self::load_cpu_graph(path)?);
        }
        Ok(())
    }

    fn load_cpu_graph(path: &Path) -> Result<rawtonic_onnx::Model> {
        rawtonic_onnx::Model::load_on(
            path,
            rawtonic_onnx::ModelKind::SuperResolution,
            &[rawtonic_onnx::Provider::Cpu],
            &[],
        )
    }

    fn optional_cpu(path: &Path) -> Option<rawtonic_onnx::Model> {
        // Older packs and an unusable optional conversion keep the original graph.
        if verified(path, CPU_ONNX_SHA256) {
            Self::load_cpu_graph(path).ok()
        } else {
            None
        }
    }

    /// The Core ML program of the model pack (`CORE_ML_PACKAGE`).
    #[cfg(target_os = "macos")]
    pub fn core_ml(model: rawtonic_denoise::coreml::CoreMlModel) -> Self {
        Self {
            backend: Backend::CoreMl {
                full: model,
                fast: OnceLock::new(),
            },
        }
    }

    /// Adds the Fast Core ML program (`CORE_ML_SINGLE_PACKAGE`) to a Core ML
    /// model; without it a Fast export on Core ML fails clearly.
    #[cfg(target_os = "macos")]
    pub fn attach_core_ml_fast(&self, model: rawtonic_denoise::coreml::CoreMlModel) {
        if let Backend::CoreMl { fast, .. } = &self.backend {
            let _ = fast.set(model);
        }
    }

    /// Whether the Fast quality can run on this model's backend: the Fast
    /// Core ML program attached, or `SINGLE_ONNX_FILE` beside the ONNX model.
    pub fn fast_installed(&self) -> bool {
        match &self.backend {
            Backend::Onnx { model, .. } => verified(
                &model.path().with_file_name(SINGLE_ONNX_FILE),
                SINGLE_ONNX_SHA256,
            ),
            #[cfg(target_os = "macos")]
            Backend::CoreMl { fast, .. } => fast.get().is_some(),
        }
    }

    /// The model of a model folder the app filled (the CLI's `--models`), on
    /// the backend the app picks there: on macOS the Core ML program the app
    /// assembled, else the ONNX model. Nothing is downloaded or assembled.
    pub fn from_dir(dir: &Path) -> Result<Self> {
        #[cfg(target_os = "macos")]
        if CORE_ML_FILES
            .iter()
            .all(|&(file, _, sha)| verified(&dir.join(file), sha))
        {
            let package = dir.join(CORE_ML_PACKAGE);
            if !CORE_ML_FILES
                .iter()
                .all(|&(_, inner, sha)| verified(&package.join(inner), sha))
            {
                bail!(
                    "the Core ML program {} is not assembled yet: export once with Super Resolution in the app",
                    package.display()
                );
            }
            let model = Self::core_ml(rawtonic_denoise::coreml::CoreMlModel::load(
                &package,
                CORE_ML_UNITS,
            )?);
            // The Fast program, when the app assembled it.
            let fast = dir.join(CORE_ML_SINGLE_PACKAGE);
            if CORE_ML_SINGLE_FILES
                .iter()
                .all(|&(_, inner, sha)| verified(&fast.join(inner), sha))
            {
                model.attach_core_ml_fast(rawtonic_denoise::coreml::CoreMlModel::load(
                    &fast,
                    CORE_ML_UNITS,
                )?);
            }
            return Ok(model);
        }
        let path: PathBuf = dir.join(ONNX_FILE);
        if !verified(&path, ONNX_SHA256) {
            bail!(
                "the Super Resolution model is not installed in {} (export once with Super Resolution in the app, or run scripts/models/install_models.sh)",
                dir.display()
            );
        }
        Self::onnx(&path)
    }

    /// Whether this model uses the ONNX Runtime CPU provider now.
    pub fn uses_cpu(&self) -> bool {
        matches!(&self.backend, Backend::Onnx { model, .. } if model.provider() == rawtonic_onnx::Provider::Cpu)
    }

    pub fn backend_name(&self) -> String {
        match self.onnx_graph(Quality::Full).ok().flatten() {
            Some(graph) => {
                let fp16 = if is_gpu_graph(graph) { ", fp16" } else { "" };
                format!("ONNX Runtime ({}{fp16})", graph.provider())
            }
            None => "Core ML".to_string(),
        }
    }

    /// Whether this model runs on ONNX Runtime (the app then installs
    /// `SINGLE_ONNX_FILE` beside it), not Core ML.
    pub fn uses_onnx(&self) -> bool {
        matches!(self.backend, Backend::Onnx { .. })
    }

    /// Enlarges one tile: `input` is [1, 3, TILE, TILE] (RGB, 0..1,
    /// first-major), the result [1, 3, 2·TILE, 2·TILE] in 0..1.
    pub fn enlarge_tile(&self, input: &[f32], quality: Quality) -> Result<Vec<f32>> {
        ensure!(
            input.len() == 3 * TILE * TILE,
            "tile of {} values",
            input.len()
        );
        let run = |session: &mut ort::session::Session| {
            let array = Array4::from_shape_vec((1, 3, TILE, TILE), input.to_vec())?;
            let outputs = session.run(ort::inputs![INPUT => Tensor::from_array(array)?])?;
            let (_, data) = outputs[OUTPUT].try_extract_tensor::<f32>()?;
            rawtonic_onnx::ensure_finite(data)?;
            Ok(data.to_vec())
        };
        let values = match &self.backend {
            Backend::Onnx { gpu_failed, .. } => {
                let graph = self
                    .onnx_graph(quality)?
                    .expect("an ONNX backend has a graph");
                if is_gpu_graph(graph) && graph.provider() != rawtonic_onnx::Provider::Cpu {
                    // No retry of the fp16 graph on the CPU: the photo
                    // starts again on the fp32 CPU graph (`enlarge`).
                    recording_failure(graph.run_without_fallback(run), gpu_failed)?
                } else {
                    graph.run(run)?
                }
            }
            #[cfg(target_os = "macos")]
            Backend::CoreMl { full, fast } => {
                let model = match quality {
                    Quality::Full => full,
                    Quality::Fast => fast.get().ok_or_else(|| {
                        anyhow::anyhow!(
                            "the Super Resolution Fast model ({CORE_ML_SINGLE_PACKAGE}) is not installed (tick Super Resolution in the Export dialog again to download it)"
                        )
                    })?,
                };
                let values = model.predict(INPUT, &[1, 3, TILE, TILE], input, OUTPUT)?.1;
                rawtonic_onnx::ensure_finite(&values)?;
                values
            }
        };
        ensure!(
            values.len() == 3 * TILE * TILE * SCALE * SCALE,
            "the model returned {} values",
            values.len()
        );
        Ok(values)
    }

    /// The graph file and provider tiles run on now (`None`: Core ML).
    fn path_now(&self, quality: Quality) -> Option<(PathBuf, rawtonic_onnx::Provider)> {
        self.onnx_graph(quality)
            .ok()
            .flatten()
            .map(|graph| (graph.path().to_path_buf(), graph.provider()))
    }

    /// `image` enlarged 2× (see [`enlarge_with`]), all its tiles on one
    /// graph and provider: when a GPU failure changed them during the photo
    /// (the fp16 graph stops at the failed tile, the fp32 graph moved to
    /// the CPU), the photo is enlarged again on the CPU path.
    pub fn enlarge(
        &self,
        image: DynamicImage,
        quality: Quality,
        cancelled: &dyn Fn() -> Result<(), String>,
    ) -> Result<DynamicImage, String> {
        on_one_path(&|| self.path_now(quality), &mut || {
            enlarge_ref(
                &image,
                &mut |tile| self.enlarge_tile(tile, quality),
                cancelled,
            )
        })
    }
}

/// Whether `Model::onnx` tries the fp16 graph first: only when a GPU may
/// run (the CPU path keeps the CPU conversion or the original graph: ONNX
/// Runtime's CPU provider casts fp16 back to fp32 on x86) and the file is
/// installed with its SHA-256 (packs before pack-2026-11-20 lack it).
fn tries_gpu_graph(cpu_only: bool, installed: impl FnOnce() -> bool) -> bool {
    !cpu_only && installed()
}

/// Whether the CPU path decides `Model::onnx`: `RAWTONIC_AI_PROVIDER=cpu`,
/// or GPU AI turned off without a forced provider.
fn cpu_only() -> bool {
    let forced = rawtonic_onnx::forced_provider();
    forced == Some(rawtonic_onnx::Provider::Cpu)
        || (forced.is_none() && !rawtonic_onnx::settings().gpu)
}

fn is_gpu_graph(model: &rawtonic_onnx::Model) -> bool {
    model.path().file_name() == Some(GPU_ONNX_FILE.as_ref())
}

/// Runs `pass` (one whole photo) and, when the path its tiles run on
/// (`path`) changed during it, once more on the new path, the first
/// result (finished or failed) dropped before.
fn on_one_path<P: PartialEq, T>(
    path: &dyn Fn() -> P,
    pass: &mut dyn FnMut() -> Result<T, String>,
) -> Result<T, String> {
    let before = path();
    let first = pass();
    if path() == before {
        return first;
    }
    drop(first);
    pass()
}

/// The graph that runs a tile now (`Model::onnx_graph`): the fp16 graph
/// (`gpu_graph`) on its GPU until a run there failed or ONNX Runtime moved
/// it to the CPU, then the fp32 CPU graph (`fp32_cpu`, loaded on first
/// call; the fp16 graph only when no fp32 graph loads); the original graph
/// as in [`active_graph`].
fn graph_now<'a, T>(
    model: &'a T,
    cpu: Option<&'a T>,
    fp32_cpu: impl FnOnce() -> Option<&'a T>,
    gpu_graph: bool,
    on_cpu: bool,
    gpu_failed: bool,
) -> &'a T {
    if gpu_graph
        && (on_cpu || gpu_failed)
        && let Some(fp32) = fp32_cpu()
    {
        return fp32;
    }
    active_graph(model, cpu, on_cpu)
}

/// The graph that runs a tile at `quality`: Full the graph that runs now
/// (`graph`), Fast the single-pass graph on the same kind of processor
/// (`single_cpu` when `graph` is on the CPU, else `single_gpu`), looked up
/// only then; an error when it is not installed (never a Full result for a
/// Fast recipe).
fn for_quality<'a, T>(
    graph: &'a T,
    quality: Quality,
    on_cpu: bool,
    single_cpu: impl FnOnce() -> Option<&'a T>,
    single_gpu: impl FnOnce() -> Option<&'a T>,
) -> Result<&'a T> {
    let single = match quality {
        Quality::Full => return Ok(graph),
        Quality::Fast if on_cpu => single_cpu(),
        Quality::Fast => single_gpu(),
    };
    single.ok_or_else(|| {
        anyhow::anyhow!(
            "the Super Resolution Fast model ({SINGLE_ONNX_FILE}) is not installed (tick Super Resolution in the Export dialog again to download it)"
        )
    })
}

/// `result` of a run of the fp16 graph on its GPU; a failure is recorded
/// in `gpu_failed`, which moves the model to the fp32 CPU graph for good.
fn recording_failure<T>(result: Result<T>, gpu_failed: &AtomicBool) -> Result<T> {
    result.inspect_err(|_| gpu_failed.store(true, Ordering::Relaxed))
}

/// The graph that runs a tile: the CPU conversion while the original graph
/// is on the CPU, else the original (on a GPU, also after it moved there).
fn active_graph<'a, T>(original: &'a T, cpu_graph: Option<&'a T>, original_on_cpu: bool) -> &'a T {
    match cpu_graph {
        Some(cpu) if original_on_cpu => cpu,
        _ => original,
    }
}

fn verified(path: &Path, sha: &str) -> bool {
    path.is_file()
        && fs::File::open(path)
            .ok()
            .and_then(|mut file| rawtonic_fs::sha256_hex_read(&mut file).ok())
            .is_some_and(|hash| hash == sha)
}

/// A channel value as the model's 0..1 float and back.
trait Channel: Primitive {
    fn to_unit(self) -> f32;
    fn from_unit(value: f32) -> Self;
}

impl Channel for u8 {
    fn to_unit(self) -> f32 {
        self as f32 / 255.0
    }
    fn from_unit(value: f32) -> Self {
        (value.clamp(0.0, 1.0) * 255.0).round() as u8
    }
}

impl Channel for u16 {
    fn to_unit(self) -> f32 {
        self as f32 / 65535.0
    }
    fn from_unit(value: f32) -> Self {
        (value.clamp(0.0, 1.0) * 65535.0).round() as u16
    }
}

impl Channel for f32 {
    fn to_unit(self) -> f32 {
        self
    }
    fn from_unit(value: f32) -> Self {
        value
    }
}

/// `image` enlarged 2× by `run` (one [1, 3, TILE, TILE] tile in, its
/// enlargement out), in the same pixel type. Tiles overlap by `MARGIN`
/// on each side and only their centres are kept; beyond the photo's edges
/// a tile repeats the edge pixels. Values are clamped to 0..1 for the
/// model (renders in the output encoding are). An alpha channel is doubled
/// by repeating each value; grey images are enlarged as RGB and kept grey.
/// `cancelled` runs before each tile.
pub fn enlarge_with(
    image: DynamicImage,
    run: &mut dyn FnMut(&[f32]) -> Result<Vec<f32>>,
    cancelled: &dyn Fn() -> Result<(), String>,
) -> Result<DynamicImage, String> {
    enlarge_ref(&image, run, cancelled)
}

fn enlarge_ref(
    image: &DynamicImage,
    run: &mut dyn FnMut(&[f32]) -> Result<Vec<f32>>,
    cancelled: &dyn Fn() -> Result<(), String>,
) -> Result<DynamicImage, String> {
    let error = |e: anyhow::Error| format!("Super Resolution failed: {e:#}");
    Ok(match image {
        DynamicImage::ImageRgb8(buf) => {
            DynamicImage::ImageRgb8(enlarge_buffer(buf, run, cancelled).map_err(error)?)
        }
        DynamicImage::ImageRgba8(buf) => {
            DynamicImage::ImageRgba8(enlarge_buffer(buf, run, cancelled).map_err(error)?)
        }
        DynamicImage::ImageRgb16(buf) => {
            DynamicImage::ImageRgb16(enlarge_buffer(buf, run, cancelled).map_err(error)?)
        }
        DynamicImage::ImageRgba16(buf) => {
            DynamicImage::ImageRgba16(enlarge_buffer(buf, run, cancelled).map_err(error)?)
        }
        DynamicImage::ImageRgb32F(buf) => {
            DynamicImage::ImageRgb32F(enlarge_buffer(buf, run, cancelled).map_err(error)?)
        }
        DynamicImage::ImageRgba32F(buf) => {
            DynamicImage::ImageRgba32F(enlarge_buffer(buf, run, cancelled).map_err(error)?)
        }
        DynamicImage::ImageLuma8(buf) => {
            DynamicImage::ImageLuma8(enlarge_buffer(buf, run, cancelled).map_err(error)?)
        }
        DynamicImage::ImageLuma16(buf) => {
            DynamicImage::ImageLuma16(enlarge_buffer(buf, run, cancelled).map_err(error)?)
        }
        other => {
            let rgb = other.to_rgba32f();
            DynamicImage::ImageRgba32F(enlarge_buffer(&rgb, run, cancelled).map_err(error)?)
        }
    })
}

/// How a pixel type's channels map to the model's RGB.
trait ModelPixel: Pixel {
    fn rgb(&self) -> [f32; 3];
    /// The pixel with enlarged `rgb` and, for alpha, `source`'s alpha.
    fn with_rgb(rgb: [f32; 3], source: &Self) -> Self;
}

// `image` implements `Pixel` for each channel type separately, so these
// are written out per type.
macro_rules! model_pixels {
    ($($t:ty),*) => {$(
        impl ModelPixel for Rgb<$t> {
            fn rgb(&self) -> [f32; 3] {
                self.0.map(Channel::to_unit)
            }
            fn with_rgb(rgb: [f32; 3], _: &Self) -> Self {
                Rgb(rgb.map(<$t>::from_unit))
            }
        }

        impl ModelPixel for Rgba<$t> {
            fn rgb(&self) -> [f32; 3] {
                [self.0[0].to_unit(), self.0[1].to_unit(), self.0[2].to_unit()]
            }
            fn with_rgb(rgb: [f32; 3], source: &Self) -> Self {
                Rgba([<$t>::from_unit(rgb[0]), <$t>::from_unit(rgb[1]), <$t>::from_unit(rgb[2]), source.0[3]])
            }
        }
    )*};
}
model_pixels!(u8, u16, f32);

macro_rules! model_greys {
    ($($t:ty),*) => {$(
        impl ModelPixel for Luma<$t> {
            fn rgb(&self) -> [f32; 3] {
                [self.0[0].to_unit(); 3]
            }
            fn with_rgb(rgb: [f32; 3], _: &Self) -> Self {
                Luma([<$t>::from_unit((rgb[0] + rgb[1] + rgb[2]) / 3.0)])
            }
        }
    )*};
}
model_greys!(u8, u16);

fn enlarge_buffer<P: ModelPixel>(
    source: &ImageBuffer<P, Vec<P::Subpixel>>,
    run: &mut dyn FnMut(&[f32]) -> Result<Vec<f32>>,
    cancelled: &dyn Fn() -> Result<(), String>,
) -> Result<ImageBuffer<P, Vec<P::Subpixel>>> {
    let (width, height) = (source.width() as usize, source.height() as usize);
    ensure!(width > 0 && height > 0, "empty image");
    let (out_w, out_h) = (width * SCALE, height * SCALE);
    let mut out: ImageBuffer<P, Vec<P::Subpixel>> = ImageBuffer::new(out_w as u32, out_h as u32);
    let plane = TILE * TILE;
    let out_tile = TILE * SCALE;
    let mut input = vec![0.0f32; 3 * plane];
    for y0 in (0..height).step_by(STEP) {
        for x0 in (0..width).step_by(STEP) {
            if let Err(e) = cancelled() {
                bail!("{e}");
            }
            // The tile: STEP kept pixels from (x0, y0) with MARGIN around.
            for ty in 0..TILE {
                let sy = (y0 + ty).saturating_sub(MARGIN).min(height - 1);
                for tx in 0..TILE {
                    let sx = (x0 + tx).saturating_sub(MARGIN).min(width - 1);
                    let rgb = source.get_pixel(sx as u32, sy as u32).rgb();
                    let at = ty * TILE + tx;
                    for (channel, value) in rgb.into_iter().enumerate() {
                        input[channel * plane + at] = value.clamp(0.0, 1.0);
                    }
                }
            }
            let enlarged = run(&input)?;
            ensure!(
                enlarged.len() == 3 * out_tile * out_tile,
                "tile result of {} values",
                enlarged.len()
            );
            let out_plane = out_tile * out_tile;
            let keep_h = STEP.min(height - y0) * SCALE;
            let keep_w = STEP.min(width - x0) * SCALE;
            for ky in 0..keep_h {
                let ty = MARGIN * SCALE + ky;
                let oy = y0 * SCALE + ky;
                for kx in 0..keep_w {
                    let tx = MARGIN * SCALE + kx;
                    let ox = x0 * SCALE + kx;
                    let at = ty * out_tile + tx;
                    let rgb = [
                        enlarged[at],
                        enlarged[out_plane + at],
                        enlarged[2 * out_plane + at],
                    ];
                    let src = source.get_pixel((ox / SCALE) as u32, (oy / SCALE) as u32);
                    out.put_pixel(ox as u32, oy as u32, P::with_rgb(rgb, src));
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Nearest-neighbour 2× of a tile: what the network would do with no
    /// learned detail.
    fn nearest(tile: &[f32]) -> Result<Vec<f32>> {
        let out_tile = TILE * SCALE;
        let mut out = vec![0.0; 3 * out_tile * out_tile];
        for c in 0..3 {
            for y in 0..out_tile {
                for x in 0..out_tile {
                    out[c * out_tile * out_tile + y * out_tile + x] =
                        tile[c * TILE * TILE + (y / SCALE) * TILE + x / SCALE];
                }
            }
        }
        Ok(out)
    }

    fn gradient(width: u32, height: u32) -> image::Rgb32FImage {
        ImageBuffer::from_fn(width, height, |x, y| {
            Rgb([
                x as f32 / width as f32,
                y as f32 / height as f32,
                ((x * 7 + y * 13) % 17) as f32 / 16.0,
            ])
        })
    }

    #[test]
    fn tiles_join_into_one_whole_enlargement() {
        // Sizes below one tile, at a step boundary, and across several tiles.
        for (w, h) in [(1, 1), (37, 5), (STEP as u32, STEP as u32 + 1), (1100, 700)] {
            let source = gradient(w, h);
            let out = enlarge_with(
                DynamicImage::ImageRgb32F(source.clone()),
                &mut nearest,
                &|| Ok(()),
            )
            .unwrap()
            .into_rgb32f();
            assert_eq!(out.dimensions(), (2 * w, 2 * h));
            for (x, y, pixel) in out.enumerate_pixels() {
                assert_eq!(
                    pixel,
                    source.get_pixel(x / 2, y / 2),
                    "({x}, {y}) of {w}x{h}"
                );
            }
        }
    }

    #[test]
    fn keeps_the_pixel_type_and_alpha() {
        let rgba = ImageBuffer::from_fn(40, 30, |x, y| {
            Rgba([
                (x * 6) as u16 * 257,
                (y * 8) as u16 * 257,
                9000,
                (x + y) as u16 * 900,
            ])
        });
        let out = enlarge_with(
            DynamicImage::ImageRgba16(rgba.clone()),
            &mut nearest,
            &|| Ok(()),
        )
        .unwrap();
        let DynamicImage::ImageRgba16(out) = out else {
            panic!("pixel type changed")
        };
        assert_eq!(out.dimensions(), (80, 60));
        assert_eq!(out.get_pixel(13, 7), rgba.get_pixel(6, 3));
        let rgb8 = DynamicImage::ImageRgb8(ImageBuffer::from_pixel(9, 4, Rgb([10u8, 200, 255])));
        assert!(matches!(
            enlarge_with(rgb8, &mut nearest, &|| Ok(())).unwrap(),
            DynamicImage::ImageRgb8(_)
        ));
    }

    #[test]
    fn the_model_sees_values_in_unit_range() {
        let wide = ImageBuffer::from_pixel(20, 20, Rgb([-0.5f32, 0.5, 3.0]));
        let mut seen = (f32::MAX, f32::MIN);
        let mut probe = |tile: &[f32]| {
            for &v in tile {
                seen = (seen.0.min(v), seen.1.max(v));
            }
            nearest(tile)
        };
        enlarge_with(DynamicImage::ImageRgb32F(wide), &mut probe, &|| Ok(())).unwrap();
        assert_eq!(seen, (0.0, 1.0));
    }

    #[test]
    fn stops_when_cancelled() {
        let source = DynamicImage::ImageRgb32F(gradient(1200, 1200));
        let mut runs = 0;
        let mut counting = |tile: &[f32]| {
            runs += 1;
            nearest(tile)
        };
        let calls = std::cell::Cell::new(0);
        let cancel_after_two = || {
            calls.set(calls.get() + 1);
            if calls.get() > 2 {
                Err("Export cancelled".to_string())
            } else {
                Ok(())
            }
        };
        let err = enlarge_with(source, &mut counting, &cancel_after_two).unwrap_err();
        assert!(err.contains("Export cancelled"), "{err}");
        assert_eq!(runs, 2);
    }

    #[test]
    fn fp16_graph_only_on_the_gpu_path_and_only_when_installed() {
        assert!(tries_gpu_graph(false, || true));
        // An older pack (or a damaged download): the original graph.
        assert!(!tries_gpu_graph(false, || false));
        // The CPU path never looks at the fp16 file.
        assert!(!tries_gpu_graph(true, || panic!("checked on the CPU path")));
    }

    /// A GPU that fails at the third tile: the fp16 graph's pass stops
    /// there and the photo starts again on the fp32 CPU graph; the fp32
    /// graph's pass, moved to the CPU, finishes and runs again on the CPU.
    #[test]
    fn a_photo_that_lost_its_gpu_midway_is_enlarged_again_on_one_path() {
        use std::cell::Cell;
        let source = gradient(1200, 900);
        for stops_at_failure in [true, false] {
            let on_gpu = Cell::new(true);
            let (gpu_tiles, cpu_tiles) = (Cell::new(0), Cell::new(0));
            let mut run = |tile: &[f32]| -> Result<Vec<f32>> {
                if on_gpu.get() {
                    gpu_tiles.set(gpu_tiles.get() + 1);
                    if gpu_tiles.get() == 3 {
                        on_gpu.set(false);
                        if stops_at_failure {
                            bail!("GPU device lost");
                        }
                    }
                } else {
                    cpu_tiles.set(cpu_tiles.get() + 1);
                }
                nearest(tile)
            };
            let mut passes = 0;
            let out = on_one_path(&|| on_gpu.get(), &mut || {
                passes += 1;
                enlarge_ref(
                    &DynamicImage::ImageRgb32F(source.clone()),
                    &mut run,
                    &|| Ok(()),
                )
            })
            .expect("the CPU pass finishes");
            assert_eq!(passes, 2, "stops at failure: {stops_at_failure}");
            assert_eq!(gpu_tiles.get(), 3);
            assert_eq!(out.width(), 2400);
            let tiles = cpu_tiles.get();
            // The whole photo on the CPU in the second pass (plus, for the
            // fp32 graph, the rest of the first pass).
            assert!(tiles >= 6, "{tiles} CPU tiles");
        }
        // No change of path: one pass, its error kept (a cancelled export).
        let mut passes = 0;
        let err = on_one_path(&|| 0, &mut || -> Result<(), String> {
            passes += 1;
            Err("Export cancelled".into())
        })
        .unwrap_err();
        assert_eq!((passes, err.as_str()), (1, "Export cancelled"));
    }

    /// The production selection (`graph_now`, `recording_failure`, as in
    /// `Model::onnx_graph` and `enlarge_tile`) inside the restart helper: an
    /// fp16 graph whose GPU fails at the third tile hands the photo to the
    /// fp32 CPU graph, which enlarges all of it in a second pass; without
    /// an fp32 graph the fp16 graph's error stays.
    #[test]
    fn a_failed_fp16_tile_moves_the_photo_to_the_fp32_cpu_graph() {
        use std::cell::Cell;
        #[derive(Debug, PartialEq)]
        enum G {
            Fp16,
            Fp32Cpu,
        }
        let source = DynamicImage::ImageRgb32F(gradient(1200, 900));
        for fp32_installed in [true, false] {
            let gpu_failed = AtomicBool::new(false);
            let fp32 = G::Fp32Cpu;
            let loads = Cell::new(0);
            let now = || {
                graph_now(
                    &G::Fp16,
                    None,
                    || {
                        loads.set(loads.get() + 1);
                        fp32_installed.then_some(&fp32)
                    },
                    true,
                    false,
                    gpu_failed.load(Ordering::Relaxed),
                )
            };
            assert_eq!(now(), &G::Fp16);
            assert_eq!(loads.get(), 0, "the fp32 graph loads only after a failure");
            let (fp16_tiles, cpu_tiles) = (Cell::new(0), Cell::new(0));
            let mut tile = |input: &[f32]| -> Result<Vec<f32>> {
                if now() == &G::Fp16 {
                    fp16_tiles.set(fp16_tiles.get() + 1);
                    let result = if fp16_tiles.get() == 3 {
                        Err(anyhow::anyhow!("GPU device lost"))
                    } else {
                        nearest(input)
                    };
                    recording_failure(result, &gpu_failed)
                } else {
                    cpu_tiles.set(cpu_tiles.get() + 1);
                    nearest(input)
                }
            };
            let out = on_one_path(&|| now() == &G::Fp16, &mut || {
                enlarge_ref(&source, &mut tile, &|| Ok(()))
            });
            assert!(gpu_failed.load(Ordering::Relaxed));
            if fp32_installed {
                assert_eq!(out.expect("the CPU pass finishes").width(), 2400);
                assert_eq!(fp16_tiles.get(), 3);
                assert!(cpu_tiles.get() >= 6, "{} CPU tiles", cpu_tiles.get());
            } else {
                assert!(out.unwrap_err().contains("GPU device lost"));
                assert_eq!(cpu_tiles.get(), 0);
            }
        }
    }

    #[test]
    fn fp16_graph_with_other_bytes_is_not_installed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(GPU_ONNX_FILE);
        assert!(!verified(&path, GPU_ONNX_SHA256));
        std::fs::write(&path, b"not the pinned graph").unwrap();
        assert!(!verified(&path, GPU_ONNX_SHA256));
        std::fs::write(&path, b"abc").unwrap();
        let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert!(verified(&path, abc));
    }

    #[test]
    fn fast_runs_the_single_graph_on_the_processor_full_runs_on() {
        let (graph, cpu, gpu) = ("full graph", "single on the CPU", "single on the GPU");
        let unused = || -> Option<&&str> { panic!("looked up a single graph") };
        // Full: the graph that runs now, whatever is installed.
        assert_eq!(
            for_quality(&graph, Quality::Full, true, unused, unused).unwrap(),
            &graph
        );
        assert_eq!(
            for_quality(&graph, Quality::Full, false, unused, unused).unwrap(),
            &graph
        );
        // Fast on the CPU and on a GPU.
        assert_eq!(
            for_quality(&graph, Quality::Fast, true, || Some(&cpu), unused).unwrap(),
            &cpu
        );
        assert_eq!(
            for_quality(&graph, Quality::Fast, false, unused, || Some(&gpu)).unwrap(),
            &gpu
        );
        // Not installed: the photo fails, never a Full result instead.
        let missing = for_quality(&graph, Quality::Fast, true, || None, unused).unwrap_err();
        assert!(missing.to_string().contains(SINGLE_ONNX_FILE), "{missing}");
        assert!(for_quality(&graph, Quality::Fast, false, unused, || None).is_err());
    }

    /// A photo whose GPU failed restarts on the CPU (`on_one_path`): a Fast
    /// recipe's second pass runs the single graph on the CPU, never the
    /// four-rotation graph.
    #[test]
    fn a_fast_photo_moved_from_the_gpu_to_the_cpu_stays_fast() {
        use std::cell::{Cell, RefCell};
        let source = DynamicImage::ImageRgb32F(gradient(1200, 900));
        let on_gpu = Cell::new(true);
        let gpu_tiles = Cell::new(0);
        let ran = RefCell::new(Vec::new());
        let (full, single_cpu, single_gpu) = ("full", "single on the CPU", "single on the GPU");
        let now = || {
            *for_quality(
                &full,
                Quality::Fast,
                !on_gpu.get(),
                || Some(&single_cpu),
                || Some(&single_gpu),
            )
            .unwrap()
        };
        let mut tile = |input: &[f32]| -> Result<Vec<f32>> {
            let graph = now();
            if graph == single_gpu {
                gpu_tiles.set(gpu_tiles.get() + 1);
                if gpu_tiles.get() == 3 {
                    on_gpu.set(false);
                    bail!("GPU device lost");
                }
            }
            ran.borrow_mut().push(graph);
            nearest(input)
        };
        let out = on_one_path(&now, &mut || enlarge_ref(&source, &mut tile, &|| Ok(())))
            .expect("the CPU pass finishes");
        assert_eq!(out.width(), 2400);
        let ran = ran.into_inner();
        assert!(ran.contains(&single_cpu));
        assert!(!ran.contains(&full), "a Fast photo ran the Full graph");
    }

    #[test]
    fn cpu_conversion_runs_only_while_the_original_is_on_the_cpu() {
        let (original, cpu) = ("original", "cpu conversion");
        assert_eq!(active_graph(&original, Some(&cpu), true), &cpu);
        // The idle GPU check moved the original to a GPU: it runs again.
        assert_eq!(active_graph(&original, Some(&cpu), false), &original);
        // No conversion in the pack: the original runs wherever it is.
        assert_eq!(active_graph(&original, None, true), &original);
    }
}
