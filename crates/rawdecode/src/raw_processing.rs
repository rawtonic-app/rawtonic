use crate::orientation::apply_orientation;
use anyhow::{Result, anyhow};
use image::{DynamicImage, ImageBuffer, Rgb};
use rawler::{
    decoders::{Orientation, RawDecodeParams},
    imgop::develop::{DemosaicAlgorithm, Intermediate, ProcessingStep, RawDevelop},
    rawimage::{RawImage, RawPhotometricInterpretation},
    rawsource::RawSource,
};
use rayon::prelude::*;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

pub fn develop_raw_image(
    file_bytes: &[u8],
    fast_demosaic: bool,
    highlight_compression: f32,
    linear_mode: String,
    cancel_token: Option<(Arc<AtomicUsize>, usize)>,
) -> Result<DynamicImage> {
    develop_raw_image_with(
        file_bytes,
        fast_demosaic,
        highlight_compression,
        linear_mode,
        cancel_token,
        None,
    )
}

/// `develop_raw_image` with an optional mosaic denoiser
/// (mosaic_denoise.rs), which full developments of Bayer mosaics run
/// before highlight reconstruction and demosaicing. Colours outside sRGB
/// keep their negative channels out to the ProPhoto gamut (gamut.rs).
pub fn develop_raw_image_with(
    file_bytes: &[u8],
    fast_demosaic: bool,
    highlight_compression: f32,
    linear_mode: String,
    cancel_token: Option<(Arc<AtomicUsize>, usize)>,
    denoise: Option<&crate::mosaic_denoise::MosaicDenoise>,
) -> Result<DynamicImage> {
    develop_raw_image_with_draft(
        file_bytes,
        fast_demosaic,
        highlight_compression,
        linear_mode,
        cancel_token,
        denoise,
        None,
        crate::hot_pixels::DeadTest::default(),
        0,
        crate::black_bias::BlackLevels::default(),
        crate::highlight_reconstruction::HighlightRebuild::default(),
    )
}

/// Receives the draft of a full development (`develop_raw_image_with_draft`):
/// the oriented image and the size the full development will have. The full
/// development starts when it returns.
pub struct DraftSink(pub Box<dyn Fn(DynamicImage, (u32, u32)) + Send + Sync>);

impl std::fmt::Debug for DraftSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DraftSink")
    }
}

/// `develop_raw_image_with`; a full development of an RGB mosaic with a
/// `draft` sink first hands it a draft made from the same prepared mosaic
/// (Bayer half-size superpixels, X-Trans 3 x 3 bins), as soon as the
/// mosaic is decoded and its highlights rebuilt, then demosaics. One decode
/// instead of a separate fast decode beside the full one, which competed
/// with it for the cores and decoded the file twice. The full development
/// is the same with or without a sink, and starts once the sink returns.
/// `dead_test` picks the test for dead photosites (hot_pixels.rs) and
/// `black` the black handling (black_bias.rs) and `highlights` how clipped
/// colours are rebuilt (highlight_reconstruction.rs; the recipe's
/// `renderVersion` decides all three), `frame_version` the frame
/// (source_frame.rs; the recipe's `sourceFrameVersion`).
#[allow(clippy::too_many_arguments)]
pub fn develop_raw_image_with_draft(
    file_bytes: &[u8],
    fast_demosaic: bool,
    highlight_compression: f32,
    linear_mode: String,
    cancel_token: Option<(Arc<AtomicUsize>, usize)>,
    denoise: Option<&crate::mosaic_denoise::MosaicDenoise>,
    draft: Option<&DraftSink>,
    dead_test: crate::hot_pixels::DeadTest,
    frame_version: u32,
    black: crate::black_bias::BlackLevels,
    highlights: crate::highlight_reconstruction::HighlightRebuild,
) -> Result<DynamicImage> {
    let (developed_image, orientation) = develop_internal(
        file_bytes,
        fast_demosaic,
        highlight_compression,
        linear_mode,
        cancel_token,
        denoise,
        draft,
        dead_test,
        frame_version,
        black,
        highlights,
    )?;
    #[cfg(feature = "performance-profiling")]
    let _orientation_stage = crate::performance::PerformanceStage::new("raw_orientation");
    Ok(apply_orientation(developed_image, orientation))
}

fn is_linear_raw_format(raw_image: &RawImage) -> bool {
    matches!(
        raw_image.photometric,
        RawPhotometricInterpretation::LinearRaw
    )
}

#[inline]
fn srgb_to_linear(value: f32) -> f32 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(3.0)
    }
}

#[inline]
fn smootherstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

#[inline]
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[inline]
fn recover_clipped_pixel(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let max_c = r.max(g).max(b);

    if max_c <= 0.50 {
        return (r, g, b);
    }

    let mut cur_r = r;
    let mut cur_g = g;
    let mut cur_b = b;

    let outer_blend = smootherstep(0.50, 1.5, max_c);

    let magenta = (cur_r.min(cur_b) - cur_g).max(0.0);
    if magenta > 0.0 {
        let target_g = cur_r.min(cur_b) * 0.80 + ((cur_r + cur_b) * 0.5) * 0.20;
        let correction = (target_g - cur_g).max(0.0);
        cur_g += correction * outer_blend;
    }

    let residual = (cur_r.min(cur_b) - cur_g).max(0.0);
    if residual > 0.0 {
        cur_g += residual * outer_blend;
    }

    let new_max = cur_r.max(cur_g).max(cur_b);
    let min_c = cur_r.min(cur_g).min(cur_b);

    let knee = smoothstep(0.50, 1.5, new_max);

    if knee > 0.0 {
        let neutrality = (min_c / new_max.max(1e-5)).clamp(0.0, 1.0);

        let core_burn = smoothstep(0.60, 3.0, new_max);

        let desat = (knee * (neutrality * 0.85 + core_burn * 0.15)).clamp(0.0, 1.0);
        let smooth_desat = desat * desat * (3.0 - 2.0 * desat);

        let neutral_value = min_c + (new_max - min_c) * 1.0;

        cur_r = cur_r * (1.0 - smooth_desat) + neutral_value * smooth_desat;
        cur_g = cur_g * (1.0 - smooth_desat) + neutral_value * smooth_desat;
        cur_b = cur_b * (1.0 - smooth_desat) + neutral_value * smooth_desat;
    }

    (cur_r, cur_g, cur_b)
}

#[allow(clippy::too_many_arguments)]
fn develop_internal(
    file_bytes: &[u8],
    fast_demosaic: bool,
    _highlight_compression: f32,
    linear_mode: String,
    cancel_token: Option<(Arc<AtomicUsize>, usize)>,
    denoise: Option<&crate::mosaic_denoise::MosaicDenoise>,
    draft: Option<&DraftSink>,
    dead_test: crate::hot_pixels::DeadTest,
    frame_version: u32,
    black: crate::black_bias::BlackLevels,
    highlights: crate::highlight_reconstruction::HighlightRebuild,
) -> Result<(DynamicImage, Orientation)> {
    let check_cancel = || -> Result<()> {
        if let Some((tracker, generation)) = &cancel_token
            && tracker.load(Ordering::SeqCst) != *generation
        {
            return Err(anyhow!("Load cancelled"));
        }
        Ok(())
    };
    let cancelled = || check_cancel().is_err();

    check_cancel()?;

    let source = RawSource::new_from_slice(file_bytes);
    let decoder = rawler::get_decoder(&source)?;

    check_cancel()?;
    let mut raw_image: RawImage = {
        #[cfg(feature = "performance-profiling")]
        let _stage = crate::performance::PerformanceStage::new("raw_decode");
        decoder.raw_image(&source, &RawDecodeParams::default(), false)?
    };
    check_cancel()?;

    let Prepared {
        orientation,
        gain_table_map_bytes,
        ..
    } = prepare_raw_image(
        decoder.as_ref(),
        &source,
        &mut raw_image,
        file_bytes,
        frame_version,
    )?;
    let is_linear_format = is_linear_raw_format(&raw_image);
    let superccd = superccd_layout(&raw_image, file_bytes);
    let cfa_opcodes = read_dng_opcode_list(decoder.as_ref(), rawler::tags::DngTag::OpcodeList2);
    // A FixVignetteRadial in OpcodeList2 (Adobe DNG Converter files of
    // Fujifilm, Sony, Canon R bodies) is one gain per pixel for all colours,
    // so it runs with the stage-3 opcodes, ahead of them; only gain maps are
    // applied to the mosaic. Ignoring it left those DNGs 0.3-0.8 EV darker
    // towards the corners than Camera Raw.
    let dng_opcodes: Vec<crate::dng_opcodes::Opcode> = cfa_opcodes
        .iter()
        .filter(|opcode| matches!(opcode, crate::dng_opcodes::Opcode::FixVignetteRadial(_)))
        .cloned()
        .chain(read_dng_opcode_list(
            decoder.as_ref(),
            rawler::tags::DngTag::OpcodeList3,
        ))
        .collect();
    // MapPolynomials in OpcodeList2 of three-colour linear DNGs: Lightroom's
    // panoramas of integer raws store sRGB-encoded JPEG XL samples and undo
    // the encoding with one polynomial per colour (they came out pale and
    // pink). Integer samples become floats in the same units, so the
    // linearised shadows keep their precision.
    if is_linear_format
        && raw_image.cpp == 3
        && cfa_opcodes
            .iter()
            .any(|opcode| matches!(opcode, crate::dng_opcodes::Opcode::MapPolynomial(_)))
    {
        if let rawler::RawImageData::Integer(samples) = &raw_image.data {
            let floats: Vec<f32> = samples.par_iter().map(|&v| v as f32).collect();
            raw_image.data = rawler::RawImageData::Float(floats);
        }
        let black: Vec<f32> = raw_image.blacklevel.as_vec();
        let white: Vec<f32> = raw_image.whitelevel.as_vec();
        let width = raw_image.width;
        if let rawler::RawImageData::Float(data) = &mut raw_image.data {
            let applied = crate::dng_opcodes::apply_map_polynomials(
                &cfa_opcodes,
                data,
                width,
                3,
                &black,
                &white,
            );
            log::info!("DNG OpcodeList2: {applied} MapPolynomial(s) applied");
        }
    }
    // Camera Raw applies the map by default (ProfileGainTableMap 100) with
    // the file's embedded profile: ProRAW, Pixel 8 Pro and Galaxy DNGs.
    let gain_table_map = gain_table_map_bytes
        .as_deref()
        .and_then(crate::dng_opcodes::parse_gain_table_map);

    let (apply_ungamma, apply_calibration) = match linear_mode.as_str() {
        "gamma" => (true, true),
        "skip_calib" => (false, false),
        "gamma_skip_calib" => (true, false),
        _ => (false, true),
    };

    if let Some(margin) = black.border_margin() {
        use_masked_black(&mut raw_image, margin);
    }
    // Canon sRAW/mRAW is YCbCr that rawler converts to RGB which is already
    // black-subtracted; rawler still applies the sensor black (~512), which
    // crushed shadows and tinted them teal (5D Mark IV mRAW, 2026-10-02).
    if is_canon_sraw(&raw_image, file_bytes) {
        raw_image.blacklevel = rawler::rawimage::BlackLevel::zero(1, 1, 3);
    }
    let original_white_level = raw_image
        .whitelevel
        .0
        .first()
        .cloned()
        .unwrap_or(u16::MAX as u32) as f32;
    let original_black_level = raw_image
        .blacklevel
        .levels
        .first()
        .map(|r| r.as_f32())
        .unwrap_or(0.0);

    log::debug!(
        "RAW levels for {} {}: black {:?}, white {:?}",
        raw_image.clean_make,
        raw_image.clean_model,
        raw_image.blacklevel.levels,
        raw_image.whitelevel.0
    );
    for level in raw_image.whitelevel.0.iter_mut() {
        *level = u32::MAX;
    }

    // Clipped sensor channels are rebuilt on the mosaic, before demosaicing
    // (highlight_reconstruction.rs). The clipping levels are measured in the
    // decoder's units first; the mosaic is then normalised and rebuilt.
    let is_rgb_mosaic = raw_image.cpp == 1
        && matches!(&raw_image.photometric, RawPhotometricInterpretation::Cfa(config) if config.cfa.is_rgb());
    let active = raw_image
        .active_area
        .map(|area| (area.p.x, area.p.y, area.d.w, area.d.h))
        .unwrap_or((0, 0, raw_image.width, raw_image.height));
    // Hot and dead photosites (hot_pixels.rs), before anything reads the
    // mosaic's levels.
    if is_rgb_mosaic {
        let range = (original_white_level - original_black_level).max(1.0);
        let wb = raw_image.wb_coeffs;
        let width = raw_image.width;
        let fixed = match (&raw_image.photometric, &mut raw_image.data) {
            (RawPhotometricInterpretation::Cfa(config), rawler::RawImageData::Integer(samples)) => {
                crate::hot_pixels::suppress(
                    samples,
                    width,
                    active,
                    &config.cfa,
                    wb,
                    dead_test,
                    |v: u16| (v as f32 - original_black_level) / range,
                    |l: f32| {
                        (l * range + original_black_level)
                            .round()
                            .clamp(0.0, u16::MAX as f32) as u16
                    },
                )
            }
            (RawPhotometricInterpretation::Cfa(config), rawler::RawImageData::Float(samples)) => {
                crate::hot_pixels::suppress(
                    samples,
                    width,
                    active,
                    &config.cfa,
                    wb,
                    dead_test,
                    |v: f32| (v - original_black_level) / range,
                    |l: f32| l * range + original_black_level,
                )
            }
            _ => 0,
        };
        if fixed > 0 {
            log::info!("Hot/dead pixels replaced: {fixed}");
        }
    }
    // Mosaic denoising (mosaic_denoise.rs), on clean photosites and before
    // highlight reconstruction reads the levels.
    if let Some(denoise) = denoise
        && is_rgb_mosaic
        && !fast_demosaic
        && superccd.is_none()
    {
        let range = (original_white_level - original_black_level).max(1.0);
        let width = raw_image.width;
        let result = match (&raw_image.photometric, &mut raw_image.data) {
            (RawPhotometricInterpretation::Cfa(config), rawler::RawImageData::Integer(samples)) => {
                crate::mosaic_denoise::bayer_pattern(&config.cfa, active.0, active.1).map(
                    |pattern| {
                        crate::mosaic_denoise::apply(
                            denoise,
                            samples,
                            width,
                            active,
                            pattern,
                            |v: u16| (v as f32 - original_black_level) / range,
                            |l: f32| {
                                (l * range + original_black_level)
                                    .round()
                                    .clamp(0.0, u16::MAX as f32)
                                    as u16
                            },
                        )
                    },
                )
            }
            (RawPhotometricInterpretation::Cfa(config), rawler::RawImageData::Float(samples)) => {
                crate::mosaic_denoise::bayer_pattern(&config.cfa, active.0, active.1).map(
                    |pattern| {
                        crate::mosaic_denoise::apply(
                            denoise,
                            samples,
                            width,
                            active,
                            pattern,
                            |v: f32| (v - original_black_level) / range,
                            |l: f32| l * range + original_black_level,
                        )
                    },
                )
            }
            _ => None,
        };
        match result {
            Some(Ok(())) => log::info!("Mosaic denoised (amount {:.2})", denoise.amount),
            Some(Err(e)) => return Err(e.context("Mosaic denoising failed")),
            None => log::info!("Mosaic denoising skipped: not a 2 x 2 Bayer mosaic"),
        }
    }
    // Signed means of the mosaic's colours before it is clipped at black
    // (black_bias.rs); not for drafts or fast developments, nor beside CFA
    // gain maps (they change the mosaic after the clip).
    let shadow_means = (black.unbiased()
        && is_rgb_mosaic
        && !fast_demosaic
        && superccd.is_none()
        && cfa_opcodes.is_empty())
    .then(|| {
        // The mosaic's units (develop_rgb_mosaic: `apply_scaling` times
        // `rescale_factor`), sensor white at 1.
        let to_white = (u32::MAX as f32 - original_black_level)
            / (original_white_level - original_black_level).max(1.0);
        shadow_means_of(&raw_image, to_white)
    })
    .flatten();
    let clip_levels = if is_rgb_mosaic {
        match (&raw_image.photometric, &raw_image.data) {
            (RawPhotometricInterpretation::Cfa(config), rawler::RawImageData::Integer(samples)) => {
                crate::highlight_reconstruction::detect_clip_levels(
                    samples,
                    raw_image.width,
                    active,
                    &config.cfa,
                    original_white_level,
                )
            }
            (RawPhotometricInterpretation::Cfa(config), rawler::RawImageData::Float(samples)) => {
                crate::highlight_reconstruction::detect_clip_levels(
                    samples,
                    raw_image.width,
                    active,
                    &config.cfa,
                    original_white_level,
                )
            }
            _ => None,
        }
    } else {
        None
    };
    // Sony stores the sensor's linear limit in the encrypted SR2 directory
    // (WhiteLevel 15360 on 14-bit bodies) below the SubIFD's 16380; values
    // above it are already saturating, so a green sky there rendered purple
    // (A7R IV, 30° off Camera Raw and Capture One). They count as clipped and
    // are rebuilt; the normalisation keeps the decoder's white.
    let clip_levels = match sony_linear_white(decoder.as_ref(), file_bytes, original_white_level) {
        Some(white) if is_rgb_mosaic => Some(crate::highlight_reconstruction::ClipLevels {
            thresholds: std::array::from_fn(|c| {
                Some(
                    clip_levels
                        .as_ref()
                        .and_then(|levels| levels.thresholds[c])
                        .map_or(white, |level| level.min(white)),
                )
            }),
        }),
        _ => clip_levels,
    };
    let mut reconstructed = None;

    check_cancel()?;
    // OpcodeList2 (lens shading per CFA colour, mostly phones) acts on the
    // normalised mosaic, as does the reconstruction. Scaling here leaves
    // black 0 and white 1, so rawler's own Rescale step becomes a no-op.
    if (!cfa_opcodes.is_empty() || clip_levels.is_some())
        && matches!(raw_image.photometric, RawPhotometricInterpretation::Cfa(_))
        && raw_image.cpp == 1
    {
        raw_image.apply_scaling()?;
        let width = raw_image.width;
        let height = raw_image.height;
        let wb_coeffs = raw_image.wb_coeffs;
        if let (RawPhotometricInterpretation::Cfa(config), rawler::RawImageData::Float(data)) =
            (&raw_image.photometric, &mut raw_image.data)
        {
            if let Some(levels) = &clip_levels {
                #[cfg(feature = "performance-profiling")]
                let _stage = crate::performance::PerformanceStage::new("raw_highlights");
                let started = std::time::Instant::now();
                let scale = |level: f32| {
                    (level - original_black_level) / (u32::MAX as f32 - original_black_level)
                };
                let scaled = crate::highlight_reconstruction::ClipLevels {
                    thresholds: levels.thresholds.map(|level| level.map(scale)),
                };
                reconstructed = crate::highlight_reconstruction::reconstruct_clipped_mosaic(
                    data,
                    width,
                    height,
                    active,
                    &config.cfa,
                    wb_coeffs,
                    &scaled,
                    highlights,
                );
                if let Some(result) = &reconstructed {
                    // Chrominance relative to the sensor white.
                    let to_white = (u32::MAX as f32 - original_black_level)
                        / (original_white_level - original_black_level).max(1.0);
                    log::info!(
                        "Highlight reconstruction: {:.1}% of samples clipped (levels {:?} of white {}), {} raised, chrominance {:?}, {:.0} ms",
                        result.clipped_fraction * 100.0,
                        levels.thresholds.map(|level| level.map(|v| v.round())),
                        original_white_level,
                        result.raised,
                        result
                            .global_chrominance
                            .map(|c| (c * to_white * 1000.0).round() / 1000.0),
                        started.elapsed().as_secs_f64() * 1000.0
                    );
                }
            }
            if !cfa_opcodes.is_empty() {
                let applied =
                    crate::dng_opcodes::apply_cfa_gain_maps(&cfa_opcodes, data, width, active);
                log::info!("Applied {applied} DNG CFA gain map(s)");
            }
        }
    }
    // With rebuilt channels the inherited desaturation of bright pixels is
    // neither needed nor wanted (it would grey the recovered sky again).
    // Floating-point linear DNGs (HDR and other merges) hold real values
    // above white, never clipped ones.
    let float_linear = is_linear_format && matches!(raw_image.data, rawler::RawImageData::Float(_));
    let desaturate_clipped = reconstructed.is_none() && !float_linear;

    // Colours beyond sRGB keep their negative channels; the normalisation
    // limits them to the ProPhoto gamut.
    let mut developer = RawDevelop {
        keep_negative: true,
        ..RawDevelop::default()
    };
    let calibrated = !is_linear_format || apply_calibration;
    let lens_context =
        (!dng_opcodes.is_empty()).then(|| DngLensContext::new(&raw_image, calibrated));

    // rawler crops the active area only while demosaicing, and its default
    // crop then scales by the ratio of the full buffer to the active area:
    // Sony lossless-compressed Medium/Small files (YCbCr, tile-padded
    // buffers) came out 1.1× too large with a green strip of padding at the
    // right and bottom. Files that are not demosaiced are cropped here.
    let undemosaiced_frame = is_linear_format
        .then(|| undemosaiced_frame(raw_image.dim(), raw_image.active_area, raw_image.crop_area));
    if is_linear_format {
        developer.steps.retain(|&step| {
            step != ProcessingStep::SRgb
                && step != ProcessingStep::Demosaic
                && step != ProcessingStep::CropActiveArea
                && step != ProcessingStep::CropDefault
                && (apply_calibration || step != ProcessingStep::Calibrate)
        });
    } else if fast_demosaic {
        developer.demosaic_algorithm = DemosaicAlgorithm::Speed;
        developer.steps.retain(|&step| step != ProcessingStep::SRgb);
    } else {
        developer.steps.retain(|&step| step != ProcessingStep::SRgb);
    }

    let denominator = (original_white_level - original_black_level).max(1.0);
    let rescale_factor = (u32::MAX as f32 - original_black_level) / denominator;

    let baseline_exposure = dng_baseline_exposure(decoder.as_ref());
    // Normalisation, DNG lens opcodes and gain table maps: the same for
    // the draft and the full development.
    let finish = |mut developed_intermediate: Intermediate| -> Result<DynamicImage> {
        let safe_highlight_compression = 1000.0;

        // Fast (draft) decodes keep values above white too: rebuilt highlights
        // must react to Exposure and Highlights the same way in the draft as in
        // the full decode.
        let clamp_limit = safe_highlight_compression;
        let low = -clamp_limit;

        check_cancel()?;

        #[cfg(feature = "performance-profiling")]
        let normalization_stage = crate::performance::PerformanceStage::new("raw_normalize");
        match &mut developed_intermediate {
            Intermediate::Monochrome(pixels) => {
                pixels
                    .data
                    .par_iter_mut()
                    .with_min_len(16_384)
                    .for_each(|p| {
                        let mut linear_val = *p * rescale_factor;
                        if is_linear_format && apply_ungamma {
                            linear_val = srgb_to_linear(linear_val.max(0.0));
                        }
                        *p = linear_val.clamp(0.0, clamp_limit);
                    });
            }
            Intermediate::ThreeColor(pixels) => {
                pixels
                    .data
                    .par_iter_mut()
                    .with_min_len(16_384)
                    .for_each(|p| {
                        let [mut r, mut g, mut b] = crate::gamut::floor_to_prophoto(
                            [p[0], p[1], p[2]].map(|v| v * rescale_factor),
                        );

                        if is_linear_format && apply_ungamma {
                            r = srgb_to_linear(r.max(0.0));
                            g = srgb_to_linear(g.max(0.0));
                            b = srgb_to_linear(b.max(0.0));
                        }

                        let (rec_r, rec_g, rec_b) = if desaturate_clipped {
                            recover_clipped_pixel(r, g, b)
                        } else {
                            (r, g, b)
                        };

                        p[0] = rec_r.clamp(low, clamp_limit);
                        p[1] = rec_g.clamp(low, clamp_limit);
                        p[2] = rec_b.clamp(low, clamp_limit);
                    });
            }
            Intermediate::FourColor(pixels) => {
                pixels
                    .data
                    .par_iter_mut()
                    .with_min_len(16_384)
                    .for_each(|p| {
                        p.iter_mut().for_each(|c| {
                            let mut linear_val = *c * rescale_factor;
                            if is_linear_format && apply_ungamma {
                                linear_val = srgb_to_linear(linear_val.max(0.0));
                            }
                            *c = linear_val.clamp(0.0, clamp_limit);
                        });
                    });
            }
        }

        #[cfg(feature = "performance-profiling")]
        drop(normalization_stage);
        check_cancel()?;

        // Lens corrections embedded in the DNG act on true sensor values, so they
        // run after clipped-highlight recovery.
        if let (Some(context), Intermediate::ThreeColor(pixels)) =
            (&lens_context, &mut developed_intermediate)
        {
            let dim = pixels.dim();
            if let Some(frame) = context.frame(dim.w, dim.h) {
                let applied = crate::dng_opcodes::apply_opcodes(
                    &dng_opcodes,
                    &mut pixels.data,
                    dim.w,
                    dim.h,
                    frame,
                    context.camera_to_output,
                );
                log::info!("Applied {applied} DNG lens opcode(s)");
            } else {
                log::warn!(
                    "DNG lens opcodes skipped: developed size does not match the stage-3 frame"
                );
            }
        }

        // The local tone map (DNG 1.6 ProfileGainTableMap) of ProRAW, Pixel 8
        // Pro and Galaxy DNGs. Files with an embedded tone curve render with it
        // (baseline_exposure.rs).
        if let (Some(map), Intermediate::ThreeColor(pixels)) =
            (&gain_table_map, &mut developed_intermediate)
        {
            let dim = pixels.dim();
            crate::dng_opcodes::apply_gain_table_map(
                map,
                &mut pixels.data,
                dim.w,
                dim.h,
                baseline_exposure,
            );
            log::info!(
                "Applied DNG ProfileGainTableMap ({}x{}x{})",
                map.points_v,
                map.points_h,
                map.table_points
            );
        }
        intermediate_into_rgb(developed_intermediate)
    };

    // The draft develops first, from a copy of the prepared mosaic, and the
    // full development starts once it is handed over. Beside the full
    // development (its own 4 threads next to the demosaicing's) it was
    // starved of the cores: a 24 MP CR3's 1.5 MP draft came 735 instead of
    // 480 ms after the start on a 6-core Ryzen, for 40-90 ms more until the
    // full development (notes: benchmark-results/linux-first-frame-2026-10-20b).
    if let Some(sink) = draft
        && !fast_demosaic
        && !is_linear_format
        && superccd.is_none()
    {
        check_cancel()?;
        #[cfg(feature = "performance-profiling")]
        let _stage = crate::performance::PerformanceStage::new("raw_draft");
        let frame = raw_image
            .crop_area
            .filter(|crop| !crop.is_empty())
            .map(|crop| crop.d)
            .or(raw_image.active_area.map(|area| area.d))
            .unwrap_or_else(|| raw_image.dim());
        let mut draft_raw = raw_image.clone();
        let mut draft_developer = RawDevelop {
            keep_negative: true,
            ..RawDevelop::default()
        };
        draft_developer.demosaic_algorithm = DemosaicAlgorithm::Speed;
        draft_developer
            .steps
            .retain(|&step| step != ProcessingStep::SRgb);
        let developed = develop_rgb_mosaic(&mut draft_raw, rescale_factor, true, &cancelled, None)
            .and_then(|own| match own {
                Some(intermediate) => Ok(intermediate),
                None => Ok(draft_developer.develop_intermediate(&draft_raw)?),
            })
            .and_then(|intermediate| {
                drop(draft_raw);
                finish(intermediate)
            });
        match developed {
            Ok(image) => {
                let image = apply_orientation(image, orientation);
                // Orientation may turn the frame.
                let (w, h) = (frame.w as u32, frame.h as u32);
                let full = if (image.width() > image.height()) == (w > h) {
                    (w, h)
                } else {
                    (h, w)
                };
                (sink.0)(image, full);
            }
            Err(e) => log::debug!("No draft: {e}"),
        }
    }

    check_cancel()?;
    let mut developed_intermediate = {
        #[cfg(test)]
        FULL_DEVELOPMENTS.with(|n| n.set(n.get() + 1));
        #[cfg(feature = "performance-profiling")]
        let _stage = crate::performance::PerformanceStage::new("raw_develop");
        // Fast developments run in parallel thumbnail workers, which would
        // keep immediate purging on almost continuously.
        let _purge = (!fast_demosaic).then(crate::allocator::ImmediatePurge::begin);
        // Fast developments of X-Trans mosaics bin 3 x 3 blocks (a third of
        // the size, like Bayer's half-size superpixels): rawler's Speed
        // demosaic keeps them at full size, which made the first frame of
        // a never-opened 40 MP RAF wait ~0.45 s for it.
        let own = if is_linear_format {
            None
        } else {
            develop_rgb_mosaic(
                &mut raw_image,
                rescale_factor,
                fast_demosaic,
                &cancelled,
                shadow_means.as_ref(),
            )?
        };
        match own {
            Some(intermediate) => intermediate,
            None if is_linear_format => developer.develop_linear_in_place(&mut raw_image)?,
            None => developer.develop_intermediate(&raw_image)?,
        }
    };

    drop(raw_image);

    if let Some(Some(frame)) = undemosaiced_frame {
        check_cancel()?;
        developed_intermediate = crop_intermediate(developed_intermediate, frame);
    }

    if let Some(layout) = superccd {
        check_cancel()?;
        developed_intermediate = undo_superccd_layout(developed_intermediate, layout)?;
    }

    Ok((finish(developed_intermediate)?, orientation))
}

#[cfg(test)]
thread_local! {
    /// Full developments started on this thread (`draft_tests`).
    static FULL_DEVELOPMENTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// What `prepare_raw_image` resolved besides the changes to the raw image.
pub(crate) struct Prepared {
    pub metadata: rawler::decoders::RawMetadata,
    pub orientation: Orientation,
    pub iso: Option<u32>,
    pub gain_table_map_bytes: Option<Vec<u8>>,
}

/// The decoded raw image as the development uses it: orientation, colour
/// pattern fixes, linearisation, default crops, white balance and colour
/// matrices, black levels, and the areas of frame version `frame_version`
/// (source_frame.rs). Shared by `develop_internal` and the decode
/// fingerprint (fingerprint.rs), so both see the same facts.
pub(crate) fn prepare_raw_image(
    decoder: &dyn rawler::decoders::Decoder,
    source: &RawSource,
    raw_image: &mut RawImage,
    file_bytes: &[u8],
    frame_version: u32,
) -> Result<Prepared> {
    crate::source_frame::apply(raw_image, decoder.format_hint(), frame_version);
    let metadata = decoder.raw_metadata(source, &RawDecodeParams::default())?;
    // Canon CRW files keep their orientation in the CIFF ImageSpec record,
    // which rawler does not read.
    let orientation = crate::capture_info::phase_one_orientation(file_bytes)
        .or(metadata.exif.orientation)
        .or_else(|| crate::capture_info::crw_orientation(file_bytes))
        .map(Orientation::from_u16)
        .unwrap_or(Orientation::Normal);

    use_mode_color_pattern(raw_image);
    let iso = metadata
        .exif
        .iso_speed_ratings
        .map(u32::from)
        .filter(|iso| *iso > 0)
        .or_else(|| crate::capture_info::crw_iso(file_bytes));
    // DNGs of the 300D (Adobe DNG Converter) carry the curve as their
    // LinearizationTable, which rawler applies already.
    if decoder.format_hint() != rawler::decoders::FormatHint::DNG {
        linearize_canon_300d(raw_image, iso);
    }
    apply_aspect_crop(raw_image, file_bytes, decoder.format_hint());
    if decoder.format_hint() == rawler::decoders::FormatHint::DNG {
        apply_dng_user_crop(raw_image, decoder);
    }
    let (gain_table_map_bytes, _, _) = prepare_color(decoder, raw_image, file_bytes, None);
    log::info!("White balance multipliers {:?}", raw_image.wb_coeffs);
    reconcile_black_level_count(raw_image);
    Ok(Prepared {
        metadata,
        orientation,
        iso,
        gain_table_map_bytes,
    })
}

/// How `develop_raw_image` frames a file, read from its metadata alone (no
/// samples decoded): see `developed_frame`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DevelopedFrame {
    /// The developed image's size, oriented: the default crop inside the
    /// active area, in-camera aspect ratios and DNG user crops included, as
    /// `prepare_raw_image` and the development frame it. `None` for
    /// SuperCCD layouts (turned by the development).
    pub size: Option<(u32, u32)>,
    /// The EXIF orientation (1–8) the development applies.
    pub orientation: u16,
}

/// The frame of the RAW in `source` as the development of frame version
/// `frame_version` (source_frame.rs) will give it. A still shown before the
/// development (the camera's embedded preview) takes this placement so the
/// developed frame replaces it in place. `None` for files rawler cannot
/// read.
pub fn developed_frame(source: &RawSource, frame_version: u32) -> Option<DevelopedFrame> {
    let file_bytes = source.buf();
    let decoder = rawler::get_decoder(source).ok()?;
    let params = RawDecodeParams::default();
    let mut raw_image = decoder.raw_image(source, &params, true).ok()?;
    let orientation = crate::capture_info::phase_one_orientation(file_bytes)
        .or_else(|| decoder.raw_metadata(source, &params).ok()?.exif.orientation)
        .or_else(|| crate::capture_info::crw_orientation(file_bytes))
        .filter(|o| (1..=8).contains(o))
        .unwrap_or(1);
    if superccd_layout(&raw_image, file_bytes).is_some() {
        return Some(DevelopedFrame {
            size: None,
            orientation,
        });
    }
    let (_, frame) = developed_areas(&mut raw_image, file_bytes, decoder.as_ref(), frame_version);
    let (w, h) = (frame.d.w as u32, frame.d.h as u32);
    Some(DevelopedFrame {
        size: (w > 0 && h > 0).then_some(if (5..=8).contains(&orientation) {
            (h, w)
        } else {
            (w, h)
        }),
        orientation,
    })
}

/// `developed_frame` of the file at `path`, memory-mapped (rawler's
/// `RawSource::new_from_slice` copies the whole file).
pub fn developed_frame_at(path: &std::path::Path, frame_version: u32) -> Option<DevelopedFrame> {
    developed_frame(&RawSource::new(path).ok()?, frame_version)
}

/// The active area (the sensor's pixels in the buffer) and the developed
/// frame within it (default, aspect and DNG user crops) of an undecoded
/// `raw_image` under frame version `frame_version`, in buffer coordinates.
pub(crate) fn developed_areas(
    raw_image: &mut rawler::RawImage,
    file_bytes: &[u8],
    decoder: &dyn rawler::decoders::Decoder,
    frame_version: u32,
) -> (rawler::imgop::Rect, rawler::imgop::Rect) {
    use rawler::imgop::{Point, Rect};
    crate::source_frame::apply(raw_image, decoder.format_hint(), frame_version);
    apply_aspect_crop(raw_image, file_bytes, decoder.format_hint());
    if decoder.format_hint() == rawler::decoders::FormatHint::DNG {
        apply_dng_user_crop(raw_image, decoder);
    }
    let full = Rect::new(Point::new(0, 0), raw_image.dim());
    let active = raw_image
        .active_area
        .map(|area| area.intersection(&full))
        .filter(|area| !area.is_empty())
        .unwrap_or(full);
    let frame = raw_image
        .crop_area
        .map(|crop| crop.intersection(&active))
        .filter(|crop| !crop.is_empty())
        .unwrap_or(active);
    (active, frame)
}

/// The sizes (width, height; unrotated) of the active area and of the
/// developed frame of the RAW in `source`, as frame version 0 develops it
/// (the camera lens data read here, Sony, Fujifilm and Samsung, describe
/// files no frame version changes). `None` for files rawler cannot read and
/// Super CCD layouts.
pub fn sensor_and_frame(source: &RawSource) -> Option<([usize; 2], [usize; 2])> {
    let decoder = rawler::get_decoder(source).ok()?;
    let mut raw_image = decoder
        .raw_image(source, &RawDecodeParams::default(), true)
        .ok()?;
    if superccd_layout(&raw_image, source.buf()).is_some() {
        return None;
    }
    let (active, frame) = developed_areas(&mut raw_image, source.buf(), decoder.as_ref(), 0);
    Some(([active.d.w, active.d.h], [frame.d.w, frame.d.h]))
}

/// The part of a decoded buffer that rawler does not demosaic (linear and
/// multi-channel files) which the developed image should keep: the active
/// area, and the default crop where it lies inside it. Both rectangles are
/// in buffer coordinates (rawler's DNG decoder makes the crop absolute, the
/// ARW decoder reads absolute values). `None` when the whole buffer is the
/// image.
fn undemosaiced_frame(
    dim: rawler::imgop::Dim2,
    active_area: Option<rawler::imgop::Rect>,
    crop_area: Option<rawler::imgop::Rect>,
) -> Option<rawler::imgop::Rect> {
    use rawler::imgop::{Point, Rect};
    let full = Rect::new(Point::new(0, 0), dim);
    let active = active_area
        .map(|area| area.intersection(&full))
        .filter(|area| !area.is_empty())
        .unwrap_or(full);
    let frame = crop_area
        .map(|crop| crop.intersection(&active))
        .filter(|crop| !crop.is_empty())
        .unwrap_or(active);
    (frame.d != dim).then_some(frame)
}

/// The default crop relative to the active area, as rawler's demosaicing
/// path and `undemosaiced_frame` leave it; `None` when they do not overlap.
fn crop_within_active(
    crop: rawler::imgop::Rect,
    active: Option<rawler::imgop::Rect>,
) -> Option<rawler::imgop::Rect> {
    match active {
        Some(area) => {
            let inside = crop.intersection(&area);
            (!inside.is_empty()).then(|| inside.adapt(&area))
        }
        None => Some(crop),
    }
}

fn crop_intermediate(intermediate: Intermediate, frame: rawler::imgop::Rect) -> Intermediate {
    let dim = intermediate.dim();
    if frame.is_empty() || frame.p.x + frame.d.w > dim.w || frame.p.y + frame.d.h > dim.h {
        log::warn!(
            "Frame {:?} does not fit the developed {}x{} image; kept whole",
            frame,
            dim.w,
            dim.h
        );
        return intermediate;
    }
    match intermediate {
        Intermediate::Monochrome(pixels) => Intermediate::Monochrome(pixels.crop(frame)),
        Intermediate::ThreeColor(pixels) => Intermediate::ThreeColor(pixels.crop(frame)),
        Intermediate::FourColor(pixels) => Intermediate::FourColor(pixels.crop(frame)),
    }
}

/// Camera Raw renders files it has no as-shot white for with its Auto
/// white balance (EOS D30: As Shot and Auto give identical renders). A grey
/// world over the active area's unclipped samples stands in for it.
fn gray_world_wb(raw_image: &RawImage) -> Option<[f32; 4]> {
    let RawPhotometricInterpretation::Cfa(config) = &raw_image.photometric else {
        return None;
    };
    let rawler::RawImageData::Integer(data) = &raw_image.data else {
        return None;
    };
    if raw_image.cpp != 1 || !config.cfa.is_rgb() {
        return None;
    }
    let width = raw_image.width;
    let area = raw_image.active_area.unwrap_or_else(|| {
        rawler::imgop::Rect::new(rawler::imgop::Point::new(0, 0), raw_image.dim())
    });
    let black = raw_image
        .blacklevel
        .levels
        .first()
        .map_or(0.0, |r| r.as_f32());
    let white = *raw_image.whitelevel.0.first()? as f32;
    let ceiling = black + 0.9 * (white - black);
    let mut sums = [0.0f64; 3];
    let mut counts = [0u64; 3];
    for row in (area.p.y..area.p.y + area.d.h).step_by(2) {
        for col in (area.p.x..area.p.x + area.d.w).step_by(2) {
            for (dr, dc) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
                let (r, c) = (row + dr, col + dc);
                let v = *data.get(r * width + c)? as f32;
                if v > black + 2.0 && v < ceiling {
                    let color = config.cfa.color_at(r - area.p.y, c - area.p.x).min(2);
                    sums[color] += (v - black) as f64;
                    counts[color] += 1;
                }
            }
        }
    }
    let means: Vec<f64> = (0..3)
        .map(|i| (counts[i] > 1000).then(|| sums[i] / counts[i] as f64))
        .collect::<Option<_>>()?;
    let green = means[1];
    Some([
        (green / means[0]) as f32,
        1.0,
        (green / means[2]) as f32,
        f32::NAN,
    ])
}

/// White balance multipliers with the smallest at 1, as the DNG SDK applies
/// a camera neutral. Some decoders report green below 1 (Kodak DCS Pro
/// SLR/c 0.76, PowerShot S2 IS 0.92), which rendered those files that much
/// darker than Camera Raw. Missing or invalid coefficients are left alone.
fn normalize_wb_to_smallest(wb: [f32; 4]) -> [f32; 4] {
    let used = wb.iter().copied().filter(|v| v.is_finite());
    let smallest = used.clone().fold(f32::INFINITY, f32::min);
    if wb[..3].iter().any(|v| !v.is_finite() || *v <= 0.0) || smallest <= 0.0 {
        return wb;
    }
    wb.map(|v| if v.is_finite() { v / smallest } else { v })
}

fn normalize_wb_to_largest(wb: [f32; 4]) -> [f32; 4] {
    let largest = wb
        .iter()
        .copied()
        .filter(|v| v.is_finite())
        .fold(0.0f32, f32::max);
    if wb.iter().any(|v| !v.is_finite() || *v <= 0.0) || largest <= 0.0 {
        return wb;
    }
    wb.map(|v| v / largest)
}

/// The EOS 300D's ISO 100 response flattens above raw 2480; Adobe DNG
/// Converter writes a linearization table that expands the top (raw 4000
/// to the equivalent of 5120) and renders the frame 0.37 EV darker than a
/// linear white of 4000 (identical table in both ISO 100 files checked;
/// none at ISO 1600). Fitted cubic (`canon_300d_linear`), within 0.08 raw
/// units of the table.
fn linearize_canon_300d(raw_image: &mut RawImage, iso: Option<u32>) {
    let is_300d = raw_image.make == "Canon"
        && matches!(
            raw_image.model.as_str(),
            "Canon EOS 300D DIGITAL" | "Canon EOS DIGITAL REBEL" | "Canon EOS Kiss Digital"
        );
    if !is_300d || iso != Some(100) || raw_image.cpp != 1 {
        return;
    }
    let lut: Vec<u16> = (0..=u16::MAX as u32).map(canon_300d_linear).collect();
    if let rawler::RawImageData::Integer(data) = &mut raw_image.data {
        for value in data.iter_mut() {
            *value = lut[*value as usize];
        }
        for level in raw_image.whitelevel.0.iter_mut() {
            *level = canon_300d_linear(4000) as u32;
        }
    }
}

fn canon_300d_linear(value: u32) -> u16 {
    const KNEE: f32 = 2480.0;
    const SPAN: f32 = 1520.0;
    let x = value.min(4000) as f32;
    let t = ((x - KNEE) / SPAN).max(0.0);
    let excess = if x > KNEE {
        ((960.232 * t + 151.665) * t + 7.963) * t + 0.141
    } else {
        0.0
    };
    (x + excess).round() as u16
}

pub(crate) fn is_canon_sraw(raw_image: &RawImage, file_bytes: &[u8]) -> bool {
    // CR2 files carry "CR", version 2 after the TIFF header.
    file_bytes.get(8..11) == Some(b"CR\x02".as_slice())
        && raw_image.cpp == 3
        && raw_image.clean_make == "Canon"
        && matches!(
            raw_image.photometric,
            RawPhotometricInterpretation::LinearRaw
        )
}

/// BaselineExposure plus BaselineExposureOffset of a DNG, in EV.
pub(crate) fn dng_baseline_exposure(decoder: &dyn rawler::decoders::Decoder) -> f32 {
    let Ok(Some(root)) = decoder.ifd(rawler::decoders::WellKnownIFD::Root) else {
        return 0.0;
    };
    [
        rawler::tags::DngTag::BaselineExposure,
        rawler::tags::DngTag::BaselineExposureOffset,
    ]
    .iter()
    .filter_map(|tag| root.get_entry(*tag))
    .map(|entry| entry.value.force_f32(0))
    .filter(|value| value.is_finite())
    .sum()
}

fn read_dng_raw_tag_bytes(
    decoder: &dyn rawler::decoders::Decoder,
    tag: rawler::tags::DngTag,
) -> Option<Vec<u8>> {
    let ifd = decoder
        .ifd(rawler::decoders::WellKnownIFD::VirtualDngRawTags)
        .ok()??;
    match ifd.get_entry(tag).map(|entry| &entry.value) {
        Some(rawler::formats::tiff::Value::Undefined(bytes))
        | Some(rawler::formats::tiff::Value::Byte(bytes)) => Some(bytes.clone()),
        _ => None,
    }
}

fn read_dng_opcode_list(
    decoder: &dyn rawler::decoders::Decoder,
    tag: rawler::tags::DngTag,
) -> Vec<crate::dng_opcodes::Opcode> {
    let Ok(Some(ifd)) = decoder.ifd(rawler::decoders::WellKnownIFD::VirtualDngRawTags) else {
        return Vec::new();
    };
    match ifd.get_entry(tag).map(|entry| &entry.value) {
        Some(rawler::formats::tiff::Value::Undefined(bytes))
        | Some(rawler::formats::tiff::Value::Byte(bytes)) => {
            crate::dng_opcodes::parse_opcode_list(bytes)
        }
        _ => Vec::new(),
    }
}

/// Geometry and colour context the DNG opcodes need, captured before the
/// raw image is released.
struct DngLensContext {
    stage_width: usize,
    stage_height: usize,
    crop: Option<rawler::imgop::Rect>,
    camera_to_output: Option<[[f32; 3]; 3]>,
    supported: bool,
}

impl DngLensContext {
    fn new(raw_image: &RawImage, calibrated: bool) -> Self {
        use rawler::imgop::{matrix, xyz};
        let active = raw_image.active_area;
        let (stage_width, stage_height) = active
            .map(|area| (area.d.w, area.d.h))
            .unwrap_or((raw_image.width, raw_image.height));
        // The developed image is the default crop inside the active area
        // (rawler's demosaicing path; `undemosaiced_frame` for the rest).
        let crop = raw_image
            .crop_area
            .and_then(|crop| crop_within_active(crop, active));
        let three_colour = match &raw_image.photometric {
            RawPhotometricInterpretation::Cfa(config) => config.cfa.is_rgb(),
            _ => raw_image.cpp == 3,
        };
        let found_matrix = raw_image
            .color_matrix
            .iter()
            .find(|(illuminant, _)| **illuminant == xyz::Illuminant::D65)
            .or_else(|| raw_image.color_matrix.iter().next())
            .map(|(_, m)| m.clone());
        let camera_to_output = match (calibrated, found_matrix) {
            (true, Some(flat)) if flat.len() % 3 == 0 && flat.len() <= 12 => {
                let mut xyz2cam = [[0.0f32; 3]; 4];
                for (i, value) in flat.iter().enumerate() {
                    xyz2cam[i / 3][i % 3] = *value;
                }
                let rgb2cam = matrix::normalize(matrix::multiply(&xyz2cam, &xyz::SRGB_TO_XYZ_D65));
                let cam2rgb = matrix::pseudo_inverse(rgb2cam);
                Some(std::array::from_fn(|r| {
                    std::array::from_fn(|c| cam2rgb[r][c])
                }))
            }
            _ => None,
        };
        Self {
            stage_width,
            stage_height,
            crop,
            camera_to_output,
            // Calibrated output without a matrix would mix planes unknowingly.
            supported: three_colour && (camera_to_output.is_some() || !calibrated),
        }
    }

    fn frame(&self, width: usize, height: usize) -> Option<crate::dng_opcodes::Stage3Frame> {
        if !self.supported || self.stage_width == 0 {
            return None;
        }
        let (left, top, crop_width, crop_height) = match self.crop {
            Some(crop) if !crop.is_empty() => (crop.p.x, crop.p.y, crop.d.w, crop.d.h),
            _ => (0, 0, self.stage_width, self.stage_height),
        };
        let scale = width as f64 / crop_width.max(1) as f64;
        if (height as f64 - crop_height as f64 * scale).abs() > 2.0 {
            return None;
        }
        Some(crate::dng_opcodes::Stage3Frame {
            width: self.stage_width as f64 * scale,
            height: self.stage_height as f64 * scale,
            left: left as f64 * scale,
            top: top as f64 * scale,
            scale,
        })
    }
}

fn cfa_color_count(raw_image: &RawImage) -> usize {
    match &raw_image.photometric {
        RawPhotometricInterpretation::Cfa(config) => config.cfa.unique_colors(),
        _ => 0,
    }
}

/// Files without usable as-shot white balance (older Canon CRWs such as
/// the IXUS 75: rawler finds no table and returns NaN) rendered green.
/// The camera's XYZ→camera matrix gives the daylight neutral instead.
fn fallback_white_balance_from_matrix(raw_image: &mut RawImage) {
    let wb = raw_image.wb_coeffs;
    // Balances beyond 12× between channels are misread values (Kodak
    // EasyShare Z981: red ×60.6, blue ×15.7), not lighting.
    let plausible = |v: f32| v.is_finite() && v > 0.0 && (1.0 / 12.0..=12.0).contains(&(v / wb[1]));
    if wb[1].is_finite() && wb[1] > 0.0 && wb[..3].iter().all(|v| plausible(*v)) {
        return;
    }
    // select_color_matrix left the matrix closest to daylight (XYZ→camera,
    // nine values); a daylight white through it is the camera's response to
    // neutral light, and its inverse (normalised to green) the balance.
    let Some(matrix) = raw_image.color_matrix.values().next() else {
        return;
    };
    // Three rows for RGB sensors, four for CYGM/RGBE (the fourth channel
    // must get a balance too: NaN rendered those cameras black).
    let rows = if matrix.len() >= 12 && raw_image.cpp == 1 && cfa_color_count(raw_image) == 4 {
        4
    } else if matrix.len() >= 9 {
        3
    } else {
        return;
    };
    // The white Adobe DNG Converter writes for files whose stored balance
    // it ignores (AsShotWhiteXY 0.332405, 0.340848 on the PowerShot S60/S70),
    // about 5450 K, instead of D65.
    let (x, y) = (0.332405f32, 0.340848f32);
    let white = [x / y, 1.0, (1.0 - x - y) / y];
    let camera: Vec<f32> = (0..rows)
        .map(|r| {
            matrix[3 * r] * white[0] + matrix[3 * r + 1] * white[1] + matrix[3 * r + 2] * white[2]
        })
        .collect();
    if camera.iter().any(|v| !v.is_finite() || *v <= 0.0) {
        return;
    }
    raw_image.wb_coeffs = [
        camera[1] / camera[0],
        1.0,
        camera[1] / camera[2],
        if rows == 4 {
            camera[1] / camera[3]
        } else {
            f32::NAN
        },
    ];
    log::info!(
        "No usable as-shot white balance in the file ({:?}); using the camera matrix's daylight neutral {:?}",
        &wb[..3],
        &raw_image.wb_coeffs[..3]
    );
}

/// Linear DNGs with a 2×2 black-level repeat per channel (Samsung Galaxy
/// S23+: 12 black levels for 3 white levels) made rawler skip its level
/// correction, so the image came out white. Average the repeat pattern per
/// channel.
fn reconcile_black_level_count(raw_image: &mut RawImage) {
    let cpp = raw_image.cpp;
    let black = &raw_image.blacklevel;
    let whites = raw_image.whitelevel.0.len();
    let levels = black.as_vec();
    if cpp < 2 || levels.len() == whites || levels.len() < cpp || !levels.len().is_multiple_of(cpp)
    {
        return;
    }
    let cells = levels.len() / cpp;
    let per_channel: Vec<f32> = (0..cpp)
        .map(|c| (0..cells).map(|i| levels[i * cpp + c]).sum::<f32>() / cells as f32)
        .collect();
    log::info!(
        "Black level count {} does not match {} white levels; using per-channel means {:?}",
        levels.len(),
        whites,
        per_channel
    );
    raw_image.blacklevel = rawler::rawimage::BlackLevel::new(&per_channel, 1, 1, cpp);
    if whites != cpp {
        let white = raw_image
            .whitelevel
            .0
            .first()
            .copied()
            .unwrap_or(u16::MAX as u32);
        raw_image.whitelevel = rawler::rawimage::WhiteLevel::new(vec![white; cpp]);
    }
}

/// How rawler laid out a Fujifilm SuperCCD sensor: the raw data is stored
/// rotated by 45° and rawler keeps that layout (a diamond-shaped frame);
/// `fuji_width` is the diamond's left vertex row, as in dcraw's `fuji_rotate`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SuperCcdLayout {
    fuji_width: usize,
    rotate_90cw: bool,
    /// Pixels to trim from every side of the upright frame.
    edge: usize,
}

/// The aspect ratio set in a Panasonic, Olympus or Sony camera (16:9, 3:2,
/// 1:1 on a 4:3 sensor; 16:9, 4:3, 1:1 on a 3:2 one) is recorded as a
/// frame; Camera Raw crops to it by default,
/// rawler kept the whole sensor (and showed e.g. a square FZ330 frame as
/// 4:3 with vignetted corners).
fn apply_aspect_crop(
    raw_image: &mut RawImage,
    file_bytes: &[u8],
    format: rawler::decoders::FormatHint,
) {
    use rawler::decoders::FormatHint;
    use rawler::imgop::{Dim2, Point, Rect};
    if !matches!(format, FormatHint::RW2 | FormatHint::ORF | FormatHint::ARW) {
        return;
    }
    let Some(aspect) = crate::capture_info::aspect_crop(file_bytes) else {
        return;
    };
    let base = raw_image
        .crop_area
        .or(raw_image.active_area)
        .unwrap_or_else(|| Rect::new(Point::new(0, 0), raw_image.dim()));
    let frame = match aspect {
        crate::capture_info::AspectCrop::Absolute(left, top, width, height) => {
            Rect::new(Point::new(left, top), Dim2::new(width, height))
        }
        crate::capture_info::AspectCrop::Relative(left, top, width, height) => Rect::new(
            Point::new(base.p.x + left, base.p.y + top),
            Dim2::new(width, height),
        ),
        crate::capture_info::AspectCrop::Centered(width, height) => {
            let (width, height) = (width.min(base.d.w), height.min(base.d.h));
            Rect::new(
                Point::new(
                    base.p.x + (base.d.w - width) / 2,
                    base.p.y + (base.d.h - height) / 2,
                ),
                Dim2::new(width, height),
            )
        }
    };
    let inside = frame.p.x >= base.p.x
        && frame.p.y >= base.p.y
        && frame.p.x + frame.d.w <= base.p.x + base.d.w
        && frame.p.y + frame.d.h <= base.p.y + base.d.h;
    let smaller = frame.d.w * frame.d.h < base.d.w * base.d.h;
    let sensible = frame.d.w * 2 >= base.d.w && frame.d.h * 2 >= base.d.h;
    if inside && smaller && sensible {
        log::info!(
            "Aspect-ratio crop {}x{} at {},{} (of {}x{})",
            frame.d.w,
            frame.d.h,
            frame.p.x,
            frame.p.y,
            base.d.w,
            base.d.h
        );
        raw_image.crop_area = Some(frame);
    }
}

/// DNG 1.5 DefaultUserCrop (top, left, bottom, right as fractions of the
/// default crop), which Camera Raw uses as the default crop: Adobe DNG
/// Converter stores the aspect ratio set in the camera there (a 16:9 GX9
/// RW2 became a 4:3 DNG with this crop).
fn apply_dng_user_crop(raw_image: &mut RawImage, decoder: &dyn rawler::decoders::Decoder) {
    use rawler::imgop::{Dim2, Point, Rect};
    let Ok(Some(ifd)) = decoder.ifd(rawler::decoders::WellKnownIFD::VirtualDngRawTags) else {
        return;
    };
    let Some(entry) = ifd.get_entry(rawler::tags::DngTag::DefaultUserCrop) else {
        return;
    };
    if entry.value.count() != 4 {
        return;
    }
    let [top, left, bottom, right] = std::array::from_fn(|i| entry.value.force_f32(i) as f64);
    let valid = (0.0..1.0).contains(&top)
        && (0.0..1.0).contains(&left)
        && bottom <= 1.0
        && right <= 1.0
        && bottom - top >= 0.25
        && right - left >= 0.25;
    if !valid || (top == 0.0 && left == 0.0 && bottom == 1.0 && right == 1.0) {
        return;
    }
    let base = raw_image
        .crop_area
        .or(raw_image.active_area)
        .unwrap_or_else(|| Rect::new(Point::new(0, 0), raw_image.dim()));
    let (w, h) = (base.d.w as f64, base.d.h as f64);
    let x0 = (left * w).round() as usize;
    let y0 = (top * h).round() as usize;
    let x1 = ((right * w).round() as usize).min(base.d.w);
    let y1 = ((bottom * h).round() as usize).min(base.d.h);
    let frame = Rect::new(
        Point::new(base.p.x + x0, base.p.y + y0),
        Dim2::new(x1 - x0, y1 - y0),
    );
    log::info!(
        "DNG DefaultUserCrop {}x{} at {},{} (of {}x{})",
        frame.d.w,
        frame.d.h,
        frame.p.x,
        frame.p.y,
        base.d.w,
        base.d.h
    );
    raw_image.crop_area = Some(frame);
}

/// Colour patterns rawler gets wrong, found by a sweep over all raw.pixls.us
/// files (claimed red and blue sites with equal means, i.e. both green;
/// notes: reference/camera-coverage.md): the Kodak EasyShare Z981 is GRBG, not
/// BGGR. (Olympus high-resolution ORFs are fixed in the vendored rawler,
/// third_party/rawler/RAWTONIC_PATCHES.md.)
fn use_mode_color_pattern(raw_image: &mut RawImage) {
    let pattern = if raw_image.clean_make == "Kodak" && raw_image.clean_model == "EasyShare Z981" {
        rawler::cfa::CFA::new("GRBG")
    } else {
        return;
    };
    if let RawPhotometricInterpretation::Cfa(config) = &mut raw_image.photometric
        && pattern.width > 0
        && config.cfa.name != pattern.name
    {
        log::info!(
            "Colour pattern {} instead of {} (mode '{}')",
            pattern.name,
            config.cfa.name,
            raw_image.camera.mode
        );
        config.cfa = pattern;
    }
}

/// White balance and colour matrix exactly as the developed image uses
/// them, for `develop_internal` and `image_color`. Returns the DNG's
/// ProfileGainTableMap bytes when it has one.
/// A white balance other than the camera's (`wb_override`, multipliers)
/// picks the matrices for that balance instead. Also returns the camera's
/// calibration matrices before they are blended into one.
#[allow(clippy::type_complexity)]
fn prepare_color(
    decoder: &dyn rawler::decoders::Decoder,
    raw_image: &mut RawImage,
    file_bytes: &[u8],
    wb_override: Option<[f32; 4]>,
) -> (
    Option<Vec<u8>>,
    std::collections::HashMap<rawler::imgop::xyz::Illuminant, rawler::imgop::xyz::FlatColorMatrix>,
    bool,
) {
    if raw_image.camera.find_hint("camera_raw_auto_wb")
        && let Some(wb) = gray_world_wb(raw_image)
    {
        raw_image.wb_coeffs = wb;
    }
    raw_image.wb_coeffs = normalize_wb_to_smallest(
        crate::multi_exposure::neutralize_wb_if_multiexposure(raw_image.wb_coeffs, file_bytes),
    );
    let is_dng = decoder.format_hint() == rawler::decoders::FormatHint::DNG;
    // Camera Raw applies a DNG's gain table map by default (ProRAW, Pixel 8
    // Pro, Galaxy Expert RAW); such files keep their embedded profile.
    let gain_table_map_bytes = is_dng
        .then(|| read_dng_raw_tag_bytes(decoder, rawler::tags::DngTag::ProfileGainTableMap))
        .flatten();
    let replaces_dng = crate::camera_matrices::replaces_dng_matrices(
        &raw_image.make,
        &raw_image.model,
        gain_table_map_bytes.is_some(),
    );
    if (!is_dng || replaces_dng)
        && let Some(matrices) =
            crate::camera_matrices::corrected_color_matrix(&raw_image.make, &raw_image.model)
    {
        raw_image.color_matrix = matrices;
    }
    if let Some(wb) = crate::color_calibration::unconverged_white_balance(
        &raw_image.color_matrix,
        raw_image.wb_coeffs,
    ) {
        raw_image.wb_coeffs = normalize_wb_to_smallest(wb);
    }
    if let Some(wb) = wb_override {
        raw_image.wb_coeffs = wb;
    }
    let forward_matrix =
        is_dng && !replaces_dng && crate::dng_color::apply_dng_color(decoder, raw_image);
    let calibrations = raw_image.color_matrix.clone();
    // rawler would use the D65 matrix or whichever its hash map yields
    // first (a different one per process for Leica's A + D50 DNGs).
    crate::color_calibration::select_color_matrix(&mut raw_image.color_matrix, raw_image.wb_coeffs);
    if !forward_matrix
        && let Some(adapted) =
            crate::dng_color::adapted_color_matrix(&raw_image.color_matrix, raw_image.wb_coeffs)
    {
        raw_image.color_matrix = adapted;
    }
    fallback_white_balance_from_matrix(raw_image);
    // Four-colour sensors (RGBE, CMYG): Camera Raw scales the balance so the
    // largest multiplier is 1, which renders them darker than the smallest-
    // at-1 rule of three-colour files by log2(largest/smallest): F828
    // 1.02 EV, PowerShot G1 0.64 EV, as measured against Camera Raw.
    if cfa_color_count(raw_image) == 4 {
        raw_image.wb_coeffs = normalize_wb_to_largest(raw_image.wb_coeffs);
    }
    (gain_table_map_bytes, calibrations, forward_matrix)
}

/// Colour of a RAW as `develop_internal` renders it, from its metadata
/// only: white-balance multipliers and the matrix from white-balanced
/// camera RGB to the decoded linear sRGB. Three-colour sensors only.
pub fn image_color(file_bytes: &[u8]) -> Option<ImageColor> {
    image_color_with(file_bytes, None)
}

/// `image_color` as the decode would render it at other white-balance
/// multipliers.
pub fn image_color_with(file_bytes: &[u8], wb_override: Option<[f32; 4]>) -> Option<ImageColor> {
    image_color_details(file_bytes, wb_override).map(|(colour, _)| colour)
}

/// `image_color_with`, and whether the colour comes from a DNG's forward
/// matrices (then the matrix is not adapted to the balance): what
/// `image_color_at` needs to give the colour at another balance without
/// the file (offline photographs).
pub fn image_color_details(
    file_bytes: &[u8],
    wb_override: Option<[f32; 4]>,
) -> Option<(ImageColor, bool)> {
    let source = RawSource::new_from_slice(file_bytes);
    let decoder = rawler::get_decoder(&source).ok()?;
    let mut raw_image = decoder
        .raw_image(&source, &RawDecodeParams::default(), true)
        .ok()?;
    // Its grey-world balance needs the pixels this decode skips.
    if raw_image.camera.find_hint("camera_raw_auto_wb") {
        return None;
    }
    let (_, calibrations, forward_matrix) =
        prepare_color(decoder.as_ref(), &mut raw_image, file_bytes, wb_override);
    if cfa_color_count(&raw_image) == 4 {
        return None;
    }
    let camera_to_srgb = camera_to_srgb_of(&raw_image.color_matrix)?;
    Some((
        ImageColor {
            camera_to_srgb,
            wb: raw_image.wb_coeffs,
            calibrations,
            unit_calibration: None,
        },
        forward_matrix,
    ))
}

/// White-balanced camera RGB → linear sRGB from the matrix the decode
/// selected (D65, else the only one).
fn camera_to_srgb_of(
    color_matrix: &std::collections::HashMap<
        rawler::imgop::xyz::Illuminant,
        rawler::imgop::xyz::FlatColorMatrix,
    >,
) -> Option<[[f32; 3]; 3]> {
    use rawler::imgop::matrix::{multiply, normalize, pseudo_inverse};
    use rawler::imgop::xyz::{Illuminant, SRGB_TO_XYZ_D65};
    let flat = color_matrix
        .iter()
        .find(|(illuminant, _)| **illuminant == Illuminant::D65)
        .or_else(|| color_matrix.iter().next())
        .map(|(_, matrix)| matrix.clone())
        .filter(|matrix| matrix.len() == 9)?;
    let mut xyz2cam = [[0.0f32; 3]; 3];
    for (i, value) in flat.iter().enumerate() {
        xyz2cam[i / 3][i % 3] = *value;
    }
    Some(pseudo_inverse(normalize(multiply(
        &xyz2cam,
        &SRGB_TO_XYZ_D65,
    ))))
}

/// The colour `image_color_with(bytes, Some(wb))` gives, from what
/// `image_color_details` returned for the file (its calibrations and
/// whether they are forward matrices), without the file: the matrix
/// selection and white adaptation of `prepare_color` at `wb`.
pub fn image_color_at(
    calibrations: &std::collections::HashMap<
        rawler::imgop::xyz::Illuminant,
        rawler::imgop::xyz::FlatColorMatrix,
    >,
    forward_matrix: bool,
    wb: [f32; 4],
) -> Option<ImageColor> {
    let mut color_matrix = calibrations.clone();
    crate::color_calibration::select_color_matrix(&mut color_matrix, wb);
    if !forward_matrix
        && let Some(adapted) = crate::dng_color::adapted_color_matrix(&color_matrix, wb)
    {
        color_matrix = adapted;
    }
    Some(ImageColor {
        camera_to_srgb: camera_to_srgb_of(&color_matrix)?,
        wb,
        calibrations: calibrations.clone(),
        unit_calibration: None,
    })
}

/// See `image_color`.
#[derive(Clone, Debug, PartialEq)]
pub struct ImageColor {
    /// White-balanced camera RGB → decoded linear sRGB (rows of the matrix).
    pub camera_to_srgb: [[f32; 3]; 3],
    pub wb: [f32; 4],
    /// The camera's calibration matrices (XYZ → camera) before blending.
    pub calibrations: std::collections::HashMap<
        rawler::imgop::xyz::Illuminant,
        rawler::imgop::xyz::FlatColorMatrix,
    >,
    /// The body's own calibration for Kelvin values (`unit_calibration`);
    /// set by `camera_profile::image_colour`.
    pub unit_calibration: Option<crate::color_calibration::CameraCalibration>,
}

/// Sony's linear white level from the SR2 directory (tag 0x787f), when it
/// lies clearly below the decoder's white.
fn sony_linear_white(
    decoder: &dyn rawler::decoders::Decoder,
    file_bytes: &[u8],
    decoder_white: f32,
) -> Option<f32> {
    if decoder.format_hint() != rawler::decoders::FormatHint::ARW {
        return None;
    }
    let source = RawSource::new_from_slice(file_bytes);
    let sr2 = rawler::decoders::arw::ArwDecoder::sr2_subifd(&source).ok()?;
    let value = &sr2.get_entry(0x787f)?.value;
    let white = (0..value.count())
        .filter_map(|i| value.get_f32(i).ok().flatten())
        .fold(f32::INFINITY, f32::min);
    (white.is_finite() && white > decoder_white * 0.5 && white < decoder_white * 0.995)
        .then_some(white)
}

/// Full-resolution develop of an RGB colour-filter mosaic (Bayer, X-Trans)
/// with Rawtonic's demosaicing (demosaic.rs) instead of rawler's PPG and
/// X-Trans averaging. Returns the same intermediate as rawler's Rescale,
/// Demosaic, CropActiveArea, WhiteBalance, Calibrate and CropDefault steps,
/// in the same units, so the rest of the pipeline is unchanged: white
/// balance is applied to the mosaic before demosaicing (the interpolation
/// works on colour differences of balanced data), then the camera matrix.
/// `None` for other layouts or without a usable colour matrix; rawler's
/// develop handles those. With `draft`, X-Trans mosaics are binned 3 x 3
/// instead of demosaiced; other drafts return `None` and use rawler's.
/// The black levels of a 2 × 2 repeat read from the masked sensor border
/// (black_bias.rs `masked_black`) where the camera has one and the
/// decoder's levels spread over the repeat's positions by more than
/// `max_extra_spread` beyond the border's own spread: per-colour levels
/// that the photosites do not show (EOS 7D maker notes at ISO 12800:
/// 2032-2061, its border 2044-2048, Adobe's level 2047). The border's
/// column banding leaves its level a few levels uncertain, so a decoder
/// level within that stays (`BlackLevels::border_margin`: 8 levels for
/// `Unbiased`, `black_bias::BORDER_NOISE` for `UnbiasedBorder`).
fn use_masked_black(raw_image: &mut RawImage, max_extra_spread: f32) {
    let RawPhotometricInterpretation::Cfa(config) = &raw_image.photometric else {
        return;
    };
    if raw_image.cpp != 1
        || raw_image.blackareas.is_empty()
        || config.cfa.width != 2
        || config.cfa.height != 2
        || !config.cfa.is_rgb()
    {
        return;
    }
    let (width, height) = (raw_image.width, raw_image.height);
    let active = raw_image
        .active_area
        .map(|a| (a.p.x, a.p.y, a.d.w, a.d.h))
        .unwrap_or((0, 0, 0, 0));
    let areas: Vec<_> = raw_image
        .blackareas
        .iter()
        .map(|a| (a.p.x, a.p.y, a.d.w, a.d.h))
        .collect();
    let masked = match &raw_image.data {
        rawler::RawImageData::Integer(v) => {
            crate::black_bias::masked_black(&areas, active, width, height, 2, 2, |i| v[i] as f32)
        }
        rawler::RawImageData::Float(v) => {
            crate::black_bias::masked_black(&areas, active, width, height, 2, 2, |i| v[i])
        }
    };
    let Some(masked) = masked else {
        return;
    };
    let decoder = raw_image.blacklevel.as_bayer_array();
    let spread = |v: &[f32]| {
        v.iter().copied().fold(f32::MIN, f32::max) - v.iter().copied().fold(f32::MAX, f32::min)
    };
    if spread(&decoder) > spread(&masked) + max_extra_spread {
        log::info!("Black level from the masked border: {masked:?} (decoder {decoder:?})");
        raw_image.blacklevel = rawler::rawimage::BlackLevel::new(&masked, 2, 2, 1);
    }
}

/// `ShadowMeans` of the active area of an RGB mosaic, signed, in the units
/// of `apply_scaling` (black 0, the scaling's white 1) times `to_white`: the
/// units the demosaic sees before white balance, sensor white at 1 (the
/// noise model's brightness bands are fractions of it; the decoder's white
/// was set to `u32::MAX` before, `develop_internal`).
fn shadow_means_of(raw_image: &RawImage, to_white: f32) -> Option<crate::black_bias::ShadowMeans> {
    let RawPhotometricInterpretation::Cfa(config) = &raw_image.photometric else {
        return None;
    };
    let roi = raw_image.active_area.unwrap_or_else(|| {
        rawler::imgop::Rect::new(rawler::imgop::Point::new(0, 0), raw_image.dim())
    });
    let width = raw_image.width;
    let levels = raw_image.blacklevel.as_bayer_array();
    let whites = raw_image.whitelevel.as_bayer_array();
    // `apply_scaling` works on 2 × 2 positions from the buffer origin.
    let pos = |r: usize, c: usize| (r % 2) * 2 + c % 2;
    let scale: [f32; 4] = std::array::from_fn(|i| to_white / (whites[i] - levels[i]).max(1.0));
    let cfa = &config.cfa;
    let color = |r: usize, c: usize| cfa.color_at(roi.p.y + r, roi.p.x + c).min(2);
    Some(match &raw_image.data {
        rawler::RawImageData::Integer(v) => crate::black_bias::ShadowMeans::measure(
            roi.d.w,
            roi.d.h,
            |r, c| {
                let (y, x) = (roi.p.y + r, roi.p.x + c);
                let i = pos(y, x);
                (v[y * width + x] as f32 - levels[i]) * scale[i]
            },
            color,
        ),
        rawler::RawImageData::Float(v) => crate::black_bias::ShadowMeans::measure(
            roi.d.w,
            roi.d.h,
            |r, c| {
                let (y, x) = (roi.p.y + r, roi.p.x + c);
                let i = pos(y, x);
                (v[y * width + x] - levels[i]) * scale[i]
            },
            color,
        ),
    })
}

fn develop_rgb_mosaic(
    raw_image: &mut RawImage,
    rescale_factor: f32,
    draft: bool,
    cancelled: crate::demosaic::Cancel,
    shadow_means: Option<&crate::black_bias::ShadowMeans>,
) -> Result<Option<Intermediate>> {
    use rawler::imgop::matrix::{multiply, normalize, pseudo_inverse};
    use rawler::imgop::xyz::{Illuminant, SRGB_TO_XYZ_D65};

    let cfa = match &raw_image.photometric {
        RawPhotometricInterpretation::Cfa(config) if raw_image.cpp == 1 && config.cfa.is_rgb() => {
            config.cfa.clone()
        }
        _ => return Ok(None),
    };
    // Own drafts are X-Trans bins only (Bayer drafts are rawler's
    // superpixels); checked before anything changes `raw_image`.
    if draft && !(cfa.width == 6 && cfa.height == 6) {
        return Ok(None);
    }
    let Some(color_matrix) = raw_image
        .color_matrix
        .iter()
        .find(|(illuminant, _)| **illuminant == Illuminant::D65)
        .or_else(|| raw_image.color_matrix.iter().next())
        .map(|(_, matrix)| matrix.clone())
        .filter(|matrix| !matrix.is_empty() && matrix.len() % 3 == 0 && matrix.len() <= 12)
    else {
        return Ok(None);
    };

    raw_image.apply_scaling()?;
    let width = raw_image.width;
    let roi = raw_image.active_area.unwrap_or_else(|| {
        rawler::imgop::Rect::new(rawler::imgop::Point::new(0, 0), raw_image.dim())
    });
    if roi.d.w < 16 || roi.d.h < 16 {
        return Ok(None);
    }
    let colors = (0..cfa.height)
        .flat_map(|r| (0..cfa.width).map(move |c| (r, c)))
        .map(|(r, c)| cfa.color_at(roi.p.y + r, roi.p.x + c) as u8)
        .collect();
    let Some(pattern) = crate::demosaic::CfaPattern::new(cfa.width, cfa.height, colors) else {
        return Ok(None);
    };
    let bin = if draft && crate::demosaic::is_xtrans_like(&pattern) {
        3
    } else if draft {
        return Ok(None);
    } else {
        1
    };
    let wb = if raw_image.wb_coeffs[0].is_nan() {
        [1.0; 4]
    } else {
        raw_image.wb_coeffs
    };

    // White-balanced mosaic of the active area with sensor white at 1.0.
    let rawler::RawImageData::Float(data) = &raw_image.data else {
        return Err(anyhow!("scaled RAW data is not floating point"));
    };
    let (roi_w, roi_h) = (roi.d.w, roi.d.h);
    let mut mosaic = vec![0.0f32; roi_w * roi_h];
    mosaic
        .par_chunks_mut(roi_w)
        .enumerate()
        .for_each(|(r, line)| {
            let src = &data[(roi.p.y + r) * width + roi.p.x..][..roi_w];
            for (c, (dst, &v)) in line.iter_mut().zip(src).enumerate() {
                let color = pattern.color(r as isize, c as isize);
                *dst = v * wb[color] * rescale_factor;
            }
        });
    // The caller only reads metadata from here on; release the full
    // sensor frame before the RGB output is allocated.
    raw_image.data = rawler::RawImageData::Float(Vec::new());
    let mut rgb = if bin > 1 {
        crate::demosaic::bin(&mosaic, roi_w, roi_h, &pattern, bin)
    } else {
        crate::demosaic::demosaic_unless(&mosaic, roi_w, roi_h, &pattern, cancelled)
            .map_err(|_| anyhow!("Load cancelled"))?
    };
    let (out_w, out_h) = (roi_w / bin, roi_h / bin);
    drop(mosaic);
    if let Some(means) = shadow_means.filter(|_| bin == 1) {
        // The means are in the mosaic's units before white balance.
        let changed = means.restore(&mut rgb, roi_w, [0, 1, 2].map(|k| wb[k]));
        log::info!("Shadow means restored on {changed} pixels");
    }
    // Tile buffers were freed on the worker threads, whose allocator heaps
    // keep those pages until they collect.
    rayon::broadcast(|_| crate::allocator::release_freed_memory());

    // Camera to linear sRGB as rawler's Calibrate step, back in rawler's
    // units (the caller rescales to sensor white).
    let mut xyz2cam = [[0.0f32; 3]; 4];
    for (i, row) in color_matrix.as_chunks::<3>().0.iter().enumerate() {
        xyz2cam[i] = [row[0], row[1], row[2]];
    }
    let cam2rgb = pseudo_inverse(normalize(multiply(&xyz2cam, &SRGB_TO_XYZ_D65)));
    let unscale = 1.0 / rescale_factor;
    // Negative channels (colours beyond sRGB) stay; the normalisation limits
    // them to the ProPhoto gamut.
    rgb.par_iter_mut().with_min_len(16_384).for_each(|p| {
        let [r, g, b] = *p;
        for (k, out) in p.iter_mut().enumerate() {
            let v = cam2rgb[k][0] * r + cam2rgb[k][1] * g + cam2rgb[k][2] * b;
            *out = v * unscale;
        }
    });
    let mut pixels = rawler::pixarray::RgbF32::new_with(rgb, out_w, out_h);

    if let Some(crop) = raw_image.crop_area {
        let crop = crop_within_active(crop, raw_image.active_area).map(|crop| {
            if bin == 1 {
                return crop;
            }
            rawler::imgop::Rect::new(
                rawler::imgop::Point::new(crop.p.x / bin, crop.p.y / bin),
                rawler::imgop::Dim2::new(crop.d.w / bin, crop.d.h / bin),
            )
        });
        if let Some(crop) = crop
            && !crop.is_empty()
            && crop.d != pixels.dim()
            && crop.p.x + crop.d.w <= out_w
            && crop.p.y + crop.d.h <= out_h
        {
            pixels = pixels.crop(crop);
        }
    }
    Ok(Some(Intermediate::ThreeColor(pixels)))
}

pub(crate) fn superccd_layout(raw_image: &RawImage, file_bytes: &[u8]) -> Option<SuperCcdLayout> {
    let camera = &raw_image.camera;
    let alt = camera.find_hint("fuji_rotation_alt");
    if !(alt || camera.find_hint("fuji_rotation")) {
        return None;
    }
    let (raw_height, raw_width) = crate::capture_info::raf_raw_size(file_bytes)?;
    let area = camera.active_area.unwrap_or([0, 0, 0, 0]);
    let (crop_width, crop_height) = (
        raw_width.checked_sub(area[0] + area[2])?,
        raw_height.checked_sub(area[1] + area[3])?,
    );
    // rawler: rotated width = crop_width + crop_height / 2 (alt: crop_height
    // + crop_width / 2), height one less; the left vertex sits at row
    // crop_width (alt: crop_width / 2).
    let (expected_width, fuji_width) = if alt {
        (crop_height + crop_width / 2, crop_width / 2)
    } else {
        (crop_width + crop_height / 2, crop_width)
    };
    if raw_image.width != expected_width || raw_image.height + 1 != expected_width {
        log::warn!(
            "SuperCCD layout mismatch: decoded {}×{}, expected width {} from raw {}×{}; frame left rotated",
            raw_image.width,
            raw_image.height,
            expected_width,
            raw_width,
            raw_height
        );
        return None;
    }
    let border = camera
        .crop_area
        .map_or(0, |crop| crop.iter().copied().max().unwrap_or(0));
    Some(SuperCcdLayout {
        fuji_width,
        rotate_90cw: camera.find_hint("fuji_rotate_90cw"),
        edge: (border as f64 * std::f64::consts::FRAC_1_SQRT_2).ceil() as usize + 12,
    })
}

/// dcraw's `fuji_rotate`: resamples the 45° diamond back to an upright
/// frame (bilinear, √½ step), optionally followed by a 90° turn.
fn undo_superccd_layout(
    intermediate: Intermediate,
    layout: SuperCcdLayout,
) -> Result<Intermediate> {
    let Intermediate::ThreeColor(pixels) = intermediate else {
        return Ok(intermediate);
    };
    let (width, height) = (pixels.width, pixels.height);
    let step = std::f64::consts::FRAC_1_SQRT_2;
    let fuji_width = layout.fuji_width as f64;
    let wide = (fuji_width / step) as usize;
    let high = ((height as f64 - fuji_width) / step) as usize;
    if wide < 2 || high < 2 || width < 2 || height < 2 {
        return Err(anyhow!("SuperCCD frame too small to rotate"));
    }
    let src = &pixels.data;
    let mut out = vec![[0.0f32; 3]; wide * high];
    out.par_chunks_mut(wide)
        .enumerate()
        .for_each(|(row, line)| {
            for (col, dst) in line.iter_mut().enumerate() {
                let r = fuji_width + (row as f64 - col as f64) * step;
                let c = (row as f64 + col as f64) * step;
                let (ur, uc) = (r.floor(), c.floor());
                if ur < 0.0 || uc < 0.0 || ur > (height - 2) as f64 || uc > (width - 2) as f64 {
                    continue;
                }
                let (fr, fc) = ((r - ur) as f32, (c - uc) as f32);
                let (ur, uc) = (ur as usize, uc as usize);
                let p00 = src[ur * width + uc];
                let p01 = src[ur * width + uc + 1];
                let p10 = src[(ur + 1) * width + uc];
                let p11 = src[(ur + 1) * width + uc + 1];
                for i in 0..3 {
                    dst[i] = (p00[i] * (1.0 - fc) + p01[i] * fc) * (1.0 - fr)
                        + (p10[i] * (1.0 - fc) + p11[i] * fc) * fr;
                }
            }
        });
    // The diamond's edges are demosaiced from half-filled neighbourhoods and
    // sample into the empty corners, and the sensor's own border (rawler's
    // crop area in storage coordinates, which it drops when rotating) lands
    // along all four sides scaled by √½: trim them.
    let edge = layout.edge;
    let (wide, high, out) = if wide > 4 * edge && high > 4 * edge {
        let (w, h) = (wide - 2 * edge, high - 2 * edge);
        let trimmed: Vec<[f32; 3]> = (0..h)
            .flat_map(|row| {
                let start = (row + edge) * wide + edge;
                out[start..start + w].iter().copied()
            })
            .collect();
        (w, h, trimmed)
    } else {
        (wide, high, out)
    };
    let mut out = out;
    let (mut out_width, mut out_height) = (wide, high);
    if layout.rotate_90cw {
        let mut turned = vec![[0.0f32; 3]; wide * high];
        for row in 0..high {
            for col in 0..wide {
                turned[col * high + (high - 1 - row)] = out[row * wide + col];
            }
        }
        out = turned;
        (out_width, out_height) = (high, wide);
    }
    log::info!(
        "SuperCCD frame {}×{} rotated back to {}×{}",
        width,
        height,
        out_width,
        out_height
    );
    Ok(Intermediate::ThreeColor(rawler::pixarray::Color2D {
        width: out_width,
        height: out_height,
        data: out,
    }))
}

fn intermediate_into_rgb(intermediate: Intermediate) -> Result<DynamicImage> {
    let dim = intermediate.dim();
    let (width, height) = (dim.w as u32, dim.h as u32);
    match intermediate {
        Intermediate::ThreeColor(pixels) => {
            // RAW pixels are opaque. Transfer the developed RGB allocation instead
            // of expanding it to RGBA only to discard alpha during preprocessing.
            let buffer =
                ImageBuffer::<Rgb<f32>, _>::from_raw(width, height, pixels.data.into_flattened())
                    .ok_or_else(|| anyhow!("Invalid developed RGB dimensions"))?;
            Ok(DynamicImage::ImageRgb32F(buffer))
        }
        Intermediate::Monochrome(pixels) => {
            let buffer = ImageBuffer::<Rgb<f32>, _>::from_fn(width, height, |x, y| {
                let p = pixels.data[(y * width + x) as usize];
                Rgb([p, p, p])
            });
            Ok(DynamicImage::ImageRgb32F(buffer))
        }
        _ => Err(anyhow!("Unsupported intermediate format for conversion")),
    }
}

pub fn get_fast_demosaic_scale_factor(
    file_bytes: &[u8],
    decoded_width: u32,
    decoded_height: u32,
) -> f32 {
    let source = RawSource::new_from_slice(file_bytes);
    if let Ok(decoder) = rawler::get_decoder(&source)
        && let Ok(raw_img) = decoder.raw_image(&source, &RawDecodeParams::default(), true)
    {
        let max_orig = (raw_img.width as f32).max(raw_img.height as f32);
        let max_comp = (decoded_width as f32).max(decoded_height as f32);
        if max_orig > 0.0 {
            let ratio = max_comp / max_orig;
            // X-Trans drafts bin 3 x 3 (a third, less the uncropped border).
            let xtrans = matches!(
                &raw_img.photometric,
                RawPhotometricInterpretation::Cfa(config) if config.cfa.width == 6
            );
            if xtrans && (0.28..0.345).contains(&ratio) {
                return 1.0 / 3.0;
            } else if ratio > 0.1 && ratio < 0.35 {
                return 0.25;
            } else if (0.35..0.75).contains(&ratio) {
                return 0.5;
            }
        }
    }
    1.0
}

#[cfg(test)]
mod frame_tests {
    use super::*;
    use crate::black_bias::BlackLevels;
    use image::GenericImageView;
    use rawler::imgop::{Dim2, Point, Rect};
    use rawler::pixarray::Color2D;

    fn rect(x: usize, y: usize, w: usize, h: usize) -> Rect {
        Rect::new(Point::new(x, y), Dim2::new(w, h))
    }

    /// A 12-bit RGGB DNG (black 256, white 4095) whose upper half is 3
    /// levels above black and whose lower half is bright, with shot noise
    /// and 10 levels of read noise (`noisy`) or without.
    fn shadow_ramp_dng(noisy: bool) -> Vec<u8> {
        let (w, h) = (192u32, 192u32);
        let mut seed = 11u64;
        let mut uniform = || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((seed >> 33) as f32 / (1u64 << 31) as f32) - 0.5
        };
        let samples: Vec<u16> = (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .map(|(_, y)| {
                let signal: f32 = if y < h / 2 { 3.0 } else { 2000.0 };
                let sigma = (100.0 + 0.5 * signal).sqrt();
                let n: f32 = (0..4).map(|_| uniform()).sum::<f32>() * 1.732;
                let v = 256.0 + signal + if noisy { sigma * n } else { 0.0 };
                v.round().clamp(0.0, 4095.0) as u16
            })
            .collect();
        crate::fingerprint::synthetic::mosaic_dng(w, h, (2, 2), &[0, 1, 1, 2], &samples)
    }

    #[test]
    fn unbiased_blacks_keep_the_dark_means_and_leave_bright_rows_alone() {
        let develop = |bytes: &[u8], black| {
            develop_raw_image_with_draft(
                bytes,
                false,
                2.5,
                "auto".into(),
                None,
                None,
                None,
                crate::hot_pixels::DeadTest::default(),
                0,
                black,
                crate::highlight_reconstruction::HighlightRebuild::default(),
            )
            .unwrap()
            .to_rgb32f()
        };
        let clean = develop(&shadow_ramp_dng(false), BlackLevels::Clipped);
        let clipped = develop(&shadow_ramp_dng(true), BlackLevels::Clipped);
        let unbiased = develop(&shadow_ramp_dng(true), BlackLevels::Unbiased);
        // Orientation 6 turns the mosaic: its upper half is the right half.
        let (w, h) = clean.dimensions();
        let mean = |img: &image::Rgb32FImage, dark: bool, k: usize| {
            let cols = if dark {
                w / 2 + 8..w - 8
            } else {
                8..w / 2 - 24
            };
            let mut sum = 0.0f64;
            let mut n = 0.0f64;
            for y in 8..h - 8 {
                for x in cols.clone() {
                    sum += img.get_pixel(x, y)[k] as f64;
                    n += 1.0;
                }
            }
            sum / n
        };
        // The clip lifts the dark half by about the signal itself; the
        // restored means are within sampling noise of the clean frame's.
        let error = |img: &image::Rgb32FImage| -> f64 {
            (0..3)
                .map(|k| (mean(img, true, k) - mean(&clean, true, k)).abs())
                .sum()
        };
        let signal: f64 = (0..3).map(|k| mean(&clean, true, k)).sum();
        assert!(
            error(&clipped) > 0.3 * signal,
            "{} of {signal}",
            error(&clipped)
        );
        assert!(
            error(&unbiased) < 0.25 * error(&clipped),
            "{} vs {}",
            error(&unbiased),
            error(&clipped)
        );
        for k in 0..3 {
            assert_eq!(mean(&unbiased, false, k), mean(&clipped, false, k));
        }
    }

    #[test]
    fn developed_frame_is_read_without_decoding_and_turned() {
        let dng = crate::fingerprint::synthetic::synthetic_dng();
        let developed = develop_raw_image(&dng, false, 2.5, "auto".into(), None).unwrap();
        let frame = developed_frame(&RawSource::new_from_slice(&dng), 0).unwrap();
        // Orientation 6: the 64 × 48 mosaic develops 48 × 64.
        assert_eq!(frame.orientation, 6);
        assert_eq!(frame.size, Some((48, 64)));
        assert_eq!(frame.size, Some(developed.dimensions()));
        assert_eq!(
            developed_frame(&RawSource::new_from_slice(b"not a raw"), 0),
            None
        );
    }

    /// Local camera fixtures only; never check photographs into the repo.
    #[test]
    #[ignore = "requires RAWTONIC_RAW_FIXTURES pointing to a local sample directory"]
    fn developed_frame_matches_the_development_of_camera_files() {
        let dir = std::env::var("RAWTONIC_RAW_FIXTURES").expect("local fixture directory");
        let (mut checked, mut wrong, mut slowest) = (0, Vec::new(), 0.0f64);
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let Ok(developed) = develop_raw_image(&bytes, false, 2.5, "auto".into(), None) else {
                continue;
            };
            checked += 1;
            let started = std::time::Instant::now();
            let read = developed_frame_at(&path, 0).and_then(|frame| frame.size);
            slowest = slowest.max(started.elapsed().as_secs_f64() * 1000.0);
            if read.is_some_and(|size| size != developed.dimensions()) {
                wrong.push((path, read, developed.dimensions()));
            }
        }
        println!("{checked} files, slowest frame read {slowest:.1} ms");
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// Sony α7 IV lossless-compressed Medium: the YCbCr buffer is padded to
    /// whole tiles (5120×3584), the sensor's active area is 4624×3080 and
    /// the default crop 4608×3072 at (8, 4). rawler scaled the crop by
    /// 5120/4624 (5102×3401 with a green strip of padding).
    #[test]
    fn sony_medium_frame_is_the_default_crop_not_the_padded_buffer() {
        let frame = undemosaiced_frame(
            Dim2::new(5120, 3584),
            Some(rect(0, 0, 4624, 3080)),
            Some(rect(8, 4, 4608, 3072)),
        );
        assert_eq!(frame, Some(rect(8, 4, 4608, 3072)));
        // Lens context: the same crop, relative to the active area.
        assert_eq!(
            crop_within_active(rect(8, 4, 4608, 3072), Some(rect(0, 0, 4624, 3080))),
            Some(rect(8, 4, 4608, 3072))
        );
    }

    #[test]
    fn frame_falls_back_to_the_active_area_and_the_whole_buffer() {
        let dim = Dim2::new(5120, 3584);
        assert_eq!(
            undemosaiced_frame(dim, Some(rect(0, 0, 4624, 3080)), None),
            Some(rect(0, 0, 4624, 3080))
        );
        // A crop outside the active area is ignored.
        assert_eq!(
            undemosaiced_frame(
                dim,
                Some(rect(0, 0, 4624, 3080)),
                Some(rect(4700, 0, 100, 100))
            ),
            Some(rect(0, 0, 4624, 3080))
        );
        // Linear DNGs whose active area is the buffer: crop as before.
        assert_eq!(
            undemosaiced_frame(
                dim,
                Some(rect(0, 0, 5120, 3584)),
                Some(rect(16, 16, 5088, 3552))
            ),
            Some(rect(16, 16, 5088, 3552))
        );
        assert_eq!(undemosaiced_frame(dim, None, None), None);
        assert_eq!(
            undemosaiced_frame(dim, Some(rect(0, 0, 5120, 3584)), None),
            None
        );
        // An active area with an offset (DNG ActiveArea) keeps the crop
        // absolute here and relative in the lens context.
        assert_eq!(
            undemosaiced_frame(
                dim,
                Some(rect(8, 8, 5000, 3500)),
                Some(rect(24, 16, 4960, 3480))
            ),
            Some(rect(24, 16, 4960, 3480))
        );
        assert_eq!(
            crop_within_active(rect(24, 16, 4960, 3480), Some(rect(8, 8, 5000, 3500))),
            Some(rect(16, 8, 4960, 3480))
        );
        assert_eq!(
            crop_within_active(rect(6000, 0, 10, 10), Some(rect(8, 8, 5000, 3500))),
            None
        );
    }

    #[test]
    fn cropping_removes_every_padding_pixel() {
        // 64×48 buffer, content 1.0 in the active 56×40 area, padding 0.0;
        // default crop 48×32 at (4, 4).
        let (w, h) = (64usize, 48usize);
        let data: Vec<[f32; 3]> = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                if x < 56 && y < 40 { [1.0; 3] } else { [0.0; 3] }
            })
            .collect();
        let frame = undemosaiced_frame(
            Dim2::new(w, h),
            Some(rect(0, 0, 56, 40)),
            Some(rect(4, 4, 48, 32)),
        )
        .unwrap();
        let cropped = crop_intermediate(
            Intermediate::ThreeColor(Color2D::new_with(data, w, h)),
            frame,
        );
        assert_eq!(cropped.dim(), Dim2::new(48, 32));
        let Intermediate::ThreeColor(pixels) = cropped else {
            panic!("three colours expected");
        };
        assert!(pixels.data.iter().all(|p| *p == [1.0; 3]));
        // A frame that does not fit leaves the image whole.
        let kept = crop_intermediate(
            Intermediate::ThreeColor(Color2D::new_with(vec![[0.5; 3]; 4], 2, 2)),
            rect(1, 1, 4, 4),
        );
        assert_eq!(kept.dim(), Dim2::new(2, 2));
    }
}

#[cfg(test)]
mod memory_tests {
    use super::*;
    use rawler::pixarray::{Color2D, Pix2D};

    #[test]
    fn raw_rgb_transfer_reuses_developed_allocation_and_preserves_opaque_pixels() {
        let pixels = vec![[0.0, 0.25, 1000.0], [0.5, 1.0, 1.5]];
        let pointer = pixels.as_ptr().cast::<f32>();
        let expected = image::Rgba32FImage::from_fn(2, 1, |x, _| {
            let p = pixels[x as usize];
            image::Rgba([p[0], p[1], p[2], 1.0])
        });
        let image =
            intermediate_into_rgb(Intermediate::ThreeColor(Color2D::new_with(pixels, 2, 1)))
                .unwrap();
        assert_eq!(image.as_rgb32f().unwrap().as_ptr(), pointer);
        assert_eq!(image.to_rgba32f(), expected);
        assert_eq!(image.as_bytes().len(), 2 * 3 * std::mem::size_of::<f32>());
    }

    #[test]
    fn monochrome_transfer_keeps_equal_channels_and_opaque_alpha() {
        let pixels = Pix2D::new_with(vec![0.0, 0.5, 1000.0], 3, 1);
        let image = intermediate_into_rgb(Intermediate::Monochrome(pixels)).unwrap();
        assert_eq!(
            image.to_rgba32f().as_raw(),
            &[
                0.0, 0.0, 0.0, 1.0, 0.5, 0.5, 0.5, 1.0, 1000.0, 1000.0, 1000.0, 1.0
            ]
        );
    }
}

#[cfg(test)]
mod canon_300d_tests {
    #[test]
    fn white_balance_has_its_smallest_multiplier_at_one() {
        let wb = super::normalize_wb_to_smallest([1.0, 0.75, 1.05, f32::NAN]);
        assert!((wb[1] - 1.0).abs() < 1e-6 && (wb[0] - 1.0 / 0.75).abs() < 1e-6);
        assert!(wb[3].is_nan());
        // Blue the smallest: unchanged apart from the scale.
        let wb = super::normalize_wb_to_smallest([1.76, 1.15, 1.0, f32::NAN]);
        assert_eq!(wb[2], 1.0);
        assert!(super::normalize_wb_to_smallest([f32::NAN; 4])[0].is_nan());
        assert_eq!(
            super::normalize_wb_to_largest([2.0, 1.0, 1.5, 1.25]),
            [1.0, 0.5, 0.75, 0.625]
        );
        // Three-colour balances (fourth value unused) are left alone.
        assert!(super::normalize_wb_to_largest([2.0, 1.0, 1.5, f32::NAN])[3].is_nan());
    }

    #[test]
    fn canon_300d_linearization_matches_adobe_table() {
        // Adobe's table / 6.4 at raw 2386, 3136, 3886 and 4000.
        assert_eq!(super::canon_300d_linear(125), 125);
        assert_eq!(super::canon_300d_linear(2386), 2386);
        assert_eq!(super::canon_300d_linear(3136), 3245);
        assert_eq!(super::canon_300d_linear(3886), 4783);
        assert_eq!(super::canon_300d_linear(4000), 5120);
        assert_eq!(super::canon_300d_linear(4095), 5120);
    }
}

#[cfg(test)]
mod draft_tests {
    use super::*;
    use std::sync::Mutex;

    /// Local camera fixtures only; never check photographs into the repo.
    #[test]
    #[ignore = "requires RAWTONIC_RAW_FIXTURES pointing to a local sample directory"]
    fn drafts_come_first_and_leave_the_full_development_unchanged() {
        let dir = std::env::var("RAWTONIC_RAW_FIXTURES").expect("local fixture directory");
        let mut count = 0;
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let started = std::time::Instant::now();
            let Ok(plain) = develop_raw_image(&bytes, false, 2.5, "auto".into(), None) else {
                continue;
            };
            let plain_ms = started.elapsed().as_secs_f64() * 1000.0;
            // Draft width, height, announced full size, ms after the start.
            type Draft = (u32, u32, (u32, u32), f64);
            let got: Arc<Mutex<Option<Draft>>> = Arc::default();
            let started = std::time::Instant::now();
            let sink_got = Arc::clone(&got);
            let sink = DraftSink(Box::new(move |image: DynamicImage, full| {
                *sink_got.lock().unwrap() = Some((
                    image.width(),
                    image.height(),
                    full,
                    started.elapsed().as_secs_f64() * 1000.0,
                ));
            }));
            let with_draft = develop_raw_image_with_draft(
                &bytes,
                false,
                2.5,
                "auto".into(),
                None,
                None,
                Some(&sink),
                crate::hot_pixels::DeadTest::default(),
                0,
                crate::black_bias::BlackLevels::default(),
                crate::highlight_reconstruction::HighlightRebuild::default(),
            )
            .unwrap();
            let full_ms = started.elapsed().as_secs_f64() * 1000.0;
            assert_eq!(
                plain.to_rgb32f().as_raw(),
                with_draft.to_rgb32f().as_raw(),
                "{}",
                path.display()
            );
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let Some((w, h, full, draft_ms)) = *got.lock().unwrap() else {
                // Linear (demosaiced) DNGs have nothing to draft.
                assert!(name.to_lowercase().ends_with(".dng"), "no draft for {name}");
                println!("{name}: no draft (linear), full at {full_ms:.0} ms");
                count += 1;
                continue;
            };
            let (fw, fh) = (plain.width(), plain.height());
            // The announced size is the full development's, give or take
            // the decoder's rounding; the draft is a half, a third (X-Trans)
            // or a quarter (rawler's superpixels of some CR3s) of it.
            assert!(
                full.0.abs_diff(fw) <= 16 && full.1.abs_diff(fh) <= 16,
                "{full:?} vs {fw}x{fh}"
            );
            let ratio = w as f32 / fw as f32;
            assert!((0.24..=0.52).contains(&ratio), "{w}x{h} of {fw}x{fh}");
            println!(
                "{name}: draft {w}x{h} at {draft_ms:.0} ms, full {fw}x{fh} at {full_ms:.0} ms (alone {plain_ms:.0} ms)"
            );
            count += 1;
        }
        assert!(count > 0);
    }

    /// The full development starts once the draft is handed over: a load
    /// cancelled from the sink (another photograph asked for while the
    /// draft shows) skips its demosaicing instead of finishing it.
    #[test]
    #[ignore = "requires RAWTONIC_RAW_FIXTURES pointing to a local sample directory"]
    fn the_full_development_starts_after_its_draft() {
        let dir = std::env::var("RAWTONIC_RAW_FIXTURES").expect("local fixture directory");
        let mut count = 0;
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            if develop_raw_image(&bytes, false, 2.5, "auto".into(), None).is_err() {
                continue;
            }
            let generation = Arc::new(AtomicUsize::new(1));
            let cancel = Arc::clone(&generation);
            let sink = DraftSink(Box::new(move |_, _| {
                cancel.store(2, Ordering::SeqCst);
            }));
            let before = FULL_DEVELOPMENTS.with(std::cell::Cell::get);
            let result = develop_raw_image_with_draft(
                &bytes,
                false,
                2.5,
                "auto".into(),
                Some((Arc::clone(&generation), 1)),
                None,
                Some(&sink),
                crate::hot_pixels::DeadTest::default(),
                0,
                crate::black_bias::BlackLevels::default(),
                crate::highlight_reconstruction::HighlightRebuild::default(),
            );
            let full = FULL_DEVELOPMENTS.with(std::cell::Cell::get) - before;
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if generation.load(Ordering::SeqCst) == 1 {
                // Linear (demosaiced) DNGs have nothing to draft.
                continue;
            }
            assert!(result.is_err(), "{name}: developed after a cancel");
            assert_eq!(
                full, 0,
                "{name}: the full development started beside its draft"
            );
            count += 1;
        }
        assert!(count > 0);
    }
}
