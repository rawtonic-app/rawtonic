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
pub const ONNX_SHA256: &str = "907326f4591a3c81a4f6d4cc90beb6524fda5636ed59e7cf6ae6f7b15676b8dc";

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
        "68de852d4762eb4fe0ae0bb9c6e149fe139d031e6e8927dbb679597b106744f0",
    ),
    (
        "superres_coreml_weight.bin",
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
    Onnx(rawtonic_onnx::Model),
    #[cfg(target_os = "macos")]
    CoreMl(rawtonic_denoise::coreml::CoreMlModel),
}

impl Model {
    /// The ONNX model on ONNX Runtime: a GPU provider when one works, else
    /// the CPU.
    pub fn onnx(path: &Path) -> Result<Self> {
        Ok(Self {
            backend: Backend::Onnx(rawtonic_onnx::Model::load(
                path,
                rawtonic_onnx::ModelKind::SuperResolution,
            )?),
        })
    }

    /// The Core ML program of the model pack (`CORE_ML_PACKAGE`).
    #[cfg(target_os = "macos")]
    pub fn core_ml(model: rawtonic_denoise::coreml::CoreMlModel) -> Self {
        Self {
            backend: Backend::CoreMl(model),
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
            return Ok(Self::core_ml(
                rawtonic_denoise::coreml::CoreMlModel::load(&package, CORE_ML_UNITS)?,
            ));
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

    pub fn backend_name(&self) -> String {
        match &self.backend {
            Backend::Onnx(model) => format!("ONNX Runtime ({})", model.provider()),
            #[cfg(target_os = "macos")]
            Backend::CoreMl(_) => "Core ML".to_string(),
        }
    }

    /// Enlarges one tile: `input` is [1, 3, TILE, TILE] (RGB, 0..1,
    /// first-major), the result [1, 3, 2·TILE, 2·TILE] in 0..1.
    pub fn enlarge_tile(&self, input: &[f32]) -> Result<Vec<f32>> {
        ensure!(input.len() == 3 * TILE * TILE, "tile of {} values", input.len());
        let values = match &self.backend {
            Backend::Onnx(model) => model.run(|session| {
                let array = Array4::from_shape_vec((1, 3, TILE, TILE), input.to_vec())?;
                let outputs = session.run(ort::inputs![INPUT => Tensor::from_array(array)?])?;
                let (_, data) = outputs[OUTPUT].try_extract_tensor::<f32>()?;
                rawtonic_onnx::ensure_finite(data)?;
                Ok(data.to_vec())
            })?,
            #[cfg(target_os = "macos")]
            Backend::CoreMl(model) => {
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

    /// `image` enlarged 2× (see [`enlarge_with`]).
    pub fn enlarge(
        &self,
        image: DynamicImage,
        cancelled: &dyn Fn() -> Result<(), String>,
    ) -> Result<DynamicImage, String> {
        enlarge_with(image, &mut |tile| self.enlarge_tile(tile), cancelled)
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
    let error = |e: anyhow::Error| format!("Super Resolution failed: {e:#}");
    Ok(match image {
        DynamicImage::ImageRgb8(buf) => DynamicImage::ImageRgb8(enlarge_buffer(&buf, run, cancelled).map_err(error)?),
        DynamicImage::ImageRgba8(buf) => DynamicImage::ImageRgba8(enlarge_buffer(&buf, run, cancelled).map_err(error)?),
        DynamicImage::ImageRgb16(buf) => DynamicImage::ImageRgb16(enlarge_buffer(&buf, run, cancelled).map_err(error)?),
        DynamicImage::ImageRgba16(buf) => DynamicImage::ImageRgba16(enlarge_buffer(&buf, run, cancelled).map_err(error)?),
        DynamicImage::ImageRgb32F(buf) => DynamicImage::ImageRgb32F(enlarge_buffer(&buf, run, cancelled).map_err(error)?),
        DynamicImage::ImageRgba32F(buf) => DynamicImage::ImageRgba32F(enlarge_buffer(&buf, run, cancelled).map_err(error)?),
        DynamicImage::ImageLuma8(buf) => DynamicImage::ImageLuma8(enlarge_buffer(&buf, run, cancelled).map_err(error)?),
        DynamicImage::ImageLuma16(buf) => DynamicImage::ImageLuma16(enlarge_buffer(&buf, run, cancelled).map_err(error)?),
        other => {
            let rgb = other.into_rgba32f();
            DynamicImage::ImageRgba32F(enlarge_buffer(&rgb, run, cancelled).map_err(error)?)
        }
    })
}

/// How a pixel type's channels map to the model's RGB.
trait ModelPixel: Pixel<Subpixel: Channel> {
    fn rgb(&self) -> [f32; 3];
    /// The pixel with enlarged `rgb` and, for alpha, `source`'s alpha.
    fn with_rgb(rgb: [f32; 3], source: &Self) -> Self;
}

impl<T: Channel> ModelPixel for Rgb<T> {
    fn rgb(&self) -> [f32; 3] {
        self.0.map(Channel::to_unit)
    }
    fn with_rgb(rgb: [f32; 3], _: &Self) -> Self {
        Rgb(rgb.map(T::from_unit))
    }
}

impl<T: Channel> ModelPixel for Rgba<T> {
    fn rgb(&self) -> [f32; 3] {
        [self.0[0].to_unit(), self.0[1].to_unit(), self.0[2].to_unit()]
    }
    fn with_rgb(rgb: [f32; 3], source: &Self) -> Self {
        Rgba([T::from_unit(rgb[0]), T::from_unit(rgb[1]), T::from_unit(rgb[2]), source.0[3]])
    }
}

impl<T: Channel> ModelPixel for Luma<T> {
    fn rgb(&self) -> [f32; 3] {
        [self.0[0].to_unit(); 3]
    }
    fn with_rgb(rgb: [f32; 3], _: &Self) -> Self {
        Luma([T::from_unit((rgb[0] + rgb[1] + rgb[2]) / 3.0)])
    }
}

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
            ensure!(enlarged.len() == 3 * out_tile * out_tile, "tile result of {} values", enlarged.len());
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
                    let rgb = [enlarged[at], enlarged[out_plane + at], enlarged[2 * out_plane + at]];
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
            let out = enlarge_with(DynamicImage::ImageRgb32F(source.clone()), &mut nearest, &|| Ok(()))
                .unwrap()
                .into_rgb32f();
            assert_eq!(out.dimensions(), (2 * w, 2 * h));
            for (x, y, pixel) in out.enumerate_pixels() {
                assert_eq!(pixel, source.get_pixel(x / 2, y / 2), "({x}, {y}) of {w}x{h}");
            }
        }
    }

    #[test]
    fn keeps_the_pixel_type_and_alpha() {
        let rgba = ImageBuffer::from_fn(40, 30, |x, y| Rgba([(x * 6) as u16 * 257, (y * 8) as u16 * 257, 9000, (x + y) as u16 * 1000]));
        let out = enlarge_with(DynamicImage::ImageRgba16(rgba.clone()), &mut nearest, &|| Ok(())).unwrap();
        let DynamicImage::ImageRgba16(out) = out else { panic!("pixel type changed") };
        assert_eq!(out.dimensions(), (80, 60));
        assert_eq!(out.get_pixel(13, 7), rgba.get_pixel(6, 3));
        let rgb8 = DynamicImage::ImageRgb8(ImageBuffer::from_pixel(9, 4, Rgb([10u8, 200, 255])));
        assert!(matches!(enlarge_with(rgb8, &mut nearest, &|| Ok(())).unwrap(), DynamicImage::ImageRgb8(_)));
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
            if calls.get() > 2 { Err("Export cancelled".to_string()) } else { Ok(()) }
        };
        let err = enlarge_with(source, &mut counting, &cancel_after_two).unwrap_err();
        assert!(err.contains("Export cancelled"), "{err}");
        assert_eq!(runs, 2);
    }
}
