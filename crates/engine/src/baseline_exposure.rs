//! Per-file base look for the Standard tone mapper.
//!
//! RAW files render with Rawtonic's neutral look (shader mode 3): its own
//! tone curve after the default exposure-ramp black, and its own colour
//! rendition (notes: reference/own-base-look.md). In blind comparisons of
//! photos from the user's library a Lightroom-like rendering was preferred
//! to the Capture One-like one (notes: reference/color-foundation.md), so
//! the look is tuned towards Camera Raw's default rendering by measurement,
//! with few parameters of its own; no other editor's curve or table is used.
//!
//! Exposure: proprietary RAWs get +0.72 EV, which puts the ILCE-7M3,
//! ILCE-7M4 and X-H2S on Lightroom's lightness (78 photos, median within
//! 0.2 Oklab L). Cameras in Adobe's table (camera_exposure.rs) get Adobe's
//! baseline plus the same offset as DNGs instead, including Adobe's
//! capture-mode rules (ISO, Canon white level / highlight tone priority /
//! sRAW, capture size; capture_info.rs); Fujifilm's is ISO dependent
//! through the file's RawExposureBias. DNGs get their BaselineExposure
//! (plus offset).
//! Files render with neutral colour (the camera matrices already put them
//! on Adobe's colour); DJI drone DNGs that Adobe has a profile for take the
//! same path as other DNGs since 2026-10-07 (their own saturation and
//! blue-turn stand-ins were 0.4-1.7 dE further from Camera Raw). DNGs
//! with a gain table map (ProRAW, Pixel 8 Pro) use their own tone curve and
//! the map. Read once per file.
use crate::image_processing::GlobalAdjustments;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// Proprietary RAWs (no BaselineExposure tag) on the Camera Raw curve, for
/// cameras without Adobe's value.
const PROPRIETARY_CAMERA_RAW_EV: f32 = 0.72;
/// Added to Adobe's per-camera baseline (camera_exposure.rs) for
/// proprietary RAWs: puts 58 cameras of 18 makers on Camera Raw's midtones
/// (median difference 0.00 EV, median absolute deviation 0.06 EV;
/// notes: benchmark-results/adobe-baseline-calibration.json). Re-fitted
/// 0.38 -> 0.41 on 2026-10-05 after the per-camera white levels were put on
/// Adobe's (Sony's nominal white had made every Sony file 0.09 EV bright
/// and pulled the fit): proprietary files of 687 models sat a median 0.026
/// EV, DNGs of 191 models 0.037 EV below Camera Raw's midtones
/// (notes: benchmark-results/broad-camera-raw-999-models.json).
const ADOBE_BASELINE_OFFSET_EV: f32 = 0.41;
/// ProRAW: full BaselineExposure, as Camera Raw renders it (tuned below).
const PRORAW_OFFSET_EV: f32 = 0.0;
/// Camera Raw opens DNGs of the phones it has its own profile for (iPhone
/// before ProRAW, Pixel 2-7, Galaxy S7-S23 outside Expert RAW) with Whites
/// +15, Blacks -5 and Clarity +8 instead of zero (read from the settings it
/// applied to every corpus DNG;
/// notes: benchmark-results/camera-raw-phone-defaults-2026-10-07.json).
/// Rawtonic's Whites lifts the midtones more than Camera Raw's, so +8
/// matched its +15 on those phones (18 models: mean dE 1.32 -> 1.06).
/// In shader units (`SCALES` in image_processing.rs: 30, 40, 125).
const PHONE_WHITES: f32 = 8.0 / 30.0;
const PHONE_BLACKS: f32 = -5.0 / 40.0;
const PHONE_CLARITY: f32 = 8.0 / 125.0;
/// iPhone DNGs (before ProRAW) sat 0.1 EV above Camera Raw's midtones with
/// the DNG offset (five models: iPhone 6s Plus, 7 Plus, 8, SE, XS).
const IPHONE_OFFSET_EV: f32 = -0.12;
/// Galaxy Expert RAW DNGs (a gain table map without a tone curve of their
/// own) match Camera Raw without the black ramp and 0.05 EV below
/// the DNG offset. With the ramp the shadows were up to 5 EV dark at
/// display -10 EV (S21 Ultra, S22 Ultra, S23, S23+, S23 Ultra: mean dE
/// 1.46 -> 1.20, largest EV residual 5.0 -> 0.65).
const GAIN_MAP_OFFSET_EV: f32 = -0.05;
const BASELINE_EXPOSURE_TAG: u16 = 50730;
const BASELINE_EXPOSURE_OFFSET_TAG: u16 = 51109;
const STANDARD_MODE: u32 = 2;
pub const CAMERA_RAW_MODE: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BaseLook {
    /// EV applied before the base tone curve.
    pub baseline_ev: f32,
    /// Render with the Camera Raw look instead of Capture One's.
    pub camera_raw: bool,
    /// Camera Raw look without the shader's flag-0 colour stand-ins.
    pub neutral_colour: bool,
    /// Tone curve embedded in the file, used by the Camera Raw look instead
    /// of the default tone curve (display EV every 0.25 EV from −12 EV).
    pub profile_curve: Option<[f32; PROFILE_CURVE_SAMPLES]>,
    /// DNG DefaultBlackRender = None (ProRAW, Pixel 8 Pro): Camera Raw
    /// skips its black subtraction, so the look has no black ramp.
    pub no_black_render: bool,
    /// Camera Raw's default settings for phones it has a profile for
    /// (`camera_raw_phone_defaults`), added on top of the user's sliders.
    pub phone_defaults: bool,
}

pub const PROFILE_CURVE_SAMPLES: usize = 49;

impl BaseLook {
    const NONE: BaseLook = BaseLook {
        baseline_ev: 0.0,
        camera_raw: false,
        neutral_colour: false,
        profile_curve: None,
        no_black_render: false,
        phone_defaults: false,
    };

    /// Sets the baseline and, for the Standard tone mapper, the variant.
    pub fn apply(self, global: &mut GlobalAdjustments) {
        global.baseline_exposure = self.baseline_ev;
        if self.camera_raw && global.tonemapper_mode == STANDARD_MODE {
            global.tonemapper_mode = CAMERA_RAW_MODE;
            // Flag in the last slot: 1 = the file's own curve (and colour),
            // 2 = built-in curve with neutral colour.
            if let Some(curve) = self.profile_curve {
                for (i, value) in curve.iter().enumerate() {
                    global.profile_curve[i / 4][i % 4] = *value;
                }
                global.profile_curve[15][3] = 1.0;
            } else if self.neutral_colour {
                global.profile_curve[15][3] = 2.0;
            }
            // Next to the flag: 1 = no black ramp.
            if self.no_black_render {
                global.profile_curve[15][2] = 1.0;
            }
            if self.phone_defaults {
                global.whites += PHONE_WHITES;
                global.blacks += PHONE_BLACKS;
                global.clarity += PHONE_CLARITY;
            }
        }
    }
}

fn cache() -> &'static Mutex<HashMap<String, BaseLook>> {
    static CACHE: OnceLock<Mutex<HashMap<String, BaseLook>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// Applies the file's base look to shader adjustments. `measures`: those of
/// the decode rendered (`image_loader::DecodeMeasures`); without them (a
/// preview proxy, a fast development) the last full decode's of `path`,
/// else the facts registered for it.
pub fn apply_base_look(
    global: &mut GlobalAdjustments,
    path: &str,
    measures: Option<&crate::image_loader::DecodeMeasures>,
) {
    base_look(path).apply(global);
    crate::camera_profile::apply(global, path);
    crate::diffraction::apply(global, path);
    let registered = || crate::source_facts::registered(path);
    let (noise_level, image_key) = match measures {
        Some(measures) => (measures.noise_level, measures.image_key),
        None => (
            crate::noise_level::for_path(path),
            crate::image_key::for_path(path),
        ),
    };
    global.noise_level = noise_level
        .or_else(|| registered().and_then(|facts| facts.noise_level))
        .unwrap_or(0.0);
    let [low, high] = image_key
        .or_else(|| registered().and_then(|facts| facts.image_key))
        .unwrap_or([0.0, 0.0]);
    global.image_key = high;
    global.image_key_low = low;
    if std::env::var_os("HL_SCRATCH_KEY").is_some() { eprintln!("HLKEY {} {} {}", high, low, global.baseline_exposure); }
}

/// The DNG BaselineExposure that gives a linear DNG made from `path` (a
/// merge, see rawtonic-merge) the same baseline as `path` itself: such DNGs
/// take the Adobe offset on top of their tag.
pub fn dng_baseline_equivalent(path: &str) -> f32 {
    base_look(path).baseline_ev - ADOBE_BASELINE_OFFSET_EV
}

/// Base look for this file (no baseline for non-RAW files).
pub fn base_look(path: &str) -> BaseLook {
    if path.is_empty() || !crate::formats::is_raw_file(path) {
        return BaseLook::NONE;
    }
    // Offline: the look the file had (source_facts).
    if let Some(facts) = crate::source_facts::registered(path)
        && let Some(look) = &facts.base_look
    {
        return look.to_base_look();
    }
    if let Some(value) = cache().lock().unwrap_or_else(|e| e.into_inner()).get(path) {
        return *value;
    }
    let value = if path.to_ascii_lowercase().ends_with(".dng") {
        let tags = read_dng_tags(path).unwrap_or_default();
        if tags.profile_curve.is_some()
            && !tags.gain_table_map
            && !tags.model_code_maker
            && !adobe_has_profile(&tags.unique_camera_model)
        {
            // A camera Adobe has no profile for (DJI Osmo Action, Mavic 3
            // Pro Cine, Sigma sd Quattro): Camera Raw renders the embedded
            // profile, its tone curve included, at the file's
            // BaselineExposure.
            BaseLook {
                baseline_ev: tags.baseline + PRORAW_OFFSET_EV,
                camera_raw: true,
                neutral_colour: true,
                profile_curve: tags.profile_curve,
                no_black_render: tags.no_black_render,
                phone_defaults: false,
            }
        } else if tags.gain_table_map && tags.profile_curve.is_some() {
            // Apple ProRAW and Pixel 8 Pro: Camera Raw renders them with the
            // embedded profile and its local tone map (applied at decode),
            // which is made for the embedded curve.
            BaseLook {
                baseline_ev: tags.baseline + PRORAW_OFFSET_EV,
                camera_raw: true,
                neutral_colour: true,
                profile_curve: tags.profile_curve,
                no_black_render: tags.no_black_render,
                phone_defaults: false,
            }
        } else {
            // Other DNGs: the same offset to the file's BaselineExposure as
            // proprietary RAWs get to Adobe's (78 DNGs of 41 makers sat a
            // median 0.15 EV below Camera Raw's midtones with +0.25).
            BaseLook {
                baseline_ev: tags.baseline
                    + ADOBE_BASELINE_OFFSET_EV
                    + if tags.gain_table_map {
                        GAIN_MAP_OFFSET_EV
                    } else {
                        0.0
                    },
                camera_raw: true,
                neutral_colour: true,
                profile_curve: None,
                // Galaxy Expert RAW (gain table map, no curve of its own):
                // without the black ramp, which matches Camera Raw.
                no_black_render: tags.no_black_render || tags.gain_table_map,
                phone_defaults: tags.phone_defaults,
            }
        }
    } else {
        // Fujifilm stores how much darker it recorded the RAW than the
        // metered exposure (more above ISO ~1250, where the gain stops
        // rising: −0.49 to −3.91 EV on a GFX 50R); Camera Raw pushes the
        // file back by it. Other makers: the measured Lightroom offset.
        let fujifilm_bias = path
            .to_ascii_lowercase()
            .ends_with(".raf")
            .then(|| read_raf_exposure_bias(path))
            .flatten();
        let adobe = adobe_baseline_for_path(path, fujifilm_bias);
        BaseLook {
            baseline_ev: match adobe {
                Some(baseline) => {
                    baseline + ADOBE_BASELINE_OFFSET_EV + adobe_white_correction_for_path(path)
                        - fujifilm_dynamic_range_ev_for_path(path)
                }
                None => fujifilm_bias
                    .map(|bias| -bias)
                    .unwrap_or(PROPRIETARY_CAMERA_RAW_EV),
            },
            camera_raw: true,
            neutral_colour: true,
            profile_curve: None,
            no_black_render: false,
            phone_defaults: false,
        }
    };
    let mut cache = cache().lock().unwrap_or_else(|e| e.into_inner());
    if cache.len() > 4096 {
        cache.clear();
    }
    cache.insert(path.to_string(), value);
    value
}

/// Adobe's BaselineExposure for this file (camera_exposure.rs rules with
/// the file's capture details), `None` for cameras without a value.
fn adobe_baseline_for_path(path: &str, fujifilm_bias: Option<f32>) -> Option<f32> {
    // rawler's metadata readers panic on some unsupported variants (X3F);
    // a missing baseline is the right outcome for those.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        adobe_baseline_for_path_inner(path, fujifilm_bias)
    }))
    .ok()
    .flatten()
}

fn adobe_baseline_for_path_inner(path: &str, fujifilm_bias: Option<f32>) -> Option<f32> {
    let (make, model, canonical, iso) = read_camera(path)?;
    // Regional names ("EOS Rebel T3i") use their base model's rules
    // ("EOS 600D") unless the table has the name itself.
    let model = if !crate::camera_exposure::has_entry(&make, &model)
        && crate::camera_exposure::has_entry(&make, &canonical)
    {
        canonical
    } else {
        model
    };
    let lower = path.to_ascii_lowercase();
    let flags = if [".pef", ".orf", ".ori"]
        .iter()
        .any(|ext| lower.ends_with(ext))
    {
        crate::capture_info::read_maker_flags(path)
    } else {
        Default::default()
    };
    let mut capture = crate::camera_exposure::Capture {
        iso,
        fujifilm_bias,
        canon: crate::capture_info::read_canon(path),
        pentax_highlight_correction: flags.pentax_highlight_correction,
        olympus_calibration: flags.olympus_calibration,
        fujifilm_dynamic_range: lower
            .ends_with(".raf")
            .then(|| crate::capture_info::fujifilm_dynamic_range(path))
            .flatten(),
        extension: crate::camera_exposure::extension_key(path),
        ..Default::default()
    };
    if crate::camera_exposure::needs_image_shape(&make, &model)
        && let Some((width, multi_channel)) = read_image_shape(path)
    {
        capture.width = Some(width);
        capture.multi_channel = multi_channel;
    }
    crate::camera_exposure::adobe_baseline(&make, &model, &capture)
}

/// The white-level extension Camera Raw applies to Fujifilm SuperCCD SR /
/// EXR captures in a wide dynamic-range mode (camera_exposure::
/// FUJIFILM_DR_BASELINE_MODELS); 0 elsewhere.
fn fujifilm_dynamic_range_ev_for_path(path: &str) -> f32 {
    if !path.to_ascii_lowercase().ends_with(".raf") {
        return 0.0;
    }
    std::panic::catch_unwind(|| {
        let (make, model, _, _) = read_camera(path)?;
        Some(crate::camera_exposure::fujifilm_white_extension_ev(
            &make,
            &model,
            crate::capture_info::fujifilm_dynamic_range(path),
        ))
    })
    .ok()
    .flatten()
    .unwrap_or(0.0)
}

/// EV that makes rawler's white level render like Adobe's for this file
/// (camera_exposure::ADOBE_WHITE_EV); 0 when they agree.
fn adobe_white_correction_for_path(path: &str) -> f32 {
    std::panic::catch_unwind(|| {
        let (make, model, canonical, iso) = read_camera(path)?;
        let model = if crate::camera_exposure::has_white_correction(&make, &model) {
            model
        } else {
            canonical
        };
        Some(crate::camera_exposure::adobe_white_correction(
            &make, &model, iso,
        ))
    })
    .ok()
    .flatten()
    .unwrap_or(0.0)
}

pub use crate::capture_info::read_camera;

/// Decoded width and whether the file has several colour channels, from a
/// metadata-only ("dummy") decode. Only for cameras whose Adobe value
/// depends on the capture size (Phase One Sensor+, Sony α1 II Medium/Small).
fn read_image_shape(path: &str) -> Option<(u32, bool)> {
    let source = rawler::rawsource::RawSource::new(std::path::Path::new(path)).ok()?;
    let decoder = rawler::get_decoder(&source).ok()?;
    let image = decoder
        .raw_image(&source, &rawler::decoders::RawDecodeParams::default(), true)
        .ok()?;
    Some((image.width as u32, image.cpp > 1))
}

/// Fujifilm's RawExposureBias (tag 0x9650 of the RAF CFA header, signed
/// rational in EV).
fn read_raf_exposure_bias(path: &str) -> Option<f32> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let mut header = [0u8; 100];
    file.read_exact(&mut header).ok()?;
    if &header[..8] != b"FUJIFILM" {
        return None;
    }
    let be32 = |b: &[u8]| u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
    let (offset, length) = (
        be32(&header[92..96]) as u64,
        be32(&header[96..100]) as usize,
    );
    if !(4..=(1 << 20)).contains(&length) {
        return None;
    }
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut block = vec![0u8; length];
    file.read_exact(&mut block).ok()?;
    parse_raf_exposure_bias(&block)
}

fn parse_raf_exposure_bias(block: &[u8]) -> Option<f32> {
    let count = u32::from_be_bytes(block.get(..4)?.try_into().ok()?) as usize;
    let mut at = 4;
    for _ in 0..count.min(4096) {
        let tag = u16::from_be_bytes(block.get(at..at + 2)?.try_into().ok()?);
        let size = u16::from_be_bytes(block.get(at + 2..at + 4)?.try_into().ok()?) as usize;
        let value = block.get(at + 4..at + 4 + size)?;
        if tag == 0x9650 && size >= 4 {
            let numerator = i16::from_be_bytes([value[0], value[1]]) as f32;
            let denominator = u16::from_be_bytes([value[2], value[3]]) as f32;
            return (denominator > 0.0)
                .then(|| numerator / denominator)
                .filter(|bias| bias.abs() <= 8.0);
        }
        at += 4 + size;
    }
    None
}

#[derive(Default)]
struct DngTags {
    baseline: f32,
    gain_table_map: bool,
    profile_curve: Option<[f32; PROFILE_CURVE_SAMPLES]>,
    no_black_render: bool,
    unique_camera_model: String,
    /// Samsung and Apple name phones by model code (SM-S901E, iPhone13,3),
    /// which Camera Raw maps to its profiles internally (Galaxy S22, …).
    model_code_maker: bool,
    phone_defaults: bool,
}

const PROFILE_TONE_CURVE_TAG: u16 = 50940;
const DEFAULT_BLACK_RENDER_TAG: u16 = 51110;
const UNIQUE_CAMERA_MODEL_TAG: u16 = 50708;

/// UniqueCameraModel of every camera profile Camera Raw 18.7 ships (Adobe
/// Standard, Camera Matching, …), sorted.
const ADOBE_PROFILE_MODELS: &str = include_str!("adobe_profile_models.txt");

/// Whether Camera Raw has its own profile for a DNG's UniqueCameraModel
/// (exactly, or per camera module: "Google Pixel 6 Pro Rear Main Camera").
/// Phones Camera Raw renders with its own profile and its phone defaults
/// (`PHONE_WHITES`): iPhones (UniqueCameraModel "iPhone10,1 back camera"),
/// Pixels Adobe has a profile for, and Galaxy phones named by model code or
/// name. Not: Galaxy files whose UniqueCameraModel is a firmware string
/// (SM-G930F "D12LLJH03SM …") or carries a vendor suffix (SM-G920F), and the
/// Galaxy S10 (SM-G973F), which Camera Raw opens with its generic defaults.
fn camera_raw_phone_defaults(make: &str, unique_camera_model: &str) -> bool {
    let model = unique_camera_model.trim();
    if make.contains("APPLE") {
        model.starts_with("iPhone")
            && (model.ends_with(" back camera") || model.ends_with(" front camera"))
    } else if make.contains("GOOGLE") {
        model.contains("Pixel") && adobe_has_profile(model)
    } else if make.contains("SAMSUNG") {
        let code = model.strip_prefix("SAMSUNG-").unwrap_or(model);
        let is_code = code.strip_prefix("SM-").is_some_and(|rest| {
            let mut chars = rest.chars();
            chars.next().is_some_and(|c| c.is_ascii_uppercase())
                && rest.len() >= 4
                && rest[1..4].chars().all(|c| c.is_ascii_digit())
                && rest.chars().all(|c| c.is_ascii_alphanumeric())
        });
        (is_code && !code.starts_with("SM-G973")) || model.starts_with("Galaxy ")
    } else {
        false
    }
}

fn adobe_has_profile(unique_camera_model: &str) -> bool {
    let model = unique_camera_model.trim();
    !model.is_empty()
        && ADOBE_PROFILE_MODELS.lines().any(|line| {
            line == model
                || line
                    .strip_prefix(model)
                    .is_some_and(|rest| rest.starts_with(' ') && rest.ends_with(" Camera"))
        })
}

/// Samples a DNG ProfileToneCurve (x, y pairs in 0..1, linear) as display EV
/// every 0.25 scene EV from −12 EV.
pub fn sample_profile_curve(pairs: &[f32]) -> Option<[f32; PROFILE_CURVE_SAMPLES]> {
    if pairs.len() < 4 || !pairs.len().is_multiple_of(2) {
        return None;
    }
    let points: Vec<(f32, f32)> = pairs
        .as_chunks::<2>()
        .0
        .iter()
        .map(|p| (p[0], p[1]))
        .collect();
    let at = |x: f32| {
        let i = points
            .partition_point(|p| p.0 < x)
            .clamp(1, points.len() - 1);
        let (a, b) = (points[i - 1], points[i]);
        let t = if b.0 > a.0 {
            ((x - a.0) / (b.0 - a.0)).clamp(0.0, 1.0)
        } else {
            0.0
        };
        a.1 + (b.1 - a.1) * t
    };
    let mut out = [0.0; PROFILE_CURVE_SAMPLES];
    for (i, value) in out.iter_mut().enumerate() {
        let x = 2f32.powf(-12.0 + 0.25 * i as f32);
        *value = at(x).max(1e-12).log2();
    }
    Some(out)
}

/// BaselineExposure (+ offset), maker, and the embedded profile pieces.
fn read_dng_tags(path: &str) -> Option<DngTags> {
    let file = std::fs::File::open(path).ok()?;
    let exif = exif::Reader::new()
        .read_from_container(&mut std::io::BufReader::new(file))
        .ok()?;
    let make = exif
        .get_field(exif::Tag::Make, exif::In::PRIMARY)
        .map(|field| field.display_value().to_string().to_ascii_uppercase())
        .unwrap_or_default();
    let model_code_maker = make.contains("SAMSUNG") || make.contains("APPLE");
    let rational = |tag: u16| {
        exif.get_field(exif::Tag(exif::Context::Tiff, tag), exif::In::PRIMARY)
            .and_then(|field| match &field.value {
                exif::Value::SRational(values) => values.first().map(|v| v.to_f64() as f32),
                exif::Value::Rational(values) => values.first().map(|v| v.to_f64() as f32),
                _ => None,
            })
    };
    // A malformed rational (YI M1: 0/0) reads as NaN and rendered black.
    let finite = |value: Option<f32>| value.filter(|v| v.is_finite()).unwrap_or(0.0);
    let baseline =
        finite(rational(BASELINE_EXPOSURE_TAG)) + finite(rational(BASELINE_EXPOSURE_OFFSET_TAG));
    let profile_curve = exif
        .get_field(
            exif::Tag(exif::Context::Tiff, PROFILE_TONE_CURVE_TAG),
            exif::In::PRIMARY,
        )
        .and_then(|field| match &field.value {
            exif::Value::Float(values) => sample_profile_curve(values),
            _ => None,
        });
    let (gain_table_map, raw_no_black_render) = raw_ifd_tags(path);
    // DefaultBlackRender: 0 = Auto, 1 = None (IFD 0 or the raw IFD).
    let no_black_render = raw_no_black_render
        || exif
            .get_field(
                exif::Tag(exif::Context::Tiff, DEFAULT_BLACK_RENDER_TAG),
                exif::In::PRIMARY,
            )
            .and_then(|field| field.value.get_uint(0))
            == Some(1);
    let unique_camera_model = exif
        .get_field(
            exif::Tag(exif::Context::Tiff, UNIQUE_CAMERA_MODEL_TAG),
            exif::In::PRIMARY,
        )
        .and_then(|field| match &field.value {
            exif::Value::Ascii(values) => values
                .first()
                .map(|bytes| String::from_utf8_lossy(bytes).trim().to_string()),
            _ => None,
        })
        .unwrap_or_default();
    let phone_defaults = !gain_table_map && camera_raw_phone_defaults(&make, &unique_camera_model);
    let baseline = baseline
        + if phone_defaults && make.contains("APPLE") {
            IPHONE_OFFSET_EV
        } else {
            0.0
        };
    Some(DngTags {
        baseline,
        gain_table_map,
        phone_defaults,
        profile_curve,
        no_black_render,
        model_code_maker,
        unique_camera_model,
    })
}

/// Tags of a DNG's raw IFD (via rawler; kamadak-exif does not read
/// sub-IFDs): a ProfileGainTableMap Rawtonic can apply, and
/// DefaultBlackRender = None (ProRAW stores it there).
fn raw_ifd_tags(path: &str) -> (bool, bool) {
    let Ok(bytes) = std::fs::read(path) else {
        return (false, false);
    };
    let source = rawler::rawsource::RawSource::new_from_slice(&bytes);
    let Ok(decoder) = rawler::get_decoder(&source) else {
        return (false, false);
    };
    let Ok(Some(ifd)) = decoder.ifd(rawler::decoders::WellKnownIFD::VirtualDngRawTags) else {
        return (false, false);
    };
    let gain_table_map = match ifd
        .get_entry(rawler::tags::DngTag::ProfileGainTableMap)
        .map(|entry| &entry.value)
    {
        Some(rawler::formats::tiff::Value::Undefined(data))
        | Some(rawler::formats::tiff::Value::Byte(data)) => {
            crate::dng_opcodes::parse_gain_table_map(data).is_some()
        }
        _ => false,
    };
    let no_black_render = matches!(
        decoder.ifd(rawler::decoders::WellKnownIFD::Raw),
        Ok(Some(raw)) if raw
            .get_entry(rawler::tags::DngTag::DefaultBlackRender)
            .is_some_and(|entry| entry.value.force_u32(0) == 1)
    );
    (gain_table_map, no_black_render)
}

#[cfg(test)]
mod adobe_check {
    /// Compares the table against Adobe DNG Converter's values: set
    /// `RAWTONIC_ADOBE_RESULTS` to a JSON-lines file with `file` (relative
    /// to `RAWTONIC_RAW_ROOT`), `BaselineExposure` and optional `raf_bias`
    /// per converted RAW, and the test prints every file off by more than
    /// 0.02 EV plus a summary (`RAWTONIC_PRINT_ALL` set: every file with
    /// camera and value, for export regression runs).
    #[test]
    #[ignore]
    fn compare_with_dng_converter_values() {
        let (Ok(results), Ok(root)) = (
            std::env::var("RAWTONIC_ADOBE_RESULTS"),
            std::env::var("RAWTONIC_RAW_ROOT"),
        ) else {
            return;
        };
        let text = std::fs::read_to_string(results).expect("results file");
        let (mut compared, mut exact, mut missing) = (0usize, 0usize, 0usize);
        let mut errors: Vec<(f32, String)> = Vec::new();
        for line in text.lines() {
            let Ok(row) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            let (Some(file), Some(adobe)) =
                (row["file"].as_str(), row["BaselineExposure"].as_f64())
            else {
                continue;
            };
            if file.to_ascii_lowercase().ends_with(".dng") {
                continue;
            }
            let path = format!("{root}/{file}");
            let bias = super::read_raf_exposure_bias(&path);
            let value = super::adobe_baseline_for_path(&path, bias);
            if std::env::var_os("RAWTONIC_PRINT_ALL").is_some() {
                let camera = super::read_camera(&path);
                println!(
                    "ALL\t{file}\t{}\t{}\t{}",
                    camera.as_ref().map_or("", |c| c.0.as_str()),
                    camera.as_ref().map_or("", |c| c.1.as_str()),
                    value.map_or(String::from("-"), |v| format!("{v:.3}"))
                );
            }
            match value {
                None => {
                    missing += 1;
                    if std::env::var_os("RAWTONIC_PRINT_MISSING").is_some() {
                        println!("MISSING\t{file}\t{adobe:.2}");
                    }
                }
                Some(value) => {
                    compared += 1;
                    let error = value - adobe as f32;
                    if error.abs() <= 0.02 {
                        exact += 1;
                    } else {
                        errors.push((error, file.to_string()));
                    }
                }
            }
        }
        errors.sort_by(|a, b| b.0.abs().total_cmp(&a.0.abs()));
        for (error, file) in &errors {
            println!("{error:+.2}  {file}");
        }
        println!(
            "compared {compared}, within 0.02 EV {exact}, off {}, no table value {missing}",
            errors.len()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{BaseLook, base_look};
    use crate::image_processing::GlobalAdjustments;

    /// The measures of the decode rendered win over the last full decode's
    /// of the same file (another recipe's decode may have come since).
    #[test]
    fn the_decode_rendered_brings_its_own_measures() {
        let path = "/nonexistent/measures-test.NEF";
        crate::noise_level::record(path, 0.03);
        crate::image_key::record(path, [0.2, 0.7]);
        let own = crate::image_loader::DecodeMeasures {
            noise_level: Some(0.01),
            image_key: Some([0.1, 0.6]),
        };
        let mut global = GlobalAdjustments::default();
        super::apply_base_look(&mut global, path, Some(&own));
        assert_eq!(
            (global.noise_level, global.image_key_low, global.image_key),
            (0.01, 0.1, 0.6)
        );
        // Without a decode (a preview): the last full decode's.
        let mut global = GlobalAdjustments::default();
        super::apply_base_look(&mut global, path, None);
        assert_eq!(
            (global.noise_level, global.image_key_low, global.image_key),
            (0.03, 0.2, 0.7)
        );
    }

    #[test]
    fn non_raw_and_unknown_files_have_no_baseline() {
        assert_eq!(base_look(""), BaseLook::NONE);
        assert_eq!(base_look("/nonexistent/photo.jpg"), BaseLook::NONE);
        // RAWs render with the Camera Raw look and neutral colour.
        let arw = base_look("/nonexistent/photo.arw");
        assert!(arw.camera_raw && arw.neutral_colour);
        assert_eq!(arw.baseline_ev, 0.72);
        // A DNG whose tags cannot be read gets the Adobe offset.
        let dng = base_look("/nonexistent/photo.dng");
        assert!(dng.camera_raw && dng.neutral_colour);
        assert_eq!(dng.baseline_ev, 0.41);
    }

    #[test]
    fn raf_exposure_bias_is_read_from_the_cfa_header() {
        // Two entries: an unrelated tag, then 0x9650 = -291/100 EV.
        let mut block = vec![0, 0, 0, 2];
        block.extend_from_slice(&[0x01, 0x00, 0x00, 0x04, 0x18, 0x42, 0x24, 0x00]);
        block.extend_from_slice(&[0x96, 0x50, 0x00, 0x04]);
        block.extend_from_slice(&(-291i16).to_be_bytes());
        block.extend_from_slice(&100u16.to_be_bytes());
        assert_eq!(super::parse_raf_exposure_bias(&block), Some(-2.91));
        assert_eq!(super::parse_raf_exposure_bias(&block[..12]), None);
    }

    #[test]
    fn profile_curve_is_sampled_in_log_log() {
        // Identity curve: display EV equals scene EV at every sample.
        let identity = super::sample_profile_curve(&[0.0, 0.0, 1.0, 1.0]).unwrap();
        for (i, value) in identity.iter().enumerate() {
            assert!(
                (value - (-12.0 + 0.25 * i as f32)).abs() < 1e-3,
                "{i}: {value}"
            );
        }
        assert!(super::sample_profile_curve(&[0.0, 0.0, 1.0]).is_none());
        // The curve reaches the shader only for the Camera Raw look.
        let look = BaseLook {
            baseline_ev: 0.0,
            camera_raw: true,
            neutral_colour: true,
            profile_curve: Some(identity),
            no_black_render: false,
            phone_defaults: false,
        };
        let mut global = GlobalAdjustments::default();
        global.tonemapper_mode = 2;
        look.apply(&mut global);
        assert_eq!(global.profile_curve[15][3], 1.0);
        assert!((global.profile_curve[12][0] - 0.0).abs() < 1e-3);
        let mut classic = GlobalAdjustments::default();
        look.apply(&mut classic);
        assert_eq!(classic.profile_curve[15][3], 0.0);
        assert_eq!(global.profile_curve[15][2], 0.0);
        // DefaultBlackRender = None turns the black ramp off.
        let unblacked = BaseLook {
            no_black_render: true,
            ..look
        };
        let mut global = GlobalAdjustments::default();
        global.tonemapper_mode = 2;
        unblacked.apply(&mut global);
        assert_eq!(global.profile_curve[15][2], 1.0);
    }

    #[test]
    fn adobe_profile_models_cover_supported_dng_cameras() {
        assert!(super::adobe_has_profile("SIGMA fp"));
        assert!(super::adobe_has_profile("DJI FC9287"));
        assert!(super::adobe_has_profile("Google Pixel 6 Pro"));
        assert!(!super::adobe_has_profile("DJI Osmo Action"));
        assert!(!super::adobe_has_profile("sd Quattro"));
        assert!(!super::adobe_has_profile(""));
        assert!(!super::adobe_has_profile("SIGMA"));
    }

    #[test]
    fn phone_defaults_follow_camera_raw() {
        use super::camera_raw_phone_defaults as phone;
        assert!(phone("APPLE", "iPhone10,1 back camera"));
        assert!(phone("GOOGLE", "Google Pixel 6 Pro"));
        assert!(phone("SAMSUNG", "SM-G960F"));
        assert!(phone("SAMSUNG", "SAMSUNG-SM-G930A"));
        assert!(phone("SAMSUNG", "Galaxy S23 Ultra"));
        assert!(!phone("SAMSUNG", "SM-G973F"));
        assert!(!phone("SAMSUNG", "SM-G920F-samsung-samsung"));
        assert!(!phone("SAMSUNG", "D12LLJH03SM D12LLLC01GM."));
        assert!(!phone("SAMSUNG", "SAMSUNG GX20"));
        assert!(!phone("GOOGLE", "Google Pixel 3a"));
        assert!(!phone("HUAWEI", "ANE-LX1"));
    }

    #[test]
    fn camera_raw_look_only_replaces_the_standard_tone_mapper() {
        let look = BaseLook {
            baseline_ev: 0.94,
            camera_raw: true,
            neutral_colour: false,
            profile_curve: None,
            no_black_render: false,
            phone_defaults: false,
        };
        for (mode, expected) in [(0u32, 0u32), (1, 1), (2, 3)] {
            let mut global = GlobalAdjustments::default();
            global.tonemapper_mode = mode;
            look.apply(&mut global);
            assert_eq!(global.tonemapper_mode, expected);
            assert_eq!(global.baseline_exposure, 0.94);
            // Without neutral colour the flag stays 0 (colour stand-ins).
            assert_eq!(global.profile_curve[15][3], 0.0);
        }
        let neutral = BaseLook {
            neutral_colour: true,
            ..look
        };
        let mut global = GlobalAdjustments::default();
        global.tonemapper_mode = 2;
        neutral.apply(&mut global);
        assert_eq!(global.profile_curve[15][3], 2.0);
    }
}

#[cfg(test)]
mod capture_debug {
    /// Prints camera, ISO, Canon capture details and image shape of the
    /// files in `RAWTONIC_CAPTURE_FILES` (newline separated).
    #[test]
    #[ignore]
    fn print_capture_of_env_files() {
        let Ok(files) = std::env::var("RAWTONIC_CAPTURE_FILES") else {
            return;
        };
        for path in files.lines().filter(|p| !p.is_empty()) {
            let orientation = rawler::rawsource::RawSource::new(std::path::Path::new(path))
                .ok()
                .and_then(|source| {
                    let decoder = rawler::get_decoder(&source).ok()?;
                    decoder
                        .raw_metadata(&source, &rawler::decoders::RawDecodeParams::default())
                        .ok()
                        .map(|m| m.exif.orientation)
                });
            println!(
                "{path}: orientation {orientation:?} camera {:?} canon {:?} flags {:?} shape {:?} adobe {:?}",
                super::read_camera(path),
                crate::capture_info::read_canon(path),
                crate::capture_info::read_maker_flags(path),
                super::read_image_shape(path),
                super::adobe_baseline_for_path(path, None)
            );
        }
    }
}
