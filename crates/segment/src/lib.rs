// SPDX-License-Identifier: AGPL-3.0-or-later
//! The automatic selections of a photo, independent of Tauri: its sky
//! (UperNet + ConvNeXt-T, MIT; Lightroom's Select Sky) and its subject
//! (BiRefNet, MIT; Select Subject) as masks at the photo's size, on ONNX
//! Runtime or, on macOS with the model pack's Core ML programs, Core ML.
//! The app loads the models (downloads, caches, lifecycle) and runs them
//! for the editor and its exports; the CLI loads them from the app's model
//! folder (`Models`), so both compute the same masks. docs/MODELS.md
//! records the models and their measurements.

use std::io::Cursor;
use std::path::Path;

use anyhow::Result;
use base64::{Engine as _, engine::general_purpose};
use image::imageops::FilterType;
use image::{DynamicImage, GenericImageView, GrayImage, ImageFormat};
use ndarray::{Array, IxDyn};
use ort::value::Tensor;
use rayon::prelude::*;

mod models;
pub use models::{Models, file_has_sha256};

// BiRefNet (Peng Zheng et al., MIT weights; the general checkpoint with a
// Swin-L backbone), exported by scripts/models/export_birefnet.py: the photo
// stretched to 1024 × 1024 in, the subject probability out. It replaced
// U²-Net: on 300 DUTS-TE images IoU 0.883 vs 0.715, boundary F 0.882 vs
// 0.693 (docs/MODELS.md).
pub const SUBJECT_FILENAME: &str = "birefnet_subject.onnx";
pub const SUBJECT_INPUT_SIZE: u32 = 1024;
pub const SUBJECT_SHA256: &str = "1019aa4d150a10830d95d05b0e0e08ed2703c39d3e168dea3333bb0272c16cbf";
// The same checkpoint as a Core ML program in fp16
// (scripts/models/convert_birefnet_coreml.py), macOS's fast path: about
// 0.2 s on the GPU instead of 4-5 s on the CPU. Against subject references
// on 36 public photos (hair, fur, feathers) its masks score as the fp32
// program's (IoU 0.9169 vs 0.9170), which took 0.5 s and held 4.4 GB during
// a run instead of 1.6 GB (docs/MODELS.md). Same input and output as the
// ONNX model. Its three files are put back together into the package Core
// ML compiles.
#[cfg(target_os = "macos")]
pub const SUBJECT_CORE_ML_PACKAGE: &str = "birefnet_fp16.mlpackage";
#[cfg(target_os = "macos")]
pub const SUBJECT_CORE_ML_FILES: [(&str, &str, &str); 3] = [
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
pub const CORE_ML_UNITS: rawtonic_denoise::coreml::ComputeUnits =
    rawtonic_denoise::coreml::ComputeUnits::CpuAndGpu;

// UperNet + ConvNeXt-T on ADE20K (Meta's checkpoint, MIT), exported by
// scripts/models/export_sky.py: the photo letterboxed into 512 × 512 in, the
// sky probability out.
pub const SKYSEG_FILENAME: &str = "skyseg_upernet_convnext_t.onnx";
pub const SKYSEG_INPUT_SIZE: u32 = 512;
pub const SKYSEG_SHA256: &str = "e5dc2a0bee8b4cdfbd866d8f811b4390d04ba621644d99de25478338a5b15bbf";
// The same checkpoint as a Core ML program (scripts/models/convert_sky_coreml.py,
// fp32), macOS's fast path: about 0.06 s on the GPU instead of 0.35-0.8 s
// on the CPU, the same probabilities as the PyTorch model (the ONNX file
// stores fp16 weights: masks within IoU 0.993). Same input and output as the
// ONNX model.
#[cfg(target_os = "macos")]
pub const SKY_CORE_ML_PACKAGE: &str = "skyseg_upernet.mlpackage";
#[cfg(target_os = "macos")]
pub const SKY_CORE_ML_FILES: [(&str, &str, &str); 3] = [
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
pub const SKY_GUIDE_SIZE: u32 = 2048;

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
    pub fn probability(
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

pub fn box_filter_horiz(src: &[f32], dest: &mut [f32], w: usize, r: usize) {
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

pub fn transpose(src: &[f32], dest: &mut [f32], w: usize, h: usize) {
    dest.par_chunks_exact_mut(h)
        .enumerate()
        .for_each(|(x, dest_col)| {
            for y in 0..h {
                dest_col[y] = src[y * w + x];
            }
        });
}

pub fn box_filter_opt(
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

/// The networks were trained on display images: a linear RAW decode is
/// sRGB-encoded after downscaling to the network's input. Fed linear values,
/// the sky network found no sky on 9 of 11 RAW landscapes with sky.
pub fn display_rgb8(image: DynamicImage, linear: bool) -> image::RgbImage {
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

/// Runs a model with one float input and returns its first output, which
/// must be finite (a GPU run that fails repeats on the CPU). `run_options`
/// lets another thread stop the run (`RunOptions::terminate`).
pub fn run_single(
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
pub fn resize_fit_triangle(image: &DynamicImage, width: u32, height: u32) -> DynamicImage {
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
pub fn sky_probability(
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
pub fn refine_sky_mask(image: &DynamicImage, linear: bool, coarse: &GrayImage) -> GrayImage {
    refine_sky_mask_with(image, linear, coarse, &SkyEdge::DEFAULT)
}

/// How the sky mask's edge is fitted to the photo (`refine_sky_mask`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SkyEdge {
    /// The guided filter's radius: the long side of its grid over this.
    pub radius_div: u32,
    pub eps: f32,
    /// The contrast curve after the filter: `lo` → 0, `hi` → 1.
    pub lo: f32,
    pub hi: f32,
    /// Decides the edge band by colour (`split_sky_by_colour`).
    pub split: Option<SkySplit>,
}

/// Settings of `split_sky_by_colour`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SkySplit {
    /// Window for the local sky and ground colours: the long side of the
    /// filter grid over this.
    pub div: u32,
    /// Weight of colour against lightness.
    pub chroma: f32,
    /// Sky and ground colours closer than `t0` decide nothing; from
    /// `t0 + t1` apart they decide fully.
    pub t0: f32,
    pub t1: f32,
    /// The network's probability above which a pixel counts as sky and
    /// below which as ground, for the local colours.
    pub sure_sky: f32,
    pub sure_ground: f32,
    /// Steepness of the colour decision around halfway between the two
    /// colours (1: linear from ground to sky colour).
    pub gain: f32,
}

impl SkyEdge {
    pub const DEFAULT: SkyEdge = SkyEdge {
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
pub fn split_sky_by_colour(
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

pub fn refine_sky_mask_with(
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
    pub fn probability(
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
pub fn triangle_weights(src: u32, dst: u32) -> Vec<(usize, Vec<f32>)> {
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
pub fn resize_triangle<const C: usize, T: Copy + Default + Send + Sync>(
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
pub fn resize_exact_triangle(image: &DynamicImage, width: u32, height: u32) -> DynamicImage {
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
pub fn resize_gray_triangle(mask: &GrayImage, width: u32, height: u32) -> GrayImage {
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
pub fn subject_input(image: &DynamicImage, linear: bool) -> Array<f32, ndarray::Ix4> {
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

impl SkyModel {
    /// A Core ML program of the model pack (`SKY_CORE_ML_PACKAGE`).
    #[cfg(target_os = "macos")]
    pub fn core_ml(model: rawtonic_denoise::coreml::CoreMlModel) -> Self {
        Self {
            backend: SkyBackend::CoreMl(model),
        }
    }
}

impl SubjectModel {
    /// A Core ML program of the model pack (`SUBJECT_CORE_ML_PACKAGE`).
    #[cfg(target_os = "macos")]
    pub fn core_ml(model: rawtonic_denoise::coreml::CoreMlModel) -> Self {
        Self {
            backend: SubjectBackend::CoreMl(model),
        }
    }
}

/// A mask as the PNG data URL a recipe stores (`maskDataBase64`).
pub fn mask_data_url(image: &GrayImage) -> Result<String, String> {
    let mut buf = Cursor::new(Vec::new());
    image
        .write_to(&mut buf, ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    let base64_str = general_purpose::STANDARD.encode(buf.get_ref());
    Ok(format!("data:image/png;base64,{}", base64_str))
}
