use crate::formats::is_raw_file;
use crate::image_processing::{
    apply_orientation, apply_srgb_to_linear, remove_raw_artifacts_and_enhance,
};
use crate::mask_generation::{MaskDefinition, PhotoGeometry, SubMask, generate_mask_bitmap};
use anyhow::{Context, Result, anyhow, ensure};
use base64::{Engine as _, engine::general_purpose};
use exif::{Reader as ExifReader, Tag};
use image::{DynamicImage, GenericImageView, ImageReader, imageops};
use rawler::Orientation;
pub use rawtonic_rawdecode::hot_pixels::DeadTest;
use rayon::prelude::*;
use serde::Deserialize;
use serde_json::Value;
use std::borrow::Cow;
use std::collections::HashMap;
use std::io::Cursor;
use std::panic;
use std::path::Path;
use std::sync::OnceLock;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Instant;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PatchMaskInfo {
    id: String,
    name: String,
    #[serde(default)]
    invert: bool,
    #[serde(default)]
    sub_masks: Vec<SubMask>,
}

pub fn srgb_to_linear_lut() -> &'static [f32; 256] {
    static LUT: OnceLock<[f32; 256]> = OnceLock::new();
    LUT.get_or_init(|| {
        let mut lut = [0.0f32; 256];
        for (i, v) in lut.iter_mut().enumerate() {
            let x = i as f32 / 255.0;
            *v = if x <= 0.04045 {
                x / 12.92
            } else {
                ((x + 0.055) / 1.055).powf(2.4)
            };
        }
        lut
    })
}

/// The recipe's AI Denoise amount (`aiDenoise`, 0 … 100) as 0 … 1; zero
/// when the Detail section or its Noise reduction tool is switched off.
pub fn ai_denoise_amount(adjustments: &Value) -> f32 {
    let visibility = &adjustments["sectionVisibility"];
    let off = |key: &str| visibility[key].as_bool() == Some(false);
    if off("details") || off("noiseReduction") {
        return 0.0;
    }
    (adjustments["aiDenoise"].as_f64().unwrap_or(0.0) as f32 / 100.0).clamp(0.0, 1.0)
}

/// The AI Denoise version new edits get (`aiDenoiseVersion`; recipes
/// without the key are version 1): 3, the mixed-precision model, since the
/// model pack carries it (pack-2026-10-18).
pub const AI_DENOISE_VERSION: u8 = 3;
/// The newest AI Denoise version this build renders.
pub const AI_DENOISE_LATEST: u8 = 3;

/// How a recipe's AI Denoise develops the mosaic.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AiDenoiseSettings {
    /// `aiDenoiseVersion`: 1 blends the network's result by the slider; 2
    /// maps the slider to the noise the network is told about; both run the
    /// first (full-precision) model. 3 is 2's mapping on the faster
    /// mixed-precision model. Each version has one model, so a saved recipe
    /// renders the same whatever models are installed.
    pub version: u8,
    /// Share of the network's result kept (`MosaicDenoise::amount`).
    pub blend: f32,
    /// Noise the network is told about, relative to the photo's.
    pub noise_scale: f32,
}

/// Noise scale of version 2 at slider 50, chosen so the effect there is
/// Camera Raw Denoise's at its default 50: on the six RawNIND scenes it was
/// fitted on flat-area noise 1.065 vs 1.06 and finest detail kept 0.894 vs
/// 0.901; on seven held-out scenes (ISO 6400–25600) a little stronger,
/// 1.16 vs 1.30 and 0.748 vs 0.778 (notes:
/// benchmark-results/ai-denoise-mac-2026-10-18).
const V2_SCALE_AT_50: f32 = 0.8;
/// Noise scale of version 2 at slider 100 (×1.25 the photo's: smoother,
/// for the noisiest files); 75 is the network as trained (scale 1).
const V2_SCALE_AT_100: f32 = 1.25;

/// The recipe's AI Denoise, or `None` when it is off (`ai_denoise_amount`).
/// Version 2: up to 50 the slider blends in the network's result at
/// `V2_SCALE_AT_50` (at 50 fully), above 50 the full result at a noise
/// scale growing geometrically to `V2_SCALE_AT_100`. A noise scale below 1
/// keeps more fine detail than blending at the same flat-area noise.
pub fn ai_denoise_settings(adjustments: &Value) -> Option<AiDenoiseSettings> {
    let amount = ai_denoise_amount(adjustments);
    if amount <= 0.0 {
        return None;
    }
    let version = adjustments["aiDenoiseVersion"]
        .as_u64()
        .map_or(1, |v| v.clamp(1, AI_DENOISE_LATEST as u64) as u8);
    Some(if version == 1 {
        AiDenoiseSettings {
            version,
            blend: amount,
            noise_scale: 1.0,
        }
    } else if amount <= 0.5 {
        AiDenoiseSettings {
            version,
            blend: amount / 0.5,
            noise_scale: V2_SCALE_AT_50,
        }
    } else {
        let t = (amount - 0.5) / 0.5;
        AiDenoiseSettings {
            version,
            blend: 1.0,
            noise_scale: V2_SCALE_AT_50 * (V2_SCALE_AT_100 / V2_SCALE_AT_50).powf(t),
        }
    })
}

impl AiDenoiseSettings {
    /// The model this version runs: 0 the first package (`nlmf_coreml_*`,
    /// the ONNX model elsewhere), 1 the mixed-precision package (version 3).
    /// Version 2 stays on the first package: between its introduction and
    /// this split the model pack never carried the mixed package, so the
    /// app rendered every version 2 edit with the first one (the macOS CLI
    /// refused such recipes instead).
    pub fn model(&self) -> usize {
        usize::from(self.version >= 3)
    }
}

/// How a source is developed into the base image: the RAW develop and
/// preprocessing settings (the app reads them from its preferences).
#[derive(Clone, Debug)]
pub struct BaseImageOptions {
    pub highlight_compression: f32,
    pub linear_mode: String,
    pub color_nr_amount: f32,
    pub sharpening_amount: f32,
    pub apply_to_non_raws: bool,
    /// AI denoising of the Bayer mosaic (full developments only); set per
    /// photo by the caller.
    pub mosaic_denoise: Option<rawtonic_rawdecode::mosaic_denoise::MosaicDenoise>,
    /// Receives a draft of a full RAW development before demosaicing
    /// (raw_processing::develop_raw_image_with_draft); set per photo by the
    /// editor for a photograph without a stored preview.
    pub draft: Option<Arc<rawtonic_rawdecode::raw_processing::DraftSink>>,
    /// The test for dead photosites (`DecodeVariant::dead_test`).
    pub dead_test: DeadTest,
    /// The frame version developed (`DecodeVariant::frame`).
    pub frame: u32,
}

/// What a recipe asks of the RAW decode beyond the app's preferences. Bases
/// decoded for different variants never stand in for each other.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct DecodeVariant {
    /// The multi-scale colour noise filter (`color_noise::multiscale`): no
    /// decode-time chroma clean-up.
    pub multiscale: bool,
    /// How dead photosites are found (hot_pixels.rs): `DeadTest::Adjacent`
    /// from `renderVersion` 3, so thin dark lines keep their colour.
    pub dead_test: DeadTest,
    /// The frame developed (`source_frame`): the recipe's
    /// `sourceFrameVersion`, as the oldest version that frames this file
    /// alike (`source_frame::effective`), so 0 on every file no frame
    /// version changes.
    pub frame: u32,
}

/// The render version from which dead photosites must also be darker than
/// their adjacent samples of the other colours (`DeadTest::Adjacent`).
pub const DEAD_TEST_RENDER_VERSION: u64 = 3;

/// The decode variant of the recipe rendered on the source file at
/// `source` (a never-edited one renders as a new edit,
/// `image_processing::apply_render_default`).
pub fn decode_variant(adjustments: &Value, source: &str) -> DecodeVariant {
    let is_raw = crate::formats::is_raw_file(source);
    let render_version = if crate::image_processing::is_new_recipe(adjustments) {
        crate::image_processing::RENDER_VERSION
    } else {
        crate::image_processing::render_version(adjustments)
    };
    DecodeVariant {
        multiscale: crate::color_noise::multiscale(adjustments, is_raw),
        dead_test: if render_version >= DEAD_TEST_RENDER_VERSION {
            DeadTest::Adjacent
        } else {
            DeadTest::SameColour
        },
        frame: if is_raw {
            crate::source_frame::effective(source, crate::source_frame::requested(adjustments))
        } else {
            0
        },
    }
}

impl BaseImageOptions {
    /// From the preference values (unset ones take their defaults); the
    /// colour noise slider maps to the filter amount here.
    pub fn from_preferences(
        raw_highlight_compression: Option<f32>,
        linear_raw_mode: String,
        raw_preprocessing_color_nr: Option<f32>,
        raw_preprocessing_sharpening: Option<f32>,
        apply_preprocessing_to_non_raws: Option<bool>,
    ) -> Self {
        let highlight_compression = raw_highlight_compression.unwrap_or(2.5);
        let linear_mode = linear_raw_mode;
        let color_nr_setting = raw_preprocessing_color_nr.unwrap_or(0.5);
        let color_nr_amount = if color_nr_setting <= 0.0 {
            0.0
        } else {
            let x = color_nr_setting.clamp(0.01, 1.0);
            (12.0 / x - 10.0).max(0.1)
        };
        let sharpening_amount = raw_preprocessing_sharpening.unwrap_or(0.35);
        let apply_to_non_raws = apply_preprocessing_to_non_raws.unwrap_or(false);
        Self {
            highlight_compression,
            linear_mode,
            color_nr_amount,
            sharpening_amount,
            apply_to_non_raws,
            mosaic_denoise: None,
            draft: None,
            dead_test: DeadTest::SameColour,
            frame: 0,
        }
    }

    /// The options for a photo whose recipe develops colour noise with
    /// `multiscale` (`color_noise::multiscale`): the multi-scale filter
    /// measures and removes colour noise itself, so the decode-time chroma
    /// clean-up (which blurs colour edges over about 4.5 px before it) is
    /// skipped; sharpening stays. The old filter keeps the clean-up.
    pub fn for_color_noise(mut self, multiscale: bool) -> Self {
        if multiscale {
            self.color_nr_amount = 0.0;
        }
        self
    }

    /// The options for a photo whose recipe asks for `variant`: its colour
    /// noise filter (`for_color_noise`), its test for dead photosites and
    /// its frame.
    pub fn for_variant(mut self, variant: DecodeVariant) -> Self {
        self.dead_test = variant.dead_test;
        self.frame = variant.frame;
        self.for_color_noise(variant.multiscale)
    }

    /// `for_variant` with the decisions taken from the recipe rendered on
    /// the source file at `source`.
    pub fn for_recipe(self, adjustments: &Value, source: &str) -> Self {
        self.for_variant(decode_variant(adjustments, source))
    }
}

impl Default for BaseImageOptions {
    /// The app's default preferences.
    fn default() -> Self {
        Self::from_preferences(
            Some(2.5),
            "auto".to_string(),
            Some(0.5),
            Some(0.35),
            Some(false),
        )
    }
}

/// What renders read from a full RAW decode besides its pixels
/// (`baseline_exposure::apply_base_look`): its noise level and image key,
/// measured on it before the decode-time clean-up. They belong to those
/// pixels: decodes of one file for different recipes (AI Denoise, colour
/// noise filter) measure differently, so they travel with the decode
/// (decoded-image cache, the open source, export sources, preview proxies)
/// instead of being looked up by path.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DecodeMeasures {
    pub noise_level: Option<f32>,
    pub image_key: Option<crate::image_key::ImageKey>,
}

impl DecodeMeasures {
    /// Measures a full decode.
    pub fn of(image: &DynamicImage) -> Self {
        Self {
            noise_level: crate::noise_level::measure(image),
            image_key: crate::image_key::measure(image),
        }
    }

    /// Records them as the last of `path` (read where no decode is at hand).
    fn record(&self, path: &str) {
        if let Some(level) = self.noise_level {
            crate::noise_level::record(path, level);
            log::info!("Noise level for '{path}': {level:.5}");
        }
        if let Some(key) = self.image_key {
            crate::image_key::record(path, key);
            log::info!("Image key for '{path}': {key:?}");
        }
    }
}

/// The base image of a source file: a RAW developed (embedded preview only
/// as a recorded fallback), any other format decoded and oriented.
pub fn load_base_image(
    bytes: &[u8],
    path_for_ext_check: &str,
    use_fast_raw_dev: bool,
    options: &BaseImageOptions,
    cancel_token: Option<(Arc<AtomicUsize>, usize)>,
) -> Result<DynamicImage> {
    load_base_image_measured(
        bytes,
        path_for_ext_check,
        use_fast_raw_dev,
        options,
        cancel_token,
    )
    .map(|(image, _)| image)
}

/// `load_base_image` with the measures of a full RAW decode (None for
/// fast developments, embedded-preview fallbacks and other formats).
pub fn load_base_image_measured(
    bytes: &[u8],
    path_for_ext_check: &str,
    use_fast_raw_dev: bool,
    options: &BaseImageOptions,
    cancel_token: Option<(Arc<AtomicUsize>, usize)>,
) -> Result<(DynamicImage, Option<DecodeMeasures>)> {
    let BaseImageOptions {
        highlight_compression,
        linear_mode,
        color_nr_amount,
        sharpening_amount,
        apply_to_non_raws,
        mosaic_denoise,
        draft,
        dead_test,
        frame,
    } = options.clone();
    if is_raw_file(path_for_ext_check) {
        let cancelled = cancel_token
            .clone()
            .map(|(tracker, generation)| move || tracker.load(Ordering::SeqCst) != generation);
        // The denoiser is only read; a panic leaves nothing half-updated.
        match panic::catch_unwind(panic::AssertUnwindSafe(move || {
            crate::raw_processing::develop_raw_image_with_draft(
                bytes,
                use_fast_raw_dev,
                highlight_compression,
                linear_mode,
                cancel_token,
                mosaic_denoise.as_ref().filter(|_| !use_fast_raw_dev),
                draft.as_deref(),
                dead_test,
                frame,
            )
        })) {
            Ok(Ok(mut image)) => {
                let measures = (!use_fast_raw_dev).then(|| {
                    let measures = DecodeMeasures::of(&image);
                    measures.record(path_for_ext_check);
                    measures
                });
                if cancelled.as_ref().is_some_and(|cancelled| cancelled()) {
                    return Err(anyhow!("Load cancelled"));
                }
                if !use_fast_raw_dev && (color_nr_amount > 0.0 || sharpening_amount > 0.0) {
                    let start = Instant::now();
                    {
                        #[cfg(feature = "performance-profiling")]
                        let _stage = crate::performance::PerformanceStage::new("raw_preprocess");
                        remove_raw_artifacts_and_enhance(
                            &mut image,
                            color_nr_amount,
                            sharpening_amount,
                        );
                    }
                    let duration = start.elapsed();
                    log::info!(
                        "Raw enhancing for '{}' took {:?}",
                        path_for_ext_check,
                        duration
                    );
                }
                // Fast developments run in parallel thumbnail workers; a
                // forced release there would make them fault pages repeatedly.
                if !use_fast_raw_dev {
                    crate::allocator::release_freed_memory();
                    note_preview_fallback(path_for_ext_check, None);
                }
                Ok((image, measures))
            }
            Ok(Err(e)) => {
                let classified = classify_raw_develop_error(path_for_ext_check, e);

                if classified.to_string().contains("Load cancelled") {
                    return Err(classified);
                }

                log::warn!(
                    "Error developing RAW file '{}': {}",
                    path_for_ext_check,
                    classified
                );
                if let Some(preview) = safe_embedded_preview_fallback(bytes, path_for_ext_check) {
                    log::warn!(
                        "Using embedded preview fallback for '{}' ({}x{})",
                        path_for_ext_check,
                        preview.width(),
                        preview.height()
                    );
                    note_preview_fallback(path_for_ext_check, Some(classified.to_string()));

                    return Ok((linearize_embedded_preview(preview), None));
                }
                Err(classified)
            }
            Err(_) => {
                log::error!("Panic while processing RAW file: {}", path_for_ext_check);
                if let Some(preview) = safe_embedded_preview_fallback(bytes, path_for_ext_check) {
                    log::warn!(
                        "Using embedded preview fallback for '{}' after RAW decoder panic ({}x{})",
                        path_for_ext_check,
                        preview.width(),
                        preview.height()
                    );
                    note_preview_fallback(
                        path_for_ext_check,
                        Some("the RAW decoder panicked".to_string()),
                    );

                    return Ok((linearize_embedded_preview(preview), None));
                }
                Err(anyhow!(
                    "Failed to process RAW file: {}",
                    path_for_ext_check
                ))
            }
        }
    } else {
        let mut image = load_image_with_orientation(bytes, cancel_token)?;

        if apply_to_non_raws
            && !use_fast_raw_dev
            && (color_nr_amount > 0.0 || sharpening_amount > 0.0)
        {
            let start = Instant::now();
            remove_raw_artifacts_and_enhance(&mut image, color_nr_amount, sharpening_amount);
            let duration = start.elapsed();
            log::info!(
                "Enhancing non-RAW '{}' took {:?}",
                path_for_ext_check,
                duration
            );
        }

        Ok((image, None))
    }
}

pub fn preview_fallbacks() -> &'static std::sync::Mutex<HashMap<String, String>> {
    static FALLBACKS: OnceLock<std::sync::Mutex<HashMap<String, String>>> = OnceLock::new();
    FALLBACKS.get_or_init(Default::default)
}

/// Records whether the last full load of a RAW used its embedded preview
/// (with the decode error) or developed the sensor data.
pub fn note_preview_fallback(path: &str, reason: Option<String>) {
    let mut fallbacks = preview_fallbacks()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    match reason {
        Some(reason) => {
            fallbacks.insert(path.to_string(), reason);
        }
        None => {
            fallbacks.remove(path);
        }
    }
}

/// Exports must develop the RAW itself: an embedded camera JPEG shown while
/// browsing is not a development and must not be written as one.
pub fn ensure_raw_developed(path: &str) -> Result<()> {
    let fallbacks = preview_fallbacks()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    match fallbacks.get(path) {
        Some(reason) => Err(anyhow!(
            "Cannot export '{}': the RAW data could not be developed ({}). Only the embedded camera preview is available; export refused.",
            path,
            reason
        )),
        None => Ok(()),
    }
}

/// Part of the error for a RAW format no open decoder reads (Nikon High
/// Efficiency): a camera sample does not help there.
pub const NO_OPEN_DECODER: &str = "has no open decoder";

/// Why `path` shows its embedded camera preview instead of a development,
/// if it does.
pub fn preview_fallback_reason(path: &str) -> Option<String> {
    preview_fallbacks()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(path)
        .cloned()
}

pub fn classify_raw_develop_error(path: &str, err: anyhow::Error) -> anyhow::Error {
    let error_text = err.to_string();
    let lowered = error_text.to_ascii_lowercase();
    let unsupported_compression =
        lowered.contains("nef compression") && lowered.contains("not supported");

    // Nikon's High Efficiency / High Efficiency* NEF (Z 8, Z 9, Z f, Z6III,
    // Z5II): a patented codec without an open decoder, which no open-source
    // RAW library reads.
    if unsupported_compression && lowered.contains("highefficency") {
        return anyhow!(
            "Nikon High Efficiency RAW cannot be developed in Rawtonic: the format is patented and {NO_OPEN_DECODER}. Record Lossless compressed RAW in the camera, or convert these files to DNG with Nikon NX Studio or Adobe DNG Converter and open the DNGs ('{}')",
            path
        );
    }

    if unsupported_compression {
        return anyhow!(
            "Unsupported RAW compression format for '{}'. Original error: {}",
            path,
            error_text
        );
    }

    err
}

pub fn tiff_jpeg_candidates(buf: &[u8]) -> Option<Vec<&[u8]>> {
    let le = match buf.get(..4)? {
        [0x49, 0x49, 0x2A, 0x00] => true,
        [0x4D, 0x4D, 0x00, 0x2A] => false,
        _ => return None,
    };
    let rd16 = |o: usize| -> Option<u64> {
        let b: [u8; 2] = buf.get(o..o + 2)?.try_into().ok()?;
        Some(if le {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        } as u64)
    };
    let rd32 = |o: usize| -> Option<u64> {
        let b: [u8; 4] = buf.get(o..o + 4)?.try_into().ok()?;
        Some(if le {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        } as u64)
    };

    let mut candidates: Vec<(u64, u64)> = Vec::new();
    let mut queue: Vec<u64> = vec![rd32(4)?];
    let mut seen = HashMap::new();

    while let Some(ifd) = queue.pop() {
        if seen.insert(ifd, ()).is_some() || seen.len() > 64 {
            continue;
        }
        let Some(n) = rd16(ifd as usize) else {
            continue;
        };

        let mut compression: u64 = 0;
        let mut strip: Option<(u64, u64)> = None;
        let mut old_jpeg: Option<(u64, u64)> = None;

        for i in 0..n {
            let e = ifd as usize + 2 + (i as usize) * 12;
            let (Some(tag), Some(count), Some(val)) = (rd16(e), rd32(e + 4), rd32(e + 8)) else {
                continue;
            };
            match tag {
                259 => compression = val,
                273 if count == 1 => strip = Some((val, strip.map_or(0, |s| s.1))),
                279 if count == 1 => strip = strip.map(|s| (s.0, val)).or(Some((0, val))),
                513 => old_jpeg = Some((val, old_jpeg.map_or(0, |s| s.1))),
                514 => old_jpeg = old_jpeg.map(|s| (s.0, val)).or(Some((0, val))),
                330 => {
                    if count == 1 {
                        queue.push(val);
                    } else {
                        for j in 0..count.min(8) {
                            if let Some(p) = rd32(val as usize + (j as usize) * 4) {
                                queue.push(p);
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        if matches!(compression, 6 | 7)
            && let Some(s) = strip
        {
            candidates.push(s);
        }
        if let Some(oj) = old_jpeg {
            candidates.push(oj);
        }
        if let Some(next) = rd32(ifd as usize + 2 + (n as usize) * 12)
            && next != 0
        {
            queue.push(next);
        }
    }

    candidates.sort_by_key(|&(_, len)| std::cmp::Reverse(len));

    Some(
        candidates
            .into_iter()
            .filter_map(|(off, len)| buf.get(off as usize..off.checked_add(len)? as usize))
            .collect(),
    )
}

pub fn largest_tiff_jpeg_preview(buf: &[u8]) -> Option<DynamicImage> {
    tiff_jpeg_candidates(buf)?
        .into_iter()
        .find_map(|bytes| image::load_from_memory_with_format(bytes, image::ImageFormat::Jpeg).ok())
}

/// The EXIF orientation (1–8) a photo is decoded with: for RAWs as the raw
/// decode reads it (Phase One, the decoder's metadata, Canon CRW), else the
/// file's EXIF.
pub fn source_orientation(bytes: &[u8], path: &str) -> u16 {
    let orientation = if is_raw_file(path) {
        crate::capture_info::phase_one_orientation(bytes)
            .or_else(|| {
                let source = rawler::rawsource::RawSource::new_from_slice(bytes);
                let decoder = rawler::get_decoder(&source).ok()?;
                decoder
                    .raw_metadata(&source, &rawler::decoders::RawDecodeParams::default())
                    .ok()?
                    .exif
                    .orientation
            })
            .or_else(|| crate::capture_info::crw_orientation(bytes))
    } else if crate::avif::is_avif(bytes) {
        Some(crate::avif::orientation(bytes))
    } else {
        exif_orientation(bytes)
    };
    orientation.filter(|o| (1..=8).contains(o)).unwrap_or(1)
}

pub fn exif_orientation(bytes: &[u8]) -> Option<u16> {
    // The generic EXIF reader copies the entire TIFF container. Reading this
    // inline primary-IFD tag avoids copying a complete RAW just for orientation.
    let endian = match bytes.get(..4) {
        Some([0x49, 0x49, 0x2a, 0]) => Some(true),
        Some([0x4d, 0x4d, 0, 0x2a]) => Some(false),
        _ => None,
    };
    if let Some(little_endian) = endian {
        let read16 = |offset: usize| -> Option<u16> {
            let value = bytes.get(offset..offset.checked_add(2)?)?.try_into().ok()?;
            Some(if little_endian {
                u16::from_le_bytes(value)
            } else {
                u16::from_be_bytes(value)
            })
        };
        let value = bytes.get(4..8)?.try_into().ok()?;
        let offset = if little_endian {
            u32::from_le_bytes(value)
        } else {
            u32::from_be_bytes(value)
        } as usize;
        let count = read16(offset)? as usize;
        for index in 0..count {
            let entry = offset.checked_add(2)?.checked_add(index.checked_mul(12)?)?;
            if read16(entry)? == 0x112 && read16(entry + 2)? == 3 {
                return read16(entry + 8).filter(|value| (1..=8).contains(value));
            }
        }
        return None;
    }
    ExifReader::new()
        .read_from_container(&mut Cursor::new(bytes))
        .ok()
        .and_then(|exif| {
            exif.get_field(Tag::Orientation, exif::In::PRIMARY)?
                .value
                .get_uint(0)
        })
        .filter(|value| (1..=8).contains(value))
        .map(|value| value as u16)
}

pub fn raf_embedded_jpeg(bytes: &[u8]) -> Option<&[u8]> {
    // RAF is not a TIFF/EXIF container. Its camera orientation is in the
    // embedded JPEG, the same metadata source used by rawler's RAW decoder.
    if !bytes.starts_with(b"FUJIFILMCCD-RAW ") {
        return None;
    }
    let offset = u32::from_be_bytes(bytes.get(84..88)?.try_into().ok()?) as usize;
    let length = u32::from_be_bytes(bytes.get(88..92)?.try_into().ok()?) as usize;
    bytes.get(offset..offset.checked_add(length)?)
}

pub fn priority_embedded_jpeg(bytes: &[u8]) -> Option<Vec<u8>> {
    let (jpeg, orientation) = if let Some(jpeg) = raf_embedded_jpeg(bytes) {
        (jpeg, exif_orientation(jpeg).unwrap_or(1))
    } else {
        let jpeg = tiff_jpeg_candidates(bytes)?.into_iter().find(|jpeg| {
            ImageReader::with_format(Cursor::new(*jpeg), image::ImageFormat::Jpeg)
                .into_dimensions()
                .is_ok()
        })?;
        (jpeg, exif_orientation(bytes).unwrap_or(1))
    };
    let mut jpeg = jpeg.to_vec();
    if exif_orientation(&jpeg) != Some(orientation) {
        // Add/replace only EXIF metadata. The camera's compressed JPEG pixels
        // stay intact and WebKit can apply orientation while decoding them.
        use little_exif::{exif_tag::ExifTag, filetype::FileExtension, metadata::Metadata};
        // Some camera previews contain no EXIF segment at all. little_exif
        // reports that as an error, so start a new segment in this copy.
        let mut metadata = Metadata::new_from_vec(&jpeg, FileExtension::JPEG).unwrap_or_default();
        metadata.set_tag(ExifTag::Orientation(vec![orientation]));
        metadata.write_to_vec(&mut jpeg, FileExtension::JPEG).ok()?;
    }
    Some(jpeg)
}

pub fn embedded_preview_fallback(bytes: &[u8], path: &str) -> Option<DynamicImage> {
    let (img, orientation) = if let Some(jpeg) = raf_embedded_jpeg(bytes) {
        let image = image::load_from_memory_with_format(jpeg, image::ImageFormat::Jpeg).ok()?;
        (image, exif_orientation(jpeg))
    } else if let Some(image) = largest_tiff_jpeg_preview(bytes) {
        (image, exif_orientation(bytes))
    } else {
        // Other RAW containers (for example CR3) also need the decoder's
        // metadata instead of assuming that the container is TIFF or JPEG.
        let source = rawler::rawsource::RawSource::new(Path::new(path)).ok()?;
        let decoder = rawler::get_decoder(&source).ok()?;
        let params = rawler::decoders::RawDecodeParams::default();
        let image = decoder
            .preview_image(&source, &params)
            .ok()?
            .or_else(|| decoder.full_image(&source, &params).ok().flatten())?;
        let orientation = decoder
            .raw_metadata(&source, &params)
            .ok()
            .and_then(|metadata| metadata.exif.orientation)
            .or_else(|| exif_orientation(bytes));
        (image, orientation)
    };

    Some(match orientation {
        Some(o) if o > 1 => apply_orientation(img, Orientation::from_u16(o)),
        _ => img,
    })
}

pub fn safe_embedded_preview_fallback(bytes: &[u8], path: &str) -> Option<DynamicImage> {
    match panic::catch_unwind(panic::AssertUnwindSafe(|| {
        embedded_preview_fallback(bytes, path)
    })) {
        Ok(preview) => preview,
        Err(_) => {
            log::warn!("Embedded RAW preview extraction panicked for '{}'", path);
            None
        }
    }
}

pub fn linearize_embedded_preview(preview: DynamicImage) -> DynamicImage {
    let preview = DynamicImage::ImageRgb32F(preview.to_rgb32f());
    let mut linear_preview = apply_srgb_to_linear(preview).into_rgb32f();
    for pixel in linear_preview.pixels_mut() {
        pixel[0] *= 0.4;
        pixel[1] *= 0.4;
        pixel[2] *= 0.4;
    }
    DynamicImage::ImageRgb32F(linear_preview)
}

/// Linear-light pixels (OpenEXR, Radiance HDR) sRGB-encoded without
/// clipping, so the renderer's `srgb_to_linear` gives them back unchanged,
/// highlights above 1 included; alpha stays as it is.
fn encode_linear(image: DynamicImage) -> DynamicImage {
    let (width, height) = (image.width(), image.height());
    let has_alpha = image.color().has_alpha();
    let channels = if has_alpha { 4 } else { 3 };
    let mut values = if has_alpha {
        image.into_rgba32f().into_raw()
    } else {
        image.into_rgb32f().into_raw()
    };
    values
        .par_chunks_mut(channels * width.max(1) as usize)
        .for_each(|row| {
            for pixel in row.chunks_exact_mut(channels) {
                for v in &mut pixel[..3] {
                    *v = crate::input_profile::encode(*v);
                }
            }
        });
    if has_alpha {
        DynamicImage::ImageRgba32F(image::ImageBuffer::from_raw(width, height, values).unwrap())
    } else {
        DynamicImage::ImageRgb32F(image::ImageBuffer::from_raw(width, height, values).unwrap())
    }
}

pub fn load_image_with_orientation(
    bytes: &[u8],
    cancel_token: Option<(Arc<AtomicUsize>, usize)>,
) -> Result<DynamicImage> {
    let check_cancel = || -> Result<()> {
        if let Some((tracker, generation)) = &cancel_token
            && tracker.load(Ordering::SeqCst) != *generation
        {
            return Err(anyhow!("Load cancelled"));
        }
        Ok(())
    };

    if crate::avif::is_avif(bytes) {
        // HEIF orientation (irot/imir) applies; an EXIF tag in the file does not.
        let decoded = crate::avif::decode(bytes).context("Failed to decode AVIF image")?;
        check_cancel()?;
        let orientation = Orientation::from_u16(decoded.orientation);
        let image = apply_orientation(crate::avif::to_working_space(decoded), orientation);
        return Ok(DynamicImage::ImageRgb32F(image.into_rgb32f()));
    }

    // JPEG XL decodes through the image crate's hook, registered here so the
    // CLI and headless export read it as the app does.
    static JXL_HOOK: std::sync::Once = std::sync::Once::new();
    JXL_HOOK.call_once(|| {
        jxl_oxide::integration::register_image_decoding_hook();
    });

    let cursor = Cursor::new(bytes);
    let mut reader = ImageReader::new(cursor.clone())
        .with_guessed_format()
        .context("Failed to guess image format")?;

    reader.no_limits();
    // OpenEXR and Radiance HDR hold linear light; the renderer reads
    // sRGB-encoded values.
    let linear_source = matches!(
        reader.format(),
        Some(image::ImageFormat::OpenExr | image::ImageFormat::Hdr)
    );

    check_cancel()?;

    // The decoder hands over the embedded ICC profile (Camera Raw honours
    // it; the renderer reads sRGB-encoded values, input_profile.rs).
    let mut decoder = reader.into_decoder().context("Failed to decode image")?;
    let icc = image::ImageDecoder::icc_profile(&mut decoder)
        .ok()
        .flatten();
    let image = DynamicImage::from_decoder(decoder).context("Failed to decode image")?;
    check_cancel()?;
    let image = match icc {
        Some(icc) => crate::input_profile::to_working_space(image, &icc),
        None if linear_source => encode_linear(image),
        None => image,
    };
    check_cancel()?;

    let oriented_image = {
        let exif_reader = ExifReader::new();
        if let Ok(exif) = exif_reader.read_from_container(&mut cursor.clone()) {
            if let Some(orientation) = exif
                .get_field(Tag::Orientation, exif::In::PRIMARY)
                .and_then(|f| f.value.get_uint(0))
            {
                check_cancel()?;
                apply_orientation(image, Orientation::from_u16(orientation as u16))
            } else {
                image
            }
        } else {
            image
        }
    };

    Ok(DynamicImage::ImageRgb32F(oriented_image.to_rgb32f()))
}

/// Keep the decoded source available for parametric mask sampling while patches
/// affect only the render image. Ordinary edits borrow the source pixel buffer.
pub fn composite_patches_for_render<'a>(
    pristine_source: &'a DynamicImage,
    adjustments: &Value,
) -> Result<Cow<'a, DynamicImage>> {
    let has_visible_patch = adjustments
        .get("aiPatches")
        .and_then(Value::as_array)
        .is_some_and(|patches| {
            patches.iter().any(|patch| {
                patch
                    .get("visible")
                    .and_then(Value::as_bool)
                    .unwrap_or(true)
                    && patch
                        .get("patchData")
                        .and_then(|data| data.get("color"))
                        .and_then(Value::as_str)
                        .is_some_and(|color| !color.is_empty())
            })
        });
    if has_visible_patch
        || crate::red_eye::is_active(adjustments)
        || crate::spot_removal::has_spots(adjustments)
        || crate::face_retouch::is_active(adjustments)
    {
        composite_patches_on_image(pristine_source, adjustments).map(Cow::Owned)
    } else {
        Ok(Cow::Borrowed(pristine_source))
    }
}

/// Whether the base image is retouched before adjustments: AI/cleanup
/// patches, red eye correction, spot removal or face retouch. Such recipes
/// need the full-resolution source.
pub fn has_base_retouch(adjustments: &Value) -> bool {
    adjustments
        .get("aiPatches")
        .and_then(Value::as_array)
        .is_some_and(|patches| !patches.is_empty())
        || crate::red_eye::is_active(adjustments)
        || crate::spot_removal::has_spots(adjustments)
        || crate::face_retouch::is_active(adjustments)
}

/// Sub-mask types of the inherited patches that no model makes: clone and
/// heal copy pixels, liquify warps them, retouch smooths them.
const HAND_MADE_PATCH_TYPES: [&str; 4] = ["clone", "heal", "liquify", "retouch"];

/// Whether the recipe shows areas made by a generative model (visible AI
/// patches with pixels: object removal, generative fill). Exports of such
/// recipes are marked (`rawtonic_metadata::DIGITAL_SOURCE_COMPOSITE_AI`).
/// Patches whose every sub-mask is a hand-made type (clone, heal, liquify,
/// retouch) do not count: no model made their pixels. Neither do patches
/// marked `"generated": false` (Fill edges' patch-based completion).
pub fn has_generated_areas(adjustments: &Value) -> bool {
    adjustments
        .get("aiPatches")
        .and_then(Value::as_array)
        .is_some_and(|patches| {
            patches.iter().any(|patch| {
                let hand_made =
                    patch
                        .get("subMasks")
                        .and_then(Value::as_array)
                        .is_some_and(|subs| {
                            !subs.is_empty()
                                && subs.iter().all(|sub| {
                                    sub.get("type")
                                        .and_then(Value::as_str)
                                        .is_some_and(|t| HAND_MADE_PATCH_TYPES.contains(&t))
                                })
                        });
                let not_generated = patch.get("generated").and_then(Value::as_bool) == Some(false);
                !hand_made
                    && !not_generated
                    && patch
                        .get("visible")
                        .and_then(Value::as_bool)
                        .unwrap_or(true)
                    && patch
                        .get("patchData")
                        .and_then(|data| data.get("color"))
                        .and_then(Value::as_str)
                        .is_some_and(|color| !color.is_empty())
            })
        })
}

/// The base image with the recipe's patches, then its red eye correction
/// (`red_eye.rs`), spot removal (`spot_removal.rs`) and face retouch
/// (`face_retouch.rs`), applied.
pub fn composite_patches_on_image(
    base_image: &DynamicImage,
    current_adjustments: &Value,
) -> Result<DynamicImage> {
    let patched = composite_ai_patches(base_image, current_adjustments)?;
    let patched = crate::red_eye::apply_red_eyes(&patched, current_adjustments).unwrap_or(patched);
    let spotted =
        crate::spot_removal::apply_spots(&patched, current_adjustments).unwrap_or(patched);
    if !crate::face_retouch::is_active(current_adjustments) {
        return Ok(spotted);
    }
    let mut image = spotted.into_rgb32f();
    crate::face_retouch::apply_face_retouch(&mut image, current_adjustments);
    Ok(DynamicImage::ImageRgb32F(image))
}

/// `composite_patches_on_image` for a reduced copy of the base image at
/// `scale` (its pixels per full-resolution pixel), such as an offline
/// photograph's Standard preview or a fit-view preview: patches, spots and
/// faces, whose positions are in full-resolution pixels, are placed and
/// sized accordingly.
pub fn composite_patches_on_scaled_image(
    base_image: &DynamicImage,
    current_adjustments: &Value,
    scale: f32,
) -> Result<DynamicImage> {
    if scale == 1.0 {
        return composite_patches_on_image(base_image, current_adjustments);
    }
    let patched = composite_ai_patches_at(base_image, current_adjustments, scale)?;
    // Lightroom's spots and red eyes not yet placed render on full-resolution
    // bases only, where they are placed exactly as the editor will place them
    // (`lightroom_retouch`); a reduced copy shows them once they are placed.
    let current_adjustments = &crate::lightroom_retouch::without_pending(current_adjustments);
    // Red eyes are kept in units of the photo's long edge: they fit as they are.
    let patched = crate::red_eye::apply_red_eyes(&patched, current_adjustments).unwrap_or(patched);
    let spots = crate::spot_removal::scaled_spots_recipe(current_adjustments, f64::from(scale));
    let spotted = crate::spot_removal::apply_spots(&patched, &spots).unwrap_or(patched);
    if !crate::face_retouch::is_active(current_adjustments) {
        return Ok(spotted);
    }
    let mut image = spotted.into_rgb32f();
    let faces = crate::face_retouch::scaled_recipe(current_adjustments, f64::from(scale));
    crate::face_retouch::apply_face_retouch(&mut image, &faces);
    Ok(DynamicImage::ImageRgb32F(image))
}

/// Whether the base retouch needs the full-resolution source even for a
/// reduced preview: AI patches and spots do (their reduced placement leaves
/// out the geometry warps), and red eyes (a small pupil measured on a
/// reduced copy is corrected less than at full resolution); face retouch
/// scales exactly (`face_retouch::scaled_recipe`).
pub fn retouch_needs_full_resolution(adjustments: &Value) -> bool {
    adjustments
        .get("aiPatches")
        .and_then(Value::as_array)
        .is_some_and(|patches| !patches.is_empty())
        || crate::red_eye::is_active(adjustments)
        || crate::spot_removal::has_spots(adjustments)
}

pub fn composite_ai_patches(
    base_image: &DynamicImage,
    current_adjustments: &Value,
) -> Result<DynamicImage> {
    composite_ai_patches_at(base_image, current_adjustments, 1.0)
}

/// The recipe's AI patches over `base_image`, a copy of the base image at
/// `scale` of its full resolution (1: the full image).
fn composite_ai_patches_at(
    base_image: &DynamicImage,
    current_adjustments: &Value,
    scale: f32,
) -> Result<DynamicImage> {
    // Full-resolution pixels at this scale.
    let scaled = |v: u32| ((v as f32 * scale).round() as u32).max(1);
    let patches_val = match current_adjustments.get("aiPatches") {
        Some(val) => val,
        None => return Ok(base_image.clone()),
    };

    let patches_arr = match patches_val.as_array() {
        Some(arr) if !arr.is_empty() => arr,
        _ => return Ok(base_image.clone()),
    };

    let visible_patches: Vec<&Value> = patches_arr
        .par_iter()
        .filter(|patch_obj| {
            let is_visible = patch_obj
                .get("visible")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            if !is_visible {
                return false;
            }
            patch_obj
                .get("patchData")
                .and_then(|data| data.get("color"))
                .and_then(|color| color.as_str())
                .is_some_and(|s| !s.is_empty())
        })
        .collect();

    if visible_patches.is_empty() {
        return Ok(base_image.clone());
    }

    let (base_w, base_h) = base_image.dimensions();

    struct DecodedPatch {
        offset_x: Option<u32>,
        offset_y: Option<u32>,
        mask: image::GrayImage,
        color: image::RgbImage,
        is_srgb_encoded: bool,
    }

    let decoded_patches: Result<Vec<DecodedPatch>> = visible_patches
        .par_iter()
        .map(|patch_obj| {
            let patch_data = patch_obj.get("patchData").context("Missing patchData")?;
            let offset_x = patch_data
                .get("offsetX")
                .and_then(|v| v.as_u64())
                .map(|v| v as u32);
            let offset_y = patch_data
                .get("offsetY")
                .and_then(|v| v.as_u64())
                .map(|v| v as u32);
            let is_cropped = offset_x.is_some() && offset_y.is_some();
            // A cropped patch's position and pixels are full-resolution ones.
            let resize_cropped = |image: image::DynamicImage| {
                if scale == 1.0 || !is_cropped {
                    return image;
                }
                let (w, h) = (image.width(), image.height());
                image.resize_exact(scaled(w), scaled(h), imageops::FilterType::Triangle)
            };
            let (offset_x, offset_y) = if scale == 1.0 {
                (offset_x, offset_y)
            } else {
                (
                    offset_x.map(|v| (v as f32 * scale).round() as u32),
                    offset_y.map(|v| (v as f32 * scale).round() as u32),
                )
            };

            let is_srgb_encoded = patch_data
                .get("isSrgbEncoded")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let mask_bitmap = if let Some(mask_b64) = patch_data
                .get("mask")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                let mask_bytes = general_purpose::STANDARD.decode(mask_b64)?;
                let mask_img = resize_cropped(image::load_from_memory(&mask_bytes)?).to_luma8();
                if !is_cropped && (mask_img.width() != base_w || mask_img.height() != base_h) {
                    imageops::resize(&mask_img, base_w, base_h, imageops::FilterType::Lanczos3)
                } else {
                    mask_img
                }
            } else {
                let patch_info: PatchMaskInfo = serde_json::from_value((*patch_obj).clone())
                    .context("Failed to deserialize patch info for mask generation")?;

                let mask_def = MaskDefinition {
                    id: patch_info.id,
                    name: patch_info.name,
                    visible: true,
                    invert: patch_info.invert,
                    opacity: 100.0,
                    mask_version: None,
                    refine_edge: None,
                    adjustments: Value::Null,
                    sub_masks: patch_info.sub_masks,
                };

                let orientation_steps = current_adjustments
                    .get("orientationSteps")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u8;
                let (trans_w, trans_h) = if orientation_steps % 2 == 1 {
                    (base_h, base_w)
                } else {
                    (base_w, base_h)
                };

                let mut gen_mask = generate_mask_bitmap(
                    &mask_def,
                    trans_w,
                    trans_h,
                    scale,
                    (0.0, 0.0),
                    &PhotoGeometry::from_adjustments(current_adjustments).with_photo_size(
                        base_w as f64 / scale as f64,
                        base_h as f64 / scale as f64,
                    ),
                    None,
                )
                .context("Failed to generate mask from sub_masks for compositing")?;

                gen_mask =
                    crate::image_processing::inverse_transform_mask(gen_mask, current_adjustments);

                if let (Some(ox), Some(oy)) = (offset_x, offset_y) {
                    let w = patch_data
                        .get("width")
                        .and_then(|v| v.as_u64())
                        .map(|v| {
                            if scale == 1.0 {
                                v as u32
                            } else {
                                scaled(v as u32)
                            }
                        })
                        .unwrap_or(base_w);
                    let h = patch_data
                        .get("height")
                        .and_then(|v| v.as_u64())
                        .map(|v| {
                            if scale == 1.0 {
                                v as u32
                            } else {
                                scaled(v as u32)
                            }
                        })
                        .unwrap_or(base_h);
                    let crop_w = w.min(base_w.saturating_sub(ox));
                    let crop_h = h.min(base_h.saturating_sub(oy));
                    gen_mask = imageops::crop_imm(&gen_mask, ox, oy, crop_w, crop_h).to_image();
                }
                gen_mask
            };

            let color_b64 = patch_data
                .get("color")
                .and_then(|v| v.as_str())
                .context("Missing color data")?;
            let color_bytes = general_purpose::STANDARD.decode(color_b64)?;
            let color_image_u8 = resize_cropped(image::load_from_memory(&color_bytes)?).to_rgb8();

            let (patch_w, patch_h) = color_image_u8.dimensions();
            let final_color = if !is_cropped && (base_w != patch_w || base_h != patch_h) {
                imageops::resize(
                    &color_image_u8,
                    base_w,
                    base_h,
                    imageops::FilterType::Lanczos3,
                )
            } else {
                color_image_u8
            };

            // The colour is read at the mask's pixel indices: a damaged
            // recipe with a smaller colour image fails here, not by panic.
            let (mask_w, mask_h) = mask_bitmap.dimensions();
            let (color_w, color_h) = final_color.dimensions();
            ensure!(
                color_w as u64 * color_h as u64 >= mask_w as u64 * mask_h as u64,
                "Patch colour {color_w}x{color_h} is smaller than its mask {mask_w}x{mask_h}"
            );

            Ok(DecodedPatch {
                offset_x,
                offset_y,
                mask: mask_bitmap,
                color: final_color,
                is_srgb_encoded,
            })
        })
        .collect();

    let decoded_patches = decoded_patches?;

    let mut composited_image = base_image.clone();
    let lut = srgb_to_linear_lut();

    let get_color = |patch: &DecodedPatch, r: u8, g: u8, b: u8| -> (f32, f32, f32) {
        if patch.is_srgb_encoded {
            (lut[r as usize], lut[g as usize], lut[b as usize])
        } else {
            (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0)
        }
    };

    match &mut composited_image {
        DynamicImage::ImageRgb32F(img_buf) => {
            for patch in decoded_patches {
                let mask_raw = patch.mask.as_raw();
                let color_raw = patch.color.as_raw();
                let patch_w = patch.mask.width() as usize;

                if let (Some(ox), Some(oy)) = (patch.offset_x, patch.offset_y) {
                    let max_x = (ox + patch.mask.width()).min(base_w);
                    let max_y = (oy + patch.mask.height()).min(base_h);

                    let crop_w = max_x.saturating_sub(ox) as usize;
                    let crop_h = max_y.saturating_sub(oy) as usize;

                    if crop_w == 0 || crop_h == 0 {
                        continue;
                    }

                    let base_w_usize = base_w as usize;
                    let ox_usize = ox as usize;
                    let oy_usize = oy as usize;

                    img_buf
                        .par_chunks_mut(base_w_usize * 3)
                        .enumerate()
                        .skip(oy_usize)
                        .take(crop_h)
                        .for_each(|(y, row)| {
                            let py = y - oy_usize;
                            let patch_row_start = py * patch_w;

                            for x in ox_usize..(ox_usize + crop_w) {
                                let px = x - ox_usize;
                                let mask_idx = patch_row_start + px;
                                let mask_value = mask_raw[mask_idx];

                                if mask_value > 0 {
                                    let color_idx = mask_idx * 3;
                                    let pr_u8 = color_raw[color_idx];
                                    let pg_u8 = color_raw[color_idx + 1];
                                    let pb_u8 = color_raw[color_idx + 2];

                                    let (pr, pg, pb) = get_color(&patch, pr_u8, pg_u8, pb_u8);

                                    let alpha = mask_value as f32 / 255.0;
                                    let one_minus_alpha = 1.0 - alpha;

                                    let base_idx = x * 3;
                                    row[base_idx] = pr * alpha + row[base_idx] * one_minus_alpha;
                                    row[base_idx + 1] =
                                        pg * alpha + row[base_idx + 1] * one_minus_alpha;
                                    row[base_idx + 2] =
                                        pb * alpha + row[base_idx + 2] * one_minus_alpha;
                                }
                            }
                        });
                } else {
                    img_buf
                        .par_chunks_mut((base_w * 3) as usize)
                        .enumerate()
                        .for_each(|(y, row)| {
                            let patch_row_start = y * patch_w;
                            for x in 0..base_w as usize {
                                let mask_idx = patch_row_start + x;
                                let mask_value = mask_raw[mask_idx];
                                if mask_value > 0 {
                                    let color_idx = mask_idx * 3;
                                    let pr_u8 = color_raw[color_idx];
                                    let pg_u8 = color_raw[color_idx + 1];
                                    let pb_u8 = color_raw[color_idx + 2];

                                    let (pr, pg, pb) = get_color(&patch, pr_u8, pg_u8, pb_u8);

                                    let alpha = mask_value as f32 / 255.0;
                                    let one_minus_alpha = 1.0 - alpha;

                                    row[x * 3] = pr * alpha + row[x * 3] * one_minus_alpha;
                                    row[x * 3 + 1] = pg * alpha + row[x * 3 + 1] * one_minus_alpha;
                                    row[x * 3 + 2] = pb * alpha + row[x * 3 + 2] * one_minus_alpha;
                                }
                            }
                        });
                }
            }
        }
        DynamicImage::ImageRgba32F(img_buf) => {
            for patch in decoded_patches {
                let mask_raw = patch.mask.as_raw();
                let color_raw = patch.color.as_raw();
                let patch_w = patch.mask.width() as usize;

                if let (Some(ox), Some(oy)) = (patch.offset_x, patch.offset_y) {
                    let max_x = (ox + patch.mask.width()).min(base_w);
                    let max_y = (oy + patch.mask.height()).min(base_h);

                    let crop_w = max_x.saturating_sub(ox) as usize;
                    let crop_h = max_y.saturating_sub(oy) as usize;

                    if crop_w == 0 || crop_h == 0 {
                        continue;
                    }

                    let base_w_usize = base_w as usize;
                    let ox_usize = ox as usize;
                    let oy_usize = oy as usize;

                    img_buf
                        .par_chunks_mut(base_w_usize * 4)
                        .enumerate()
                        .skip(oy_usize)
                        .take(crop_h)
                        .for_each(|(y, row)| {
                            let py = y - oy_usize;
                            let patch_row_start = py * patch_w;

                            for x in ox_usize..(ox_usize + crop_w) {
                                let px = x - ox_usize;
                                let mask_idx = patch_row_start + px;
                                let mask_value = mask_raw[mask_idx];

                                if mask_value > 0 {
                                    let color_idx = mask_idx * 3;
                                    let pr_u8 = color_raw[color_idx];
                                    let pg_u8 = color_raw[color_idx + 1];
                                    let pb_u8 = color_raw[color_idx + 2];

                                    let (pr, pg, pb) = get_color(&patch, pr_u8, pg_u8, pb_u8);
                                    let alpha = mask_value as f32 / 255.0;
                                    let one_minus_alpha = 1.0 - alpha;

                                    let base_idx = x * 4;
                                    row[base_idx] = pr * alpha + row[base_idx] * one_minus_alpha;
                                    row[base_idx + 1] =
                                        pg * alpha + row[base_idx + 1] * one_minus_alpha;
                                    row[base_idx + 2] =
                                        pb * alpha + row[base_idx + 2] * one_minus_alpha;
                                }
                            }
                        });
                } else {
                    img_buf
                        .par_chunks_mut((base_w * 4) as usize)
                        .enumerate()
                        .for_each(|(y, row)| {
                            let patch_row_start = y * patch_w;
                            for x in 0..base_w as usize {
                                let mask_idx = patch_row_start + x;
                                let mask_value = mask_raw[mask_idx];
                                if mask_value > 0 {
                                    let color_idx = mask_idx * 3;
                                    let pr_u8 = color_raw[color_idx];
                                    let pg_u8 = color_raw[color_idx + 1];
                                    let pb_u8 = color_raw[color_idx + 2];

                                    let (pr, pg, pb) = get_color(&patch, pr_u8, pg_u8, pb_u8);

                                    let alpha = mask_value as f32 / 255.0;
                                    let one_minus_alpha = 1.0 - alpha;

                                    row[x * 4] = pr * alpha + row[x * 4] * one_minus_alpha;
                                    row[x * 4 + 1] = pg * alpha + row[x * 4 + 1] * one_minus_alpha;
                                    row[x * 4 + 2] = pb * alpha + row[x * 4 + 2] * one_minus_alpha;
                                }
                            }
                        });
                }
            }
        }
        _ => {
            let mut rgba32_img = composited_image.to_rgba32f();
            for patch in decoded_patches {
                let mask_raw = patch.mask.as_raw();
                let color_raw = patch.color.as_raw();
                let patch_w = patch.mask.width() as usize;

                if let (Some(ox), Some(oy)) = (patch.offset_x, patch.offset_y) {
                    let max_x = (ox + patch.mask.width()).min(base_w);
                    let max_y = (oy + patch.mask.height()).min(base_h);

                    let crop_w = max_x.saturating_sub(ox) as usize;
                    let crop_h = max_y.saturating_sub(oy) as usize;

                    if crop_w == 0 || crop_h == 0 {
                        continue;
                    }

                    let base_w_usize = base_w as usize;
                    let ox_usize = ox as usize;
                    let oy_usize = oy as usize;

                    rgba32_img
                        .par_chunks_mut(base_w_usize * 4)
                        .enumerate()
                        .skip(oy_usize)
                        .take(crop_h)
                        .for_each(|(y, row)| {
                            let py = y - oy_usize;
                            let patch_row_start = py * patch_w;

                            for x in ox_usize..(ox_usize + crop_w) {
                                let px = x - ox_usize;
                                let mask_idx = patch_row_start + px;
                                let mask_value = mask_raw[mask_idx];

                                if mask_value > 0 {
                                    let color_idx = mask_idx * 3;
                                    let pr_u8 = color_raw[color_idx];
                                    let pg_u8 = color_raw[color_idx + 1];
                                    let pb_u8 = color_raw[color_idx + 2];

                                    let (pr, pg, pb) = get_color(&patch, pr_u8, pg_u8, pb_u8);
                                    let alpha = mask_value as f32 / 255.0;
                                    let one_minus_alpha = 1.0 - alpha;

                                    let base_idx = x * 4;
                                    row[base_idx] = pr * alpha + row[base_idx] * one_minus_alpha;
                                    row[base_idx + 1] =
                                        pg * alpha + row[base_idx + 1] * one_minus_alpha;
                                    row[base_idx + 2] =
                                        pb * alpha + row[base_idx + 2] * one_minus_alpha;
                                }
                            }
                        });
                } else {
                    rgba32_img
                        .par_chunks_mut((base_w * 4) as usize)
                        .enumerate()
                        .for_each(|(y, row)| {
                            let patch_row_start = y * patch_w;
                            for x in 0..base_w as usize {
                                let mask_idx = patch_row_start + x;
                                let mask_value = mask_raw[mask_idx];
                                if mask_value > 0 {
                                    let color_idx = mask_idx * 3;
                                    let pr_u8 = color_raw[color_idx];
                                    let pg_u8 = color_raw[color_idx + 1];
                                    let pb_u8 = color_raw[color_idx + 2];

                                    let (pr, pg, pb) = get_color(&patch, pr_u8, pg_u8, pb_u8);
                                    let alpha = mask_value as f32 / 255.0;
                                    let one_minus_alpha = 1.0 - alpha;

                                    row[x * 4] = pr * alpha + row[x * 4] * one_minus_alpha;
                                    row[x * 4 + 1] = pg * alpha + row[x * 4 + 1] * one_minus_alpha;
                                    row[x * 4 + 2] = pb * alpha + row[x * 4 + 2] * one_minus_alpha;
                                }
                            }
                        });
                }
            }
            composited_image = DynamicImage::ImageRgba32F(rgba32_img);
        }
    }

    Ok(composited_image)
}

#[cfg(test)]
mod private_preview_tests {
    use super::*;
    use crate::file_io::read_file_mapped;
    use std::fs;

    /// OpenEXR holds linear light: loaded sRGB-encoded (the renderer's
    /// input), so the renderer gets the stored values back, also above 1.
    #[test]
    fn exr_loads_as_encoded_linear_light() {
        let linear = [0.18f32, 1.0, 4.0];
        let image = image::Rgb32FImage::from_fn(2, 1, |_, _| image::Rgb(linear));
        let mut bytes = Vec::new();
        DynamicImage::ImageRgb32F(image)
            .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::OpenExr)
            .unwrap();
        let loaded = load_image_with_orientation(&bytes, None)
            .unwrap()
            .into_rgb32f();
        let decode = |v: f32| {
            if v <= 0.040_45 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        for (stored, expected) in loaded.get_pixel(0, 0).0.iter().zip(linear) {
            assert!(
                (decode(*stored) - expected).abs() < 1e-3 * expected.max(1.0),
                "{stored} → {expected}"
            );
        }
    }

    /// JPEG XL decodes without the desktop app registering the decoder
    /// first (the CLI and headless export load through here too).
    #[test]
    fn jpeg_xl_loads_without_the_app() {
        let pixels: Vec<u8> = (0..4 * 4 * 3).map(|i| (i * 5) as u8).collect();
        let bytes = jxl_encoder::LosslessConfig::new()
            .encode(&pixels, 4, 4, jxl_encoder::PixelLayout::Rgb8)
            .unwrap();
        let image = load_image_with_orientation(&bytes, None).unwrap();
        assert_eq!((image.width(), image.height()), (4, 4));
    }

    fn orientation_tiff(orientation: u16) -> Vec<u8> {
        let mut bytes = b"II\x2a\x00\x08\x00\x00\x00\x01\x00".to_vec();
        bytes.extend_from_slice(&0x112u16.to_le_bytes());
        bytes.extend_from_slice(&3u16.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&orientation.to_le_bytes());
        bytes.extend_from_slice(&[0; 6]);
        bytes
    }

    fn jpeg_with_orientation(jpeg: &[u8], orientation: u16) -> Vec<u8> {
        let mut exif = b"Exif\0\0".to_vec();
        exif.extend(orientation_tiff(orientation));
        let mut bytes = jpeg[..2].to_vec();
        bytes.extend_from_slice(&[0xff, 0xe1]);
        bytes.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
        bytes.extend(exif);
        bytes.extend_from_slice(&jpeg[2..]);
        bytes
    }

    fn raf_with_jpeg(jpeg: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0; 148];
        let magic = b"FUJIFILMCCD-RAW ";
        bytes[..magic.len()].copy_from_slice(magic);
        bytes[84..88].copy_from_slice(&148u32.to_be_bytes());
        bytes[88..92].copy_from_slice(&(jpeg.len() as u32).to_be_bytes());
        bytes.extend_from_slice(jpeg);
        bytes
    }

    fn assert_oriented_pixels(actual: &DynamicImage, original: &DynamicImage, orientation: u16) {
        let (width, height) = original.dimensions();
        let expected_dimensions = if (5..=8).contains(&orientation) {
            (height, width)
        } else {
            (width, height)
        };
        assert_eq!(actual.dimensions(), expected_dimensions);
        for (x, y, pixel) in actual.pixels() {
            // Independent EXIF destination-to-source mapping, including mirrors.
            let (sx, sy) = match orientation {
                2 => (width - 1 - x, y),
                3 => (width - 1 - x, height - 1 - y),
                4 => (x, height - 1 - y),
                5 => (y, x),
                6 => (y, height - 1 - x),
                7 => (width - 1 - y, height - 1 - x),
                8 => (width - 1 - y, x),
                _ => (x, y),
            };
            assert_eq!(pixel, original.get_pixel(sx, sy));
        }
    }

    #[test]
    fn embedded_raw_previews_apply_camera_orientation_once_without_recipe_rotation() {
        let image = DynamicImage::ImageRgb8(image::ImageBuffer::from_fn(7, 3, |x, y| {
            image::Rgb([(x * 29) as u8, (y * 83) as u8, ((x + y) * 21) as u8])
        }));
        let mut jpeg = Cursor::new(Vec::new());
        image.write_to(&mut jpeg, image::ImageFormat::Jpeg).unwrap();
        let jpeg = jpeg.into_inner();
        let decoded = image::load_from_memory(&jpeg).unwrap();
        for orientation in 1..=8 {
            let embedded = jpeg_with_orientation(&jpeg, orientation);
            let raf = raf_with_jpeg(&embedded);
            // Reproduces the old bug: the generic EXIF reader cannot read RAF.
            assert_eq!(exif_orientation(&raf), None);
            assert_eq!(exif_orientation(&embedded), Some(orientation));
            let actual = embedded_preview_fallback(&raf, "synthetic.raf").unwrap();
            assert_oriented_pixels(&actual, &decoded, orientation);
            let priority = priority_embedded_jpeg(&raf).unwrap();
            assert_eq!(priority, embedded, "RAF JPEG bytes need no transcoding");
            assert_eq!(exif_orientation(&priority), Some(orientation));

            // TIFF-based RAW previews retain their primary camera orientation.
            let mut tiff = orientation_tiff(orientation);
            tiff[8..10].copy_from_slice(&3u16.to_le_bytes());
            tiff.truncate(22);
            for (tag, value) in [(513u16, 50u32), (514, jpeg.len() as u32)] {
                tiff.extend_from_slice(&tag.to_le_bytes());
                tiff.extend_from_slice(&4u16.to_le_bytes());
                tiff.extend_from_slice(&1u32.to_le_bytes());
                tiff.extend_from_slice(&value.to_le_bytes());
            }
            tiff.extend_from_slice(&0u32.to_le_bytes());
            tiff.extend_from_slice(&jpeg);
            let actual = embedded_preview_fallback(&tiff, "synthetic.dng").unwrap();
            assert_oriented_pixels(&actual, &decoded, orientation);
            let priority = priority_embedded_jpeg(&tiff).unwrap();
            assert_eq!(exif_orientation(&priority), Some(orientation));
            assert_eq!(
                image::load_from_memory(&priority).unwrap().to_rgb8(),
                decoded.to_rgb8(),
                "orientation metadata must not re-encode camera JPEG pixels"
            );
        }
    }

    #[test]
    fn raf_preview_bounds_and_missing_orientation_are_safe() {
        let image = DynamicImage::new_rgb8(7, 3);
        let mut jpeg = Cursor::new(Vec::new());
        image.write_to(&mut jpeg, image::ImageFormat::Jpeg).unwrap();
        let raf = raf_with_jpeg(jpeg.get_ref());
        let preview = embedded_preview_fallback(&raf, "synthetic.raf").unwrap();
        assert_eq!(preview.dimensions(), (7, 3));
        assert!(raf_embedded_jpeg(b"FUJIFILMCCD-RAW ").is_none());
        let mut raf = raf_with_jpeg(&[0xff, 0xd8, 0xff, 0xd9]);
        raf[84..88].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(raf_embedded_jpeg(&raf).is_none());
        let mut raf = raf_with_jpeg(&[0xff, 0xd8, 0xff, 0xd9]);
        raf[88..92].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(raf_embedded_jpeg(&raf).is_none());
        assert_eq!(exif_orientation(&orientation_tiff(0)), None);
        assert_eq!(exif_orientation(&orientation_tiff(9)), None);
    }

    #[test]
    fn a_patch_colour_smaller_than_its_mask_fails_instead_of_panicking() {
        use image::{ImageBuffer, Luma, Rgb};
        use serde_json::json;

        let encode_png = |image: DynamicImage| {
            let mut bytes = Cursor::new(Vec::new());
            image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
            general_purpose::STANDARD.encode(bytes.into_inner())
        };
        // A damaged recipe: a 20x10 mask with a 4x2 colour.
        let color = encode_png(DynamicImage::ImageRgb8(ImageBuffer::from_pixel(
            4,
            2,
            Rgb([255, 0, 0]),
        )));
        let mask = encode_png(DynamicImage::ImageLuma8(ImageBuffer::from_pixel(
            20,
            10,
            Luma([255]),
        )));
        let adjustments = json!({"aiPatches":[{"visible":true,"patchData":{
            "color":color,"mask":mask,"offsetX":40,"offsetY":20,"width":20,"height":10}}]});
        let black = DynamicImage::ImageRgb32F(ImageBuffer::new(100, 60));
        let error = composite_patches_on_image(&black, &adjustments).unwrap_err();
        assert!(
            error.to_string().contains("smaller than its mask"),
            "{error}"
        );
    }

    #[test]
    fn patches_land_in_place_on_a_reduced_copy() {
        use image::{ImageBuffer, Luma, Rgb};
        use serde_json::json;

        let encode_png = |image: DynamicImage| {
            let mut bytes = Cursor::new(Vec::new());
            image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
            general_purpose::STANDARD.encode(bytes.into_inner())
        };
        // A 20x10 red patch at (40, 20) of a 100x60 photograph.
        let color = encode_png(DynamicImage::ImageRgb8(ImageBuffer::from_pixel(
            20,
            10,
            Rgb([255, 0, 0]),
        )));
        let mask = encode_png(DynamicImage::ImageLuma8(ImageBuffer::from_pixel(
            20,
            10,
            Luma([255]),
        )));
        let adjustments = json!({"aiPatches":[{"visible":true,"patchData":{
            "color":color,"mask":mask,"offsetX":40,"offsetY":20,"width":20,"height":10}}]});
        let black = |w, h| DynamicImage::ImageRgb32F(ImageBuffer::new(w, h));
        let full = composite_patches_on_image(&black(100, 60), &adjustments).unwrap();
        assert_eq!(full.get_pixel(45, 25)[0], 255);
        assert_eq!(full.get_pixel(39, 25)[0], 0);
        // At half size it covers (20, 10) to (30, 15).
        let half = composite_patches_on_scaled_image(&black(50, 30), &adjustments, 0.5)
            .unwrap()
            .to_rgb32f();
        assert!(half.get_pixel(25, 12)[0] > 0.9);
        assert!(half.get_pixel(18, 12)[0] < 0.1);
        assert!(half.get_pixel(25, 17)[0] < 0.1);
        // Scale 1 is the full-resolution path itself.
        assert_eq!(
            composite_patches_on_scaled_image(&black(100, 60), &adjustments, 1.0)
                .unwrap()
                .to_rgb32f()
                .as_raw(),
            full.to_rgb32f().as_raw()
        );
    }

    #[test]
    fn render_patches_preserve_pristine_mask_samples_and_borrow_unpatched_pixels() {
        use crate::mask_generation::prepare_parametric_mask_source;
        use image::{ImageBuffer, Luma, Rgb};
        use serde_json::json;

        let pristine = DynamicImage::ImageRgb32F(ImageBuffer::from_fn(2, 1, |x, _| {
            if x == 0 {
                Rgb([1.0, 0.0, 0.0])
            } else {
                Rgb([0.0, 0.0, 1.0])
            }
        }));
        for adjustments in [
            json!({}),
            json!({"aiPatches":[]}),
            json!({"aiPatches":[{"visible":false,"patchData":{"color":"unused"}}]}),
            json!({"aiPatches":[{"visible":true,"patchData":{"color":""}}]}),
        ] {
            assert!(!has_generated_areas(&adjustments));
            let render = composite_patches_for_render(&pristine, &adjustments).unwrap();
            assert!(matches!(render, Cow::Borrowed(_)));
            assert!(std::ptr::eq(render.as_ref(), &pristine));
        }
        let encode_png = |image: DynamicImage| {
            let mut bytes = Cursor::new(Vec::new());
            image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
            general_purpose::STANDARD.encode(bytes.into_inner())
        };
        let color = encode_png(DynamicImage::ImageRgb8(ImageBuffer::from_pixel(
            2,
            1,
            Rgb([0, 0, 255]),
        )));
        let mask = encode_png(DynamicImage::ImageLuma8(ImageBuffer::from_fn(
            2,
            1,
            |x, _| Luma([if x == 0 { 255 } else { 0 }]),
        )));
        let adjustments =
            json!({"aiPatches":[{"visible":true,"patchData":{"color":color,"mask":mask}}]});
        assert!(has_generated_areas(&adjustments));
        // Clone, heal, liquify and retouch patches are not generated; a
        // patch with any other sub-mask (an object-removal brush) is.
        for kind in ["clone", "heal", "liquify", "retouch"] {
            let hand_made = json!({"aiPatches":[{"visible":true,
                "subMasks":[{"type":kind}],"patchData":{"color":color,"mask":mask}}]});
            assert!(!has_generated_areas(&hand_made), "{kind}");
        }
        let removal = json!({"aiPatches":[{"visible":true,
            "subMasks":[{"type":"heal"},{"type":"brush"}],"patchData":{"color":color,"mask":mask}}]});
        assert!(has_generated_areas(&removal));
        // An object removed with Remove's Detect objects (an object mask
        // filled by the inpainting model) is generated too.
        let object = json!({"aiPatches":[{"visible":true,"name":"Remove",
            "subMasks":[{"type":"quick-eraser"}],"patchData":{"color":color,"mask":mask}}]});
        assert!(has_generated_areas(&object));
        // A refined selection (added to and subtracted from with Shift/Alt)
        // is one patch of several quick-eraser components: still generated.
        let refined = json!({"aiPatches":[{"visible":true,"name":"Remove",
            "subMasks":[{"type":"quick-eraser","mode":"additive"},{"type":"quick-eraser","mode":"additive"},
                {"type":"quick-eraser","mode":"subtractive"}],"patchData":{"color":color,"mask":mask}}]});
        assert!(has_generated_areas(&refined));
        // Fill edges completes from the photo's own pixels, without a model.
        let fill = json!({"aiPatches":[{"visible":true,"name":"Fill edges","generated":false,
            "subMasks":[],"patchData":{"color":color,"mask":mask}}]});
        assert!(!has_generated_areas(&fill));
        let render = composite_patches_for_render(&pristine, &adjustments).unwrap();
        assert!(matches!(render, Cow::Owned(_)));
        assert_eq!(render.get_pixel(0, 0), image::Rgba([0, 0, 255, 255]));
        assert_eq!(pristine.get_pixel(0, 0), image::Rgba([255, 0, 0, 255]));
        let definition: MaskDefinition = serde_json::from_value(json!({
            "id":"color", "name":"Color", "visible":true, "invert":false,
            "adjustments":{}, "subMasks":[{"id":"sample","type":"color",
                "visible":true,"mode":"additive","parameters":{"targetX":0,"targetY":0,"tolerance":20}}]
        })).unwrap();
        let mask_pixels = |source: &DynamicImage| {
            let source = prepare_parametric_mask_source(source, false, 1.0, &adjustments).unwrap();
            generate_mask_bitmap(
                &definition,
                2,
                1,
                1.0,
                (0.0, 0.0),
                &PhotoGeometry::default(),
                Some(&source),
            )
            .unwrap()
        };
        assert_eq!(mask_pixels(&pristine).as_raw(), &[255, 0]);
        // This would select both pixels if the composited render image leaked
        // into the mask source, demonstrating why both inputs stay separate.
        assert_eq!(mask_pixels(&render).as_raw(), &[255, 255]);
    }

    /// Local camera fixtures only; never check private photographs into the repo.
    #[test]
    #[ignore = "requires RAWTONIC_RAW_FIXTURES pointing to a local sample directory"]
    fn private_raw_previews() {
        let dir = std::env::var("RAWTONIC_RAW_FIXTURES").expect("local fixture directory");
        let mut count = 0;
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if !crate::formats::is_raw_file(path.to_string_lossy().as_ref()) {
                continue;
            }
            let start = std::time::Instant::now();
            let bytes = std::fs::read(&path).unwrap();
            let preview = safe_embedded_preview_fallback(&bytes, &path.to_string_lossy())
                .expect("camera must expose a browser preview without RAW development");
            assert!(preview.width().max(preview.height()) >= 480);
            let priority = priority_embedded_jpeg(&bytes)
                .expect("camera must expose a priority JPEG without RAW development");
            let source = rawler::rawsource::RawSource::new(&path).unwrap();
            let decoder = rawler::get_decoder(&source).unwrap();
            let metadata = decoder
                .raw_metadata(&source, &rawler::decoders::RawDecodeParams::default())
                .unwrap();
            let orientation = metadata.exif.orientation.unwrap_or(1);
            assert_eq!(exif_orientation(&priority), Some(orientation));
            let priority_unoriented = image::load_from_memory(&priority).unwrap();
            assert_oriented_pixels(&preview, &priority_unoriented, orientation);
            if let Some(jpeg) = raf_embedded_jpeg(&bytes) {
                let original = image::load_from_memory(jpeg).unwrap();
                assert_oriented_pixels(&preview, &original, orientation);
            }
            println!(
                "validated oriented preview {}x{} in {:.1} ms",
                preview.width(),
                preview.height(),
                start.elapsed().as_secs_f64() * 1000.0
            );
            count += 1;
        }
        assert!(count > 0);
        println!("{} camera previews verified", count);
    }

    #[test]
    #[ignore = "private CPU extraction probe; excludes file reads, WebKit decoding and presentation"]
    fn priority_preview_timing_probe() {
        let directory = std::env::var("RAWTONIC_RAW_FIXTURES").expect("local fixture directory");
        for (index, entry) in fs::read_dir(directory).unwrap().enumerate() {
            let path = entry.unwrap().path();
            if !is_raw_file(&path) {
                continue;
            }
            let bytes = read_file_mapped(&path).unwrap();
            for sample in 0..12 {
                let start = Instant::now();
                let jpeg = priority_embedded_jpeg(&bytes).expect("priority camera JPEG");
                let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
                println!(
                    "{}",
                    serde_json::json!({
                        "stage": "priority_jpeg_extraction", "fixture": index, "sample": sample,
                        "ms": elapsed_ms, "jpeg_bytes": jpeg.len(), "orientation": exif_orientation(&jpeg)
                    })
                );
            }
        }
    }

    #[test]
    fn nikon_high_efficiency_raws_explain_the_alternatives() {
        let error = super::classify_raw_develop_error(
            "/photos/DSC_0001.NEF",
            anyhow::anyhow!("NEF compression Some(HighEfficencyStar) is not supported"),
        )
        .to_string();
        assert!(error.contains("Nikon High Efficiency RAW"));
        assert!(error.contains("Lossless compressed") && error.contains("DNG"));
        assert!(error.contains(super::NO_OPEN_DECODER));
        let other = super::classify_raw_develop_error(
            "/photos/DSC_0002.NEF",
            anyhow::anyhow!("NEF compression Some(Lossy) is not supported"),
        )
        .to_string();
        assert!(other.starts_with("Unsupported RAW compression format"));
    }

    #[test]
    fn exports_refuse_raws_shown_through_their_embedded_preview() {
        let path = "/nonexistent/preview-fallback-test.NEF";
        assert!(super::ensure_raw_developed(path).is_ok());
        super::note_preview_fallback(path, Some("unsupported compression".to_string()));
        let error = super::ensure_raw_developed(path).unwrap_err().to_string();
        assert!(error.contains("unsupported compression") && error.contains("export refused"));
        assert_eq!(
            super::preview_fallback_reason(path).as_deref(),
            Some("unsupported compression")
        );
        super::note_preview_fallback(path, None);
        assert!(super::ensure_raw_developed(path).is_ok());
        assert_eq!(super::preview_fallback_reason(path), None);
    }
}

#[cfg(test)]
mod ai_denoise_tests {
    use super::ai_denoise_amount;
    use serde_json::json;

    #[test]
    fn the_amount_follows_the_recipe_and_the_tool_switches() {
        assert_eq!(ai_denoise_amount(&json!({})), 0.0);
        assert_eq!(ai_denoise_amount(&json!({ "aiDenoise": 60 })), 0.6);
        assert_eq!(ai_denoise_amount(&json!({ "aiDenoise": 250 })), 1.0);
        let off = json!({ "aiDenoise": 60, "sectionVisibility": { "noiseReduction": false } });
        assert_eq!(ai_denoise_amount(&off), 0.0);
        let details_off = json!({ "aiDenoise": 60, "sectionVisibility": { "details": false } });
        assert_eq!(ai_denoise_amount(&details_off), 0.0);
    }

    #[test]
    fn ai_denoise_version_2_maps_the_slider_to_the_noise_scale() {
        use super::{AiDenoiseSettings, ai_denoise_settings};
        assert_eq!(
            ai_denoise_settings(&json!({ "aiDenoise": 0, "aiDenoiseVersion": 2 })),
            None
        );
        // Without the key: version 1, the slider is the blend.
        let v1 = ai_denoise_settings(&json!({ "aiDenoise": 60 })).unwrap();
        assert_eq!((v1.version, v1.blend, v1.noise_scale), (1, 0.6, 1.0));
        let at = |v: f64| {
            ai_denoise_settings(&json!({ "aiDenoise": v, "aiDenoiseVersion": 2 })).unwrap()
        };
        assert_eq!(
            at(25.0),
            AiDenoiseSettings {
                version: 2,
                blend: 0.5,
                noise_scale: 0.8
            }
        );
        assert_eq!(
            at(50.0),
            AiDenoiseSettings {
                version: 2,
                blend: 1.0,
                noise_scale: 0.8
            }
        );
        assert!(
            (at(75.0).noise_scale - 1.0).abs() < 1e-6,
            "75 = the network as trained"
        );
        assert!((at(100.0).noise_scale - 1.25).abs() < 1e-6);
        // Unknown later versions read as the newest known.
        assert_eq!(
            ai_denoise_settings(&json!({ "aiDenoise": 50, "aiDenoiseVersion": 9 }))
                .unwrap()
                .version,
            3
        );
        // Version 3: the same mapping on the mixed-precision model.
        let v3 = ai_denoise_settings(&json!({ "aiDenoise": 50, "aiDenoiseVersion": 3 })).unwrap();
        assert_eq!((v3.blend, v3.noise_scale, v3.model()), (1.0, 0.8, 1));
        assert_eq!((at(50.0).model(), v1.model()), (0, 0));
    }
}

#[cfg(test)]
mod color_noise_option_tests {
    use super::BaseImageOptions;
    use serde_json::{Value, json};

    fn preferences() -> BaseImageOptions {
        BaseImageOptions::from_preferences(
            Some(2.5),
            "auto".to_string(),
            Some(0.5),
            Some(0.35),
            Some(false),
        )
    }

    #[test]
    fn the_multiscale_filter_skips_only_the_decode_time_chroma_clean_up() {
        let today = preferences();
        assert!(today.color_nr_amount > 0.0);
        let multiscale = preferences().for_color_noise(true);
        assert_eq!(multiscale.color_nr_amount, 0.0);
        assert_eq!(multiscale.sharpening_amount, today.sharpening_amount);
        let highlights = today.highlight_compression;
        assert_eq!(multiscale.highlight_compression, highlights);
        let old = preferences().for_color_noise(false);
        assert_eq!(old.color_nr_amount, today.color_nr_amount);
        assert_eq!(old.sharpening_amount, today.sharpening_amount);
    }

    /// The chroma clean-up amount for `recipe`.
    fn amount(recipe: &Value, is_raw: bool) -> f32 {
        let source = if is_raw { "/p/A.NEF" } else { "/p/A.jpg" };
        preferences().for_recipe(recipe, source).color_nr_amount
    }

    #[test]
    fn a_v1_recipe_keeps_todays_base_and_a_v2_recipe_skips_the_clean_up() {
        let today = preferences().color_nr_amount;
        let v1 = json!({ "colorNoiseReduction": 25, "colorNoiseVersion": 1 });
        let legacy = json!({ "colorNoiseReduction": 25 });
        let v2 = json!({ "colorNoiseReduction": 25, "colorNoiseVersion": 2 });
        let v3 = json!({ "colorNoiseReduction": 25, "colorNoiseVersion": 3 });
        let v5 = json!({ "colorNoiseReduction": 25, "colorNoiseVersion": 5 });
        let v6 = json!({ "colorNoiseReduction": 25, "colorNoiseVersion": 6 });
        let v7 = json!({ "colorNoiseReduction": 25, "colorNoiseVersion": 7 });
        let v8 = json!({ "colorNoiseReduction": 25, "colorNoiseVersion": 8 });
        let v9 = json!({ "colorNoiseReduction": 25, "colorNoiseVersion": 9 });
        let v10 = json!({ "colorNoiseReduction": 25, "colorNoiseVersion": 10 });
        let v11 = json!({ "colorNoiseReduction": 25, "colorNoiseVersion": 11 });
        for recipe in [&v1, &legacy] {
            assert_eq!(amount(recipe, true), today);
        }
        // Every multi-scale version shares the base without the clean-up.
        assert_eq!(amount(&v2, true), 0.0);
        assert_eq!(amount(&v3, true), 0.0);
        assert_eq!(amount(&v5, true), 0.0);
        assert_eq!(amount(&v6, true), 0.0);
        assert_eq!(amount(&v7, true), 0.0);
        assert_eq!(amount(&v8, true), 0.0);
        assert_eq!(amount(&v9, true), 0.0);
        assert_eq!(amount(&v10, true), 0.0);
        assert_eq!(amount(&v11, true), 0.0);
        // Never-edited RAWs develop with the multi-scale filter.
        assert_eq!(amount(&Value::Null, true), 0.0);
        assert_eq!(amount(&json!({}), true), 0.0);
        // Non-RAW files without a version keep the old filter.
        assert_eq!(amount(&json!({}), false), today);
    }
}

#[cfg(test)]
mod decode_variant_tests {
    use super::{DeadTest, decode_variant};
    use serde_json::json;

    #[test]
    fn the_render_version_decides_the_dead_photosite_test() {
        let test = |recipe| decode_variant(&recipe, "/p/A.NEF").dead_test;
        // Saved edits before render version 3 keep the first test.
        assert_eq!(test(json!({ "exposure": 1 })), DeadTest::SameColour);
        assert_eq!(test(json!({ "renderVersion": 2 })), DeadTest::SameColour);
        assert_eq!(test(json!({ "renderVersion": 3 })), DeadTest::Adjacent);
        assert_eq!(test(json!({ "renderVersion": 9 })), DeadTest::Adjacent);
        // A never-edited photo renders as a new edit.
        assert_eq!(test(serde_json::Value::Null), DeadTest::Adjacent);
        assert_eq!(test(json!({})), DeadTest::Adjacent);
        // The colour noise filter is decided as before.
        assert!(decode_variant(&json!({}), "/p/A.NEF").multiscale);
        assert!(!decode_variant(&json!({ "colorNoiseReduction": 25 }), "/p/A.NEF").multiscale);
    }
}
