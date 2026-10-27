// SPDX-License-Identifier: AGPL-3.0-or-later
//! Speed and agreement of a model on a GPU provider against the CPU, for
//! `rawtonic ai-bench` and the ignored provider tests. Inputs are built from
//! the model's declared inputs (a photo when one is given, else a synthetic
//! picture), as the app prepares them; the same inputs go to both providers.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use ort::session::Session;
use ort::value::TensorElementType;
use ort::value::{DynValue, Tensor, ValueType};

use crate::{Model, ModelKind, Provider};

/// An 8-bit RGB picture, row by row.
pub struct Picture {
    pub width: usize,
    pub height: usize,
    pub rgb: Vec<u8>,
}

impl Picture {
    /// Smooth gradients with a bright disc and a dark bar, so masks and
    /// depth are not degenerate when no photo is given.
    pub fn synthetic(width: usize, height: usize) -> Self {
        let mut rgb = Vec::with_capacity(width * height * 3);
        for y in 0..height {
            for x in 0..width {
                let (u, v) = (x as f32 / width as f32, y as f32 / height as f32);
                let disc = ((u - 0.5).powi(2) + (v - 0.55).powi(2)).sqrt() < 0.2;
                let bar = (0.1..0.2).contains(&u);
                let (r, g, b) = if disc {
                    (230.0, 180.0, 60.0)
                } else if bar {
                    (30.0, 30.0, 40.0)
                } else {
                    (90.0 + 120.0 * v, 140.0 + 60.0 * (1.0 - v), 200.0 - 80.0 * u)
                };
                rgb.extend([r as u8, g as u8, b as u8]);
            }
        }
        Self { width, height, rgb }
    }

    /// Sample at (u, v) in 0..1 (nearest pixel), as 0..1 RGB.
    fn at(&self, u: f32, v: f32) -> [f32; 3] {
        let x = ((u * self.width as f32) as usize).min(self.width - 1);
        let y = ((v * self.height as f32) as usize).min(self.height - 1);
        let i = (y * self.width + x) * 3;
        [
            self.rgb[i] as f32 / 255.0,
            self.rgb[i + 1] as f32 / 255.0,
            self.rgb[i + 2] as f32 / 255.0,
        ]
    }
}

#[derive(Clone, Debug, Default)]
pub struct Timing {
    pub load_ms: f64,
    pub first_run_ms: f64,
    /// The later runs, sorted.
    pub runs_ms: Vec<f64>,
}

impl Timing {
    pub fn median_ms(&self) -> Option<f64> {
        (!self.runs_ms.is_empty()).then(|| self.runs_ms[self.runs_ms.len() / 2])
    }
}

/// How one output on the GPU compares with the CPU's.
#[derive(Clone, Debug)]
pub struct Agreement {
    pub output: String,
    pub max_abs: f64,
    pub mean_abs: f64,
    /// `max_abs` over the CPU output's range (its magnitude for a single value).
    pub max_rel_range: f64,
    /// For masks: intersection over union of the thresholded masks.
    pub iou: Option<f64>,
    pub finite: bool,
}

#[derive(Clone, Debug)]
pub struct Report {
    pub file: String,
    pub kind: ModelKind,
    pub cpu: Timing,
    pub gpu_provider: Provider,
    /// The GPU device, when the provider loaded the model.
    pub gpu_device: Option<String>,
    pub gpu: Option<Timing>,
    pub agreement: Vec<Agreement>,
    pub gpu_error: Option<String>,
}

/// Times `path` on the CPU and on `gpu` (`runs` timed runs after the
/// first) and compares the outputs.
pub fn bench_file(
    path: &Path,
    gpu: Provider,
    runs: usize,
    dirs: &[PathBuf],
    picture: &Picture,
) -> Result<Report> {
    let file = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_string();
    let kind = ModelKind::from_file_name(&file)
        .ok_or_else(|| anyhow!("{file}: not a file of the model pack"))?;
    let (cpu_model, cpu_load) =
        timed(|| Model::load_checked(path, kind, &[Provider::Cpu], dirs, false))?;
    let inputs =
        cpu_model.run_without_fallback(|session| Ok(inputs_for(session, kind, picture)))??;
    let (cpu_outputs, cpu) = time_runs(&cpu_model, &inputs, runs, cpu_load)?;
    let mut report = Report {
        file,
        kind,
        cpu,
        gpu_provider: gpu,
        gpu_device: None,
        gpu: None,
        agreement: Vec::new(),
        gpu_error: None,
    };
    drop(cpu_model);
    if gpu == Provider::Cpu {
        return Ok(report);
    }
    // Unchecked: the benchmark reports the GPU's output as it is.
    let loaded = timed(|| Model::load_checked(path, kind, &[gpu], dirs, false));
    let (gpu_model, gpu_load) = match loaded {
        Ok(loaded) => loaded,
        Err(e) => {
            report.gpu_error = Some(format!("{e:#}"));
            return Ok(report);
        }
    };
    report.gpu_device = crate::device(gpu, dirs).ok().map(|d| d.label.clone());
    match time_runs(&gpu_model, &inputs, runs, gpu_load) {
        Ok((gpu_outputs, timing)) => {
            report.gpu = Some(timing);
            report.agreement = compare(kind, &cpu_outputs, &gpu_outputs);
        }
        Err(e) => report.gpu_error = Some(format!("{e:#}")),
    }
    Ok(report)
}

fn timed<T>(f: impl FnOnce() -> Result<T>) -> Result<(T, f64)> {
    let started = Instant::now();
    let value = f()?;
    Ok((value, started.elapsed().as_secs_f64() * 1000.0))
}

/// An output as numbers (uint8 outputs widened).
#[derive(Clone)]
pub(crate) struct Output {
    name: String,
    values: Vec<f32>,
}

fn time_runs(
    model: &Model,
    inputs: &[(String, Input)],
    runs: usize,
    load_ms: f64,
) -> Result<(Vec<Output>, Timing)> {
    let mut timing = Timing {
        load_ms,
        ..Default::default()
    };
    let mut first = None;
    for i in 0..=runs {
        let started = Instant::now();
        let outputs = model.run_without_fallback(|session| run_once(session, inputs))?;
        let ms = started.elapsed().as_secs_f64() * 1000.0;
        if i == 0 {
            timing.first_run_ms = ms;
            first = Some(outputs);
        } else {
            timing.runs_ms.push(ms);
        }
    }
    timing.runs_ms.sort_by(f64::total_cmp);
    Ok((first.unwrap(), timing))
}

#[derive(Clone)]
pub(crate) enum Input {
    F32(Vec<i64>, Vec<f32>),
    U8(Vec<i64>, Vec<u8>),
    I64(Vec<i64>, Vec<i64>),
}

pub(crate) fn run_once(session: &mut Session, inputs: &[(String, Input)]) -> Result<Vec<Output>> {
    let mut values: Vec<(String, DynValue)> = Vec::new();
    for (name, input) in inputs {
        let value = match input.clone() {
            Input::F32(shape, data) => Tensor::from_array((shape, data))?.into_dyn(),
            Input::U8(shape, data) => Tensor::from_array((shape, data))?.into_dyn(),
            Input::I64(shape, data) => Tensor::from_array((shape, data))?.into_dyn(),
        };
        values.push((name.clone(), value));
    }
    let outputs = session.run(values)?;
    let mut result = Vec::new();
    for (name, value) in outputs.iter() {
        let values = if let Ok((_, data)) = value.try_extract_tensor::<f32>() {
            data.to_vec()
        } else if let Ok((_, data)) = value.try_extract_tensor::<u8>() {
            data.iter().map(|&v| v as f32).collect()
        } else {
            continue;
        };
        result.push(Output {
            name: name.to_string(),
            values,
        });
    }
    Ok(result)
}

/// Inputs for `kind` with the shapes the model declares, from `picture`
/// as the app prepares a photo (deterministic).
pub(crate) fn inputs_for(
    session: &Session,
    kind: ModelKind,
    picture: &Picture,
) -> Result<Vec<(String, Input)>> {
    {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut noise = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 40) as f32 / (1u64 << 24) as f32
        };
        let mut inputs = Vec::new();
        for input in session.inputs() {
            let ValueType::Tensor { ty, shape, .. } = input.dtype() else {
                return Err(anyhow!("{}: not a tensor input", input.name()));
            };
            let name = input.name();
            let mut dims: Vec<i64> = shape.iter().copied().collect();
            for (i, d) in dims.iter_mut().enumerate() {
                if *d < 0 {
                    *d = match (kind, name, i) {
                        (ModelKind::Inpaint, _, 2 | 3) => 512,
                        (ModelKind::Relight, _, 3) => 350,
                        (ModelKind::Relight, _, 4) => 518,
                        (ModelKind::SamDecoder, _, 1) => 2,
                        (ModelKind::TagText, _, 1) => 64,
                        _ => 1,
                    };
                }
            }
            let count: usize = dims.iter().map(|&d| d as usize).product();
            let input = match (kind, name, ty) {
                (ModelKind::SamDecoder, "point_coords", _) => {
                    Input::F32(dims, vec![512.0, 512.0, 0.0, 0.0])
                }
                (ModelKind::SamDecoder, "point_labels", _) => Input::F32(dims, vec![1.0, -1.0]),
                (ModelKind::SamDecoder, "has_mask_input", _) => Input::F32(dims, vec![0.0]),
                (ModelKind::SamDecoder, "orig_im_size", _) => {
                    Input::F32(dims, vec![1024.0, 1536.0])
                }
                (ModelKind::SamDecoder, "mask_input", _) => Input::F32(dims, vec![0.0; count]),
                (ModelKind::SamDecoder, _, _) => {
                    Input::F32(dims, (0..count).map(|_| noise() - 0.5).collect())
                }
                (ModelKind::Inpaint, "mask", _) => {
                    let (h, w) = (dims[dims.len() - 2] as usize, dims[dims.len() - 1] as usize);
                    let mask = (0..h * w)
                        .map(|i| {
                            let (y, x) = (i / w, i % w);
                            if (h * 2 / 5..h * 3 / 5).contains(&y)
                                && (w * 2 / 5..w * 3 / 5).contains(&x)
                            {
                                0
                            } else {
                                255
                            }
                        })
                        .collect();
                    Input::U8(dims, mask)
                }
                (ModelKind::Denoise, _, _) => {
                    let side = dims[2] as usize;
                    let plane = side * side;
                    let mut data = vec![0.0f32; count];
                    let (shot, read) = (2e-3f32, 1e-5f32);
                    for c in 0..4 {
                        for i in 0..plane {
                            let rgb = picture.at(
                                (i % side) as f32 / side as f32,
                                (i / side) as f32 / side as f32,
                            );
                            let linear = rgb[[0, 1, 2, 1][c]].powf(2.2) * 0.5;
                            let sd = (shot * linear + read).sqrt();
                            let noisy = (linear + (noise() - 0.5) * 3.4 * sd).clamp(0.0, 1.0);
                            data[c * plane + i] = noisy;
                            data[(c + 4) * plane + i] = (shot * noisy + read).sqrt();
                        }
                    }
                    Input::F32(dims, data)
                }
                (ModelKind::MoebiusUnet, "t", _) => Input::F32(dims, vec![500.0]),
                (ModelKind::MoebiusUnet | ModelKind::MoebiusDecoder, _, _) => {
                    Input::F32(dims, (0..count).map(|_| (noise() - 0.5) * 2.0).collect())
                }
                (ModelKind::MoebiusEncoder, _, _) => {
                    // The photo in -1..1, once per batch item, with a
                    // photo's grain (±2 levels): on the flat synthetic
                    // picture the encoder's latent depends on rounding (ONNX
                    // Runtime's CPU and PyTorch differ by 0.17 of a range of
                    // 5.5; with the grain by 0.013, as the GPU does).
                    let planes = image_planes(&dims, picture, false);
                    Input::F32(
                        dims,
                        planes
                            .iter()
                            .cycle()
                            .take(count)
                            .map(|v| v * 2.0 - 1.0 + (noise() - 0.5) * 4.0 / 127.5)
                            .collect(),
                    )
                }
                (ModelKind::TagText, _, _) => Input::I64(
                    dims,
                    (0..count).map(|_| 1 + (noise() * 1000.0) as i64).collect(),
                ),
                (_, _, TensorElementType::Uint8) => Input::U8(
                    dims.clone(),
                    image_planes(&dims, picture, false)
                        .iter()
                        .map(|v| (v * 255.0).round() as u8)
                        .collect(),
                ),
                (_, _, TensorElementType::Float32) => {
                    let mut planes = image_planes(
                        &dims,
                        picture,
                        matches!(kind, ModelKind::FaceMesh | ModelKind::FaceParser),
                    );
                    match kind {
                        ModelKind::Subject
                        | ModelKind::Sky
                        | ModelKind::Depth
                        | ModelKind::Relight => {
                            let (mean, std) = ([0.485, 0.456, 0.406], [0.229, 0.224, 0.225]);
                            let plane = planes.len() / 3;
                            for (i, v) in planes.iter_mut().enumerate() {
                                let c = i / plane;
                                *v = (*v - mean[c]) / std[c];
                            }
                        }
                        ModelKind::TagVision => {
                            planes.iter_mut().for_each(|v| *v = (*v - 0.5) / 0.5)
                        }
                        ModelKind::FaceDetector => planes.iter_mut().for_each(|v| *v *= 255.0),
                        _ => {}
                    }
                    Input::F32(dims, planes)
                }
                (_, _, other) => return Err(anyhow!("{name}: element type {other:?} not handled")),
            };
            inputs.push((name.to_string(), input));
        }
        Ok(inputs)
    }
}

/// The picture resized to an NCHW (or, with `nhwc`, NHWC) tensor of 0..1
/// values with 3 channels.
fn image_planes(dims: &[i64], picture: &Picture, nhwc: bool) -> Vec<f32> {
    let (h, w) = if nhwc {
        (dims[1] as usize, dims[2] as usize)
    } else {
        (dims[dims.len() - 2] as usize, dims[dims.len() - 1] as usize)
    };
    let mut out = vec![0.0f32; 3 * h * w];
    for y in 0..h {
        for x in 0..w {
            let rgb = picture.at(x as f32 / w as f32, y as f32 / h as f32);
            for (c, value) in rgb.into_iter().enumerate() {
                let i = if nhwc {
                    (y * w + x) * 3 + c
                } else {
                    c * h * w + y * w + x
                };
                out[i] = value;
            }
        }
    }
    out
}

pub(crate) fn compare(kind: ModelKind, cpu: &[Output], gpu: &[Output]) -> Vec<Agreement> {
    cpu.iter()
        .filter_map(|c| {
            let g = gpu.iter().find(|g| g.name == c.name)?;
            if g.values.len() != c.values.len() {
                return Some(Agreement {
                    output: format!(
                        "{} (length {} vs {})",
                        c.name,
                        c.values.len(),
                        g.values.len()
                    ),
                    max_abs: f64::INFINITY,
                    mean_abs: f64::INFINITY,
                    max_rel_range: f64::INFINITY,
                    iou: None,
                    finite: false,
                });
            }
            let (lo, hi) = c
                .values
                .iter()
                .fold((f32::MAX, f32::MIN), |(lo, hi), &v| (lo.min(v), hi.max(v)));
            // A single value (a score) has no range: its magnitude instead.
            let range = ((hi - lo) as f64)
                .max(lo.abs().max(hi.abs()) as f64)
                .max(1e-12);
            let mut max_abs = 0f64;
            let mut sum = 0f64;
            for (a, b) in c.values.iter().zip(&g.values) {
                let d = (*a as f64 - *b as f64).abs();
                max_abs = max_abs.max(d);
                sum += d;
            }
            let threshold = match (kind, c.name.as_str()) {
                (ModelKind::Subject, _) | (ModelKind::Sky, _) => Some(0.5),
                (ModelKind::SamDecoder, "masks") => Some(0.0),
                _ => None,
            };
            let iou = threshold.map(|t| {
                let (mut inter, mut union) = (0usize, 0usize);
                for (a, b) in c.values.iter().zip(&g.values) {
                    let (a, b) = (*a > t, *b > t);
                    inter += (a && b) as usize;
                    union += (a || b) as usize;
                }
                if union == 0 {
                    1.0
                } else {
                    inter as f64 / union as f64
                }
            });
            Some(Agreement {
                output: c.name.clone(),
                max_abs,
                mean_abs: sum / c.values.len().max(1) as f64,
                max_rel_range: max_abs / range,
                iou,
                finite: g.values.iter().all(|v| v.is_finite()),
            })
        })
        .collect()
}

/// Why the GPU's outputs are not good enough, if they are not: masks must
/// agree (IoU 0.99), other outputs within 1 % of the CPU output's range
/// (MI-GAN's 8-bit image: a mean difference of at most 2 levels).
pub fn disagreement(kind: ModelKind, agreement: &[Agreement]) -> Option<String> {
    if agreement.is_empty() {
        return Some("no output to compare".into());
    }
    agreement.iter().find_map(|a| {
        let bad = !a.finite
            || a.iou.is_some_and(|v| v < 0.99)
            || (kind != ModelKind::Inpaint && a.max_rel_range > 1e-2)
            || (kind == ModelKind::Inpaint && a.mean_abs > 2.0);
        bad.then(|| {
            format!(
                "{}: largest difference {:.3e} ({:.2e} of its range){}",
                a.output,
                a.max_abs,
                a.max_rel_range,
                a.iou
                    .map(|v| format!(", mask IoU {v:.4}"))
                    .unwrap_or_default()
            )
        })
    })
}

/// The model files of the pack in `dir`, sorted (the SigLIP 2 text encoder
/// only with `with_text`: it is 1.1 GB).
pub fn model_files(dir: &Path, with_text: bool) -> Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .and_then(ModelKind::from_file_name)
                .is_some_and(|k| with_text || k != ModelKind::TagText)
        })
        .collect();
    files.sort();
    Ok(files)
}
