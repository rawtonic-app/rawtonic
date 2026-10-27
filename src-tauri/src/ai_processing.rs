mod relight;
pub use rawtonic_engine::masks::{
    AiDepthMaskParameters, AiForegroundMaskParameters, AiPeopleMaskParameters, AiSkyMaskParameters,
    AiSubjectMaskParameters,
};
pub use relight::{
    SubjectContext, analyze_subject, get_or_init_people_normals_model, get_or_init_relight_model,
    run_relight_model,
};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use image::imageops::{self, FilterType};
use image::{DynamicImage, GenericImageView, GrayImage, ImageBuffer, Luma, Rgba, RgbaImage};
use ndarray::{Array, IxDyn};
use ort::value::Tensor;
use rayon::prelude::*;
use sha2::{Digest, Sha256};
use tauri::Emitter;
use tauri::Manager;
use tokenizers::Tokenizer;
use tokio::sync::Mutex as TokioMutex;

/// Rawtonic's model pack: files rebuilt from their authors' releases, with
/// licenses (docs/MODELS.md, scripts/models/build_model_pack.py);
/// `scripts/models/install_models.sh` installs them ahead with `gh`.
const MODEL_PACK_URL: &str =
    "https://github.com/rawtonic-app/models/releases/download/pack-2026-10-27b";

// SAM 2.1 Hiera-T (Meta, Apache-2.0 weights), exported by
// scripts/models/export_sam2.py: the photo stretched to 1024 x 1024 in,
// point/box masks out. It replaced SAM ViT-B on 2026-10-09 (docs/MODELS.md).
const ENCODER_FILENAME: &str = "sam2.1_tiny_encoder.onnx";
const DECODER_FILENAME: &str = "sam2.1_tiny_decoder.onnx";
const SAM_INPUT_SIZE: u32 = 1024;
/// Long edge of the decoder's first-pass mask, which only places the second
/// pass's prompt points (four pixels per cell of its 256 × 256 mask).
const SAM_FIRST_PASS_EDGE: u32 = 1024;
/// Long edge up to which the final mask and its edge refinement run at the
/// photo's size; above it they run on a grid that many times coarser and
/// the refinement's coefficients are applied to the full-size photo.
const SAM_REFINE_EDGE: u32 = 2048;
const ENCODER_SHA256: &str = "b6249b5dfb882d3eaf447d57273340005cd6efd9645c0004f86ab197a7170226";
const DECODER_SHA256: &str = "998c1cc053a2aad703fd2519fe890a1f80115b238a0da6d7b5fba70b210131d7";

// BiRefNet (Peng Zheng et al., MIT weights; the general checkpoint with a
// Swin-L backbone), exported by scripts/models/export_birefnet.py: the photo
// stretched to 1024 × 1024 in, the subject probability out. It replaced
// U²-Net: on 300 DUTS-TE images IoU 0.883 vs 0.715, boundary F 0.882 vs
// 0.693 (docs/MODELS.md).
const SUBJECT_FILENAME: &str = "birefnet_subject.onnx";
const SUBJECT_INPUT_SIZE: u32 = 1024;
const SUBJECT_SHA256: &str = "1019aa4d150a10830d95d05b0e0e08ed2703c39d3e168dea3333bb0272c16cbf";
// The same checkpoint as a Core ML program in fp16
// (scripts/models/convert_birefnet_coreml.py), macOS's fast path: about
// 0.2 s on the GPU instead of 4-5 s on the CPU. Against subject references
// on 36 public photos (hair, fur, feathers) its masks score as the fp32
// program's (IoU 0.9169 vs 0.9170), which took 0.5 s and held 4.4 GB during
// a run instead of 1.6 GB (docs/MODELS.md). Same input and output as the
// ONNX model. Its three files are put back together into the package Core
// ML compiles.
#[cfg(target_os = "macos")]
const SUBJECT_CORE_ML_PACKAGE: &str = "birefnet_fp16.mlpackage";
#[cfg(target_os = "macos")]
const SUBJECT_CORE_ML_FILES: [(&str, &str, &str); 3] = [
    (
        "birefnet_fp16_coreml_Manifest.json",
        "Manifest.json",
        "935b1ef0c97fe51b4999919c28b9c91151c648c72571c1d5c3c49dd6e9b3b184",
    ),
    (
        "birefnet_fp16_coreml_model.mlmodel",
        "Data/com.apple.CoreML/model.mlmodel",
        "5ca548c32b1427581c1011b65b60027686b925dfbb218db80b3084cfef27c544",
    ),
    (
        "birefnet_fp16_coreml_weight.bin",
        "Data/com.apple.CoreML/weights/weight.bin",
        "3e890026789c6dbe487caff5d28dde5df6baea9e4e117fc8071b07d88b1259ae",
    ),
];
/// Core ML's processors for the mask models (BiRefNet, sky): CPU and GPU.
/// With the Neural Engine BiRefNet's first load compiled for about 15
/// minutes and runs were no faster (docs/MODELS.md).
#[cfg(target_os = "macos")]
const CORE_ML_UNITS: rawtonic_denoise::coreml::ComputeUnits =
    rawtonic_denoise::coreml::ComputeUnits::CpuAndGpu;

// UperNet + ConvNeXt-T on ADE20K (Meta's checkpoint, MIT), exported by
// scripts/models/export_sky.py: the photo letterboxed into 512 × 512 in, the
// sky probability out.
const SKYSEG_FILENAME: &str = "skyseg_upernet_convnext_t.onnx";
const SKYSEG_INPUT_SIZE: u32 = 512;
const SKYSEG_SHA256: &str = "e5dc2a0bee8b4cdfbd866d8f811b4390d04ba621644d99de25478338a5b15bbf";
// The same checkpoint as a Core ML program (scripts/models/convert_sky_coreml.py,
// fp32), macOS's fast path: about 0.06 s on the GPU instead of 0.35-0.8 s
// on the CPU, the same probabilities as the PyTorch model (the ONNX file
// stores fp16 weights: masks within IoU 0.993). Same input and output as the
// ONNX model.
#[cfg(target_os = "macos")]
const SKY_CORE_ML_PACKAGE: &str = "skyseg_upernet.mlpackage";
#[cfg(target_os = "macos")]
const SKY_CORE_ML_FILES: [(&str, &str, &str); 3] = [
    (
        "skyseg_coreml_Manifest.json",
        "Manifest.json",
        "935b1ef0c97fe51b4999919c28b9c91151c648c72571c1d5c3c49dd6e9b3b184",
    ),
    (
        "skyseg_coreml_model.mlmodel",
        "Data/com.apple.CoreML/model.mlmodel",
        "514793240a846abdee20271682172fc7da03c4642e5d9fff37f3edd18502aec5",
    ),
    (
        "skyseg_coreml_weight.bin",
        "Data/com.apple.CoreML/weights/weight.bin",
        "33bf57800ebd7ccd2005133e997dd064c8b59d0e7c8cac99d8a69728642206f3",
    ),
];
/// Long edge of the photo that guides the sky mask's edges.
const SKY_GUIDE_SIZE: u32 = 2048;

// SigLIP 2 B/16 for automatic tags (scripts/models/export_siglip2.py). The
// built-in tags need only the image encoder and their precomputed text
// embeddings; the text encoder and tokenizer load for the user's own tags.
const SIGLIP_VISION_FILENAME: &str = "siglip2_vision.onnx";
const SIGLIP_VISION_SHA256: &str =
    "d6e4f91fd3779a524d912eef6113ccc816e2c01044e2e27cc166493b0e730fe8";
const SIGLIP_TAGS_FILENAME: &str = "siglip2_tags.bin";
const SIGLIP_TAGS_SHA256: &str = "625a8fd388c3cc762ce20818df1cacf478b6d6747a0c3664c9fe13776479c449";
const SIGLIP_TEXT_FILENAME: &str = "siglip2_text.onnx";
const SIGLIP_TEXT_SHA256: &str = "1ee7842ea9a96bf8e19951d237db8ee00ca482bf1745027967be976420acc557";
const SIGLIP_TOKENIZER_FILENAME: &str = "siglip2_tokenizer.json";
const SIGLIP_TOKENIZER_SHA256: &str =
    "1de36bea24b2b16b25cc27a4b5ea9c86e2e2ecc3c018cb7d5338f0030d84e4bb";
/// Width of a SigLIP 2 B/16 embedding.
pub const SIGLIP_EMBED_DIM: usize = 768;

// MI-GAN (Picsart AI Research, MIT weights): the authors' ONNX pipeline,
// which crops around the hole, inpaints at 512 × 512 and blends back.
const INPAINT_FILENAME: &str = "migan_pipeline_v2.onnx";
const INPAINT_SHA256: &str = "6f1f3530a1a2324b19752018ce756088b07973cda8d7d890034ace5c8a48c40b";

const DEPTH_FILENAME: &str = "depth_anything_v2_vits.onnx";
const DEPTH_INPUT_SIZE: u32 = 518;
const DEPTH_SHA256: &str = "d2b11a11c1d4a12b47608fa65a17ee9a4c605b55ee1730c8e3b526304f2562be";

/// SAM 2.1 for object (click and box) masks and Quick Erase, loaded on its
/// own: a first object mask after a launch waits only for these 64 MB, not
/// for BiRefNet (470 MB), sky and depth.
pub struct SamModels {
    pub encoder: rawtonic_onnx::Model,
    pub decoder: rawtonic_onnx::Model,
}

pub struct TagModels {
    pub vision: rawtonic_onnx::Model,
    /// `SIGLIP_EMBED_DIM` values per entry of `TAG_CANDIDATES`, in its order.
    pub tag_embeddings: Vec<f32>,
    /// Loaded only when the user has their own tags.
    pub text: Option<TextEncoder>,
}

pub struct TextEncoder {
    pub session: rawtonic_onnx::Model,
    pub tokenizer: Tokenizer,
}

#[derive(Clone)]
pub struct ImageEmbeddings {
    pub path_hash: String,
    /// SAM 2's image embedding [1, 256, 64, 64].
    pub embeddings: Array<f32, IxDyn>,
    /// SAM 2's high-resolution features [1, 32, 256, 256] and [1, 64, 128, 128].
    pub high_res_feats: [Array<f32, IxDyn>; 2],
    pub original_size: (u32, u32),
}

/// BiRefNet for Subject and Background masks, loaded on its own (470 MB):
/// opening the masking tools loads and runs it in the background without
/// loading sky and depth.
pub struct SubjectModel {
    backend: SubjectBackend,
}

enum SubjectBackend {
    Onnx(rawtonic_onnx::Model),
    #[cfg(target_os = "macos")]
    CoreMl(rawtonic_denoise::coreml::CoreMlModel),
}

/// The sky model: UperNet on ONNX Runtime (every platform) or, on macOS
/// with its files installed, on Core ML.
pub struct SkyModel {
    backend: SkyBackend,
}

enum SkyBackend {
    Onnx(rawtonic_onnx::Model),
    #[cfg(target_os = "macos")]
    CoreMl(rawtonic_denoise::coreml::CoreMlModel),
}

impl SkyModel {
    /// The ONNX model on ONNX Runtime: a GPU provider when one works, else
    /// the CPU.
    pub fn onnx(path: &Path) -> Result<Self> {
        Ok(Self {
            backend: SkyBackend::Onnx(rawtonic_onnx::Model::load(
                path,
                rawtonic_onnx::ModelKind::Sky,
            )?),
        })
    }

    pub fn backend_name(&self) -> String {
        match &self.backend {
            SkyBackend::Onnx(model) => format!("ONNX Runtime ({})", model.provider()),
            #[cfg(target_os = "macos")]
            SkyBackend::CoreMl(_) => "Core ML".to_string(),
        }
    }

    /// Runs on ONNX Runtime (Core ML needs none).
    pub fn on_onnx(&self) -> bool {
        matches!(self.backend, SkyBackend::Onnx(_))
    }

    /// As `SubjectModel::warm_up`.
    pub fn warm_up(&self) -> Result<bool> {
        match &self.backend {
            SkyBackend::Onnx(_) => Ok(false),
            #[cfg(target_os = "macos")]
            SkyBackend::CoreMl(model) => {
                let size = SKYSEG_INPUT_SIZE as usize;
                model.warm_up("pixel_values", &[1, 3, size, size], "sky")?;
                Ok(true)
            }
        }
    }
    /// The sky probability (`SKYSEG_INPUT_SIZE`² values) for a normalised
    /// [1, 3, size, size] input. `run_options` lets another thread stop an
    /// ONNX run.
    fn probability(
        &self,
        input: Array<f32, ndarray::Ix4>,
        run_options: Option<&ort::session::RunOptions>,
    ) -> Result<Vec<f32>> {
        match &self.backend {
            SkyBackend::Onnx(model) => Ok(run_single(model, &input.into_dyn(), run_options)?
                .into_raw_vec_and_offset()
                .0),
            #[cfg(target_os = "macos")]
            SkyBackend::CoreMl(model) => {
                let shape = input.shape().to_vec();
                let data = input.into_raw_vec_and_offset().0;
                let values = model.predict("pixel_values", &shape, &data, "sky")?.1;
                rawtonic_onnx::ensure_finite(&values)?;
                Ok(values)
            }
        }
    }
}

/// A finished Subject or Sky mask: the network's probability at the size of
/// the photo with its geometry, as the PNG data URL the recipe stores.
#[derive(Clone)]
pub struct CachedMask {
    /// The photo and its geometry (`ai_commands::geometry_key`).
    pub key: String,
    pub mask_data_base64: Arc<String>,
}

/// Subject and Sky masks kept, each (the photos last worked on, at about
/// 0.1-2 MB each).
pub const AI_MASKS_KEPT: usize = 4;

#[derive(Clone)]
pub struct CachedDepthMap {
    pub path_hash: String,
    pub depth_image: GrayImage,
    pub original_size: (u32, u32),
}

#[derive(Default)]
pub struct AiState {
    /// UperNet for Sky masks, loaded on its own (119 MB): Sky does not wait
    /// for the depth model.
    pub sky_model: Option<Arc<SkyModel>>,
    pub depth_model: Option<Arc<rawtonic_onnx::Model>>,
    pub relight_model: Option<Arc<rawtonic_onnx::Model>>,
    /// DAViD people normals for Relight (`relight::run_people_normals`).
    pub people_normals_model: Option<Arc<rawtonic_onnx::Model>>,
    pub sam_models: Option<Arc<SamModels>>,
    pub tag_models: Option<Arc<TagModels>>,
    pub inpaint_model: Option<Arc<rawtonic_onnx::Model>>,
    /// Moebius, the guide of Remove's large holes (`moebius`).
    pub moebius: Option<Arc<crate::moebius::Moebius>>,
    pub embeddings: Option<ImageEmbeddings>,
    pub depth_map: Option<CachedDepthMap>,
    pub subject_model: Option<Arc<SubjectModel>>,
    /// Finished Subject masks, newest last (`AI_MASKS_KEPT`).
    pub subject_masks: Vec<CachedMask>,
    /// Finished Sky masks, newest last (`AI_MASKS_KEPT`).
    pub sky_masks: Vec<CachedMask>,
}

/// Puts a freshly loaded model into the state with `store`, unless the
/// models were unloaded since its load began (`generation`): then its caller
/// uses it once and the next use loads again (`ai_lifecycle`).
fn keep_model(
    ai_state_mutex: &Mutex<Option<AiState>>,
    generation: u64,
    store: impl FnOnce(&mut AiState),
) {
    let mut ai = ai_state_mutex.lock().unwrap();
    if crate::ai_lifecycle::LIFECYCLE.keep(generation) {
        store(ai.get_or_insert_with(AiState::default));
    } else {
        log::info!("AI: a model loaded during an unload is not kept");
    }
}

fn edt_1d(f: &mut [f32], v: &mut [usize], z: &mut [f32], d: &mut [f32]) {
    let n = f.len();
    if n == 0 {
        return;
    }
    let mut k = 0;
    v[0] = 0;
    z[0] = f32::NEG_INFINITY;
    z[1] = f32::INFINITY;
    for q in 1..n {
        let mut s = ((f[q] + (q * q) as f32) - (f[v[k]] + (v[k] * v[k]) as f32))
            / (2.0 * (q as f32 - v[k] as f32));
        while s <= z[k] {
            if k == 0 {
                break;
            }
            k -= 1;
            s = ((f[q] + (q * q) as f32) - (f[v[k]] + (v[k] * v[k]) as f32))
                / (2.0 * (q as f32 - v[k] as f32));
        }
        k += 1;
        v[k] = q;
        z[k] = s;
        z[k + 1] = f32::INFINITY;
    }
    k = 0;
    for (q, d_q) in d[..n].iter_mut().enumerate() {
        while z[k + 1] < q as f32 {
            k += 1;
        }
        let diff = q as f32 - v[k] as f32;
        *d_q = diff * diff + f[v[k]];
    }
    f.copy_from_slice(&d[..n]);
}

/// Euclidean distance of every `true` cell of `grid` to the nearest `false`
/// one (0 for `false` cells).
pub(crate) fn edt_2d(grid: &[bool], width: usize, height: usize) -> Vec<f32> {
    let area = width * height;
    let mut f = vec![0.0; area];
    for i in 0..area {
        f[i] = if grid[i] { 1e10 } else { 0.0 };
    }

    let max_dim = width.max(height);
    let mut v = vec![0; max_dim];
    let mut z = vec![0.0; max_dim + 1];
    let mut d = vec![0.0; max_dim];

    for y in 0..height {
        let start = y * width;
        let end = start + width;
        edt_1d(&mut f[start..end], &mut v, &mut z, &mut d);
    }

    let mut col = vec![0.0; height];
    for x in 0..width {
        for y in 0..height {
            col[y] = f[y * width + x];
        }
        edt_1d(&mut col, &mut v, &mut z, &mut d);
        for y in 0..height {
            f[y * width + x] = col[y];
        }
    }

    f.into_iter().map(|v| v.sqrt()).collect()
}

fn box_filter_horiz(src: &[f32], dest: &mut [f32], w: usize, r: usize) {
    dest.par_chunks_exact_mut(w)
        .zip(src.par_chunks_exact(w))
        .for_each(|(dest_row, src_row)| {
            let mut prefix = vec![0.0; w + 1];
            for x in 0..w {
                prefix[x + 1] = prefix[x] + src_row[x];
            }
            for (x, dest_val) in dest_row.iter_mut().enumerate() {
                let x_min = x.saturating_sub(r);
                let x_max = (x + r).min(w - 1);
                let count = (x_max - x_min + 1) as f32;
                *dest_val = (prefix[x_max + 1] - prefix[x_min]) / count;
            }
        });
}

fn transpose(src: &[f32], dest: &mut [f32], w: usize, h: usize) {
    dest.par_chunks_exact_mut(h)
        .enumerate()
        .for_each(|(x, dest_col)| {
            for y in 0..h {
                dest_col[y] = src[y * w + x];
            }
        });
}

fn box_filter_opt(
    src: &[f32],
    dest: &mut [f32],
    temp1: &mut [f32],
    temp2: &mut [f32],
    w: usize,
    h: usize,
    r: usize,
) {
    box_filter_horiz(src, temp1, w, r);
    transpose(temp1, temp2, w, h);
    box_filter_horiz(temp2, temp1, h, r);
    transpose(temp1, dest, h, w);
}

pub fn fast_guided_filter(
    guide_hr: &GrayImage,
    mask_lr: &GrayImage,
    r: usize,
    eps: f32,
) -> GrayImage {
    let (hr_w, hr_h) = guide_hr.dimensions();
    let (lr_w, lr_h) = mask_lr.dimensions();

    let w = lr_w as usize;
    let h = lr_h as usize;
    let area = w * h;

    let guide_lr = resize_gray_triangle(guide_hr, lr_w, lr_h);

    let mut i_lr = vec![0.0; area];
    let mut p_lr = vec![0.0; area];
    let mut ip_lr = vec![0.0; area];
    let mut ii_lr = vec![0.0; area];

    let guide_lr_raw = guide_lr.as_raw();
    let mask_lr_raw = mask_lr.as_raw();

    i_lr.par_iter_mut()
        .zip(p_lr.par_iter_mut())
        .zip(ip_lr.par_iter_mut())
        .zip(ii_lr.par_iter_mut())
        .zip(guide_lr_raw.par_iter())
        .zip(mask_lr_raw.par_iter())
        .for_each(|(((((i_out, p_out), ip_out), ii_out), &g_val), &m_val)| {
            let i_val = g_val as f32 / 255.0;
            let p_val = m_val as f32 / 255.0;
            *i_out = i_val;
            *p_out = p_val;
            *ip_out = i_val * p_val;
            *ii_out = i_val * i_val;
        });

    let mut temp1 = vec![0.0; area];
    let mut temp2 = vec![0.0; area];

    let mut mean_i = vec![0.0; area];
    let mut mean_p = vec![0.0; area];
    let mut mean_ip = vec![0.0; area];
    let mut mean_ii = vec![0.0; area];

    box_filter_opt(&i_lr, &mut mean_i, &mut temp1, &mut temp2, w, h, r);
    box_filter_opt(&p_lr, &mut mean_p, &mut temp1, &mut temp2, w, h, r);
    box_filter_opt(&ip_lr, &mut mean_ip, &mut temp1, &mut temp2, w, h, r);
    box_filter_opt(&ii_lr, &mut mean_ii, &mut temp1, &mut temp2, w, h, r);

    let mut a = vec![0.0; area];
    let mut b = vec![0.0; area];

    a.par_iter_mut()
        .zip(b.par_iter_mut())
        .zip(mean_i.par_iter())
        .zip(mean_p.par_iter())
        .zip(mean_ip.par_iter())
        .zip(mean_ii.par_iter())
        .for_each(|(((((a_out, b_out), &m_i), &m_p), &m_ip), &m_ii)| {
            let cov_ip = m_ip - m_i * m_p;
            let var_i = m_ii - m_i * m_i;
            let a_val = cov_ip / (var_i + eps);
            *a_out = a_val;
            *b_out = m_p - a_val * m_i;
        });

    let mut mean_a = vec![0.0; area];
    let mut mean_b = vec![0.0; area];

    box_filter_opt(&a, &mut mean_a, &mut temp1, &mut temp2, w, h, r);
    box_filter_opt(&b, &mut mean_b, &mut temp1, &mut temp2, w, h, r);

    // `imageops::resize` of a float image, in parallel: the same values,
    // clamped to 0..1 as the image crate clamps float pixels.
    let upsample = |plane: &[f32]| {
        resize_triangle::<1, f32>(plane, (lr_w, lr_h), (hr_w, hr_h), (0.0, 1.0), |v| v)
    };
    let mean_a_hr = upsample(&mean_a);
    let mean_b_hr = upsample(&mean_b);

    let mean_a_hr_raw = &mean_a_hr;
    let mean_b_hr_raw = &mean_b_hr;
    let guide_hr_raw = guide_hr.as_raw();

    let mut final_mask_raw = vec![0u8; (hr_w * hr_h) as usize];

    final_mask_raw
        .par_iter_mut()
        .zip(mean_a_hr_raw.par_iter())
        .zip(mean_b_hr_raw.par_iter())
        .zip(guide_hr_raw.par_iter())
        .for_each(|(((out, &a_val), &b_val), &i_val)| {
            let i_v = i_val as f32 / 255.0;
            let q = a_val * i_v + b_val;
            *out = (q.clamp(0.0, 1.0) * 255.0).round() as u8;
        });

    GrayImage::from_raw(hr_w, hr_h, final_mask_raw).unwrap()
}

/// The app's model folder (`<app data>/models`).
pub(crate) fn models_dir(app_handle: &tauri::AppHandle) -> Result<PathBuf> {
    get_models_dir(app_handle)
}

/// GPU execution providers for the AI models (rawtonic-onnx): the plugin
/// libraries bundled beside ONNX Runtime in `resources` (WebGPU), then the
/// model folder (where `install_models.sh --cuda` puts NVIDIA's CUDA
/// plugin). `gpu` is the preference; off, every model runs on the CPU.
/// Nothing is downloaded here.
pub fn configure_acceleration(app_handle: &tauri::AppHandle, resources: &Path, gpu: bool) {
    let mut plugin_dirs = rawtonic_onnx::default_plugin_dirs();
    plugin_dirs.push(resources.to_path_buf());
    if let Ok(models) = app_handle.path().app_data_dir().map(|d| d.join("models")) {
        plugin_dirs.push(models);
    }
    plugin_dirs.dedup();
    // Runs before the logger is set up, like the ORT_DYLIB_PATH line.
    println!(
        "AI: GPU providers {} (plugin folders {:?})",
        if gpu { "allowed" } else { "off by preference" },
        plugin_dirs
    );
    // A model runs on a GPU only after its output there matched the CPU's
    // once; the verdicts are kept per device.
    let checks_file = app_handle
        .path()
        .app_data_dir()
        .ok()
        .map(|d| d.join("ai-gpu-checks.tsv"));
    rawtonic_onnx::configure(rawtonic_onnx::Settings {
        gpu,
        plugin_dirs,
        checks_file,
    });
}

/// The local ONNX Runtime, or an error saying it is missing or broken.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub(crate) fn ensure_runtime() -> Result<()> {
    ensure_local_runtime()
}

fn get_models_dir(app_handle: &tauri::AppHandle) -> Result<PathBuf> {
    let models_dir = app_handle.path().app_data_dir()?.join("models");
    if !models_dir.exists() {
        fs::create_dir_all(&models_dir)?;
    }
    Ok(models_dir)
}

/// Writes a verified download to `dest`: a temporary file of its own
/// renamed over the old one (never deleted first, never a name another
/// load or the CLI writes at the same time).
fn persist_downloaded_asset(dest: &Path, bytes: &[u8]) -> Result<()> {
    if bytes.is_empty() {
        return Err(anyhow::anyhow!(
            "Downloaded asset for {} was empty",
            dest.display()
        ));
    }
    let parent = dest.parent().ok_or_else(|| {
        anyhow::anyhow!(
            "Cannot determine parent directory for downloaded asset {}",
            dest.display()
        )
    })?;
    fs::create_dir_all(parent)?;
    rawtonic_metadata::atomic_write(dest, bytes)
}

/// The download URL of a file of the model pack.
fn model_url(source: &str) -> String {
    format!("{MODEL_PACK_URL}/{source}")
}

/// Downloads `source`, calling `progress(received, total)` as its bytes
/// arrive (`total` 0 when the server does not say).
async fn fetch_model(source: &str, progress: &mut (dyn FnMut(u64, u64) + Send)) -> Result<Vec<u8>> {
    let url = model_url(source);
    let mut response = reqwest::get(&url).await?;
    if !response.status().is_success() {
        return Err(anyhow::anyhow!(
            "Could not download {url} (HTTP {}). Check the internet connection and try again.",
            response.status()
        ));
    }
    let total = response.content_length().unwrap_or(0);
    let mut bytes = Vec::with_capacity(total as usize);
    while let Some(chunk) = response.chunk().await? {
        bytes.extend_from_slice(&chunk);
        progress(bytes.len() as u64, total);
    }
    Ok(bytes)
}

/// Replaces `dest` only with a download whose SHA-256 matches: a failed or
/// corrupt download leaves the file that was there.
async fn download_verified_model(
    source: &str,
    dest: &Path,
    expected_hash: &str,
    progress: &mut (dyn FnMut(u64, u64) + Send),
) -> Result<()> {
    let bytes = fetch_model(source, progress).await?;
    persist_verified_asset(dest, &bytes, expected_hash)
        .map_err(|e| anyhow::anyhow!("{}: {e}", model_url(source)))
}

fn persist_verified_asset(dest: &Path, bytes: &[u8], expected_hash: &str) -> Result<()> {
    let hash = hex::encode(Sha256::digest(bytes));
    if hash != expected_hash {
        return Err(anyhow::anyhow!(
            "download has SHA-256 {hash}, expected {expected_hash}"
        ));
    }
    persist_downloaded_asset(dest, bytes)
}

/// `true` when the installed model file at `path` has the SHA-256
/// `expected_hash`. A file is read and hashed once; after that, as long as it
/// looks the same (`verified_files`: size, modification and change time,
/// inode), the earlier result stands. Hashing every launch cost about 1 s
/// for BiRefNet's Core ML weights (then 927 MB) alone.
pub(crate) fn verify_sha256(path: &Path, expected_hash: &str) -> Result<bool> {
    let Ok(meta) = fs::metadata(path) else {
        return Ok(false);
    };
    if !meta.is_file() {
        return Ok(false);
    }
    let seen = verified_files::Fingerprint::of(&meta);
    if verified_files::known(path, expected_hash, &seen) {
        return Ok(true);
    }
    let start = std::time::Instant::now();
    let matches = file_has_sha256(path, expected_hash)?;
    log::info!(
        "AI: SHA-256 of {} {} in {} ms",
        path.display(),
        if matches { "verified" } else { "differs" },
        start.elapsed().as_millis()
    );
    if matches {
        verified_files::record(path, expected_hash, seen);
    } else {
        verified_files::forget(path);
    }
    Ok(matches)
}

/// Results of `verify_sha256`, kept per models folder in
/// `verified-files.tsv` (written atomically) and in memory.
///
/// What a file looked like when it was hashed: its size, modification time,
/// status-change time and inode/device (Unix; creation time on Windows).
/// Writing to a file or replacing it changes these; the status-change time
/// cannot be set back by tools that preserve modification times, so a file
/// changed after the check is hashed again before use.
mod verified_files {
    use std::collections::HashMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    pub const FILE_NAME: &str = "verified-files.tsv";

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Fingerprint {
        len: u64,
        modified_ns: i128,
        changed_ns: i128,
        inode: u64,
        device: u64,
    }

    impl Fingerprint {
        pub fn of(meta: &fs::Metadata) -> Self {
            let ns = |t: std::io::Result<std::time::SystemTime>| {
                t.ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_nanos() as i128)
            };
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                Self {
                    len: meta.len(),
                    modified_ns: ns(meta.modified()),
                    changed_ns: meta.ctime() as i128 * 1_000_000_000 + meta.ctime_nsec() as i128,
                    inode: meta.ino(),
                    device: meta.dev(),
                }
            }
            #[cfg(not(unix))]
            {
                Self {
                    len: meta.len(),
                    modified_ns: ns(meta.modified()),
                    changed_ns: ns(meta.created()),
                    inode: 0,
                    device: 0,
                }
            }
        }

        fn fields(&self) -> String {
            format!(
                "{}\t{}\t{}\t{}\t{}",
                self.len, self.modified_ns, self.changed_ns, self.inode, self.device
            )
        }

        fn parse(fields: &[&str]) -> Option<Self> {
            Some(Self {
                len: fields.first()?.parse().ok()?,
                modified_ns: fields.get(1)?.parse().ok()?,
                changed_ns: fields.get(2)?.parse().ok()?,
                inode: fields.get(3)?.parse().ok()?,
                device: fields.get(4)?.parse().ok()?,
            })
        }
    }

    /// Per folder: file name → (SHA-256, fingerprint when it was hashed).
    type Folder = HashMap<String, (String, Fingerprint)>;
    static FOLDERS: Mutex<Option<HashMap<PathBuf, Folder>>> = Mutex::new(None);

    fn split(path: &Path) -> Option<(PathBuf, String)> {
        Some((
            path.parent()?.to_path_buf(),
            path.file_name()?.to_str()?.to_string(),
        ))
    }

    fn load(dir: &Path) -> Folder {
        let Ok(text) = fs::read_to_string(dir.join(FILE_NAME)) else {
            return Folder::new();
        };
        text.lines()
            .filter_map(|line| {
                let fields: Vec<&str> = line.split('\t').collect();
                let (sha, rest) = fields.split_first()?;
                let (name, rest) = rest.split_last()?;
                Some((
                    name.to_string(),
                    (sha.to_string(), Fingerprint::parse(rest)?),
                ))
            })
            .collect()
    }

    fn save(dir: &Path, folder: &Folder) {
        let mut lines: Vec<String> = folder
            .iter()
            .map(|(name, (sha, seen))| format!("{sha}\t{}\t{name}", seen.fields()))
            .collect();
        lines.sort();
        let text = lines.join("\n") + "\n";
        if let Err(e) = rawtonic_metadata::atomic_write(&dir.join(FILE_NAME), text) {
            log::warn!("AI: {FILE_NAME} not saved in {}: {e}", dir.display());
        }
    }

    fn with_folder<T>(dir: &Path, f: impl FnOnce(&mut Folder) -> T) -> T {
        let mut folders = FOLDERS.lock().unwrap_or_else(|e| e.into_inner());
        let folders = folders.get_or_insert_with(HashMap::new);
        let folder = folders
            .entry(dir.to_path_buf())
            .or_insert_with(|| load(dir));
        f(folder)
    }

    /// `true` when `path` was hashed to `sha` and looks the same now.
    pub fn known(path: &Path, sha: &str, seen: &Fingerprint) -> bool {
        let Some((dir, name)) = split(path) else {
            return false;
        };
        with_folder(&dir, |folder| {
            folder
                .get(&name)
                .is_some_and(|(known_sha, known)| known_sha == sha && known == seen)
        })
    }

    pub fn record(path: &Path, sha: &str, seen: Fingerprint) {
        let Some((dir, name)) = split(path) else {
            return;
        };
        with_folder(&dir, |folder| {
            if folder.get(&name) == Some(&(sha.to_string(), seen.clone())) {
                return;
            }
            folder.insert(name, (sha.to_string(), seen));
            save(&dir, folder);
        });
    }

    /// Keeps a file's recorded check after a change of its metadata alone
    /// (a new hard link): only right after `path` was checked against `sha`.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub fn refresh(path: &Path, sha: &str) {
        if let Ok(meta) = fs::metadata(path) {
            record(path, sha, Fingerprint::of(&meta));
        }
    }

    pub fn forget(path: &Path) {
        let Some((dir, name)) = split(path) else {
            return;
        };
        with_folder(&dir, |folder| {
            if folder.remove(&name).is_some() {
                save(&dir, folder);
            }
        });
    }

    /// Drops what is kept in memory (tests: a new launch).
    #[cfg(test)]
    pub fn reset_memory() {
        *FOLDERS.lock().unwrap() = None;
    }
}

/// Reads and hashes the whole file.
fn file_has_sha256(path: &Path, expected_hash: &str) -> Result<bool> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 1 << 20];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    let hash = hasher.finalize();
    let hex_hash = hex::encode(hash);
    Ok(hex_hash == expected_hash)
}

/// Downloads `file` of the model pack into `models_dir` unless it is there
/// with `expected_hash`.
pub(crate) async fn download_verified(
    app_handle: &tauri::AppHandle,
    models_dir: &Path,
    file: &str,
    expected_hash: &str,
    model_name: &str,
) -> Result<()> {
    let dest_path = models_dir.join(file);
    let is_valid = verify_sha256(&dest_path, expected_hash)?;

    if !is_valid {
        if dest_path.exists() {
            println!("Model {} has incorrect hash. Re-downloading.", model_name);
        }
        let _ = app_handle.emit("ai-model-download-start", model_name);
        // At most one event per percent.
        let mut shown = u64::MAX;
        let mut progress = |received: u64, total: u64| {
            let percent = (received * 100).checked_div(total).unwrap_or(0);
            if percent != shown {
                shown = percent;
                let _ = app_handle.emit(
                    "ai-model-download-progress",
                    serde_json::json!({ "model": model_name, "file": file, "received": received, "total": total }),
                );
            }
        };
        let download_result =
            download_verified_model(file, &dest_path, expected_hash, &mut progress).await;
        let _ = app_handle.emit("ai-model-download-finish", model_name);
        download_result?;
    }
    Ok(())
}

/// Serialises loading the sky model, apart from `ai_init_lock`: a Sky click
/// does not wait for BiRefNet's Core ML load (1-3 s) or SAM's.
static SKY_INIT_LOCK: TokioMutex<()> = TokioMutex::const_new(());

/// The sky model (UperNet), loaded on its own. With `download` false (a
/// background request) only an installed model is loaded: `None` when it
/// is not installed or the runtime is missing.
pub async fn get_or_init_sky_model(
    app_handle: &tauri::AppHandle,
    ai_state_mutex: &Mutex<Option<AiState>>,
    download: bool,
) -> Result<Option<Arc<SkyModel>>> {
    let loaded = || {
        ai_state_mutex
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|state| state.sky_model.clone())
    };
    if let Some(model) = loaded() {
        return Ok(Some(model));
    }
    let _guard = SKY_INIT_LOCK.lock().await;
    if let Some(model) = loaded() {
        return Ok(Some(model));
    }
    let generation = crate::ai_lifecycle::LIFECYCLE.generation();
    let models_dir = get_models_dir(app_handle)?;
    let path = models_dir.join(SKYSEG_FILENAME);
    let start = std::time::Instant::now();
    // Core ML on macOS: its files (in the model pack) are fetched on a
    // request that may download, like the ONNX model elsewhere.
    #[cfg(target_os = "macos")]
    if let Some(model) = load_core_ml(
        app_handle,
        &models_dir,
        SKY_CORE_ML_PACKAGE,
        &SKY_CORE_ML_FILES,
        download,
        "sky",
        "Sky Model",
    )
    .await
    {
        log::info!(
            "AI sky mask: Core ML model loaded in {} ms",
            start.elapsed().as_millis()
        );
        let model = Arc::new(SkyModel {
            backend: SkyBackend::CoreMl(model),
        });
        keep_model(ai_state_mutex, generation, |ai| {
            ai.sky_model = Some(model.clone())
        });
        return Ok(Some(model));
    }
    if !download && ensure_local_runtime().is_err() {
        return Ok(None);
    }
    ensure_local_runtime()?;
    if download {
        download_verified(
            app_handle,
            &models_dir,
            SKYSEG_FILENAME,
            SKYSEG_SHA256,
            "Sky Model",
        )
        .await?;
    } else if !verify_sha256(&path, SKYSEG_SHA256)? {
        return Ok(None);
    }
    let model = tokio::task::spawn_blocking(move || SkyModel::onnx(&path)).await??;
    log::info!(
        "AI sky mask: model loaded on {} in {} ms",
        model.backend_name(),
        start.elapsed().as_millis()
    );
    crate::register_exit_handler();
    let model = Arc::new(model);
    keep_model(ai_state_mutex, generation, |ai| {
        ai.sky_model = Some(model.clone())
    });
    Ok(Some(model))
}

/// Depth Anything V2 for depth masks, downloaded and loaded on first use.
pub async fn get_or_init_depth_model(
    app_handle: &tauri::AppHandle,
    ai_state_mutex: &Mutex<Option<AiState>>,
    ai_init_lock: &TokioMutex<()>,
) -> Result<Arc<rawtonic_onnx::Model>> {
    ensure_local_runtime()?;
    let loaded = || {
        ai_state_mutex
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|state| state.depth_model.clone())
    };
    if let Some(model) = loaded() {
        return Ok(model);
    }
    let _guard = ai_init_lock.lock().await;
    if let Some(model) = loaded() {
        return Ok(model);
    }
    let generation = crate::ai_lifecycle::LIFECYCLE.generation();
    let models_dir = get_models_dir(app_handle)?;
    download_verified(
        app_handle,
        &models_dir,
        DEPTH_FILENAME,
        DEPTH_SHA256,
        "Depth Model",
    )
    .await?;
    let path = models_dir.join(DEPTH_FILENAME);
    let model = tokio::task::spawn_blocking(move || {
        rawtonic_onnx::Model::load(&path, rawtonic_onnx::ModelKind::Depth)
    })
    .await??;
    crate::register_exit_handler();
    let model = Arc::new(model);
    keep_model(ai_state_mutex, generation, |ai| {
        ai.depth_model = Some(model.clone())
    });
    Ok(model)
}

/// Loads SAM 2.1 ahead of the first object mask when its files are already
/// installed (never downloads).
pub async fn preload_sam_models(
    app_handle: &tauri::AppHandle,
    ai_state_mutex: &Mutex<Option<AiState>>,
    ai_init_lock: &TokioMutex<()>,
) -> Result<()> {
    let already = ai_state_mutex
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|state| state.sam_models.is_some());
    if already || ensure_local_runtime().is_err() {
        return Ok(());
    }
    let models_dir = get_models_dir(app_handle)?;
    let installed = verify_sha256(&models_dir.join(ENCODER_FILENAME), ENCODER_SHA256)?
        && verify_sha256(&models_dir.join(DECODER_FILENAME), DECODER_SHA256)?;
    if !installed {
        return Ok(());
    }
    get_or_init_sam_models(app_handle, ai_state_mutex, ai_init_lock).await?;
    Ok(())
}

/// SAM 2.1's encoder and decoder (`SamModels`), downloaded and loaded on
/// first use.
pub async fn get_or_init_sam_models(
    app_handle: &tauri::AppHandle,
    ai_state_mutex: &Mutex<Option<AiState>>,
    ai_init_lock: &TokioMutex<()>,
) -> Result<Arc<SamModels>> {
    ensure_local_runtime()?;
    let loaded = || {
        ai_state_mutex
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|state| state.sam_models.clone())
    };
    if let Some(models) = loaded() {
        return Ok(models);
    }
    let _guard = ai_init_lock.lock().await;
    if let Some(models) = loaded() {
        return Ok(models);
    }
    let generation = crate::ai_lifecycle::LIFECYCLE.generation();
    let models_dir = get_models_dir(app_handle)?;
    for (file, hash, name) in [
        (ENCODER_FILENAME, ENCODER_SHA256, "SAM Encoder"),
        (DECODER_FILENAME, DECODER_SHA256, "SAM Decoder"),
    ] {
        download_verified(app_handle, &models_dir, file, hash, name).await?;
    }
    let start = std::time::Instant::now();
    let models = Arc::new(SamModels {
        encoder: rawtonic_onnx::Model::load(
            &models_dir.join(ENCODER_FILENAME),
            rawtonic_onnx::ModelKind::SamEncoder,
        )?,
        decoder: rawtonic_onnx::Model::load(
            &models_dir.join(DECODER_FILENAME),
            rawtonic_onnx::ModelKind::SamDecoder,
        )?,
    });
    log::info!(
        "AI object mask: models loaded in {} ms",
        start.elapsed().as_millis()
    );
    crate::register_exit_handler();
    keep_model(ai_state_mutex, generation, |ai| {
        ai.sam_models = Some(models.clone())
    });
    Ok(models)
}

/// Whether the Subject (`sky` false) or Sky model is installed, so that a
/// background request would load it: its Core ML files (macOS), else its
/// ONNX model and the runtime. Files already checked are only looked at.
pub fn mask_model_installed(app_handle: &tauri::AppHandle, sky: bool) -> bool {
    let Ok(models_dir) = get_models_dir(app_handle) else {
        return false;
    };
    let verified =
        |file: &str, sha: &str| verify_sha256(&models_dir.join(file), sha).unwrap_or(false);
    #[cfg(target_os = "macos")]
    {
        let core_ml: &[(&str, &str, &str)] = if sky {
            &SKY_CORE_ML_FILES
        } else {
            &SUBJECT_CORE_ML_FILES
        };
        if core_ml.iter().all(|&(file, _, sha)| verified(file, sha)) {
            return true;
        }
    }
    let (onnx, sha) = if sky {
        (SKYSEG_FILENAME, SKYSEG_SHA256)
    } else {
        (SUBJECT_FILENAME, SUBJECT_SHA256)
    };
    verified(onnx, sha) && ensure_local_runtime().is_ok()
}

/// BiRefNet for Subject and Background masks. With `download` false (a
/// background request) only an installed model is loaded: `None` when it
/// is not installed.
pub async fn get_or_init_subject_model(
    app_handle: &tauri::AppHandle,
    ai_state_mutex: &Mutex<Option<AiState>>,
    ai_init_lock: &TokioMutex<()>,
    download: bool,
) -> Result<Option<Arc<SubjectModel>>> {
    let loaded = || {
        ai_state_mutex
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|state| state.subject_model.clone())
    };
    if let Some(model) = loaded() {
        return Ok(Some(model));
    }
    let _guard = ai_init_lock.lock().await;
    if let Some(model) = loaded() {
        return Ok(Some(model));
    }
    let generation = crate::ai_lifecycle::LIFECYCLE.generation();
    let models_dir = get_models_dir(app_handle)?;
    let start = std::time::Instant::now();
    #[cfg(target_os = "macos")]
    let core_ml = load_subject_core_ml(app_handle, &models_dir, download).await;
    #[cfg(not(target_os = "macos"))]
    let core_ml: Option<SubjectModel> = None;
    let model = match core_ml {
        Some(model) => model,
        None => {
            if !download && ensure_local_runtime().is_err() {
                return Ok(None);
            }
            ensure_local_runtime()?;
            let path = models_dir.join(SUBJECT_FILENAME);
            if download {
                download_verified(
                    app_handle,
                    &models_dir,
                    SUBJECT_FILENAME,
                    SUBJECT_SHA256,
                    "Subject Model",
                )
                .await?;
            } else if !verify_sha256(&path, SUBJECT_SHA256)? {
                return Ok(None);
            }
            tokio::task::spawn_blocking(move || SubjectModel::onnx(&path)).await??
        }
    };
    log::info!(
        "AI subject mask: {} model loaded in {} ms",
        model.backend_name(),
        start.elapsed().as_millis()
    );
    crate::register_exit_handler();
    let model = Arc::new(model);
    keep_model(ai_state_mutex, generation, |ai| {
        ai.subject_model = Some(model.clone())
    });
    Ok(Some(model))
}

/// BiRefNet on Core ML when its files are installed (with `download`,
/// fetched first): `None` when they are missing or Core ML fails, and the
/// ONNX model is used instead. The files are linked into the package, which
/// Core ML compiles once into `birefnet_fp16.mlmodelc` beside it.
#[cfg(target_os = "macos")]
async fn load_subject_core_ml(
    app_handle: &tauri::AppHandle,
    models_dir: &Path,
    download: bool,
) -> Option<SubjectModel> {
    load_core_ml(
        app_handle,
        models_dir,
        SUBJECT_CORE_ML_PACKAGE,
        &SUBJECT_CORE_ML_FILES,
        download,
        "subject",
        "Subject Model",
    )
    .await
    .map(|model| SubjectModel {
        backend: SubjectBackend::CoreMl(model),
    })
}

/// Puts the files of a Core ML program of the model pack (`files`: pack
/// file, place in the package, SHA-256) into `package`, each checked by its
/// SHA-256 (with `download`, fetched first when missing). `Ok(false)` when
/// a file is missing and `download` is false.
#[cfg(target_os = "macos")]
pub(crate) async fn assemble_package(
    app_handle: &tauri::AppHandle,
    models_dir: &Path,
    package: &Path,
    files: &[(&str, &str, &str)],
    download: bool,
    model_name: &str,
) -> Result<bool> {
    for &(file, inner, sha) in files {
        let source = models_dir.join(file);
        if !verify_sha256(&source, sha).unwrap_or(false) {
            if !download {
                return Ok(false);
            }
            download_verified(app_handle, models_dir, file, sha, model_name).await?;
        }
        // A new hard link changes the file's status-change time: keep the
        // check just made, or the next launch hashes it again.
        if link_into_package(&source, &package.join(inner), sha)? {
            verified_files::refresh(&source, sha);
        }
    }
    Ok(true)
}

/// A Core ML program of the model pack (`files`: pack file, place in the
/// package, SHA-256), assembled into `package` in the models folder and
/// loaded on the CPU and GPU: `None` when its files are missing (with
/// `download`, fetched first) or Core ML fails, and the ONNX model is used.
#[cfg(target_os = "macos")]
async fn load_core_ml(
    app_handle: &tauri::AppHandle,
    models_dir: &Path,
    package: &str,
    files: &[(&str, &str, &str)],
    download: bool,
    label: &str,
    model_name: &str,
) -> Option<rawtonic_denoise::coreml::CoreMlModel> {
    let package = models_dir.join(package);
    let start = std::time::Instant::now();
    match assemble_package(
        app_handle, models_dir, &package, files, download, model_name,
    )
    .await
    {
        Ok(true) => {}
        Ok(false) => return None,
        Err(e) => {
            log::warn!("AI {label} mask: no Core ML model ({e}); using ONNX Runtime");
            return None;
        }
    }
    let files_ms = start.elapsed().as_millis();
    let marker = first_run_marker(app_handle, label, files);
    let loaded = tokio::task::spawn_blocking(move || {
        let compiled = rawtonic_denoise::coreml::compiled_path(&package)
            .join("coremldata.bin")
            .exists();
        // Core ML's copy of the weights read into the page cache while it
        // parses the program: a load after a while no longer waits for its
        // page-by-page reads (0.53-0.67 s instead of 0.26 s for Subject).
        let prefetch = rawtonic_denoise::coreml::prefetch_device_cache(&package);
        let load = std::time::Instant::now();
        let model = rawtonic_denoise::coreml::CoreMlModel::load(&package, CORE_ML_UNITS)
            .map(|model| model.with_first_run_marker(marker));
        let load_ms = load.elapsed().as_millis();
        let prefetched = match prefetch {
            Some(reader) if reader.is_finished() => match reader.join() {
                Ok((bytes, seconds)) => format!(
                    "{:.0} MB of its cache read ahead in {:.0} ms",
                    bytes as f64 / 1e6,
                    seconds * 1e3
                ),
                Err(_) => "reading its cache ahead failed".to_string(),
            },
            Some(_) => "its cache still being read ahead".to_string(),
            None => "no cache to read ahead".to_string(),
        };
        (model, compiled, load_ms, prefetched, thread_qos())
    })
    .await;
    if let Ok((_, compiled, load_ms, prefetched, qos)) = &loaded {
        log::info!(
            "AI {label} mask: Core ML load: package files {files_ms} ms, \
             Core ML {load_ms} ms ({}, {prefetched}, thread QoS {qos}), total {} ms",
            if *compiled {
                "compiled model found"
            } else {
                "compiled first"
            },
            start.elapsed().as_millis()
        );
    }
    match loaded.map(|(model, ..)| model) {
        Ok(Ok(model)) => Some(model),
        Ok(Err(e)) => {
            log::warn!("AI {label} mask: Core ML failed ({e}); using ONNX Runtime");
            None
        }
        Err(e) => {
            log::warn!("AI {label} mask: Core ML failed ({e}); using ONNX Runtime");
            None
        }
    }
}

/// Whether the Subject (`sky` false) or Sky model's Core ML files are
/// installed and it has never run in this app on this macOS build
/// (`FirstRunMarker`), so its first use would wait for Metal to build its
/// GPU kernels. Always false away from macOS, and without the Core ML files
/// (an ONNX model alone needs no warm-up and would only be loaded for
/// nothing).
pub fn core_ml_first_run_pending(app_handle: &tauri::AppHandle, sky: bool) -> bool {
    #[cfg(target_os = "macos")]
    {
        let (label, files) = if sky {
            ("sky", &SKY_CORE_ML_FILES)
        } else {
            ("subject", &SUBJECT_CORE_ML_FILES)
        };
        let Ok(models_dir) = get_models_dir(app_handle) else {
            return false;
        };
        first_run_pending(
            &models_dir,
            files,
            first_run_marker(app_handle, label, files).as_ref(),
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app_handle, sky);
        false
    }
}

/// `core_ml_first_run_pending` for the files of a Core ML program in
/// `models_dir` (only looked at; the load checks their hashes) and its
/// marker.
#[cfg(target_os = "macos")]
fn first_run_pending(
    models_dir: &Path,
    files: &[(&str, &str, &str)],
    marker: Option<&rawtonic_denoise::coreml::FirstRunMarker>,
) -> bool {
    files
        .iter()
        .all(|&(file, _, _)| models_dir.join(file).is_file())
        && marker.is_some_and(|marker| !marker.is_set())
}

/// The first-run marker of a Core ML program of the model pack, named after
/// `label` and its files' hashes, in the app's cache folder.
#[cfg(target_os = "macos")]
fn first_run_marker(
    app_handle: &tauri::AppHandle,
    label: &str,
    files: &[(&str, &str, &str)],
) -> Option<rawtonic_denoise::coreml::FirstRunMarker> {
    let dir = app_handle
        .path()
        .app_cache_dir()
        .ok()?
        .join("ai-first-runs");
    let hashes: String = files.iter().map(|&(_, _, sha)| &sha[..8]).collect();
    rawtonic_denoise::coreml::FirstRunMarker::new(&dir, &format!("{label}-{hashes}"))
}

/// Runs `f` on a thread of its own at utility QoS (macOS): below the
/// photo's decoding and rendering. Core ML's first run of BiRefNet took as
/// long as at the default class (11.3 vs 14.3 s, a busy machine), at
/// background QoS 34 s.
pub(crate) fn at_utility_qos<T: Send>(f: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|scope| {
        let worker = std::thread::Builder::new()
            .name("ai-warm-up".into())
            .spawn_scoped(scope, || {
                #[cfg(target_os = "macos")]
                unsafe {
                    libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_UTILITY, 0);
                }
                f()
            })
            .expect("a thread for the warm-up");
        worker
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    })
}

/// The calling thread's quality-of-service class (logged with model loads:
/// Core ML loads about 4x slower at background QoS).
pub(crate) fn thread_qos() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        let mut class = libc::qos_class_t::QOS_CLASS_UNSPECIFIED;
        let mut relative = 0;
        unsafe { libc::pthread_get_qos_class_np(libc::pthread_self(), &mut class, &mut relative) };
        match class {
            libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE => "user-interactive",
            libc::qos_class_t::QOS_CLASS_USER_INITIATED => "user-initiated",
            libc::qos_class_t::QOS_CLASS_DEFAULT => "default",
            libc::qos_class_t::QOS_CLASS_UTILITY => "utility",
            libc::qos_class_t::QOS_CLASS_BACKGROUND => "background",
            _ => "unspecified",
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        "n/a"
    }
}

/// Puts a verified model file at `target` inside a package (a hard link,
/// else a copy); a changed package loses its compiled model. `true` when
/// the file was put there now.
#[cfg(target_os = "macos")]
fn link_into_package(source: &Path, target: &Path, sha: &str) -> Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let same = match (fs::metadata(source), fs::metadata(target)) {
        (Ok(a), Ok(b)) => {
            (a.dev(), a.ino()) == (b.dev(), b.ino())
                || (a.len() == b.len() && file_has_sha256(target, sha)?)
        }
        _ => false,
    };
    if same {
        return Ok(false);
    }
    let package = target
        .ancestors()
        .find(|p| p.extension().is_some_and(|e| e == "mlpackage"))
        .ok_or_else(|| anyhow::anyhow!("{} is not inside a package", target.display()))?;
    let _ = fs::remove_dir_all(package.with_extension("mlmodelc"));
    fs::create_dir_all(target.parent().unwrap())?;
    let staging = target.with_extension("partial");
    let _ = fs::remove_file(&staging);
    if fs::hard_link(source, &staging).is_err() {
        fs::copy(source, &staging)?;
    }
    fs::rename(&staging, target)?;
    Ok(true)
}

/// The tagging models; `with_text` also loads the text encoder for the
/// user's own tags (a 1.1 GB download, only then).
pub async fn get_or_init_tag_models(
    app_handle: &tauri::AppHandle,
    ai_state_mutex: &Mutex<Option<AiState>>,
    ai_init_lock: &TokioMutex<()>,
    with_text: bool,
) -> Result<Arc<TagModels>> {
    ensure_local_runtime()?;
    let cached = || {
        ai_state_mutex
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|state| state.tag_models.clone())
            .filter(|models| !with_text || models.text.is_some())
    };
    if let Some(models) = cached() {
        return Ok(models);
    }

    let _guard = ai_init_lock.lock().await;
    if let Some(models) = cached() {
        return Ok(models);
    }
    let pressure_generation = crate::ai_lifecycle::LIFECYCLE.pressure_generation();

    let models_dir = get_models_dir(app_handle)?;
    let mut files = vec![
        (
            SIGLIP_VISION_FILENAME,
            SIGLIP_VISION_SHA256,
            "Tagging Model",
        ),
        (SIGLIP_TAGS_FILENAME, SIGLIP_TAGS_SHA256, "Tag List"),
    ];
    if with_text {
        files.push((
            SIGLIP_TEXT_FILENAME,
            SIGLIP_TEXT_SHA256,
            "Tagging Text Model",
        ));
        files.push((
            SIGLIP_TOKENIZER_FILENAME,
            SIGLIP_TOKENIZER_SHA256,
            "Tagging Tokenizer",
        ));
    }
    for (file, hash, name) in files {
        download_verified(app_handle, &models_dir, file, hash, name).await?;
    }

    let vision = rawtonic_onnx::Model::load(
        &models_dir.join(SIGLIP_VISION_FILENAME),
        rawtonic_onnx::ModelKind::TagVision,
    )?;
    let tag_embeddings = read_tag_embeddings(
        &fs::read(models_dir.join(SIGLIP_TAGS_FILENAME))?,
        crate::tagging_utils::candidates::TAG_CANDIDATES.len(),
    )?;
    let text = if with_text {
        Some(TextEncoder {
            session: rawtonic_onnx::Model::load(
                &models_dir.join(SIGLIP_TEXT_FILENAME),
                rawtonic_onnx::ModelKind::TagText,
            )?,
            tokenizer: Tokenizer::from_file(models_dir.join(SIGLIP_TOKENIZER_FILENAME))
                .map_err(|e| anyhow::anyhow!(e.to_string()))?,
        })
    } else {
        None
    };

    crate::register_exit_handler();

    let models = Arc::new(TagModels {
        vision,
        tag_embeddings,
        text,
    });

    // Dropped only under memory pressure (`ai_lifecycle`): tagging runs in
    // the Library, where the masking models are unloaded. A load that
    // overlapped such an unload is used once and not kept.
    let mut ai = ai_state_mutex.lock().unwrap();
    if crate::ai_lifecycle::LIFECYCLE.keep_tag_models(pressure_generation) {
        ai.get_or_insert_with(AiState::default).tag_models = Some(models.clone());
    } else {
        log::info!("AI: tagging models loaded during a memory-pressure unload are not kept");
    }
    Ok(models)
}

/// Little-endian f32 embeddings, `SIGLIP_EMBED_DIM` per tag.
pub(crate) fn read_tag_embeddings(bytes: &[u8], tags: usize) -> Result<Vec<f32>> {
    let expected = tags * SIGLIP_EMBED_DIM * 4;
    if bytes.len() != expected {
        return Err(anyhow::anyhow!(
            "{SIGLIP_TAGS_FILENAME} has {} bytes, expected {expected} for {tags} tags",
            bytes.len()
        ));
    }
    Ok(bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|&b| f32::from_le_bytes(b))
        .collect())
}

pub async fn get_or_init_inpaint_model(
    app_handle: &tauri::AppHandle,
    ai_state_mutex: &Mutex<Option<AiState>>,
    ai_init_lock: &TokioMutex<()>,
) -> Result<Arc<rawtonic_onnx::Model>> {
    ensure_local_runtime()?;
    if let Some(model) = ai_state_mutex
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|state| state.inpaint_model.clone())
    {
        return Ok(model);
    }

    let _guard = ai_init_lock.lock().await;
    let generation = crate::ai_lifecycle::LIFECYCLE.generation();

    if let Some(model) = ai_state_mutex
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|state| state.inpaint_model.clone())
    {
        return Ok(model);
    }

    let models_dir = get_models_dir(app_handle)?;
    download_verified(
        app_handle,
        &models_dir,
        INPAINT_FILENAME,
        INPAINT_SHA256,
        "Inpainting Model",
    )
    .await?;

    let model_path = models_dir.join(INPAINT_FILENAME);
    let model = Arc::new(rawtonic_onnx::Model::load(
        &model_path,
        rawtonic_onnx::ModelKind::Inpaint,
    )?);

    crate::register_exit_handler();

    keep_model(ai_state_mutex, generation, |ai| {
        ai.inpaint_model = Some(model.clone())
    });
    Ok(model)
}

/// Serialises loading Moebius, apart from `ai_init_lock`: its first use
/// downloads about 620 MB, which must not hold up the masking models.
static MOEBIUS_INIT_LOCK: TokioMutex<()> = TokioMutex::const_new(());

/// Moebius (`moebius`), downloaded on first use.
pub async fn get_or_init_moebius(
    app_handle: &tauri::AppHandle,
    ai_state_mutex: &Mutex<Option<AiState>>,
) -> Result<Arc<crate::moebius::Moebius>> {
    let loaded = || {
        ai_state_mutex
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|state| state.moebius.clone())
    };
    if let Some(model) = loaded() {
        return Ok(model);
    }
    let _guard = MOEBIUS_INIT_LOCK.lock().await;
    if let Some(model) = loaded() {
        return Ok(model);
    }
    let generation = crate::ai_lifecycle::LIFECYCLE.generation();
    let models_dir = get_models_dir(app_handle)?;
    let start = std::time::Instant::now();
    let model = Arc::new(crate::moebius::load(app_handle, &models_dir).await?);
    log::info!(
        "AI fill: Moebius loaded in {} ms",
        start.elapsed().as_millis()
    );
    keep_model(ai_state_mutex, generation, |ai| {
        ai.moebius = Some(model.clone())
    });
    Ok(model)
}

/// Fills the masked area (mask > 0) of `image` with MI-GAN. Only a crop
/// around the mask goes to the network; the mask's grey levels blend the
/// result into the photo.
pub fn run_inpainting(
    image: &DynamicImage,
    mask: &GrayImage,
    session: &rawtonic_onnx::Model,
) -> Result<RgbaImage> {
    let (w, h) = image.dimensions();

    let (mut min_x, mut min_y) = (w, h);
    let (mut max_x, mut max_y) = (0u32, 0u32);
    let mut has_mask = false;

    for (x, y, p) in mask.enumerate_pixels() {
        if p[0] > 0 {
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
            has_mask = true;
        }
    }

    if !has_mask {
        return Ok(image.to_rgba8());
    }

    let mask_w = max_x - min_x + 1;
    let mask_h = max_y - min_y + 1;

    let pad_x = 128.max((mask_w as f32 * 1.5) as u32);
    let pad_y = 128.max((mask_h as f32 * 1.5) as u32);

    let x0 = min_x.saturating_sub(pad_x);
    let y0 = min_y.saturating_sub(pad_y);
    let x1 = (max_x + pad_x).min(w.saturating_sub(1));
    let y1 = (max_y + pad_y).min(h.saturating_sub(1));

    let crop_w = x1 - x0 + 1;
    let crop_h = y1 - y0 + 1;

    let rgba = image.to_rgba8();
    let cropped_img = imageops::crop_imm(&rgba, x0, y0, crop_w, crop_h).to_image();
    let cropped_mask = imageops::crop_imm(mask, x0, y0, crop_w, crop_h).to_image();

    // The pipeline takes uint8 RGB and a binary mask with 255 = keep.
    let (cw, ch) = (crop_w as usize, crop_h as usize);
    let mut img_tensor = Array::<u8, _>::zeros((1, 3, ch, cw));
    let mut msk_tensor = Array::<u8, _>::zeros((1, 1, ch, cw));
    for (x, y, p) in cropped_img.enumerate_pixels() {
        let (x, y) = (x as usize, y as usize);
        for c in 0..3 {
            img_tensor[[0, c, y, x]] = p[c];
        }
        msk_tensor[[0, 0, y, x]] = if cropped_mask.get_pixel(x as u32, y as u32)[0] > 0 {
            0
        } else {
            255
        };
    }
    // The pipeline crops tightly around its hole before scaling to 512 px,
    // so a large hole filled nearly all of the model's view and came back
    // as an invented object (a yellow blot where a potato in a fruit bowl
    // was). Four 2 × 2 px marks at the corners of a box 2.5 × the hole's
    // size widen that crop to it; the model fills them too, but only the
    // real hole is taken from its result. Measured on 27 objects in 21 RAW
    // photos, 10 of them held out (docs/MODELS.md, "Object removal").
    const CONTEXT: f32 = 2.5;
    if crop_w >= 2 && crop_h >= 2 {
        let marks = |lo: u32, hi: u32, origin: u32, size: u32| {
            let centre = (lo + hi - 2 * origin) as f32 / 2.0;
            let half = (hi - lo + 1) as f32 * CONTEXT / 2.0;
            let max = (size - 2) as f32;
            [
                (centre - half).clamp(0.0, max) as usize,
                (centre + half).clamp(0.0, max) as usize,
            ]
        };
        let mark_x = marks(min_x, max_x, x0, crop_w);
        let mark_y = marks(min_y, max_y, y0, crop_h);
        for &mx in &mark_x {
            for &my in &mark_y {
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    msk_tensor[[0, 0, my + dy, mx + dx]] = 0;
                }
            }
        }
    }

    let output = session.run(|session| {
        let outputs = session.run(ort::inputs![
            "image" => Tensor::from_array(img_tensor.clone().into_dyn())?,
            "mask" => Tensor::from_array(msk_tensor.clone().into_dyn())?
        ])?;
        Ok(outputs[0].try_extract_array::<u8>()?.to_owned())
    })?;
    anyhow::ensure!(
        output.shape() == [1, 3, ch, cw],
        "inpainting returned shape {:?}",
        output.shape()
    );

    let mut final_image = rgba;
    for y in 0..crop_h {
        for x in 0..crop_w {
            let m = cropped_mask.get_pixel(x, y)[0];
            if m > 0 {
                let alpha = m as f32 / 255.0;
                let (gx, gy) = (x0 + x, y0 + y);
                let orig = *final_image.get_pixel(gx, gy);
                let mix = |c: usize| {
                    let filled = output[[0, c, y as usize, x as usize]] as f32;
                    (filled * alpha + orig[c] as f32 * (1.0 - alpha)) as u8
                };
                final_image.put_pixel(gx, gy, Rgba([mix(0), mix(1), mix(2), 255]));
            }
        }
    }

    Ok(final_image)
}

/// The networks were trained on display images: a linear RAW decode is
/// sRGB-encoded after downscaling to the network's input. Fed linear values,
/// the sky network found no sky on 9 of 11 RAW landscapes with sky.
fn display_rgb8(image: DynamicImage, linear: bool) -> image::RgbImage {
    if !linear {
        return image.into_rgb8();
    }
    let float = image.into_rgb32f();
    let encode = |v: f32| {
        let x = v.clamp(0.0, 1.0);
        let e = if x <= 0.003_130_8 {
            12.92 * x
        } else {
            1.055 * x.powf(1.0 / 2.4) - 0.055
        };
        (e * 255.0).round() as u8
    };
    image::RgbImage::from_fn(float.width(), float.height(), |x, y| {
        let p = float.get_pixel(x, y);
        image::Rgb([encode(p[0]), encode(p[1]), encode(p[2])])
    })
}

/// `options` can stop the run (`RunOptions::terminate`).
pub fn generate_image_embeddings(
    image: &DynamicImage,
    encoder: &rawtonic_onnx::Model,
    linear: bool,
    options: Option<&ort::session::RunOptions>,
) -> Result<ImageEmbeddings> {
    let (orig_width, orig_height) = image.dimensions();

    // SAM 2 sees the photo stretched to a square, as SAM2ImagePredictor
    // feeds it; prompts are scaled per axis (`sam_scale`).
    let resized_image = image.resize_exact(SAM_INPUT_SIZE, SAM_INPUT_SIZE, FilterType::Triangle);
    let rgb_image = display_rgb8(resized_image, linear);
    let raw_pixels = rgb_image.as_raw();
    let side = SAM_INPUT_SIZE as usize;
    let plane = side * side;
    let mut planar = vec![0u8; 3 * plane];
    for (i, pixel) in raw_pixels.as_chunks::<3>().0.iter().enumerate() {
        planar[i] = pixel[0];
        planar[plane + i] = pixel[1];
        planar[2 * plane + i] = pixel[2];
    }
    let input = Array::from_shape_vec((1, 3, side, side), planar)?;

    let start = std::time::Instant::now();
    let (embeddings, high_res_0, high_res_1) = encoder.run(|session| {
        let inputs = ort::inputs!["image" => Tensor::from_array(input.clone())?];
        let outputs = match options {
            Some(options) => session.run_with_options(inputs, options)?,
            None => session.run(inputs)?,
        };
        let output = |name: &str| -> Result<Array<f32, IxDyn>> {
            let array = outputs[name].try_extract_array::<f32>()?.to_owned();
            rawtonic_onnx::ensure_finite(array.iter())?;
            Ok(array)
        };
        Ok((
            output("image_embed")?,
            output("high_res_feats_0")?,
            output("high_res_feats_1")?,
        ))
    })?;
    log::info!(
        "AI object mask: encoder {} ms on {}",
        start.elapsed().as_millis(),
        encoder.provider()
    );

    Ok(ImageEmbeddings {
        path_hash: "".to_string(),
        embeddings,
        high_res_feats: [high_res_0, high_res_1],
        original_size: (orig_width, orig_height),
    })
}

/// Scale from photo pixels to SAM's stretched 1024 x 1024 input, per axis.
fn sam_scale(original_size: (u32, u32)) -> (f64, f64) {
    (
        SAM_INPUT_SIZE as f64 / original_size.0 as f64,
        SAM_INPUT_SIZE as f64 / original_size.1 as f64,
    )
}

/// The photo's lightness as the guided filter's guide: a linear RAW decode
/// is sRGB-encoded first, else its shadows collapse into a few levels and
/// the mask edge stops following the photo there.
fn guide_luma8(image: &DynamicImage, linear: bool) -> GrayImage {
    if !linear {
        return image.to_luma8();
    }
    let owned;
    let float = match image.as_rgb32f() {
        Some(f) => f,
        None => {
            owned = image.to_rgb32f();
            &owned
        }
    };
    let (w, h) = float.dimensions();
    let mut out = vec![0u8; (w * h) as usize];
    out.par_iter_mut()
        .zip(float.as_raw().par_chunks_exact(3))
        .for_each(|(o, p)| {
            let y = (0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]).clamp(0.0, 1.0);
            let e = if y <= 0.003_130_8 {
                12.92 * y
            } else {
                1.055 * y.powf(1.0 / 2.4) - 0.055
            };
            *o = (e * 255.0).round() as u8;
        });
    GrayImage::from_raw(w, h, out).unwrap()
}

/// Radius in photo pixels of the guided filter that fits a SAM mask to the
/// photo's edges: one cell of the decoder's 256 × 256 low-res mask, whose
/// edge is uncertain by about that much. On 500 DUTS-TE photos (300-400 px)
/// this lifts boundary F from 0.609 to 0.664 (click) and 0.790 to 0.863
/// (box) against the former 8-24 px; larger radii smear the edge
/// (docs/MODELS.md).
fn sam_refine_radius(long_edge: u32) -> f32 {
    (long_edge as f32 / 256.0).max(1.0)
}

/// `imageops::blur`, the same pixels, on bands of `band` rows in parallel:
/// each band is blurred with enough rows around it to cover the kernel
/// (`imageops::blur` is single-threaded; 0.64 s at 45 MP and sigma 10).
fn parallel_blur(image: &GrayImage, sigma: f32, band: u32) -> GrayImage {
    let (w, h) = image.dimensions();
    // The kernel is 2 * ((sigma - 0.8) / 0.3 + 1) + 1 wide at most + 1
    // (image's `kernel_size_from_sigma`), so half of it fits in this.
    let reach = (((sigma.max(0.8) - 0.8) / 0.3 + 1.0) * 2.0 + 3.0).ceil() as u32 / 2 + 2;
    if h <= band || w == 0 {
        return imageops::blur(image, sigma);
    }
    let bands: Vec<u32> = (0..h).step_by(band as usize).collect();
    let parts: Vec<GrayImage> = bands
        .par_iter()
        .map(|&y0| {
            let y1 = (y0 + band).min(h);
            let top = y0.saturating_sub(reach);
            let bottom = (y1 + reach).min(h);
            let view = imageops::crop_imm(image, 0, top, w, bottom - top).to_image();
            let blurred = imageops::blur(&view, sigma);
            imageops::crop_imm(&blurred, 0, y0 - top, w, y1 - y0).to_image()
        })
        .collect();
    let mut out = Vec::with_capacity((w * h) as usize);
    for part in parts {
        out.extend_from_slice(part.as_raw());
    }
    GrayImage::from_raw(w, h, out).unwrap()
}

/// Fits the decoder's hard mask (photo size) to the edges of `guide`: a
/// feather, a guided filter of `radius`, then a slight contrast curve.
///
/// With `step` 1 `mask` has the guide's size. With a larger `step` it is
/// `step` times smaller (rounded up): the feather and the guided filter's
/// coefficients are computed at that size against the guide averaged down,
/// and only the coefficients are interpolated back to apply them to the
/// full-size guide (He and Sun's fast guided filter), so the edge still
/// follows the photo's pixels.
fn refine_sam_mask(mask: &GrayImage, guide: &GrayImage, radius: f32, step: u32) -> GrayImage {
    let contrast = |v: f32| (((v - 0.03) / 0.94).clamp(0.0, 1.0) * 255.0).round() as u8;
    if step <= 1 {
        let feathered = parallel_blur(mask, radius / 3.0, 512);
        let mut refined = fast_guided_filter(guide, &feathered, radius as usize, 0.01);
        for p in refined.pixels_mut() {
            p[0] = contrast(p[0] as f32 / 255.0);
        }
        return refined;
    }
    let radius_lr = radius / step as f32;
    let feathered = parallel_blur(mask, radius_lr / 3.0, 512);
    let guide_lr = box_downsample(guide, step, mask.dimensions());
    let (a, b) = guided_coefficients(&guide_lr, &feathered, (radius_lr as usize).max(1), 0.01);
    let (lw, lh) = (mask.width() as usize, mask.height() as usize);
    let (w, h) = guide.dimensions();
    let s = step as f32;
    let mut out = vec![0u8; (w * h) as usize];
    out.par_chunks_mut(w as usize)
        .enumerate()
        .for_each(|(y, row)| {
            let fy = ((y as f32 + 0.5) / s - 0.5).clamp(0.0, (lh - 1) as f32);
            let (y0, ty) = (fy.floor() as usize, fy.fract());
            let y1 = (y0 + 1).min(lh - 1);
            for (x, o) in row.iter_mut().enumerate() {
                let fx = ((x as f32 + 0.5) / s - 0.5).clamp(0.0, (lw - 1) as f32);
                let (x0, tx) = (fx.floor() as usize, fx.fract());
                let x1 = (x0 + 1).min(lw - 1);
                let lerp = |c: &[f32]| {
                    let top = c[y0 * lw + x0] * (1.0 - tx) + c[y0 * lw + x1] * tx;
                    let bottom = c[y1 * lw + x0] * (1.0 - tx) + c[y1 * lw + x1] * tx;
                    top * (1.0 - ty) + bottom * ty
                };
                let i = guide.as_raw()[y * w as usize + x] as f32 / 255.0;
                *o = contrast((lerp(&a) * i + lerp(&b)).clamp(0.0, 1.0));
            }
        });
    GrayImage::from_raw(w, h, out).unwrap()
}

/// The mean of each `step` × `step` block of `image` (blocks at the right
/// and bottom edges are clipped), an image of `size`.
fn box_downsample(image: &GrayImage, step: u32, size: (u32, u32)) -> GrayImage {
    let (w, h) = image.dimensions();
    let (lw, lh) = size;
    let mut out = vec![0u8; (lw * lh) as usize];
    out.par_chunks_mut(lw as usize)
        .enumerate()
        .for_each(|(ly, row)| {
            let (y0, y1) = (ly as u32 * step, ((ly as u32 + 1) * step).min(h));
            for (lx, o) in row.iter_mut().enumerate() {
                let (x0, x1) = (lx as u32 * step, ((lx as u32 + 1) * step).min(w));
                let mut sum = 0u32;
                for y in y0..y1 {
                    let start = (y * w) as usize;
                    sum += image.as_raw()[start + x0 as usize..start + x1 as usize]
                        .iter()
                        .map(|&v| v as u32)
                        .sum::<u32>();
                }
                let n = ((y1 - y0) * (x1 - x0)).max(1);
                *o = ((sum + n / 2) / n) as u8;
            }
        });
    GrayImage::from_raw(lw, lh, out).unwrap()
}

/// The guided filter's smoothed coefficients (mean a, mean b) of `mask`
/// against `guide` (same size).
fn guided_coefficients(
    guide: &GrayImage,
    mask: &GrayImage,
    r: usize,
    eps: f32,
) -> (Vec<f32>, Vec<f32>) {
    let (w, h) = (guide.width() as usize, guide.height() as usize);
    let area = w * h;
    let i: Vec<f32> = guide.as_raw().iter().map(|&v| v as f32 / 255.0).collect();
    let p: Vec<f32> = mask.as_raw().iter().map(|&v| v as f32 / 255.0).collect();
    let ip: Vec<f32> = i.iter().zip(&p).map(|(a, b)| a * b).collect();
    let ii: Vec<f32> = i.iter().map(|a| a * a).collect();
    let (mut t1, mut t2) = (vec![0.0; area], vec![0.0; area]);
    let mut mean = |src: &[f32]| {
        let mut dest = vec![0.0; area];
        box_filter_opt(src, &mut dest, &mut t1, &mut t2, w, h, r);
        dest
    };
    let (mi, mp, mip, mii) = (mean(&i), mean(&p), mean(&ip), mean(&ii));
    let mut a = vec![0.0; area];
    let mut b = vec![0.0; area];
    for k in 0..area {
        let cov = mip[k] - mi[k] * mp[k];
        let var = mii[k] - mi[k] * mi[k];
        a[k] = cov / (var + eps);
        b[k] = mp[k] - a[k] * mi[k];
    }
    (mean(&a), mean(&b))
}

/// The factor by which the mask is refined below the photo's size: its
/// long edge stays at most `edge`.
fn refine_step(long_edge: u32, edge: u32) -> u32 {
    long_edge.div_ceil(edge).max(1)
}

/// A click (`start_point == end_point`) or box mask at the size of the
/// photo. With `original_image` the mask is fitted to its edges
/// (`linear`: the image is a linear RAW decode).
pub fn run_sam_decoder(
    decoder: &rawtonic_onnx::Model,
    embeddings: &ImageEmbeddings,
    start_point: (f64, f64),
    end_point: (f64, f64),
    original_image: Option<&DynamicImage>,
    linear: bool,
) -> Result<GrayImage> {
    run_sam_decoder_prompts(
        decoder,
        embeddings,
        &[SamPrompt::legacy(start_point, end_point)],
        original_image,
        linear,
    )
}

/// One prompt of an Object selection in photo pixels: a point, or a box by
/// its corners; `exclude` takes it out of the selection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SamPrompt {
    Point {
        at: (f64, f64),
        exclude: bool,
    },
    Box {
        min: (f64, f64),
        max: (f64, f64),
        exclude: bool,
    },
}

impl SamPrompt {
    /// The prompt of a recipe's one box or click (`start == end`).
    pub fn legacy(start: (f64, f64), end: (f64, f64)) -> Self {
        if (start.0 - end.0).abs() < 1e-6 && (start.1 - end.1).abs() < 1e-6 {
            SamPrompt::Point {
                at: start,
                exclude: false,
            }
        } else {
            SamPrompt::Box {
                min: (start.0.min(end.0), start.1.min(end.1)),
                max: (start.0.max(end.0), start.1.max(end.1)),
                exclude: false,
            }
        }
    }
}

/// One decoder call of an Object selection, in photo pixels: its points
/// and SAM's labels for them (1 include, 0 exclude, 2 and 3 a box's
/// corners), the exclude points the refining pass keeps, and whether its
/// mask is taken out of the selection.
#[derive(Debug, Clone, PartialEq)]
pub struct SamCall {
    pub points: Vec<(f64, f64)>,
    pub labels: Vec<f32>,
    pub excluded: Vec<(f64, f64)>,
    pub subtract: bool,
}

/// The decoder calls of an Object selection's prompts. SAM takes one box
/// per call and segments one object, so each included point or box is a
/// call of its own (the selection is their union), with every exclude point
/// as a negative point; an excluded box is segmented on its own and taken
/// out (SAM has no negative box). Exclude points alone select nothing.
pub fn sam_calls(prompts: &[SamPrompt]) -> Vec<SamCall> {
    let excluded: Vec<(f64, f64)> = prompts
        .iter()
        .filter_map(|p| match *p {
            SamPrompt::Point { at, exclude: true } => Some(at),
            _ => None,
        })
        .collect();
    let with_excluded = |mut points: Vec<(f64, f64)>, mut labels: Vec<f32>| {
        points.extend(&excluded);
        labels.extend(excluded.iter().map(|_| 0.0f32));
        SamCall {
            points,
            labels,
            excluded: excluded.clone(),
            subtract: false,
        }
    };
    prompts
        .iter()
        .filter_map(|p| match *p {
            SamPrompt::Point { exclude: true, .. } => None,
            SamPrompt::Point { at, .. } => Some(with_excluded(vec![at], vec![1.0])),
            SamPrompt::Box {
                min,
                max,
                exclude: false,
            } => Some(with_excluded(vec![min, max], vec![2.0, 3.0])),
            SamPrompt::Box { min, max, .. } => Some(SamCall {
                points: vec![min, max],
                labels: vec![2.0, 3.0],
                excluded: Vec::new(),
                subtract: true,
            }),
        })
        .collect()
}

/// The mask (0 or 255) of an Object selection's decoder calls: the union of
/// the included ones without the subtracted ones.
fn sam_selection_mask(
    decoder: &rawtonic_onnx::Model,
    embeddings: &ImageEmbeddings,
    calls: &[SamCall],
    out_size: (u32, u32),
) -> Result<GrayImage> {
    if let [call] = calls
        && !call.subtract
    {
        return sam_call_mask(decoder, embeddings, call, out_size);
    }
    let mut selection = GrayImage::new(out_size.0, out_size.1);
    let mut taken_out = GrayImage::new(out_size.0, out_size.1);
    for call in calls {
        let started = std::time::Instant::now();
        let mask = sam_call_mask(decoder, embeddings, call, out_size)?;
        log::info!(
            "AI object mask: decoder call ({} points{}) {} ms",
            call.points.len(),
            if call.subtract { ", subtracted" } else { "" },
            started.elapsed().as_millis()
        );
        let target = if call.subtract {
            &mut taken_out
        } else {
            &mut selection
        };
        for (t, m) in target.iter_mut().zip(mask.iter()) {
            *t = (*t).max(*m);
        }
    }
    for (s, out) in selection.iter_mut().zip(taken_out.iter()) {
        if *out > 0 {
            *s = 0;
        }
    }
    Ok(selection)
}

/// [`run_sam_decoder`] for a list of prompts (see [`sam_calls`]).
pub fn run_sam_decoder_prompts(
    decoder: &rawtonic_onnx::Model,
    embeddings: &ImageEmbeddings,
    prompts: &[SamPrompt],
    original_image: Option<&DynamicImage>,
    linear: bool,
) -> Result<GrayImage> {
    let calls = sam_calls(prompts);
    if let Some(img) = original_image {
        let guide = guide_luma8(img, linear);
        let (w, h) = guide.dimensions();
        let step = refine_step(w.max(h), SAM_REFINE_EDGE);
        let mask = sam_selection_mask(
            decoder,
            embeddings,
            &calls,
            (w.div_ceil(step), h.div_ceil(step)),
        )?;
        Ok(refine_sam_mask(
            &mask,
            &guide,
            sam_refine_radius(w.max(h)),
            step,
        ))
    } else {
        let gray_mask = sam_selection_mask(decoder, embeddings, &calls, embeddings.original_size)?;
        let feathered_mask = image::imageops::blur(&gray_mask, 2.0);
        let (orig_width, orig_height) = embeddings.original_size;
        Ok(image::imageops::resize(
            &feathered_mask,
            orig_width,
            orig_height,
            FilterType::Triangle,
        ))
    }
}

/// The decoder's `point_coords` (1 × N × 2, model input pixels) and
/// `point_labels` (1 × N) inputs.
fn sam_prompt_arrays(
    coords: &[(f32, f32)],
    labels: &[f32],
) -> Result<(Array<f32, IxDyn>, Array<f32, IxDyn>)> {
    let coords_flat: Vec<f32> = coords.iter().flat_map(|&(x, y)| [x, y]).collect();
    Ok((
        Array::from_shape_vec((1, coords.len(), 2), coords_flat)?.into_dyn(),
        Array::from_shape_vec((1, labels.len()), labels.to_vec())?.into_dyn(),
    ))
}

/// The decoder's mask (0 or 255) of one box or click at the size of the
/// photo (see [`sam_call_mask`]).
#[cfg(test)]
fn sam_hard_mask(
    decoder: &rawtonic_onnx::Model,
    embeddings: &ImageEmbeddings,
    start_point: (f64, f64),
    end_point: (f64, f64),
    out_size: (u32, u32),
) -> Result<GrayImage> {
    let calls = sam_calls(&[SamPrompt::legacy(start_point, end_point)]);
    sam_call_mask(decoder, embeddings, &calls[0], out_size)
}

/// The decoder's mask (0 or 255) of one call at the size of the photo: the
/// call's prompt, then a second pass with interior/exterior points, the
/// first mask's box, the call's exclude points and the mask as input.
fn sam_call_mask(
    decoder: &rawtonic_onnx::Model,
    embeddings: &ImageEmbeddings,
    call: &SamCall,
    out_size: (u32, u32),
) -> Result<GrayImage> {
    let (orig_width, orig_height) = embeddings.original_size;
    let (scale_x, scale_y) = sam_scale(embeddings.original_size);
    let to_input = |x: f64, y: f64| ((x * scale_x) as f32, (y * scale_y) as f32);

    let iters = 2;

    let mut point_coords: Vec<(f32, f32)> =
        call.points.iter().map(|&(x, y)| to_input(x, y)).collect();
    let mut point_labels = call.labels.clone();

    let mut mask_input = Array::zeros((1, 1, 256, 256)).into_dyn();
    let mut has_mask_input = 0.0f32;

    let orig_im_size =
        Array::from_shape_vec((2,), vec![out_size.1 as f32, out_size.0 as f32])?.into_dyn();
    let first_scale = (SAM_FIRST_PASS_EDGE as f64 / orig_width.max(orig_height) as f64).min(1.0);
    let first_w = ((orig_width as f64 * first_scale).round() as u32).max(1);
    let first_h = ((orig_height as f64 * first_scale).round() as u32).max(1);
    // From the first pass's mask pixels to photo pixels.
    let (back_x, back_y) = (
        orig_width as f64 / first_w as f64,
        orig_height as f64 / first_h as f64,
    );

    let mut final_mask_data: Vec<u8> = Vec::new();
    let mut final_w = 0;
    let mut final_h = 0;

    for i in 0..iters {
        let (coords_array, labels_array) = sam_prompt_arrays(&point_coords, &point_labels)?;

        // Only the last pass needs the photo's size; the first pass's mask
        // only yields prompt points, so it comes at most SAM_FIRST_PASS_EDGE
        // long (at the photo's size its EDT took about 2 s at 45 MP).
        let pass_size = if i == iters - 1 {
            orig_im_size.clone()
        } else {
            Array::from_shape_vec((2,), vec![first_h as f32, first_w as f32])?.into_dyn()
        };
        // The tensors are built inside the run, which repeats on the CPU
        // if the GPU fails (rawtonic_onnx::Model::run).
        let mask_tensor = decoder.run(|session| {
            let feature =
                |a: &Array<f32, IxDyn>| Tensor::from_array(a.as_standard_layout().into_owned());
            let outputs = session.run(ort::inputs![
                "image_embed" => feature(&embeddings.embeddings)?,
                "high_res_feats_0" => feature(&embeddings.high_res_feats[0])?,
                "high_res_feats_1" => feature(&embeddings.high_res_feats[1])?,
                "point_coords" => feature(&coords_array)?,
                "point_labels" => feature(&labels_array)?,
                "mask_input" => feature(&mask_input)?,
                "has_mask_input" => Tensor::from_array(Array::from_elem((1,), has_mask_input).into_dyn())?,
                "orig_im_size" => feature(&pass_size)?
            ])?;
            let masks = outputs["masks"].try_extract_array::<f32>()?.to_owned();
            rawtonic_onnx::ensure_finite(masks.iter())?;
            Ok(masks)
        })?;

        let mask_dims = mask_tensor.shape();
        let h = mask_dims[2];
        let w = mask_dims[3];
        let area = h * w;

        let mask_slice = mask_tensor.as_slice().unwrap();
        let first_mask_slice = &mask_slice[0..area];

        if i == iters - 1 {
            final_mask_data = first_mask_slice
                .iter()
                .map(|&val| if val > 0.0 { 255 } else { 0 })
                .collect();
            final_w = w;
            final_h = h;
            break;
        }

        let mut binary_mask = vec![false; area];
        let mut mask_area = 0.0;
        let mut min_x = w;
        let mut min_y = h;
        let mut max_x = 0;
        let mut max_y = 0;

        for (idx, &val) in first_mask_slice.iter().enumerate() {
            if val > 0.0 {
                binary_mask[idx] = true;
                let x = idx % w;
                let y = idx / w;
                min_x = min_x.min(x);
                max_x = max_x.max(x);
                min_y = min_y.min(y);
                max_y = max_y.max(y);
                mask_area += 1.0;
            }
        }

        if mask_area == 0.0 || min_x > max_x {
            final_w = out_size.0 as usize;
            final_h = out_size.1 as usize;
            final_mask_data = vec![0; final_w * final_h];
            break;
        }

        let dt_in = edt_2d(&binary_mask, w, h);
        let mut max_in = 0.0;
        let mut pos_idx = 0;
        for (idx, &v) in dt_in.iter().enumerate() {
            if v > max_in {
                max_in = v;
                pos_idx = idx;
            }
        }
        let pos_y = pos_idx / w;
        let pos_x = pos_idx % w;

        let mut rev_mask = vec![false; area];
        for (idx, is_true) in binary_mask.iter().enumerate() {
            rev_mask[idx] = !is_true;
        }
        let mut dt_out = edt_2d(&rev_mask, w, h);

        for y in 0..h {
            for x in 0..w {
                if x < min_x || x > max_x || y < min_y || y > max_y {
                    dt_out[y * w + x] = 0.0;
                }
            }
        }

        let mut max_out = 0.0;
        let mut neg_idx = 0;
        for (idx, &v) in dt_out.iter().enumerate() {
            if v > max_out {
                max_out = v;
                neg_idx = idx;
            }
        }
        let neg_y = neg_idx / w;
        let neg_x = neg_idx % w;

        point_coords.clear();
        point_labels.clear();

        let photo = |x: usize, y: usize| to_input(x as f64 * back_x, y as f64 * back_y);
        point_coords.push(photo(pos_x, pos_y));
        point_labels.push(1.0);
        point_coords.push(photo(neg_x, neg_y));
        point_labels.push(0.0);
        point_coords.push(photo(min_x, min_y));
        point_labels.push(2.0);
        point_coords.push(photo(max_x, max_y));
        point_labels.push(3.0);
        for &(x, y) in &call.excluded {
            point_coords.push(to_input(x, y));
            point_labels.push(0.0);
        }

        let mut gaus_dt = vec![0.0f32; area];
        let variance = (mask_area / 4.0_f32).max(1.0_f32);
        for (idx, &is_true) in binary_mask.iter().enumerate() {
            if is_true {
                let diff = dt_in[idx] - max_in;
                gaus_dt[idx] = (-(diff * diff) / variance).exp();
            }
        }

        let mask_f32_vec: Vec<f32> = first_mask_slice
            .iter()
            .map(|&v| if v > 0.0 { 15.0 } else { -15.0 })
            .collect();

        let img_mask_f32 =
            ImageBuffer::<Luma<f32>, Vec<f32>>::from_raw(w as u32, h as u32, mask_f32_vec).unwrap();
        let img_gaus_f32 =
            ImageBuffer::<Luma<f32>, Vec<f32>>::from_raw(w as u32, h as u32, gaus_dt).unwrap();

        // SAM 2's low-res mask frame is the stretched square input, so the
        // photo-sized mask is stretched to 256 x 256 as well.
        let resized_mask = imageops::resize(&img_mask_f32, 256, 256, FilterType::Triangle);
        let resized_gaus = imageops::resize(&img_gaus_f32, 256, 256, FilterType::Triangle);

        let rm_raw = resized_mask.as_raw();
        let rg_raw = resized_gaus.as_raw();
        let mut mask_input_flat = vec![0.0f32; 256 * 256];

        for i in 0..(256 * 256) {
            let m_val = rm_raw[i];
            let mut g_val = rg_raw[i];
            if g_val <= 0.0 {
                g_val = 1.0;
            }
            mask_input_flat[i] = m_val * g_val;
        }

        mask_input = Array::from_shape_vec((1, 1, 256, 256), mask_input_flat)
            .unwrap()
            .into_dyn();
        has_mask_input = 1.0;
    }

    GrayImage::from_raw(final_w as u32, final_h as u32, final_mask_data)
        .ok_or_else(|| anyhow::anyhow!("Failed to create mask image from raw data"))
}

/// Runs a model with one float input and returns its first output, which
/// must be finite (a GPU run that fails repeats on the CPU). `run_options`
/// lets another thread stop the run (`RunOptions::terminate`).
fn run_single(
    model: &rawtonic_onnx::Model,
    input: &Array<f32, IxDyn>,
    run_options: Option<&ort::session::RunOptions>,
) -> Result<Array<f32, IxDyn>> {
    model.run(|session| {
        let input = ort::inputs![Tensor::from_array(input.as_standard_layout().into_owned())?];
        let outputs = match run_options {
            Some(options) => session.run_with_options(input, options)?,
            None => session.run(input)?,
        };
        let output = outputs[0].try_extract_array::<f32>()?.to_owned();
        rawtonic_onnx::ensure_finite(output.iter())?;
        Ok(output)
    })
}

/// `image.resize(width, height, Triangle)` (the largest size that fits,
/// aspect kept, as the image crate computes it), in parallel where
/// `resize_exact_triangle` is.
fn resize_fit_triangle(image: &DynamicImage, width: u32, height: u32) -> DynamicImage {
    let (w, h) = image.dimensions();
    if (w, h) == (width, height) {
        return image.clone();
    }
    let ratio = f64::min(width as f64 / w as f64, height as f64 / h as f64);
    let fit = |v: u32| ((v as f64 * ratio).round() as u32).max(1);
    resize_exact_triangle(image, fit(w), fit(h))
}

/// The sky of a photo (Lightroom's Select Sky) as a mask of its size: the
/// network's probability, its edge fitted to the photo (`refine_sky_mask`).
/// A photo without sky stays black.
pub fn run_sky_seg_model(
    image: &DynamicImage,
    sky_seg_session: &SkyModel,
    linear: bool,
    run_options: Option<&ort::session::RunOptions>,
) -> Result<GrayImage> {
    let (orig_width, orig_height) = image.dimensions();
    let coarse = sky_probability(image, sky_seg_session, linear, run_options)?;

    let refine = std::time::Instant::now();
    let refined = refine_sky_mask(image, linear, &coarse);
    let refine_ms = refine.elapsed().as_millis();

    let resize = std::time::Instant::now();
    let full = resize_gray_triangle(&refined, orig_width, orig_height);
    log::info!(
        "AI sky mask: edges {refine_ms} ms, full-size mask {} ms",
        resize.elapsed().as_millis()
    );
    Ok(full)
}

/// The network's sky probability (0 … 255) for the photo fitted into its
/// `SKYSEG_INPUT_SIZE`² input, at the fitted size.
fn sky_probability(
    image: &DynamicImage,
    sky_seg_session: &SkyModel,
    linear: bool,
    run_options: Option<&ort::session::RunOptions>,
) -> Result<GrayImage> {
    let start = std::time::Instant::now();

    let resized_image = resize_fit_triangle(image, SKYSEG_INPUT_SIZE, SKYSEG_INPUT_SIZE);
    let (resized_w, resized_h) = resized_image.dimensions();
    let resized_rgb = display_rgb8(resized_image, linear);
    let raw_pixels = resized_rgb.as_raw();

    let paste_x = ((SKYSEG_INPUT_SIZE - resized_w) / 2) as usize;
    let paste_y = ((SKYSEG_INPUT_SIZE - resized_h) / 2) as usize;

    let side = SKYSEG_INPUT_SIZE as usize;
    let mut input_tensor: Array<f32, _> = Array::zeros((1, 3, side, side));

    let mean = [0.485, 0.456, 0.406];
    let std = [0.229, 0.224, 0.225];

    let rw = resized_w as usize;
    let rh = resized_h as usize;

    {
        let planes = input_tensor
            .as_slice_mut()
            .expect("a new array is contiguous");
        let (r_plane, rest) = planes.split_at_mut(side * side);
        let (g_plane, b_plane) = rest.split_at_mut(side * side);
        for (c, plane) in [r_plane, g_plane, b_plane].into_iter().enumerate() {
            plane
                .par_chunks_mut(side)
                .skip(paste_y)
                .take(rh)
                .enumerate()
                .for_each(|(y, row)| {
                    for x in 0..rw {
                        let v = raw_pixels[(y * rw + x) * 3 + c];
                        row[x + paste_x] = (v as f32 / 255.0 - mean[c]) / std[c];
                    }
                });
        }
    }
    let input_ms = start.elapsed().as_millis();

    let out_slice = sky_seg_session.probability(input_tensor, run_options)?;
    let network_ms = start.elapsed().as_millis() - input_ms;
    anyhow::ensure!(
        out_slice.len() == side * side,
        "the sky model returned {} values",
        out_slice.len()
    );

    // The sky probability (0 … 1) as is: a photo without sky stays black.
    let mut cropped_mask_data = Vec::with_capacity(rw * rh);
    for y in 0..rh {
        let src_y = y + paste_y;
        for x in 0..rw {
            let val = out_slice[src_y * side + x + paste_x];
            cropped_mask_data.push((val.clamp(0.0, 1.0) * 255.0).round() as u8);
        }
    }
    log::info!(
        "AI sky mask: input {input_ms} ms, network {network_ms} ms on {}",
        sky_seg_session.backend_name()
    );
    GrayImage::from_raw(resized_w, resized_h, cropped_mask_data)
        .ok_or_else(|| anyhow::anyhow!("Failed to create mask from Sky Segmentation output"))
}

/// The network predicts at a quarter of its input; a guided filter on the
/// photo brings back thin edges (spires, branches, lamp posts). Returns the
/// refined mask at the guide's size (at most `SKY_GUIDE_SIZE` / 2 long).
fn refine_sky_mask(image: &DynamicImage, linear: bool, coarse: &GrayImage) -> GrayImage {
    refine_sky_mask_with(image, linear, coarse, &SkyEdge::DEFAULT)
}

/// How the sky mask's edge is fitted to the photo (`refine_sky_mask`).
#[derive(Clone, Copy, Debug, PartialEq)]
struct SkyEdge {
    /// The guided filter's radius: the long side of its grid over this.
    radius_div: u32,
    eps: f32,
    /// The contrast curve after the filter: `lo` → 0, `hi` → 1.
    lo: f32,
    hi: f32,
    /// Decides the edge band by colour (`split_sky_by_colour`).
    split: Option<SkySplit>,
}

/// Settings of `split_sky_by_colour`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct SkySplit {
    /// Window for the local sky and ground colours: the long side of the
    /// filter grid over this.
    div: u32,
    /// Weight of colour against lightness.
    chroma: f32,
    /// Sky and ground colours closer than `t0` decide nothing; from
    /// `t0 + t1` apart they decide fully.
    t0: f32,
    t1: f32,
    /// The network's probability above which a pixel counts as sky and
    /// below which as ground, for the local colours.
    sure_sky: f32,
    sure_ground: f32,
    /// Steepness of the colour decision around halfway between the two
    /// colours (1: linear from ground to sky colour).
    gain: f32,
}

impl SkyEdge {
    const DEFAULT: SkyEdge = SkyEdge {
        radius_div: 50,
        eps: 1e-3,
        lo: 0.25,
        hi: 0.75,
        // On 13 CC0 landscapes against reference masks (1024 px, 0.5):
        // IoU 0.960 → 0.982, boundary F 0.587 → 0.814, sky leaking into the
        // ground next to the edge 0.20 → 0.07 (docs/MODELS.md).
        split: Some(SkySplit {
            div: 10,
            chroma: 1.0,
            t0: 0.03,
            t1: 0.1,
            sure_sky: 0.95,
            sure_ground: 0.05,
            gain: 2.0,
        }),
    };
}

/// Decides the mask's edge band by colour: where the filtered mask is
/// neither fully sky nor fully ground, a pixel is sky as far as its colour
/// lies towards the sky's colour nearby rather than the ground's (both
/// taken from where the network is sure). This separates what one
/// lightness guide cannot: bright rock against a blue sky, thin dark
/// branches against the sky between them. Where sky and ground nearby have
/// about the same colour, the filtered mask stays.
fn split_sky_by_colour(
    guide_hr: &image::RgbImage,
    mask_lr: &GrayImage,
    refined: &mut GrayImage,
    split: &SkySplit,
) {
    let (hr_w, hr_h) = guide_hr.dimensions();
    let (lr_w, lr_h) = mask_lr.dimensions();
    let (w, h) = (lr_w as usize, lr_h as usize);
    let area = w * h;
    let feature = |p: &[u8]| -> [f32; 3] {
        let (r, g, b) = (
            p[0] as f32 / 255.0,
            p[1] as f32 / 255.0,
            p[2] as f32 / 255.0,
        );
        let y = 0.299 * r + 0.587 * g + 0.114 * b;
        [y, split.chroma * (b - y), split.chroma * (r - y)]
    };
    let guide_src: Vec<f32> = guide_hr.as_raw().par_iter().map(|&v| v as f32).collect();
    let guide_lr =
        resize_triangle::<3, u8>(&guide_src, (hr_w, hr_h), (lr_w, lr_h), (0.0, 255.0), |v| {
            v.round() as u8
        });
    let features: Vec<[f32; 3]> = guide_lr.par_chunks_exact(3).map(feature).collect();
    let sure_sky = (split.sure_sky * 255.0).round() as u8;
    let sure_ground = (split.sure_ground * 255.0).round() as u8;
    let radius = (lr_w.max(lr_h) / split.div.max(1)).max(2) as usize;
    let mut temp1 = vec![0.0; area];
    let mut temp2 = vec![0.0; area];
    // Share of the window that is sure, below which a window's colour is
    // blended with that of a window three times as wide.
    const FULL_SHARE: f32 = 0.05;
    // Local mean colour of the sure sky (or ground) and its share of the
    // window, on the filter grid: in a window of `radius`, blended with one
    // three times as wide where the narrow one has few sure pixels (no seam
    // where they run out); brought to the guide's size.
    let mut local = |sure: &(dyn Fn(u8) -> bool + Sync)| -> (Vec<Vec<f32>>, Vec<f32>) {
        let weight: Vec<f32> = mask_lr
            .as_raw()
            .par_iter()
            .map(|&v| if sure(v) { 1.0 } else { 0.0 })
            .collect();
        let mut window = |r: usize| {
            let mut share = vec![0.0; area];
            box_filter_opt(&weight, &mut share, &mut temp1, &mut temp2, w, h, r);
            let mut colours = Vec::with_capacity(3);
            for c in 0..3 {
                let plane: Vec<f32> = features
                    .par_iter()
                    .zip(weight.par_iter())
                    .map(|(f, &wt)| f[c] * wt)
                    .collect();
                let mut mean = vec![0.0; area];
                box_filter_opt(&plane, &mut mean, &mut temp1, &mut temp2, w, h, r);
                mean.par_iter_mut()
                    .zip(share.par_iter())
                    .for_each(|(m, &s)| *m = if s > 1e-4 { *m / s } else { 0.0 });
                colours.push(mean);
            }
            (colours, share)
        };
        let (mut colours, mut share) = window(radius);
        let (wide, wide_share) = window(radius * 3);
        for i in 0..area {
            let a = (share[i] / FULL_SHARE).clamp(0.0, 1.0);
            for c in 0..3 {
                colours[c][i] = a * colours[c][i] + (1.0 - a) * wide[c][i];
            }
            share[i] = share[i].max(wide_share[i]);
        }
        let to_hr = |plane: Vec<f32>| {
            resize_triangle::<1, f32>(
                &plane,
                (lr_w, lr_h),
                (hr_w, hr_h),
                (f32::MIN, f32::MAX),
                |v| v,
            )
        };
        (colours.into_iter().map(to_hr).collect(), to_hr(share))
    };
    let (sky, sky_share) = local(&|v| v >= sure_sky);
    let (ground, ground_share) = local(&|v| v <= sure_ground);
    let guide = guide_hr.as_raw();
    refined
        .as_mut()
        .par_iter_mut()
        .enumerate()
        .for_each(|(i, v)| {
            if *v == 0 || *v == 255 {
                return;
            }
            let f = feature(&guide[3 * i..3 * i + 3]);
            let s = [sky[0][i], sky[1][i], sky[2][i]];
            let g = [ground[0][i], ground[1][i], ground[2][i]];
            let d = [s[0] - g[0], s[1] - g[1], s[2] - g[2]];
            let len2 = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
            let len = len2.sqrt();
            // Fades out where sure sky or ground run out, and where their
            // colours are alike.
            let present = (sky_share[i].min(ground_share[i]) / (FULL_SHARE / 5.0)).clamp(0.0, 1.0);
            let weight = present * ((len - split.t0) / split.t1.max(1e-4)).clamp(0.0, 1.0);
            if weight <= 0.0 {
                return;
            }
            let along = ((f[0] - g[0]) * d[0] + (f[1] - g[1]) * d[1] + (f[2] - g[2]) * d[2])
                / len2.max(1e-6);
            let alpha = ((along - 0.5) * split.gain + 0.5).clamp(0.0, 1.0);
            let mixed = weight * alpha + (1.0 - weight) * (*v as f32 / 255.0);
            *v = (mixed.clamp(0.0, 1.0) * 255.0).round() as u8;
        });
}

fn refine_sky_mask_with(
    image: &DynamicImage,
    linear: bool,
    coarse: &GrayImage,
    edge: &SkyEdge,
) -> GrayImage {
    let (orig_width, orig_height) = image.dimensions();
    let guide_source = if orig_width.max(orig_height) > SKY_GUIDE_SIZE {
        resize_fit_triangle(image, SKY_GUIDE_SIZE, SKY_GUIDE_SIZE)
    } else {
        image.clone()
    };
    let guide_rgb = display_rgb8(guide_source, linear);
    let (guide_w, guide_h) = guide_rgb.dimensions();
    let (lr_w, lr_h) = ((guide_w / 2).max(1), (guide_h / 2).max(1));
    let mask_lr = resize_gray_triangle(coarse, lr_w, lr_h);
    // A radius beyond the network's soft edge (about 1/50 of the photo),
    // then a contrast curve: halos where the photo is flat (sky beside a
    // tree, distant hills) fall away and the edge follows the photo.
    let radius = (lr_w.max(lr_h) / edge.radius_div.max(1)).max(2) as usize;
    let guide = DynamicImage::ImageRgb8(guide_rgb.clone()).to_luma8();
    let mut refined = fast_guided_filter(&guide, &mask_lr, radius, edge.eps);
    let span = (edge.hi - edge.lo).max(1e-3);
    for p in refined.pixels_mut() {
        let v = p[0] as f32 / 255.0;
        p[0] = (((v - edge.lo) / span).clamp(0.0, 1.0) * 255.0).round() as u8;
    }
    if let Some(split) = &edge.split {
        split_sky_by_colour(&guide_rgb, &mask_lr, &mut refined, split);
    }
    refined
}

impl SubjectModel {
    /// The ONNX model on ONNX Runtime: a GPU provider when one works, else
    /// the CPU (every platform).
    pub fn onnx(path: &Path) -> Result<Self> {
        Ok(Self {
            backend: SubjectBackend::Onnx(rawtonic_onnx::Model::load(
                path,
                rawtonic_onnx::ModelKind::Subject,
            )?),
        })
    }

    pub fn backend_name(&self) -> String {
        match &self.backend {
            SubjectBackend::Onnx(model) => format!("ONNX Runtime ({})", model.provider()),
            #[cfg(target_os = "macos")]
            SubjectBackend::CoreMl(_) => "Core ML".to_string(),
        }
    }

    /// Runs on ONNX Runtime (Core ML needs none).
    pub fn on_onnx(&self) -> bool {
        matches!(self.backend, SubjectBackend::Onnx(_))
    }

    /// One run on a constant input, which builds Core ML's GPU kernels
    /// (`core_ml_first_run_pending`); `false` on ONNX Runtime, which is not
    /// run.
    pub fn warm_up(&self) -> Result<bool> {
        match &self.backend {
            SubjectBackend::Onnx(_) => Ok(false),
            #[cfg(target_os = "macos")]
            SubjectBackend::CoreMl(model) => {
                let size = SUBJECT_INPUT_SIZE as usize;
                model.warm_up("pixel_values", &[1, 3, size, size], "subject")?;
                Ok(true)
            }
        }
    }
    /// BiRefNet's subject probability (`SUBJECT_INPUT_SIZE`² values) for
    /// a normalised [1, 3, size, size] input. `run_options` lets another
    /// thread stop an ONNX run (`RunOptions::terminate`).
    fn probability(
        &self,
        input: Array<f32, ndarray::Ix4>,
        run_options: Option<&ort::session::RunOptions>,
    ) -> Result<Vec<f32>> {
        match &self.backend {
            SubjectBackend::Onnx(model) => model.run(|session| {
                let t_input = Tensor::from_array(input.clone().into_dyn())?;
                let outputs = match run_options {
                    Some(options) => session.run_with_options(ort::inputs![t_input], options)?,
                    None => session.run(ort::inputs![t_input])?,
                };
                let (_, data) = outputs[0].try_extract_tensor::<f32>()?;
                rawtonic_onnx::ensure_finite(data)?;
                Ok(data.to_vec())
            }),
            #[cfg(target_os = "macos")]
            SubjectBackend::CoreMl(model) => {
                let shape = input.shape().to_vec();
                let data = input.into_raw_vec_and_offset().0;
                Ok(model.predict("pixel_values", &shape, &data, "subject")?.1)
            }
        }
    }
}

/// Weights of `imageops::resize`'s Triangle filter from `src` to `dst`
/// samples: per output sample its first input sample and the normalised
/// weights, computed as the image crate does.
fn triangle_weights(src: u32, dst: u32) -> Vec<(usize, Vec<f32>)> {
    let ratio = src as f32 / dst as f32;
    let sratio = if ratio < 1.0 { 1.0 } else { ratio };
    let support = sratio;
    (0..dst)
        .map(|out| {
            let input = (out as f32 + 0.5) * ratio;
            let left = ((input - support).floor() as i64).clamp(0, src as i64 - 1);
            let right = ((input + support).ceil() as i64).clamp(left + 1, src as i64);
            let input = input - 0.5;
            let mut weights: Vec<f32> = (left..right)
                .map(|i| {
                    let x = (i as f32 - input) / sratio;
                    if x.abs() < 1.0 { 1.0 - x.abs() } else { 0.0 }
                })
                .collect();
            let mut sum = 0.0;
            for w in &weights {
                sum += w;
            }
            for w in &mut weights {
                *w /= sum;
            }
            (left as usize, weights)
        })
        .collect()
}

/// `imageops::resize` with the Triangle filter on `C` interleaved f32
/// channels, rows in parallel: the same weights and order of sums (columns
/// first, then rows), so the same values; results clamped to `lo..=hi` and
/// converted by `store`.
fn resize_triangle<const C: usize, T: Copy + Default + Send + Sync>(
    src: &[f32],
    (width, height): (u32, u32),
    (new_width, new_height): (u32, u32),
    (lo, hi): (f32, f32),
    store: impl Fn(f32) -> T + Sync,
) -> Vec<T> {
    let (w, nw) = (width as usize, new_width as usize);
    let vertical = triangle_weights(height, new_height);
    let mut tmp = vec![0f32; w * new_height as usize * C];
    tmp.par_chunks_mut(w * C)
        .zip(vertical.par_iter())
        .for_each(|(row, (top, weights))| {
            for x in 0..w {
                let mut t = [0f32; C];
                for (i, weight) in weights.iter().enumerate() {
                    let p = ((top + i) * w + x) * C;
                    for c in 0..C {
                        t[c] += src[p + c] * weight;
                    }
                }
                row[x * C..x * C + C].copy_from_slice(&t);
            }
        });
    let horizontal = triangle_weights(width, new_width);
    let mut out = vec![T::default(); nw * new_height as usize * C];
    out.par_chunks_mut(nw * C)
        .zip(tmp.par_chunks(w * C))
        .for_each(|(row, line)| {
            for (x, (left, weights)) in horizontal.iter().enumerate() {
                let mut t = [0f32; C];
                for (i, weight) in weights.iter().enumerate() {
                    let p = (left + i) * C;
                    for c in 0..C {
                        t[c] += line[p + c] * weight;
                    }
                }
                for c in 0..C {
                    row[x * C + c] = store(t[c].clamp(lo, hi));
                }
            }
        });
    out
}

/// `image.resize_exact(width, height, Triangle)`, in parallel for float RGB
/// photos (RAW decodes; 45 MP to 1024² in about a tenth of the time), the
/// image crate's own for other formats.
fn resize_exact_triangle(image: &DynamicImage, width: u32, height: u32) -> DynamicImage {
    match image {
        DynamicImage::ImageRgb32F(rgb) if rgb.dimensions() != (width, height) => {
            let data = resize_triangle::<3, f32>(
                rgb.as_raw(),
                rgb.dimensions(),
                (width, height),
                (0.0, 1.0),
                |v| v,
            );
            DynamicImage::ImageRgb32F(image::Rgb32FImage::from_raw(width, height, data).unwrap())
        }
        // 8-bit photos (JPEG, PNG): the crate sums in f32 and rounds.
        DynamicImage::ImageRgb8(rgb) if rgb.dimensions() != (width, height) => {
            let src: Vec<f32> = rgb.as_raw().par_iter().map(|&v| v as f32).collect();
            let data = resize_triangle::<3, u8>(
                &src,
                rgb.dimensions(),
                (width, height),
                (0.0, 255.0),
                |v| v.round() as u8,
            );
            DynamicImage::ImageRgb8(image::RgbImage::from_raw(width, height, data).unwrap())
        }
        _ => image.resize_exact(width, height, FilterType::Triangle),
    }
}

/// `imageops::resize(mask, width, height, Triangle)` for a mask, in parallel.
fn resize_gray_triangle(mask: &GrayImage, width: u32, height: u32) -> GrayImage {
    if mask.dimensions() == (width, height) {
        return mask.clone();
    }
    let src: Vec<f32> = mask.as_raw().iter().map(|&v| v as f32).collect();
    let out = resize_triangle::<1, u8>(
        &src,
        mask.dimensions(),
        (width, height),
        (0.0, 255.0),
        |v| v.round() as u8,
    );
    GrayImage::from_raw(width, height, out).unwrap()
}

/// BiRefNet's input: the photo stretched to `SUBJECT_INPUT_SIZE`², sRGB
/// 8-bit, ImageNet normalised.
fn subject_input(image: &DynamicImage, linear: bool) -> Array<f32, ndarray::Ix4> {
    let size = SUBJECT_INPUT_SIZE;
    let resized = display_rgb8(resize_exact_triangle(image, size, size), linear);
    let raw_pixels = resized.as_raw();
    let mean = [0.485, 0.456, 0.406];
    let std = [0.229, 0.224, 0.225];
    let side = size as usize;
    let mut input_tensor: Array<f32, _> = Array::zeros((1, 3, side, side));
    for y in 0..side {
        for x in 0..side {
            let idx = (y * side + x) * 3;
            for c in 0..3 {
                input_tensor[[0, c, y, x]] =
                    (raw_pixels[idx + c] as f32 / 255.0 - mean[c]) / std[c];
            }
        }
    }
    input_tensor
}

/// The subject of a photo (Lightroom's Select Subject) as a mask of its
/// size: BiRefNet's probability at 1024 × 1024, resized back. A photo
/// without a clear subject stays dark (no min-max stretching).
pub fn run_subject_model(
    image: &DynamicImage,
    model: &SubjectModel,
    linear: bool,
    run_options: Option<&ort::session::RunOptions>,
) -> Result<GrayImage> {
    let (orig_width, orig_height) = image.dimensions();
    let size = SUBJECT_INPUT_SIZE;
    let start = std::time::Instant::now();
    let input = subject_input(image, linear);
    let input_ms = start.elapsed().as_millis();
    let probability = model.probability(input, run_options)?;
    let network_ms = start.elapsed().as_millis() - input_ms;
    anyhow::ensure!(
        probability.len() == (size * size) as usize,
        "the subject model returned {} values",
        probability.len()
    );
    let mask_data: Vec<u8> = probability
        .iter()
        .map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)
        .collect();
    let mask = GrayImage::from_raw(size, size, mask_data)
        .ok_or_else(|| anyhow::anyhow!("Failed to create mask from the subject model output"))?;
    let resize = std::time::Instant::now();
    let full = resize_gray_triangle(&mask, orig_width, orig_height);
    log::info!(
        "AI subject mask: input {input_ms} ms, network {network_ms} ms, full-size mask {} ms",
        resize.elapsed().as_millis()
    );
    Ok(full)
}

fn depth_anything_values(
    image: &DynamicImage,
    depth_session: &rawtonic_onnx::Model,
    linear: bool,
    max_code: f32,
) -> Result<(u32, u32, Vec<f32>)> {
    let resized_image = image.resize(DEPTH_INPUT_SIZE, DEPTH_INPUT_SIZE, FilterType::Triangle);
    let (resized_w, resized_h) = resized_image.dimensions();
    let resized_rgb = display_rgb8(resized_image, linear);
    let raw_pixels = resized_rgb.as_raw();

    let paste_x = ((DEPTH_INPUT_SIZE - resized_w) / 2) as usize;
    let paste_y = ((DEPTH_INPUT_SIZE - resized_h) / 2) as usize;

    let mut input_tensor: Array<f32, _> =
        Array::zeros((1, 3, DEPTH_INPUT_SIZE as usize, DEPTH_INPUT_SIZE as usize));

    let mean = [0.485, 0.456, 0.406];
    let std = [0.229, 0.224, 0.225];

    let rw = resized_w as usize;
    let rh = resized_h as usize;

    for y in 0..rh {
        for x in 0..rw {
            let idx = (y * rw + x) * 3;
            let dest_y = y + paste_y;
            let dest_x = x + paste_x;

            input_tensor[[0, 0, dest_y, dest_x]] =
                (raw_pixels[idx] as f32 / 255.0 - mean[0]) / std[0];
            input_tensor[[0, 1, dest_y, dest_x]] =
                (raw_pixels[idx + 1] as f32 / 255.0 - mean[1]) / std[1];
            input_tensor[[0, 2, dest_y, dest_x]] =
                (raw_pixels[idx + 2] as f32 / 255.0 - mean[2]) / std[2];
        }
    }

    let output_tensor = run_single(depth_session, &input_tensor.into_dyn(), None)?;
    let out_slice = output_tensor.as_slice().unwrap();

    let usize_size = DEPTH_INPUT_SIZE as usize;

    let mut min_val = f32::MAX;
    let mut max_val = f32::MIN;
    for y in 0..rh {
        let src_y = y + paste_y;
        for x in 0..rw {
            let src_x = x + paste_x;
            let val = out_slice[src_y * usize_size + src_x];
            min_val = min_val.min(val);
            max_val = max_val.max(val);
        }
    }

    let range = max_val - min_val;
    let scale = if range > 1e-6 { max_code / range } else { 0.0 };

    let mut cropped_depth_data = Vec::with_capacity(rw * rh);

    for y in 0..rh {
        let src_y = y + paste_y;
        for x in 0..rw {
            let src_x = x + paste_x;
            let val = out_slice[src_y * usize_size + src_x];
            let pixel = if range > 1e-6 {
                ((val - min_val) * scale).clamp(0.0, max_code)
            } else {
                0.0
            };
            cropped_depth_data.push(pixel);
        }
    }

    Ok((resized_w, resized_h, cropped_depth_data))
}

pub fn run_depth_anything_model(
    image: &DynamicImage,
    model: &rawtonic_onnx::Model,
    linear: bool,
) -> Result<GrayImage> {
    let (w, h, values) = depth_anything_values(image, model, linear, 255.)?;
    GrayImage::from_raw(w, h, values.iter().map(|v| *v as u8).collect())
        .ok_or_else(|| anyhow::anyhow!("Invalid depth dimensions"))
}

/// Legacy analysis precision gate; new Relight uses camera geometry.
#[cfg(test)]
pub fn run_depth_anything_model16(
    image: &DynamicImage,
    model: &rawtonic_onnx::Model,
    linear: bool,
) -> Result<image::ImageBuffer<image::Luma<u16>, Vec<u16>>> {
    let (w, h, values) = depth_anything_values(image, model, linear, 65535.)?;
    image::ImageBuffer::from_raw(w, h, values.iter().map(|v| v.round() as u16).collect())
        .ok_or_else(|| anyhow::anyhow!("Invalid depth dimensions"))
}

/// ONNX Runtime from `ORT_DYLIB_PATH` (bundled by the `RAWTONIC_BUNDLE_ONNX=1`
/// build), loaded once: an error starting with
/// `rawtonic_onnx::RUNTIME_UNAVAILABLE` when it is missing or cannot be
/// loaded (the frontend shows its own text for it), never `ort`'s panic.
pub(crate) fn ensure_local_runtime() -> anyhow::Result<()> {
    rawtonic_onnx::ensure_runtime().map_err(anyhow::Error::new)
}

#[cfg(test)]
mod tool_tests {
    /// Runs MI-GAN on a gradient with a square hole: the hole is filled close
    /// to its surroundings and everything else is kept. Needs the model in
    /// `RAWTONIC_INPAINT_MODEL` and ONNX Runtime in `ORT_DYLIB_PATH`.
    #[test]
    #[ignore]
    fn inpainting_fills_only_the_hole() {
        let Some(model) = std::env::var_os("RAWTONIC_INPAINT_MODEL") else {
            return;
        };
        let session = rawtonic_onnx::Model::load(
            std::path::Path::new(&model),
            rawtonic_onnx::ModelKind::Inpaint,
        )
        .unwrap();
        let image = image::RgbImage::from_fn(640, 480, |x, y| {
            image::Rgb([(x / 3) as u8, (y / 2) as u8, 128])
        });
        let mask = image::GrayImage::from_fn(640, 480, |x, y| {
            image::Luma([if (300..340).contains(&x) && (200..240).contains(&y) {
                255
            } else {
                0
            }])
        });
        let out = super::run_inpainting(
            &image::DynamicImage::ImageRgb8(image.clone()),
            &mask,
            &session,
        )
        .unwrap();
        // ONNX Runtime aborts during process exit, as in the app.
        crate::register_exit_handler();
        let mut worst = 0i32;
        for (x, y, p) in out.enumerate_pixels() {
            let q = image.get_pixel(x, y);
            if mask.get_pixel(x, y)[0] == 0 {
                assert_eq!(&p.0[..3], &q.0[..], "kept at ({x}, {y})");
            } else {
                for c in 0..3 {
                    worst = worst.max((p[c] as i32 - q[c] as i32).abs());
                }
            }
        }
        assert!(worst < 40, "hole differs from the gradient by {worst}");
    }

    /// The sky model on a RAW-like photo: the sky half of a landscape export
    /// comes out white and the ground black. Needs the model in
    /// `RAWTONIC_SKY_MODEL`, a photo in `RAWTONIC_SKY_PHOTO` (sky in the
    /// upper third, none in the lower third) and ONNX Runtime in
    /// `ORT_DYLIB_PATH`.
    #[test]
    #[ignore]
    fn sky_mask_marks_the_sky() {
        let (Some(model), Some(photo)) = (
            std::env::var_os("RAWTONIC_SKY_MODEL"),
            std::env::var_os("RAWTONIC_SKY_PHOTO"),
        ) else {
            return;
        };
        let session = super::SkyModel::onnx(std::path::Path::new(&model)).unwrap();
        let image = image::open(photo).unwrap();
        let mask = super::run_sky_seg_model(&image, &session, false, None).unwrap();
        crate::register_exit_handler();
        if let Some(out) = std::env::var_os("RAWTONIC_SKY_OUT") {
            mask.save(out).unwrap();
        }
        assert_eq!(
            mask.dimensions(),
            image::GenericImageView::dimensions(&image)
        );
        let (w, h) = mask.dimensions();
        let mean = |y0: u32, y1: u32| {
            let mut sum = 0u64;
            for y in y0..y1 {
                for x in 0..w {
                    sum += mask.get_pixel(x, y)[0] as u64;
                }
            }
            sum as f64 / ((y1 - y0) as f64 * w as f64)
        };
        assert!(mean(0, h / 3) > 200.0, "sky {}", mean(0, h / 3));
        assert!(mean(2 * h / 3, h) < 30.0, "ground {}", mean(2 * h / 3, h));
    }

    /// Tool: Sky masks of photos as the app computes them (8-bit photos as
    /// they are, RAWs as their sRGB-encoded linear decode), written as
    /// `<stem>.png` at the photo's size, for comparing edge settings with
    /// reference masks. `SKY_FILES` (`;`-separated), `SKY_OUT`, `AI_MODELS`,
    /// `ORT_DYLIB_PATH`; `SKY_EDGE` overrides the edge settings, e.g.
    /// `color=1,div=100,eps=0.0001,lo=0.25,hi=0.75`.
    #[test]
    #[ignore = "tool: sky masks for evaluation"]
    fn print_sky_masks() {
        let var = |k: &str| std::env::var(k).unwrap();
        let models = std::path::PathBuf::from(var("AI_MODELS"));
        let out = std::path::PathBuf::from(var("SKY_OUT"));
        std::fs::create_dir_all(&out).unwrap();
        // SKY_COREML: a Core ML package instead of the ONNX model (macOS).
        let sky = match std::env::var_os("SKY_COREML") {
            #[cfg(target_os = "macos")]
            Some(package) => super::SkyModel {
                backend: super::SkyBackend::CoreMl(
                    rawtonic_denoise::coreml::CoreMlModel::load(
                        std::path::Path::new(&package),
                        super::CORE_ML_UNITS,
                    )
                    .unwrap(),
                ),
            },
            _ => super::SkyModel::onnx(&models.join(super::SKYSEG_FILENAME)).unwrap(),
        };
        let mut edge = super::SkyEdge::DEFAULT;
        for setting in std::env::var("SKY_EDGE").unwrap_or_default().split(',') {
            let Some((key, value)) = setting.split_once('=') else {
                continue;
            };
            match key {
                "div" => edge.radius_div = value.parse().unwrap(),
                "eps" => edge.eps = value.parse().unwrap(),
                "lo" => edge.lo = value.parse().unwrap(),
                "hi" => edge.hi = value.parse().unwrap(),
                "split" if value == "0" => edge.split = None,
                "split" | "sdiv" | "k" | "t0" | "t1" | "ssky" | "sground" | "gain" => {
                    let split = edge
                        .split
                        .get_or_insert(super::SkyEdge::DEFAULT.split.unwrap());
                    match key {
                        "gain" => split.gain = value.parse().unwrap(),
                        "sdiv" => split.div = value.parse().unwrap(),
                        "k" => split.chroma = value.parse().unwrap(),
                        "t0" => split.t0 = value.parse().unwrap(),
                        "t1" => split.t1 = value.parse().unwrap(),
                        "ssky" => split.sure_sky = value.parse().unwrap(),
                        "sground" => split.sure_ground = value.parse().unwrap(),
                        _ => {}
                    }
                }
                _ => panic!("unknown edge setting {key}"),
            }
        }
        println!("{edge:?}");
        let settings = crate::app_settings::AppSettings::default();
        for path in var("SKY_FILES").split(';') {
            let bytes = std::fs::read(path).unwrap();
            let image = crate::image_loader::load_base_image_from_bytes(
                &bytes, path, false, &settings, None,
            )
            .unwrap();
            let extension = std::path::Path::new(path)
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            let linear = !["jpg", "jpeg", "png", "tif", "tiff"].contains(&extension.as_str());
            let start = std::time::Instant::now();
            let (w, h) = image::GenericImageView::dimensions(&image);
            let coarse = super::sky_probability(&image, &sky, linear, None).unwrap();
            let network = start.elapsed().as_millis();
            let refine = std::time::Instant::now();
            let refined = super::refine_sky_mask_with(&image, linear, &coarse, &edge);
            let refine = refine.elapsed().as_millis();
            let mask = super::resize_gray_triangle(&refined, w, h);
            let elapsed = format!(
                "input+network {network} ms, edges {refine} ms, total {}",
                start.elapsed().as_millis()
            );
            let stem = std::path::Path::new(path)
                .file_stem()
                .unwrap()
                .to_string_lossy()
                .to_string();
            mask.save(out.join(format!("{stem}.png"))).unwrap();
            println!("{path}\t{:?} linear {linear}\t{elapsed} ms", image.color());
        }
        crate::register_exit_handler();
    }

    #[test]
    fn models_come_from_the_pack() {
        assert_eq!(
            super::model_url(super::ENCODER_FILENAME),
            format!("{}/sam2.1_tiny_encoder.onnx", super::MODEL_PACK_URL)
        );
        assert_eq!(
            super::model_url(super::SKYSEG_FILENAME),
            format!("{}/skyseg_upernet_convnext_t.onnx", super::MODEL_PACK_URL)
        );
    }

    #[test]
    fn a_download_with_the_wrong_hash_leaves_the_existing_model() {
        use sha2::Digest;
        let dir = tempfile::tempdir().unwrap();
        let model = dir.path().join("model.onnx");
        std::fs::write(&model, b"old model").unwrap();
        let good = hex::encode(sha2::Sha256::digest(b"new model"));
        assert!(super::persist_verified_asset(&model, b"corrupt", &good).is_err());
        assert_eq!(std::fs::read(&model).unwrap(), b"old model");
        super::persist_verified_asset(&model, b"new model", &good).unwrap();
        assert_eq!(std::fs::read(&model).unwrap(), b"new model");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    /// A model file is hashed once; later checks (also after a relaunch)
    /// trust it while it looks the same, and a changed file is hashed again.
    #[test]
    fn a_verified_model_file_is_hashed_again_only_after_it_changed() {
        use sha2::Digest;
        let dir = tempfile::tempdir().unwrap();
        let model = dir.path().join("model.onnx");
        std::fs::write(&model, b"model bytes").unwrap();
        let good = hex::encode(sha2::Sha256::digest(b"model bytes"));
        assert!(super::verify_sha256(&model, &good).unwrap());
        let list = dir.path().join(super::verified_files::FILE_NAME);
        let text = std::fs::read_to_string(&list).unwrap();
        assert!(text.starts_with(&good) && text.trim_end().ends_with("\tmodel.onnx"));

        // A relaunch reads the list: the recorded result stands without
        // hashing (shown by a list entry whose hash only the list knows).
        let other = hex::encode(sha2::Sha256::digest(b"something else"));
        std::fs::write(&list, text.replacen(&good, &other, 1)).unwrap();
        super::verified_files::reset_memory();
        assert!(super::verify_sha256(&model, &other).unwrap());

        // Writing to the file changes it: it is hashed again.
        super::verified_files::reset_memory();
        std::fs::write(&list, &text).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&model, b"model bytez").unwrap();
        assert!(!super::verify_sha256(&model, &good).unwrap());
        assert!(
            !std::fs::read_to_string(&list)
                .unwrap()
                .contains("model.onnx"),
            "a file that no longer matches leaves the list"
        );
        assert!(!super::verify_sha256(&dir.path().join("missing.onnx"), &good).unwrap());
    }

    /// Linking a checked file into its Core ML package changes the file's
    /// status-change time; the check stays recorded, so the next launch does
    /// not hash the file again (it did, once: 0.27 s for Subject's weights).
    #[cfg(target_os = "macos")]
    #[test]
    fn linking_a_checked_file_into_its_package_keeps_the_check() {
        use sha2::Digest;
        let dir = tempfile::tempdir().unwrap();
        let model = dir.path().join("x_coreml_weight.bin");
        std::fs::write(&model, b"weights").unwrap();
        let good = hex::encode(sha2::Sha256::digest(b"weights"));
        assert!(super::verify_sha256(&model, &good).unwrap());
        let target = dir.path().join("x.mlpackage/Data/weights/weight.bin");
        assert!(super::link_into_package(&model, &target, &good).unwrap());
        super::verified_files::refresh(&model, &good);
        assert!(!super::link_into_package(&model, &target, &good).unwrap());

        // A relaunch trusts the list (an entry whose hash only the list knows).
        let list = dir.path().join(super::verified_files::FILE_NAME);
        let other = hex::encode(sha2::Sha256::digest(b"something else"));
        let text = std::fs::read_to_string(&list).unwrap();
        std::fs::write(&list, text.replacen(&good, &other, 1)).unwrap();
        super::verified_files::reset_memory();
        assert!(super::verify_sha256(&model, &other).unwrap());
    }

    /// A package file of the right size but other content (a stale or
    /// damaged copy) is replaced by the checked file, and the compiled
    /// model goes with it. AI Denoise's packages compared sizes only.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_package_file_of_the_same_size_but_other_content_is_replaced() {
        use sha2::Digest;
        let dir = tempfile::tempdir().unwrap();
        let model = dir.path().join("x_coreml_weight.bin");
        std::fs::write(&model, b"weights").unwrap();
        let good = hex::encode(sha2::Sha256::digest(b"weights"));
        let target = dir.path().join("x.mlpackage/Data/weights/weight.bin");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, b"stale!!").unwrap();
        let compiled = dir.path().join("x.mlmodelc");
        std::fs::create_dir_all(&compiled).unwrap();
        assert!(super::link_into_package(&model, &target, &good).unwrap());
        assert_eq!(std::fs::read(&target).unwrap(), b"weights");
        assert!(!compiled.exists(), "the compiled model is built again");
    }

    #[test]
    fn linear_raw_decodes_reach_the_networks_srgb_encoded() {
        let linear = image::Rgb32FImage::from_pixel(2, 1, image::Rgb([0.18, 0.0, 1.0]));
        let display = super::display_rgb8(image::DynamicImage::ImageRgb32F(linear.clone()), true);
        // Linear 18 % grey is 118 in sRGB (46 if passed through unencoded).
        assert_eq!(display.get_pixel(0, 0).0, [118, 0, 255]);
        let plain = super::display_rgb8(image::DynamicImage::ImageRgb32F(linear), false);
        assert_eq!(plain.get_pixel(0, 0).0, [46, 0, 255]);
    }

    #[test]
    fn the_mask_edge_guide_of_a_linear_decode_is_srgb_encoded() {
        let linear = image::Rgb32FImage::from_pixel(2, 1, image::Rgb([0.18, 0.18, 0.18]));
        let image = image::DynamicImage::ImageRgb32F(linear);
        // Linear 18 % grey is 118 in sRGB; the plain 8-bit conversion gives 46.
        assert_eq!(super::guide_luma8(&image, true).get_pixel(0, 0)[0], 118);
        assert_eq!(super::guide_luma8(&image, false).get_pixel(0, 0)[0], 46);
    }

    /// The parallel Triangle resize gives the image crate's values exactly:
    /// float RGB photos down and up (with values beyond 1, which both clamp)
    /// and masks up and down.
    #[test]
    fn the_parallel_resize_equals_the_image_crate() {
        let rgb = image::Rgb32FImage::from_fn(613, 409, |x, y| {
            let v = ((x * 31 + y * 17 + (x * y) % 13) % 97) as f32 / 80.0;
            image::Rgb([v, 1.0 - v * 0.5, (x as f32 / 613.0).powf(2.2)])
        });
        let photo = image::DynamicImage::ImageRgb32F(rgb);
        let photo8 = image::DynamicImage::ImageRgb8(photo.to_rgb8());
        for (w, h) in [(256, 256), (100, 333), (613, 200), (1024, 1024), (900, 700)] {
            assert_eq!(
                super::resize_exact_triangle(&photo, w, h),
                photo.resize_exact(w, h, image::imageops::FilterType::Triangle),
                "{w}x{h}"
            );
            assert_eq!(
                super::resize_exact_triangle(&photo8, w, h),
                photo8.resize_exact(w, h, image::imageops::FilterType::Triangle),
                "8-bit {w}x{h}"
            );
        }
        for (w, h) in [(512, 512), (2048, 2048), (613, 409), (100, 50)] {
            for p in [&photo, &photo8] {
                assert_eq!(
                    super::resize_fit_triangle(p, w, h),
                    p.resize(w, h, image::imageops::FilterType::Triangle),
                    "fit {w}x{h}"
                );
            }
        }
        // A float plane (the guided filter's coefficients), values beyond
        // 0..1 clamped as the crate clamps float pixels.
        let plane = image::ImageBuffer::<image::Luma<f32>, Vec<f32>>::from_fn(97, 61, |x, y| {
            image::Luma([((x * 7 + y * 13) % 29) as f32 / 20.0 - 0.3])
        });
        for (w, h) in [(400, 250), (50, 30)] {
            assert_eq!(
                super::resize_triangle::<1, f32>(
                    plane.as_raw(),
                    (97, 61),
                    (w, h),
                    (0.0, 1.0),
                    |v| v
                ),
                image::imageops::resize(&plane, w, h, image::imageops::FilterType::Triangle)
                    .into_raw(),
                "plane {w}x{h}"
            );
        }
        let mask = image::GrayImage::from_fn(128, 96, |x, y| {
            image::Luma([((x * 37 + y * 11 + (x ^ y)) % 256) as u8])
        });
        for (w, h) in [(1000, 751), (4096, 2731), (64, 64), (128, 300)] {
            assert_eq!(
                super::resize_gray_triangle(&mask, w, h),
                image::imageops::resize(&mask, w, h, image::imageops::FilterType::Triangle),
                "{w}x{h}"
            );
        }
    }

    #[test]
    fn the_banded_blur_equals_the_plain_blur() {
        let image = image::GrayImage::from_fn(97, 301, |x, y| {
            image::Luma([((x * 31 + y * 17 + (x * y) % 13) % 256) as u8])
        });
        for sigma in [0.3, 1.0, 2.6, 7.8, 23.4] {
            let plain = image::imageops::blur(&image, sigma);
            for band in [16, 50, 128] {
                assert_eq!(
                    super::parallel_blur(&image, sigma, band),
                    plain,
                    "sigma {sigma} band {band}"
                );
            }
        }
    }

    /// The refinement on a coarser grid gives the full-size result within a
    /// few levels: a disc on a photo with the same disc, a mask at full
    /// size and one three times coarser.
    #[test]
    fn the_coarse_refinement_matches_the_full_size_one() {
        let (w, h) = (900u32, 600u32);
        let inside = |x: f32, y: f32| (x - 450.0).powi(2) + (y - 300.0).powi(2) < 200.0f32.powi(2);
        let guide = image::GrayImage::from_fn(w, h, |x, y| {
            image::Luma([if inside(x as f32, y as f32) { 200 } else { 40 }])
        });
        let full = image::GrayImage::from_fn(w, h, |x, y| {
            image::Luma([if inside(x as f32, y as f32) { 255 } else { 0 }])
        });
        let step = super::refine_step(w, 300);
        assert_eq!(step, 3);
        let coarse = image::GrayImage::from_fn(w / 3, h / 3, |x, y| {
            image::Luma([if inside(x as f32 * 3.0 + 1.0, y as f32 * 3.0 + 1.0) {
                255
            } else {
                0
            }])
        });
        let radius = super::sam_refine_radius(w);
        let a = super::refine_sam_mask(&full, &guide, radius, 1);
        let b = super::refine_sam_mask(&coarse, &guide, radius, step);
        assert_eq!(b.dimensions(), (w, h));
        // Same edge position (threshold 128) almost everywhere; the soft
        // transition differs by a few levels on average.
        let (mut flipped, mut total) = (0usize, 0i64);
        for (p, q) in a.pixels().zip(b.pixels()) {
            flipped += ((p[0] > 127) != (q[0] > 127)) as usize;
            total += (p[0] as i64 - q[0] as i64).abs();
        }
        let mean = total as f64 / (w * h) as f64;
        assert!(
            flipped * 1000 < (w * h) as usize,
            "{flipped} pixels flipped"
        );
        assert!(mean < 1.0, "mean difference {mean:.2}");
    }

    #[test]
    fn the_refinement_radius_follows_the_photo_size() {
        let r = |e| super::sam_refine_radius(e);
        assert!(r(400) < r(1600) && r(1600) < r(6000));
    }

    /// Terminating the run options stops SAM's encoder at once (a cancelled
    /// encoding ahead), and the model stays usable; models from `$AI_MODELS`,
    /// runtime from `ORT_DYLIB_PATH`.
    #[test]
    #[ignore = "needs AI_MODELS and ORT_DYLIB_PATH"]
    fn a_terminated_encoding_stops_and_the_encoder_stays_usable() {
        let models = std::path::PathBuf::from(std::env::var("AI_MODELS").unwrap());
        let encoder =
            rawtonic_onnx::Model::load_file(&models.join(super::ENCODER_FILENAME)).unwrap();
        let image = image::DynamicImage::ImageRgb8(image::RgbImage::new(1600, 1200));
        super::generate_image_embeddings(&image, &encoder, false, None).unwrap();
        let full = std::time::Instant::now();
        super::generate_image_embeddings(&image, &encoder, false, None).unwrap();
        let full = full.elapsed();
        let options = std::sync::Arc::new(ort::session::RunOptions::new().unwrap());
        let stopper = {
            let options = options.clone();
            std::thread::spawn(move || {
                std::thread::sleep(full / 10);
                options.terminate().unwrap();
            })
        };
        let stopped = std::time::Instant::now();
        assert!(super::generate_image_embeddings(&image, &encoder, false, Some(&options)).is_err());
        let stopped = stopped.elapsed();
        stopper.join().unwrap();
        println!(
            "full encoding {full:?}, terminated after {stopped:?} on {}",
            encoder.provider()
        );
        assert!(
            stopped < full,
            "terminated after {stopped:?}, full {full:?}"
        );
        super::generate_image_embeddings(&image, &encoder, false, None).unwrap();
    }

    /// An Object selection built prompt by prompt on one photo, timed per
    /// prompt with the encoder's embedding computed once (as the app keeps
    /// it): "decoder" is the decoder calls alone, "mask" adds the edge
    /// refinement the app runs. `AI_PROMPTS`: `;`-separated steps, each
    /// `p:x,y` (click) or `b:x0,y0,x1,y1` (box) in fractions of the photo,
    /// `-` in front excludes; `AI_OUT` gets an overlay per step as a new file
    /// (an existing one stops the test). Models from
    /// `$AI_MODELS`, runtime from `ORT_DYLIB_PATH`, photo `$AI_FILE`.
    #[test]
    #[ignore = "needs AI_MODELS, ORT_DYLIB_PATH, AI_FILE, AI_PROMPTS and AI_OUT"]
    fn print_object_prompt_sequence() {
        let var = |k: &str| std::env::var(k).unwrap();
        let models = std::path::PathBuf::from(var("AI_MODELS"));
        let out = std::path::PathBuf::from(var("AI_OUT"));
        let session = |name: &str| rawtonic_onnx::Model::load_file(&models.join(name)).unwrap();
        let encoder = session(super::ENCODER_FILENAME);
        let decoder = session(super::DECODER_FILENAME);
        let path = var("AI_FILE");
        let bytes = std::fs::read(&path).unwrap();
        let settings = crate::app_settings::AppSettings::default();
        let image =
            crate::image_loader::load_base_image_from_bytes(&bytes, &path, false, &settings, None)
                .unwrap();
        let linear = std::env::var("AI_LINEAR").is_ok();
        let (w, h) = (image.width(), image.height());
        let started = std::time::Instant::now();
        let embeddings = super::generate_image_embeddings(&image, &encoder, linear, None).unwrap();
        println!(
            "{path} {w}x{h}: encoder {} ms on {}",
            started.elapsed().as_millis(),
            encoder.provider()
        );
        let at = |fx: f64, fy: f64| (fx * w as f64, fy * h as f64);
        let mut prompts = Vec::new();
        for (step, spec) in var("AI_PROMPTS").split(';').enumerate() {
            let (exclude, spec) = match spec.strip_prefix('-') {
                Some(rest) => (true, rest),
                None => (false, spec),
            };
            let (kind, numbers) = spec.split_once(':').unwrap();
            let n: Vec<f64> = numbers.split(',').map(|v| v.parse().unwrap()).collect();
            prompts.push(match kind {
                "p" => super::SamPrompt::Point {
                    at: at(n[0], n[1]),
                    exclude,
                },
                _ => super::SamPrompt::Box {
                    min: at(n[0], n[1]),
                    max: at(n[2], n[3]),
                    exclude,
                },
            });
            // Warm the decoder once, as the app's earlier prompts have.
            if step == 0 {
                super::run_sam_decoder_prompts(&decoder, &embeddings, &prompts, None, linear)
                    .unwrap();
            }
            let refine = super::refine_step(w.max(h), super::SAM_REFINE_EDGE);
            let calls = super::sam_calls(&prompts);
            let started = std::time::Instant::now();
            super::sam_selection_mask(
                &decoder,
                &embeddings,
                &calls,
                (w.div_ceil(refine), h.div_ceil(refine)),
            )
            .unwrap();
            let decoder_ms = started.elapsed().as_secs_f64() * 1000.0;
            let started = std::time::Instant::now();
            let mask = super::run_sam_decoder_prompts(
                &decoder,
                &embeddings,
                &prompts,
                Some(&image),
                linear,
            )
            .unwrap();
            let mask_ms = started.elapsed().as_secs_f64() * 1000.0;
            let share = mask.pixels().filter(|p| p[0] > 127).count() as f64 / (w * h) as f64;
            println!(
                "step {} ({spec}{}): {} decoder calls, decoder {decoder_ms:.0} ms, mask {mask_ms:.0} ms, selected {:.1} %",
                step + 1,
                if exclude { ", excluded" } else { "" },
                calls.len(),
                share * 100.0
            );
            let small = |img: &image::RgbImage| {
                let s = 1200.0 / w.max(h) as f32;
                image::imageops::resize(
                    img,
                    (w as f32 * s).round() as u32,
                    (h as f32 * s).round() as u32,
                    image::imageops::FilterType::Triangle,
                )
            };
            let mut overlay = image.to_rgb8();
            for (pixel, m) in overlay.pixels_mut().zip(mask.pixels()) {
                let a = m[0] as f32 / 255.0 * 0.55;
                pixel[0] = (pixel[0] as f32 * (1.0 - a) + 255.0 * a) as u8;
                pixel[1] = (pixel[1] as f32 * (1.0 - a)) as u8;
                pixel[2] = (pixel[2] as f32 * (1.0 - a)) as u8;
            }
            // Written whole beside it, then published under a new name only
            // (never over an existing file, the photo itself included).
            let mut png = Vec::new();
            small(&overlay)
                .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
                .unwrap();
            let mut temp = tempfile::NamedTempFile::new_in(&out).unwrap();
            std::io::Write::write_all(&mut temp, &png).unwrap();
            temp.as_file().sync_all().unwrap();
            let target = out.join(format!("step{}.png", step + 1));
            rawtonic_metadata::no_clobber::persist(temp, &target)
                .unwrap_or_else(|e| panic!("{}: {e}", target.display()));
        }
    }

    /// The subject mask of a box around a bright patch covers the patch in
    /// every aspect ratio (a decoder that bakes in its export example's crop
    /// misplaces it); models from `$AI_MODELS`, runtime from `ORT_DYLIB_PATH`.
    #[test]
    #[ignore = "needs AI_MODELS and ORT_DYLIB_PATH"]
    fn subject_masks_follow_the_box_in_every_aspect_ratio() {
        let models = std::path::PathBuf::from(std::env::var("AI_MODELS").unwrap());
        let session = |name: &str| rawtonic_onnx::Model::load_file(&models.join(name)).unwrap();
        let encoder = session(super::ENCODER_FILENAME);
        let decoder = session(super::DECODER_FILENAME);
        for (w, h) in [(1800u32, 1200u32), (1600, 1200), (1200, 1800), (1400, 1400)] {
            let (x0, y0, x1, y1) = (w * 3 / 10, h * 4 / 10, w * 6 / 10, h * 8 / 10);
            let inside = |x: u32, y: u32| (x0..x1).contains(&x) && (y0..y1).contains(&y);
            let image = image::RgbImage::from_fn(w, h, |x, y| {
                if inside(x, y) {
                    image::Rgb([235, 225, 210])
                } else {
                    image::Rgb([40, 60, 90])
                }
            });
            let image = image::DynamicImage::ImageRgb8(image);
            let embeddings =
                super::generate_image_embeddings(&image, &encoder, false, None).unwrap();
            let margin = (w.min(h) / 50) as f64;
            let mask = super::run_sam_decoder(
                &decoder,
                &embeddings,
                (x0 as f64 - margin, y0 as f64 - margin),
                (x1 as f64 + margin, y1 as f64 + margin),
                None,
                false,
            )
            .unwrap();
            assert_eq!(mask.dimensions(), (w, h));
            let (mut both, mut either) = (0u64, 0u64);
            for (x, y, p) in mask.enumerate_pixels() {
                let (m, t) = (p[0] > 127, inside(x, y));
                both += (m && t) as u64;
                either += (m || t) as u64;
            }
            let iou = both as f64 / either as f64;
            assert!(iou > 0.9, "{w}x{h}: mask IoU with the patch {iou:.3}");
        }
    }

    /// Boundary F of two binary masks: the share of each one's boundary
    /// pixels within `tol` pixels of the other's, harmonic mean.
    fn boundary_f(pred: &[bool], truth: &[bool], w: usize, h: usize, tol: f32) -> f64 {
        let boundary = |m: &[bool]| -> Vec<bool> {
            (0..m.len())
                .map(|i| {
                    let (x, y) = (i % w, i / w);
                    m[i] && (x == 0
                        || y == 0
                        || x == w - 1
                        || y == h - 1
                        || !m[i - 1]
                        || !m[i + 1]
                        || !m[i - w]
                        || !m[i + w])
                })
                .collect()
        };
        let (bp, bt) = (boundary(pred), boundary(truth));
        let count = |b: &[bool]| b.iter().filter(|&&v| v).count();
        let (np, nt) = (count(&bp), count(&bt));
        if np == 0 || nt == 0 {
            return (np == nt) as u8 as f64;
        }
        let not = |b: &[bool]| b.iter().map(|&v| !v).collect::<Vec<_>>();
        let to_t = super::edt_2d(&not(&bt), w, h);
        let to_p = super::edt_2d(&not(&bp), w, h);
        let near = |b: &[bool], d: &[f32]| (0..b.len()).filter(|&i| b[i] && d[i] <= tol).count();
        let precision = near(&bp, &to_t) as f64 / np as f64;
        let recall = near(&bt, &to_p) as f64 / nt as f64;
        if precision + recall == 0.0 {
            0.0
        } else {
            2.0 * precision * recall / (precision + recall)
        }
    }

    /// Tool: quality of the app's click and box masks against ground-truth
    /// masks, per edge refinement: mean IoU and boundary F (3 px; both at
    /// threshold 128) and mean absolute error of the soft mask. `$SAM_EVAL`
    /// lists `image.jpg;mask.png` lines (a held-out dataset such as DUTS-TE;
    /// docs/MODELS.md); the click goes to the mask's innermost pixel, the box
    /// is the mask's bounding box. `$SAM_UPSCALE` (default 1) enlarges photo
    /// and mask first (a stand-in for larger photos; tolerance scaled too);
    /// `$SAM_LINEAR=1` feeds the photo as a linear decode, as the app does
    /// for RAWs. Models from `$AI_MODELS`, runtime from `ORT_DYLIB_PATH`.
    #[test]
    #[ignore = "tool: needs SAM_EVAL, AI_MODELS and ORT_DYLIB_PATH"]
    fn print_sam_mask_quality() {
        use image::GenericImageView;
        let models = std::path::PathBuf::from(std::env::var("AI_MODELS").unwrap());
        let session = |name: &str| rawtonic_onnx::Model::load_file(&models.join(name)).unwrap();
        let encoder = session(super::ENCODER_FILENAME);
        let decoder = session(super::DECODER_FILENAME);
        let list = std::fs::read_to_string(std::env::var("SAM_EVAL").unwrap()).unwrap();
        let upscale: u32 = std::env::var("SAM_UPSCALE").map_or(1, |v| v.parse().unwrap());
        let linear = std::env::var("SAM_LINEAR").is_ok_and(|v| v == "1");
        // Refinements: None = the decoder's hard mask, else the radius for
        // the long edge. "app" is `sam_refine_radius`, "8-24 px" the rule
        // before 2026-10-10, "k cells" k × (long edge / 256), SAM's low-res
        // mask cell.
        type Rule = (&'static str, fn(u32) -> Option<f32>);
        let rules: [Rule; 8] = [
            ("hard", |_| None),
            ("app, full size", |e| Some(super::sam_refine_radius(e))),
            ("8-24 px", |e| Some((e as f32 * 0.0075).clamp(8.0, 24.0))),
            ("0.5 cells", |e| Some((e as f32 / 256.0 * 0.5).max(1.0))),
            ("1 cell", |e| Some((e as f32 / 256.0).max(1.0))),
            ("1.5 cells", |e| Some((e as f32 / 256.0 * 1.5).max(1.0))),
            ("2 cells", |e| Some((e as f32 / 256.0 * 2.0).max(1.0))),
            ("3 cells", |e| Some((e as f32 / 256.0 * 3.0).max(1.0))),
        ];
        const COARSE_EDGES: [u32; 3] = [super::SAM_REFINE_EDGE, 1024, 512];
        // [rule][click, box][IoU, boundary F, MAE] sums; extra rows: the app
        // rule on coarser grids, then with the linear guide used before
        // 2026-10-10.
        let mut sums = vec![[[0.0f64; 3]; 2]; rules.len() + COARSE_EDGES.len() + 1];
        let mut count = 0usize;
        let (mut encode_s, mut decode_s, mut refine_s) = (0.0, 0.0, 0.0);
        for line in list.lines().filter(|l| !l.trim().is_empty()) {
            let (image_path, mask_path) = line.split_once(';').unwrap();
            let mut image = image::open(image_path).unwrap();
            let mut truth = image::open(mask_path).unwrap().to_luma8();
            if image.dimensions() != truth.dimensions() {
                continue;
            }
            if upscale > 1 {
                let (w, h) = image.dimensions();
                let (uw, uh) = (w * upscale, h * upscale);
                image = image.resize_exact(uw, uh, image::imageops::FilterType::Lanczos3);
                truth =
                    image::imageops::resize(&truth, uw, uh, image::imageops::FilterType::Triangle);
            }
            let (w, h) = truth.dimensions();
            let inside: Vec<bool> = truth.pixels().map(|p| p[0] > 127).collect();
            if !inside.contains(&true) {
                continue;
            }
            if linear {
                let mut f = image.to_rgb32f();
                for v in f.iter_mut() {
                    *v = if *v <= 0.04045 {
                        *v / 12.92
                    } else {
                        ((*v + 0.055) / 1.055).powf(2.4)
                    };
                }
                image = image::DynamicImage::ImageRgb32F(f);
            }
            let distance = super::edt_2d(&inside, w as usize, h as usize);
            let innermost = (0..distance.len()).fold(0, |best, i| {
                if distance[i] > distance[best] {
                    i
                } else {
                    best
                }
            });
            let click = (
                (innermost % w as usize) as f64,
                (innermost / w as usize) as f64,
            );
            let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
            for (x, y, p) in truth.enumerate_pixels() {
                if p[0] > 127 {
                    (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
                }
            }
            let start = std::time::Instant::now();
            let embeddings =
                super::generate_image_embeddings(&image, &encoder, linear, None).unwrap();
            encode_s += start.elapsed().as_secs_f64();
            let tol = 3.0 * upscale as f32;
            let score = |mask: &image::GrayImage| -> [f64; 3] {
                let pred: Vec<bool> = mask.pixels().map(|p| p[0] > 127).collect();
                let (mut both, mut either) = (0u64, 0u64);
                for (&m, &t) in pred.iter().zip(&inside) {
                    both += (m && t) as u64;
                    either += (m || t) as u64;
                }
                let mae = mask
                    .pixels()
                    .zip(truth.pixels())
                    .map(|(p, t)| (p[0] as f64 - t[0] as f64).abs() / 255.0)
                    .sum::<f64>()
                    / pred.len() as f64;
                [
                    both as f64 / either.max(1) as f64,
                    boundary_f(&pred, &inside, w as usize, h as usize, tol),
                    mae,
                ]
            };
            let start = std::time::Instant::now();
            let box_prompt = ((x0 as f64, y0 as f64), (x1 as f64, y1 as f64));
            let hard_at = |size: (u32, u32)| {
                [
                    super::sam_hard_mask(&decoder, &embeddings, click, click, size).unwrap(),
                    super::sam_hard_mask(&decoder, &embeddings, box_prompt.0, box_prompt.1, size)
                        .unwrap(),
                ]
            };
            let hard = hard_at((w, h));
            decode_s += start.elapsed().as_secs_f64() / 2.0;
            let guide = super::guide_luma8(&image, linear);
            let start = std::time::Instant::now();
            let mut line_out = format!("{image_path}:");
            for (r, (name, rule)) in rules.iter().enumerate() {
                for (k, mask) in hard.iter().enumerate() {
                    let refined = match rule(w.max(h)) {
                        None => mask.clone(),
                        Some(radius) => super::refine_sam_mask(mask, &guide, radius, 1),
                    };
                    let s = score(&refined);
                    for m in 0..3 {
                        sums[r][k][m] += s[m];
                    }
                    if *name == "app, full size" {
                        line_out += &format!(" {} {:.3}", ["click", "box"][k], s[0]);
                    }
                }
            }
            refine_s += start.elapsed().as_secs_f64() / (2 * (rules.len() - 1)) as f64;
            // The app's rule on a coarser grid (the final mask and the
            // refinement's coefficients at long edge <= `edge`).
            let radius = super::sam_refine_radius(w.max(h));
            for (e, edge) in COARSE_EDGES.iter().enumerate() {
                let step = super::refine_step(w.max(h), *edge);
                let coarse = hard_at((w.div_ceil(step), h.div_ceil(step)));
                for (k, mask) in coarse.iter().enumerate() {
                    let s = score(&super::refine_sam_mask(mask, &guide, radius, step));
                    for m in 0..3 {
                        sums[rules.len() + e][k][m] += s[m];
                    }
                }
            }
            if linear {
                let old_guide = image.to_luma8();
                for (k, mask) in hard.iter().enumerate() {
                    let s = score(&super::refine_sam_mask(mask, &old_guide, radius, 1));
                    for m in 0..3 {
                        sums[rules.len() + COARSE_EDGES.len()][k][m] += s[m];
                    }
                }
            }
            println!("{line_out}");
            count += 1;
        }
        crate::register_exit_handler();
        let n = count.max(1) as f64;
        println!(
            "{count} images (upscale {upscale}, linear {linear}): encode {:.2} s, decode (two passes) {:.3} s, refinement {:.3} s per mask",
            encode_s / n,
            decode_s / n,
            refine_s / n
        );
        println!("refinement: click IoU / boundary F / MAE | box IoU / boundary F / MAE");
        let names = rules
            .iter()
            .map(|(name, _)| name.to_string())
            .chain(COARSE_EDGES.iter().map(|e| format!("app, grid edge {e}")))
            .chain(linear.then(|| "app, linear guide (old)".to_string()));
        for (r, name) in names.enumerate() {
            let [c, b] = sums[r];
            println!(
                "{name:24} {:.3} / {:.3} / {:.4} | {:.3} / {:.3} / {:.4}",
                c[0] / n,
                c[1] / n,
                c[2] / n,
                b[0] / n,
                b[1] / n,
                b[2] / n
            );
        }
    }

    /// Tool: Rawtonic's sky and subject masks of the RAWs in `$AI_FILES`
    /// (`;`-separated), written as PNGs (long edge 1200) to `$AI_OUT`;
    /// models from `$AI_MODELS`, runtime from `ORT_DYLIB_PATH`.
    #[test]
    #[ignore = "tool: needs AI_FILES, AI_OUT, AI_MODELS and ORT_DYLIB_PATH"]
    fn print_sky_and_subject_masks() {
        let var = |k: &str| std::env::var(k).unwrap();
        let models = std::path::PathBuf::from(var("AI_MODELS"));
        let out = std::path::PathBuf::from(var("AI_OUT"));
        let sky = super::SkyModel::onnx(&models.join(super::SKYSEG_FILENAME)).unwrap();
        let subject = super::SubjectModel::onnx(&models.join(super::SUBJECT_FILENAME)).unwrap();
        let settings = crate::app_settings::AppSettings::default();
        for path in var("AI_FILES").split(';') {
            let bytes = std::fs::read(path).unwrap();
            let image = crate::image_loader::load_base_image_from_bytes(
                &bytes, path, false, &settings, None,
            )
            .unwrap();
            let mean: f32 = {
                let f = image.to_rgb32f();
                f.as_raw().iter().sum::<f32>() / f.as_raw().len() as f32
            };
            println!("{path}\t{:?} mean {mean:.3}", image.color());
            // As the app does for RAWs; AI_LINEAR=1 feeds the linear decode.
            let linear = std::env::var("AI_LINEAR").is_err();
            let stem = std::path::Path::new(path)
                .file_stem()
                .unwrap()
                .to_string_lossy()
                .to_string();
            for (label, mask) in [
                (
                    "sky",
                    super::run_sky_seg_model(&image, &sky, linear, None).unwrap(),
                ),
                (
                    "subject",
                    super::run_subject_model(&image, &subject, linear, None).unwrap(),
                ),
            ] {
                let (w, h) = mask.dimensions();
                let s = 1200.0 / w.max(h) as f32;
                let small = image::imageops::resize(
                    &mask,
                    (w as f32 * s).round() as u32,
                    (h as f32 * s).round() as u32,
                    image::imageops::FilterType::Triangle,
                );
                small.save(out.join(format!("{stem}_{label}.png"))).unwrap();
            }
            println!("{path}\tdone");
        }
    }

    /// Tool: time of the Subject mask's steps around the network on a 45 MP
    /// photo (input, full-size mask, PNG).
    #[test]
    #[ignore = "tool: timing"]
    fn print_subject_step_times() {
        use std::time::Instant;
        let (w, h) = (8192u32, 5464u32);
        let image = image::DynamicImage::ImageRgb32F(image::Rgb32FImage::from_fn(w, h, |x, y| {
            image::Rgb([x as f32 / w as f32, y as f32 / h as f32, 0.3])
        }));
        let start = Instant::now();
        let input = super::subject_input(&image, true);
        println!("input {:?} ({:?})", start.elapsed(), input.shape());
        let mask =
            image::GrayImage::from_fn(1024, 1024, |x, y| image::Luma([((x ^ y) & 255) as u8]));
        let start = Instant::now();
        let _ = image.resize_exact(1024, 1024, image::imageops::FilterType::Triangle);
        println!("input, image crate {:?}", start.elapsed());
        let start = Instant::now();
        let full = super::resize_gray_triangle(&mask, w, h);
        println!("full-size mask {:?}", start.elapsed());
        let start = Instant::now();
        let _ = image::imageops::resize(&mask, w, h, image::imageops::FilterType::Triangle);
        println!("full-size mask, image crate {:?}", start.elapsed());
        let start = Instant::now();
        let mut buf = std::io::Cursor::new(Vec::new());
        full.write_to(&mut buf, image::ImageFormat::Png).unwrap();
        println!("PNG {:?}, {} bytes", start.elapsed(), buf.get_ref().len());
    }

    /// Tool: BiRefNet on Core ML against the ONNX model on the photos in
    /// `$AI_FILES` (`;`-separated, as the app prepares them): load times
    /// (`$SUBJECT_COREML`, the .mlpackage; its .mlmodelc is made beside it
    /// on the first load), inference times and the masks' IoU at 128.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "tool: needs AI_FILES, AI_MODELS, SUBJECT_COREML and ORT_DYLIB_PATH"]
    fn print_subject_core_ml_agreement() {
        use rawtonic_denoise::coreml::{ComputeUnits, CoreMlModel};
        use std::time::Instant;
        let var = |k: &str| std::env::var(k).unwrap();
        let package = std::path::PathBuf::from(var("SUBJECT_COREML"));
        let units = match std::env::var("SUBJECT_UNITS").as_deref() {
            Ok("all") => ComputeUnits::All,
            Ok("ne") => ComputeUnits::CpuAndNeuralEngine,
            _ => super::CORE_ML_UNITS,
        };
        let mut loads = Vec::new();
        let mut core_ml = None;
        let low_precision = std::env::var("SUBJECT_LOWP").is_ok();
        for _ in 0..3 {
            let start = Instant::now();
            core_ml = Some(CoreMlModel::load_with(&package, units, low_precision).unwrap());
            loads.push(start.elapsed().as_secs_f32());
        }
        println!("{units:?}: loads {loads:?} s");
        let core_ml = super::SubjectModel {
            backend: super::SubjectBackend::CoreMl(core_ml.unwrap()),
        };
        let onnx = super::SubjectModel::onnx(
            &std::path::PathBuf::from(var("AI_MODELS")).join(super::SUBJECT_FILENAME),
        )
        .unwrap();
        let settings = crate::app_settings::AppSettings::default();
        let (mut ious, mut times) = (Vec::new(), Vec::new());
        for path in var("AI_FILES").split(';') {
            let bytes = std::fs::read(path).unwrap();
            let image = crate::image_loader::load_base_image_from_bytes(
                &bytes, path, false, &settings, None,
            )
            .unwrap();
            let raw = !path.to_lowercase().ends_with(".jpg");
            let start = Instant::now();
            let a = super::run_subject_model(&image, &core_ml, raw, None).unwrap();
            let t_core_ml = start.elapsed().as_secs_f32();
            let start = Instant::now();
            let b = super::run_subject_model(&image, &onnx, raw, None).unwrap();
            let t_onnx = start.elapsed().as_secs_f32();
            let (mut inter, mut union) = (0u64, 0u64);
            for (p, q) in a.pixels().zip(b.pixels()) {
                let (p, q) = (p[0] >= 128, q[0] >= 128);
                inter += (p && q) as u64;
                union += (p || q) as u64;
            }
            let iou = if union == 0 {
                1.0
            } else {
                inter as f64 / union as f64
            };
            println!("{path}\tIoU {iou:.5}\tCore ML {t_core_ml:.2} s\tONNX {t_onnx:.2} s");
            ious.push(iou);
            times.push(t_core_ml);
        }
        let n = ious.len() as f64;
        println!(
            "{} photos: IoU mean {:.5}, min {:.5}; Core ML mean {:.2} s",
            ious.len(),
            ious.iter().sum::<f64>() / n,
            ious.iter().cloned().fold(1.0, f64::min),
            times.iter().sum::<f32>() / times.len() as f32
        );
    }

    /// Tool: the app's Subject masks (`run_subject_model`, mask at the
    /// photo's size) of the photos in `$AI_FILES` (`;`-separated, decoded
    /// as the app does) from two Core ML packages, `$SUBJECT_COREML_A` and
    /// `$SUBJECT_COREML_B` (e.g. fp32 and fp16), written to `$AI_OUT` as
    /// `<stem>_a.png` and `<stem>_b.png`, with the network's sRGB view of
    /// the photo at full size (`<stem>_full.jpg`) and at 2048 px
    /// (`<stem>_2048.png`) for references and crops.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "tool: needs AI_FILES, AI_OUT, SUBJECT_COREML_A and SUBJECT_COREML_B"]
    fn print_subject_precision_masks() {
        use rawtonic_denoise::coreml::CoreMlModel;
        use std::time::Instant;
        let var = |k: &str| std::env::var(k).unwrap();
        let out = std::path::PathBuf::from(var("AI_OUT"));
        let load = |key: &str| super::SubjectModel {
            backend: super::SubjectBackend::CoreMl(
                CoreMlModel::load(&std::path::PathBuf::from(var(key)), super::CORE_ML_UNITS)
                    .unwrap(),
            ),
        };
        let (a, b) = (load("SUBJECT_COREML_A"), load("SUBJECT_COREML_B"));
        let settings = crate::app_settings::AppSettings::default();
        for path in var("AI_FILES").split(';') {
            let stem = std::path::Path::new(path)
                .file_stem()
                .unwrap()
                .to_string_lossy()
                .to_string();
            let bytes = std::fs::read(path).unwrap();
            let image = match crate::image_loader::load_base_image_from_bytes(
                &bytes, path, false, &settings, None,
            ) {
                Ok(image) => image,
                Err(e) => {
                    println!("{path}\tnot decoded: {e}");
                    continue;
                }
            };
            let raw = !path.to_lowercase().ends_with(".jpg");
            let start = Instant::now();
            let mask_a = super::run_subject_model(&image, &a, raw, None).unwrap();
            let t_a = start.elapsed().as_secs_f32();
            let start = Instant::now();
            let mask_b = super::run_subject_model(&image, &b, raw, None).unwrap();
            let t_b = start.elapsed().as_secs_f32();
            mask_a.save(out.join(format!("{stem}_a.png"))).unwrap();
            mask_b.save(out.join(format!("{stem}_b.png"))).unwrap();
            let view = super::display_rgb8(image.clone(), raw);
            let (w, h) = view.dimensions();
            let mut jpeg = std::io::BufWriter::new(
                std::fs::File::create(out.join(format!("{stem}_full.jpg"))).unwrap(),
            );
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 92)
                .encode_image(&view)
                .unwrap();
            let s = 2048.0 / w.max(h) as f32;
            image::imageops::resize(
                &view,
                (w as f32 * s).round() as u32,
                (h as f32 * s).round() as u32,
                image::imageops::FilterType::Triangle,
            )
            .save(out.join(format!("{stem}_2048.png")))
            .unwrap();
            println!("{path}\t{stem}\t{w}x{h}\tA {t_a:.2} s\tB {t_b:.2} s");
        }
    }

    /// Memory of Subject and Sky on Core ML, step by step (`ai_lifecycle`):
    /// the process footprint after each step and the highest value seen
    /// during it (sampled every 2 ms), over `MEMORY_CYCLES` (default 2)
    /// load → run → idle → unload cycles. `$SUBJECT_COREML` and
    /// `$SKY_COREML` are the .mlpackage folders (compiled beside them on
    /// the first load).
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "tool: needs SUBJECT_COREML and SKY_COREML"]
    fn print_mask_model_memory() {
        use rawtonic_denoise::coreml::CoreMlModel;
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::time::{Duration, Instant};
        let footprint = || {
            let mut info: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
            unsafe {
                libc::proc_pid_rusage(
                    std::process::id() as libc::c_int,
                    libc::RUSAGE_INFO_V4,
                    (&mut info as *mut libc::rusage_info_v4).cast(),
                )
            };
            info.ri_phys_footprint
        };
        let highest = std::sync::Arc::new(AtomicU64::new(0));
        let sampler = highest.clone();
        std::thread::spawn(move || {
            loop {
                sampler.fetch_max(footprint(), Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(2));
            }
        });
        let step = |name: &str, f: &mut dyn FnMut()| {
            highest.store(footprint(), Ordering::Relaxed);
            let start = Instant::now();
            f();
            let ms = start.elapsed().as_millis();
            std::thread::sleep(Duration::from_millis(20));
            println!(
                "{name:<24} {ms:>6} ms   footprint {:>6.0} MB   highest during the step {:>6.0} MB",
                footprint() as f64 / 1e6,
                highest.load(Ordering::Relaxed) as f64 / 1e6
            );
        };
        let subject_package = std::path::PathBuf::from(std::env::var("SUBJECT_COREML").unwrap());
        let sky_package = std::path::PathBuf::from(std::env::var("SKY_COREML").unwrap());
        let cycles = std::env::var("MEMORY_CYCLES").map_or(2, |c| c.parse().unwrap());
        let subject_input = vec![0.25f32; 3 * 1024 * 1024];
        let sky_input = vec![0.25f32; 3 * 512 * 512];
        step("start", &mut || {});
        for cycle in 0..cycles {
            println!("cycle {cycle}");
            let (mut subject, mut sky) = (None, None);
            step("load Subject", &mut || {
                subject = Some(CoreMlModel::load(&subject_package, super::CORE_ML_UNITS).unwrap())
            });
            step("load Sky", &mut || {
                sky = Some(CoreMlModel::load(&sky_package, super::CORE_ML_UNITS).unwrap())
            });
            for run in 1..=2 {
                step(&format!("Subject run {run}"), &mut || {
                    let model = subject.as_ref().unwrap();
                    model
                        .predict(
                            "pixel_values",
                            &[1, 3, 1024, 1024],
                            &subject_input,
                            "subject",
                        )
                        .unwrap();
                });
            }
            step("Sky run", &mut || {
                let model = sky.as_ref().unwrap();
                model
                    .predict("pixel_values", &[1, 3, 512, 512], &sky_input, "sky")
                    .unwrap();
            });
            for seconds in [1, 2, 5] {
                std::thread::sleep(Duration::from_secs(seconds));
                step(&format!("idle {seconds} s more"), &mut || {});
            }
            step("unload both", &mut || {
                subject = None;
                sky = None;
            });
            std::thread::sleep(Duration::from_secs(3));
            step("3 s after unloading", &mut || {});
        }
    }

    /// Whether a warm-up on a constant input makes a photo's first run as
    /// fast as later ones: decodes `$AI_FILES` (one photo, as the app
    /// does), loads `$WARM_MODEL` (`subject` from `$SUBJECT_COREML`, or `sky`
    /// from `$SKY_COREML`), runs it once on zeros when `$WARM_UP` is `1`,
    /// then times three network runs on the photo, with the process
    /// footprint and its highest value per step. Run each way in fresh
    /// processes. (2026-10-11, M5 Pro, fp16: Subject's first photo run
    /// 0.41-0.46 s without, 0.17-0.31 s after a 0.41-0.48 s warm-up; Sky
    /// 0.10 s vs 0.06-0.07 s. In the app the same warm-up did not shorten
    /// the photo's first run; notes: reference/birefnet-fp16.md.)
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "tool: needs AI_FILES, WARM_MODEL, SUBJECT_COREML or SKY_COREML"]
    fn print_mask_warm_up() {
        use rawtonic_denoise::coreml::CoreMlModel;
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::time::{Duration, Instant};
        let footprint = || {
            let mut info: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
            unsafe {
                libc::proc_pid_rusage(
                    std::process::id() as libc::c_int,
                    libc::RUSAGE_INFO_V4,
                    (&mut info as *mut libc::rusage_info_v4).cast(),
                )
            };
            info.ri_phys_footprint
        };
        let highest = std::sync::Arc::new(AtomicU64::new(0));
        let sampler = highest.clone();
        std::thread::spawn(move || {
            loop {
                sampler.fetch_max(footprint(), Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(2));
            }
        });
        let step = |name: &str, f: &mut dyn FnMut()| {
            highest.store(footprint(), Ordering::Relaxed);
            let start = Instant::now();
            f();
            let ms = start.elapsed().as_millis();
            std::thread::sleep(Duration::from_millis(20));
            println!(
                "{name:<20} {ms:>6} ms   footprint {:>6.0} MB   highest during the step {:>6.0} MB",
                footprint() as f64 / 1e6,
                highest.load(Ordering::Relaxed) as f64 / 1e6
            );
        };
        let var = |k: &str| std::env::var(k).unwrap();
        let path = var("AI_FILES");
        let settings = crate::app_settings::AppSettings::default();
        let bytes = std::fs::read(&path).unwrap();
        let image =
            crate::image_loader::load_base_image_from_bytes(&bytes, &path, false, &settings, None)
                .unwrap();
        let warm = std::env::var("WARM_UP").as_deref() == Ok("1");
        let load = |key: &str| {
            CoreMlModel::load(std::path::Path::new(&var(key)), super::CORE_ML_UNITS).unwrap()
        };
        let zeros = |size: u32| ndarray::Array4::<f32>::zeros((1, 3, size as usize, size as usize));
        match var("WARM_MODEL").as_str() {
            "subject" => {
                let mut model = None;
                step("load", &mut || {
                    model = Some(super::SubjectModel {
                        backend: super::SubjectBackend::CoreMl(load("SUBJECT_COREML")),
                    })
                });
                let model = model.unwrap();
                if warm {
                    step("warm-up", &mut || {
                        model
                            .probability(zeros(super::SUBJECT_INPUT_SIZE), None)
                            .unwrap();
                    });
                }
                for run in 1..=3 {
                    let input = super::subject_input(&image, true);
                    step(&format!("photo run {run}"), &mut || {
                        model.probability(input.clone(), None).unwrap();
                    });
                }
            }
            "sky" => {
                let mut model = None;
                step("load", &mut || {
                    model = Some(super::SkyModel {
                        backend: super::SkyBackend::CoreMl(load("SKY_COREML")),
                    })
                });
                let model = model.unwrap();
                if warm {
                    step("warm-up", &mut || {
                        model
                            .probability(zeros(super::SKYSEG_INPUT_SIZE), None)
                            .unwrap();
                    });
                }
                for run in 1..=3 {
                    step(&format!("photo run {run}"), &mut || {
                        super::sky_probability(&image, &model, true, None).unwrap();
                    });
                }
            }
            other => panic!("WARM_MODEL {other}"),
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod first_run_tests {
    use rawtonic_denoise::coreml::FirstRunMarker;

    /// Only installed Core ML files with no first run here ask for a
    /// warm-up: an ONNX model alone is not loaded for it.
    #[test]
    fn a_warm_up_needs_the_core_ml_files_and_no_first_run() {
        let dir = tempfile::tempdir().unwrap();
        let files = super::SUBJECT_CORE_ML_FILES;
        let marker = FirstRunMarker::new(&dir.path().join("first-runs"), "subject").unwrap();
        std::fs::write(dir.path().join(super::SUBJECT_FILENAME), b"onnx").unwrap();
        assert!(!super::first_run_pending(dir.path(), &files, Some(&marker)));
        for (file, _, _) in files {
            std::fs::write(dir.path().join(file), b"x").unwrap();
        }
        assert!(super::first_run_pending(dir.path(), &files, Some(&marker)));
        assert!(!super::first_run_pending(dir.path(), &files, None));
        marker.set().unwrap();
        assert!(!super::first_run_pending(dir.path(), &files, Some(&marker)));
    }
}

#[cfg(test)]
mod sam_prompt_tests {
    use super::{SamCall, SamPrompt, sam_calls, sam_prompt_arrays};

    fn include(at: (f64, f64)) -> SamPrompt {
        SamPrompt::Point { at, exclude: false }
    }

    /// A recipe's one click or box gives the call the decoder always had: a
    /// point labelled 1, or the box's corners (any drag direction) 2 and 3.
    #[test]
    fn one_click_or_box_is_the_single_call_of_before() {
        assert_eq!(
            sam_calls(&[SamPrompt::legacy((10.0, 20.0), (10.0, 20.0))]),
            [SamCall {
                points: vec![(10.0, 20.0)],
                labels: vec![1.0],
                excluded: vec![],
                subtract: false,
            }]
        );
        assert_eq!(
            sam_calls(&[SamPrompt::legacy((300.0, 40.0), (100.0, 200.0))]),
            [SamCall {
                points: vec![(100.0, 40.0), (300.0, 200.0)],
                labels: vec![2.0, 3.0],
                excluded: vec![],
                subtract: false,
            }]
        );
    }

    /// Each included prompt is a call of its own with every exclude point
    /// (label 0) after its own; an excluded box is a call to subtract.
    #[test]
    fn several_prompts_give_one_call_per_object_with_the_exclude_points() {
        let calls = sam_calls(&[
            include((10.0, 10.0)),
            SamPrompt::Box {
                min: (50.0, 50.0),
                max: (90.0, 80.0),
                exclude: false,
            },
            SamPrompt::Point {
                at: (12.0, 30.0),
                exclude: true,
            },
            SamPrompt::Box {
                min: (60.0, 60.0),
                max: (70.0, 70.0),
                exclude: true,
            },
        ]);
        assert_eq!(
            calls,
            [
                SamCall {
                    points: vec![(10.0, 10.0), (12.0, 30.0)],
                    labels: vec![1.0, 0.0],
                    excluded: vec![(12.0, 30.0)],
                    subtract: false,
                },
                SamCall {
                    points: vec![(50.0, 50.0), (90.0, 80.0), (12.0, 30.0)],
                    labels: vec![2.0, 3.0, 0.0],
                    excluded: vec![(12.0, 30.0)],
                    subtract: false,
                },
                SamCall {
                    points: vec![(60.0, 60.0), (70.0, 70.0)],
                    labels: vec![2.0, 3.0],
                    excluded: vec![],
                    subtract: true,
                },
            ]
        );
    }

    #[test]
    fn exclude_points_alone_select_nothing() {
        assert!(
            sam_calls(&[SamPrompt::Point {
                at: (1.0, 2.0),
                exclude: true,
            }])
            .is_empty()
        );
    }

    /// The decoder's inputs: `point_coords` 1 × N × 2 and `point_labels`
    /// 1 × N, in the call's order.
    #[test]
    fn prompt_arrays_have_the_decoder_shapes() {
        let (coords, labels) =
            sam_prompt_arrays(&[(1.0, 2.0), (3.0, 4.0), (5.0, 6.0)], &[1.0, 1.0, 0.0]).unwrap();
        assert_eq!(coords.shape(), [1, 3, 2]);
        assert_eq!(
            coords.iter().copied().collect::<Vec<_>>(),
            [1.0, 2.0, 3.0, 4.0, 5.0, 6.0]
        );
        assert_eq!(labels.shape(), [1, 3]);
        assert_eq!(labels.iter().copied().collect::<Vec<_>>(), [1.0, 1.0, 0.0]);
    }
}

#[cfg(test)]
mod relight_tests {
    #[test]
    #[ignore = "requires RELIGHT_PHOTO, RELIGHT_MODEL, RELIGHT_OUT and ONNX Runtime"]
    fn relight_depth_preserves_surface_precision() {
        let image = image::open(std::env::var("RELIGHT_PHOTO").expect("RELIGHT_PHOTO")).unwrap();
        let model = rawtonic_onnx::Model::load(
            std::path::Path::new(&std::env::var("RELIGHT_MODEL").expect("RELIGHT_MODEL")),
            rawtonic_onnx::ModelKind::Depth,
        )
        .unwrap();
        let start = std::time::Instant::now();
        let depth = super::run_depth_anything_model16(&image, &model, false).unwrap();
        println!(
            "Relight first inference + preprocessing: {:.1} ms, {}x{}",
            start.elapsed().as_secs_f64() * 1000.,
            depth.width(),
            depth.height()
        );
        let distinct = depth
            .as_raw()
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>()
            .len();
        assert!(
            distinct > 256,
            "depth lost its surface precision: {distinct} values"
        );
        assert!(depth.width().max(depth.height()) <= super::DEPTH_INPUT_SIZE);
        let dir = std::path::PathBuf::from(std::env::var("RELIGHT_OUT").expect("RELIGHT_OUT"));
        std::fs::create_dir_all(&dir).unwrap();
        // Test artifacts only; never write alongside or over the photograph.
        depth.save(dir.join("depth16.png")).unwrap();
        for i in 0..3 {
            let start = std::time::Instant::now();
            let next = super::run_depth_anything_model16(&image, &model, false).unwrap();
            assert_eq!(depth.dimensions(), next.dimensions());
            println!(
                "Relight warm inference {i}: {:.1} ms",
                start.elapsed().as_secs_f64() * 1000.
            );
        }
    }
}

#[cfg(test)]
mod download_tests {
    use super::persist_downloaded_asset;

    /// A download replaces the installed file by a rename, without deleting
    /// it first, through a temporary file of its own: the fixed
    /// `.NAME.download` another load or the CLI may be writing is left alone.
    #[test]
    fn a_download_replaces_the_model_without_touching_other_writers_files() {
        let dir = tempfile::tempdir().unwrap();
        let model = dir.path().join("model.onnx");
        std::fs::write(&model, b"old model").unwrap();
        let other = dir.path().join(".model.onnx.download");
        std::fs::write(&other, b"another load").unwrap();
        persist_downloaded_asset(&model, b"new model").unwrap();
        assert_eq!(std::fs::read(&model).unwrap(), b"new model");
        assert_eq!(std::fs::read(&other).unwrap(), b"another load");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
        assert!(persist_downloaded_asset(&model, b"").is_err());
        assert_eq!(std::fs::read(&model).unwrap(), b"new model");
    }
}
