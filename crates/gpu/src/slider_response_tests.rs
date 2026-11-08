//! Slider response regression: every global slider's response on generated
//! charts, pinned in `tests/data/slider_response.json`, so a change to a
//! slider's behaviour cannot pass unnoticed (Shadows once gained a texture
//! term no test saw).
//!
//! The charts are made here (no photographs): a log grey ramp, textured
//! patches at four grey levels (gratings of 2 and 8 px period, band-limited
//! noise, a flat patch), a hue wheel at three chroma levels and an edge.
//! Each is rendered through the export path (`process_and_get_dynamic_image_inner`,
//! `ReadbackShader::Export`, 16 bit) with a new RAW edit's recipe versions,
//! the Standard look (the global sliders also under the Camera Raw look,
//! `camera-raw/…`, which every RAW file renders with) and its image key measured as the app measures a
//! decode; Sharpening and noise reduction stay at 0, so the texture metrics
//! show the slider alone. Metrics are CIELAB (D65) of the sRGB output:
//!
//! - `ramp_dl`: mean ΔL* per band of the unchanged render's L* (0–10, …, 90–100);
//! - `tex_rms`: L* rms of the texture in each patch (grey level × pattern),
//!   `tex_dl`: ΔL* of the flat patches;
//! - `hue_dl`, `hue_dc`, `hue_dh`: ΔL*, ΔC* and hue change (degrees) of each
//!   hue-wheel patch (chroma row × 12 hue sectors);
//! - `edge`: overshoot and undershoot (L* beyond the plateau next to the
//!   edge), ΔL* of the dark and bright plateau.
//!
//! The unchanged render (`baseline`) pins the absolute values (`ramp_l`,
//! `tex_l`, `hue_l`, `hue_c`, `hue_h`, the edge's plateau L*). A hue change is
//! compared as ΔH* (degrees × chroma), so near-grey patches do not flicker.
//!
//! Run: `cargo test -p rawtonic-gpu -- --ignored slider_response`. An
//! intended change regenerates the file with `UPDATE_SLIDER_RESPONSE=1`
//! (the PR must explain the change); `SLIDER_RESPONSE_OUT=<file>` writes the
//! measurement elsewhere as well (to compare backends).

use super::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fmt::Write as _;

const WIDTH: u32 = 512;
const HEIGHT: u32 = 256;

const DATA_FILE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/data/slider_response.json"
);

/// Slider values: the four of the task, in the slider's own units
/// (Exposure in EV, its ±5 range would only clip the charts).
const AMOUNTS: [f64; 4] = [-100.0, -50.0, 50.0, 100.0];
const EXPOSURE_EV: [f64; 4] = [-2.0, -1.0, 1.0, 2.0];

const GLOBAL_SLIDERS: [&str; 13] = [
    "exposure",
    "contrast",
    "highlights",
    "shadows",
    "whites",
    "blacks",
    "clarity",
    "structure",
    "dehaze",
    "vibrance",
    "saturation",
    "temperature",
    "tint",
];
const HSL_BANDS: [&str; 8] = [
    "reds", "oranges", "yellows", "greens", "aquas", "blues", "purples", "magentas",
];
const HSL_PROPERTIES: [&str; 3] = ["hue", "saturation", "luminance"];

/// Texture chart: grey levels (scene linear) of its four columns and its
/// rows' patterns; a pattern modulates the level by ±`TEXTURE_EV`.
const TEXTURE_LEVELS: [f32; 4] = [0.02, 0.06, 0.18, 0.5];
const TEXTURE_PATTERNS: [&str; 3] = ["2px", "8px", "noise"];
const TEXTURE_EV: f32 = 0.3;
/// Hue wheel: CIELAB chroma of its rows, at L* 50 (scene grey 0.184).
const HUE_CHROMAS: [f64; 3] = [15.0, 35.0, 55.0];
const HUE_SECTORS: usize = 12;
/// Edge chart: the two sides' scene levels.
const EDGE_LEVELS: [f32; 2] = [0.03, 0.4];

/// The default tolerance (CIELAB units) of a metric the file names none for.
const DEFAULT_TOLERANCE: f64 = 0.2;

/// The recipe of a new RAW edit (`initialAdjustmentsFor(true)` in
/// src/utils/adjustments.ts) as far as these charts render it.
fn base_recipe() -> Value {
    let hsl: serde_json::Map<String, Value> = HSL_BANDS
        .iter()
        .map(|band| {
            (
                band.to_string(),
                json!({"hue": 0, "saturation": 0, "luminance": 0}),
            )
        })
        .collect();
    let mut recipe = versions();
    recipe["toneMapper"] = json!("standard");
    recipe["baseCurve"] = json!("standard");
    recipe["hsl"] = Value::Object(hsl);
    recipe
}

/// The recipe versions the file is pinned at: a new edit's.
fn versions() -> Value {
    use rawtonic_engine::image_processing as ip;
    json!({
        "toneVersion": ip::TONE_VERSION,
        "clarityVersion": ip::CLARITY_VERSION,
        "temperatureVersion": ip::TEMPERATURE_VERSION,
        "renderVersion": ip::RENDER_VERSION,
        "colorNoiseVersion": rawtonic_engine::color_noise::VERSION,
    })
}

/// Prefix of the renders under the Camera Raw look, the look every RAW file
/// gets (`baseline_exposure::base_look`); the others use the Standard look.
const CAMERA_RAW_PREFIX: &str = "camera-raw/";

fn is_baseline(name: &str) -> bool {
    name.strip_prefix(CAMERA_RAW_PREFIX).unwrap_or(name) == "baseline"
}

/// The base look a new RAW file renders with, without the file's own
/// baseline exposure, curve or phone defaults.
fn camera_raw_look() -> rawtonic_engine::baseline_exposure::BaseLook {
    rawtonic_engine::baseline_exposure::BaseLook {
        baseline_ev: 0.0,
        camera_raw: true,
        neutral_colour: true,
        profile_curve: None,
        no_black_render: false,
        phone_defaults: false,
        capture_gain_ev: 0.0,
    }
}

/// Every pinned render: its name and recipe. The global sliders are pinned
/// under both looks: Standard's and Camera Raw's tone code are separate
/// (Whites of tone versions 10 and 12 act only under the latter, which is
/// what every RAW file renders with); the colour mixer only under Standard.
fn renders() -> Vec<(String, Value)> {
    let base = base_recipe();
    let mut out = vec![("baseline".to_string(), base.clone())];
    let sign = |v: f64| if v > 0.0 { "+" } else { "" };
    for prefix in ["", CAMERA_RAW_PREFIX] {
        if !prefix.is_empty() {
            out.push((format!("{prefix}baseline"), base.clone()));
        }
        for slider in GLOBAL_SLIDERS {
            let amounts = if slider == "exposure" {
                EXPOSURE_EV
            } else {
                AMOUNTS
            };
            for amount in amounts {
                let mut recipe = base.clone();
                recipe[slider] = json!(amount);
                out.push((format!("{prefix}{slider}={}{amount}", sign(amount)), recipe));
            }
        }
    }
    for band in HSL_BANDS {
        for property in HSL_PROPERTIES {
            for amount in AMOUNTS {
                let mut recipe = base.clone();
                recipe["hsl"][band][property] = json!(amount);
                out.push((
                    format!("hsl.{band}.{property}={}{amount}", sign(amount)),
                    recipe,
                ));
            }
        }
    }
    out
}

/// Metric names and the number of values each holds.
fn metric_shape(baseline: bool) -> Vec<(&'static str, usize)> {
    let texture = TEXTURE_LEVELS.len() * TEXTURE_PATTERNS.len();
    let hue = HUE_CHROMAS.len() * HUE_SECTORS;
    let (ramp, tex_l, hue_l, hue_c, hue_h) = if baseline {
        ("ramp_l", "tex_l", "hue_l", "hue_c", "hue_h")
    } else {
        ("ramp_dl", "tex_dl", "hue_dl", "hue_dc", "hue_dh")
    };
    vec![
        (ramp, 10),
        ("tex_rms", texture),
        (tex_l, TEXTURE_LEVELS.len()),
        (hue_l, hue),
        (hue_c, hue),
        (hue_h, hue),
        ("edge", 4),
    ]
}

/// The value labels of each metric, written into the file for its readers.
fn labels() -> Value {
    let ramp: Vec<String> = (0..10)
        .map(|b| format!("L* {}-{}", b * 10, b * 10 + 10))
        .collect();
    let texture: Vec<String> = TEXTURE_PATTERNS
        .iter()
        .flat_map(|p| TEXTURE_LEVELS.iter().map(move |l| format!("{p} @ {l}")))
        .collect();
    let levels: Vec<String> = TEXTURE_LEVELS
        .iter()
        .map(|l| format!("flat @ {l}"))
        .collect();
    let hue: Vec<String> = HUE_CHROMAS
        .iter()
        .flat_map(|c| (0..HUE_SECTORS).map(move |s| format!("C* {c} h {}", s * 360 / HUE_SECTORS)))
        .collect();
    json!({
        "ramp": ramp,
        "tex_rms": texture,
        "tex": levels,
        "hue": hue,
        "edge": ["overshoot", "undershoot", "dark plateau", "bright plateau"],
    })
}

// ---- charts ---------------------------------------------------------------

fn chart(f: impl Fn(u32, u32) -> [f32; 3]) -> DynamicImage {
    DynamicImage::ImageRgb32F(ImageBuffer::from_fn(WIDTH, HEIGHT, |x, y| {
        image::Rgb(f(x, y))
    }))
}

/// Scene EV of a ramp column: 2^-12 to 2^2 (black to past white).
fn ramp_ev(x: u32) -> f32 {
    -12.0 + 14.0 * x as f32 / (WIDTH - 1) as f32
}

fn ramp_chart() -> DynamicImage {
    chart(|x, _| {
        let v = ramp_ev(x).exp2();
        [v, v, v]
    })
}

const TEXTURE_CELL: (u32, u32) = (WIDTH / 4, HEIGHT / 4);

/// Band-limited noise of unit rms with a fixed seed: white noise, blurred
/// twice with a 3 × 3 box (most energy at periods of 4–8 px).
fn band_limited_noise() -> Vec<f32> {
    let (w, h) = (WIDTH as usize, HEIGHT as usize);
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut noise: Vec<f32> = (0..w * h)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 40) as f32 / (1u64 << 24) as f32 - 0.5
        })
        .collect();
    for _ in 0..2 {
        let source = noise.clone();
        for y in 0..h {
            for x in 0..w {
                let mut sum = 0.0;
                for dy in -1i32..=1 {
                    for dx in -1i32..=1 {
                        let xx = (x as i32 + dx).clamp(0, w as i32 - 1) as usize;
                        let yy = (y as i32 + dy).clamp(0, h as i32 - 1) as usize;
                        sum += source[yy * w + xx];
                    }
                }
                noise[y * w + x] = sum / 9.0;
            }
        }
    }
    let rms = (noise.iter().map(|v| v * v).sum::<f32>() / noise.len() as f32).sqrt();
    noise.iter().map(|v| v / rms).collect()
}

fn texture_chart() -> DynamicImage {
    let noise = band_limited_noise();
    chart(|x, y| {
        let level = TEXTURE_LEVELS[(x / TEXTURE_CELL.0) as usize];
        let s = match y / TEXTURE_CELL.1 {
            0 => {
                if x % 2 == 0 {
                    1.0
                } else {
                    -1.0
                }
            }
            1 => (std::f32::consts::TAU * x as f32 / 8.0).sin(),
            2 => (noise[(y * WIDTH + x) as usize] * 0.5).clamp(-1.0, 1.0),
            _ => 0.0,
        };
        let v = level * (TEXTURE_EV * s).exp2();
        [v, v, v]
    })
}

const HUE_CELL: (u32, u32) = (42, 80);
const HUE_ORIGIN: (u32, u32) = (4, 8);

/// Linear sRGB of a CIELAB colour (D65), negative channels clipped.
fn lab_to_linear_srgb([l, a, b]: [f64; 3]) -> [f32; 3] {
    let fy = (l + 16.0) / 116.0;
    let (fx, fz) = (fy + a / 500.0, fy - b / 200.0);
    let inv = |t: f64| {
        if t > 6.0 / 29.0 {
            t * t * t
        } else {
            3.0 * (6.0f64 / 29.0).powi(2) * (t - 4.0 / 29.0)
        }
    };
    let (x, y, z) = (0.95047 * inv(fx), inv(fy), 1.08883 * inv(fz));
    [
        3.2404542 * x - 1.5371385 * y - 0.4985314 * z,
        -0.9692660 * x + 1.8760108 * y + 0.0415560 * z,
        0.0556434 * x - 0.2040259 * y + 1.0572252 * z,
    ]
    .map(|v| v.max(0.0) as f32)
}

fn hue_chart() -> DynamicImage {
    chart(|x, y| {
        let column = x.checked_sub(HUE_ORIGIN.0).map(|x| x / HUE_CELL.0);
        let row = y.checked_sub(HUE_ORIGIN.1).map(|y| y / HUE_CELL.1);
        match (column, row) {
            (Some(c), Some(r))
                if (c as usize) < HUE_SECTORS && (r as usize) < HUE_CHROMAS.len() =>
            {
                let h = (c as f64 * 360.0 / HUE_SECTORS as f64).to_radians();
                let chroma = HUE_CHROMAS[r as usize];
                lab_to_linear_srgb([50.0, chroma * h.cos(), chroma * h.sin()])
            }
            _ => [0.184; 3],
        }
    })
}

fn edge_chart() -> DynamicImage {
    chart(|x, _| [EDGE_LEVELS[(x >= WIDTH / 2) as usize]; 3])
}

// ---- measurement ----------------------------------------------------------

/// CIELAB (D65) of an sRGB-encoded colour.
fn srgb_to_lab(rgb: [f32; 3]) -> [f64; 3] {
    let lin = rgb.map(|e| {
        let e = e as f64;
        if e <= 0.04045 {
            e / 12.92
        } else {
            ((e + 0.055) / 1.055).powf(2.4)
        }
    });
    let x = 0.4124564 * lin[0] + 0.3575761 * lin[1] + 0.1804375 * lin[2];
    let y = 0.2126729 * lin[0] + 0.7151522 * lin[1] + 0.0721750 * lin[2];
    let z = 0.0193339 * lin[0] + 0.1191920 * lin[1] + 0.9503041 * lin[2];
    let f = |t: f64| {
        if t > (6.0f64 / 29.0).powi(3) {
            t.cbrt()
        } else {
            t / (3.0 * (6.0f64 / 29.0).powi(2)) + 4.0 / 29.0
        }
    };
    let (fx, fy, fz) = (f(x / 0.95047), f(y), f(z / 1.08883));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

struct Lab(Vec<[f64; 3]>);

impl Lab {
    fn at(&self, x: u32, y: u32) -> [f64; 3] {
        self.0[(y * WIDTH + x) as usize]
    }
    /// Mean CIELAB over a rectangle.
    fn mean(&self, xs: std::ops::Range<u32>, ys: std::ops::Range<u32>) -> [f64; 3] {
        let mut sum = [0.0; 3];
        let mut n = 0.0;
        for y in ys {
            for x in xs.clone() {
                let p = self.at(x, y);
                for c in 0..3 {
                    sum[c] += p[c];
                }
                n += 1.0;
            }
        }
        sum.map(|s| s / n)
    }
    /// Rms of L* around its mean over a rectangle.
    fn l_rms(&self, xs: std::ops::Range<u32>, ys: std::ops::Range<u32>) -> f64 {
        let mean = self.mean(xs.clone(), ys.clone())[0];
        let mut sum = 0.0;
        let mut n = 0.0;
        for y in ys {
            for x in xs.clone() {
                sum += (self.at(x, y)[0] - mean).powi(2);
                n += 1.0;
            }
        }
        (sum / n).sqrt()
    }
}

/// The four charts, each with its upload hash and measured image key.
struct Charts {
    images: [(DynamicImage, u64, Option<[f32; 3]>); 4],
}

impl Charts {
    fn new() -> Self {
        let keyed = |image: DynamicImage, hash: u64| {
            let key = rawtonic_rawdecode::image_key::measure(&image);
            (image, hash, key)
        };
        Charts {
            images: [
                keyed(ramp_chart(), 0x5157_0001),
                keyed(texture_chart(), 0x5157_0002),
                keyed(hue_chart(), 0x5157_0003),
                keyed(edge_chart(), 0x5157_0004),
            ],
        }
    }
}

fn render(
    context: &GpuContext,
    slot: &GpuProcessorSlot,
    (image, hash, key): &(DynamicImage, u64, Option<[f32; 3]>),
    recipe: &Value,
    camera_raw: bool,
) -> Lab {
    let mut adjustments = get_all_adjustments_from_json(recipe, true, None);
    if camera_raw {
        camera_raw_look().apply(&mut adjustments.global);
        assert_eq!(adjustments.global.tonemapper_mode, 3, "Camera Raw look");
    }
    // As `baseline_exposure::apply_base_look` sets a decode's key.
    let [low, high, median] = key.unwrap_or([0.0; 3]);
    adjustments.global.image_key = high;
    adjustments.global.image_key_low = low;
    adjustments.global.image_key_median = median;
    let output = process_and_get_dynamic_image_inner(
        context,
        slot,
        &Mutex::new(None),
        RenderSource::Image {
            image,
            transform_hash: *hash,
        },
        RenderRequest {
            adjustments,
            mask_bitmaps: &[],
            lut: None,
            roi: None,
        },
        "slider_response_test",
        RenderOutputPrecision::SixteenBit,
        false,
        None,
        None,
        ReadbackShader::Export,
    )
    .unwrap();
    assert_eq!((output.width(), output.height()), (WIDTH, HEIGHT));
    let rgb = output.to_rgb32f();
    Lab(rgb.pixels().map(|p| srgb_to_lab(p.0)).collect())
}

/// One render's measurement of all four charts (absolute values).
struct Measured {
    ramp_columns: Vec<f64>,
    tex_rms: Vec<f64>,
    tex_l: Vec<f64>,
    hue: Vec<[f64; 3]>,
    /// Overshoot, undershoot, dark and bright plateau L*.
    edge: [f64; 4],
}

fn measure(
    context: &GpuContext,
    slot: &GpuProcessorSlot,
    charts: &Charts,
    recipe: &Value,
    camera_raw: bool,
) -> Measured {
    let [ramp, texture, hue, edge] = &charts.images;
    let rows = HEIGHT / 2 - 32..HEIGHT / 2 + 32;

    let lab = render(context, slot, ramp, recipe, camera_raw);
    let ramp_columns = (0..WIDTH)
        .map(|x| lab.mean(x..x + 1, rows.clone())[0])
        .collect();

    let lab = render(context, slot, texture, recipe, camera_raw);
    let inset = 12;
    let cell = |column: u32, row: u32| {
        let (w, h) = TEXTURE_CELL;
        (
            column * w + inset..(column + 1) * w - inset,
            row * h + inset..(row + 1) * h - inset,
        )
    };
    let mut tex_rms = Vec::new();
    for row in 0..TEXTURE_PATTERNS.len() as u32 {
        for column in 0..TEXTURE_LEVELS.len() as u32 {
            let (xs, ys) = cell(column, row);
            tex_rms.push(lab.l_rms(xs, ys));
        }
    }
    let tex_l = (0..TEXTURE_LEVELS.len() as u32)
        .map(|column| {
            let (xs, ys) = cell(column, 3);
            lab.mean(xs, ys)[0]
        })
        .collect();

    let lab = render(context, slot, hue, recipe, camera_raw);
    let mut hue_values = Vec::new();
    for row in 0..HUE_CHROMAS.len() as u32 {
        for column in 0..HUE_SECTORS as u32 {
            let x = HUE_ORIGIN.0 + column * HUE_CELL.0;
            let y = HUE_ORIGIN.1 + row * HUE_CELL.1;
            let [l, a, b] = lab.mean(x + 8..x + HUE_CELL.0 - 8, y + 8..y + HUE_CELL.1 - 8);
            let h = b.atan2(a).to_degrees().rem_euclid(360.0);
            hue_values.push([l, a.hypot(b), h]);
        }
    }

    let lab = render(context, slot, edge, recipe, camera_raw);
    let profile: Vec<f64> = (0..WIDTH)
        .map(|x| lab.mean(x..x + 1, rows.clone())[0])
        .collect();
    let mid = (WIDTH / 2) as usize;
    let plateau =
        |r: std::ops::Range<usize>| profile[r.clone()].iter().sum::<f64>() / r.len() as f64;
    let (dark, bright) = (
        plateau(16..48),
        plateau(WIDTH as usize - 48..WIDTH as usize - 16),
    );
    let overshoot = profile[mid..mid + 64]
        .iter()
        .fold(f64::MIN, |m, &v| m.max(v))
        - bright;
    let undershoot = dark
        - profile[mid - 64..mid]
            .iter()
            .fold(f64::MAX, |m, &v| m.min(v));

    Measured {
        ramp_columns,
        tex_rms,
        tex_l,
        hue: hue_values,
        edge: [overshoot, undershoot, dark, bright],
    }
}

/// Each band's columns: those whose unchanged L* falls into it (8 px kept
/// clear of the borders).
fn ramp_bands(baseline: &Measured) -> Vec<Vec<usize>> {
    let mut bands = vec![Vec::new(); 10];
    for x in 8..WIDTH as usize - 8 {
        let band = (baseline.ramp_columns[x] / 10.0).floor().clamp(0.0, 9.0) as usize;
        bands[band].push(x);
    }
    bands
}

fn band_mean(columns: &[f64], band: &[usize]) -> f64 {
    if band.is_empty() {
        return 0.0;
    }
    band.iter().map(|&x| columns[x]).sum::<f64>() / band.len() as f64
}

fn signed_degrees(d: f64) -> f64 {
    (d + 180.0).rem_euclid(360.0) - 180.0
}

/// The pinned metrics of one render, and the chroma of its hue patches
/// (to weigh hue changes).
fn metrics(
    measured: &Measured,
    baseline: Option<&Measured>,
    bands: &[Vec<usize>],
) -> (BTreeMap<String, Vec<f64>>, Vec<f64>) {
    let mut out = BTreeMap::new();
    let chroma: Vec<f64> = measured.hue.iter().map(|p| p[1]).collect();
    let ramp: Vec<f64> = bands
        .iter()
        .map(|band| band_mean(&measured.ramp_columns, band))
        .collect();
    out.insert("tex_rms".to_string(), measured.tex_rms.clone());
    match baseline {
        None => {
            out.insert("ramp_l".into(), ramp);
            out.insert("tex_l".into(), measured.tex_l.clone());
            out.insert("hue_l".into(), measured.hue.iter().map(|p| p[0]).collect());
            out.insert("hue_c".into(), chroma.clone());
            out.insert("hue_h".into(), measured.hue.iter().map(|p| p[2]).collect());
            out.insert("edge".into(), measured.edge.to_vec());
        }
        Some(base) => {
            let base_ramp: Vec<f64> = bands
                .iter()
                .map(|band| band_mean(&base.ramp_columns, band))
                .collect();
            out.insert(
                "ramp_dl".into(),
                ramp.iter().zip(&base_ramp).map(|(a, b)| a - b).collect(),
            );
            out.insert(
                "tex_dl".into(),
                measured
                    .tex_l
                    .iter()
                    .zip(&base.tex_l)
                    .map(|(a, b)| a - b)
                    .collect(),
            );
            let pairs = || measured.hue.iter().zip(&base.hue);
            out.insert("hue_dl".into(), pairs().map(|(a, b)| a[0] - b[0]).collect());
            out.insert("hue_dc".into(), pairs().map(|(a, b)| a[1] - b[1]).collect());
            // No hue to speak of below C* 1.
            out.insert(
                "hue_dh".into(),
                pairs()
                    .map(|(a, b)| {
                        if a[1] < 1.0 {
                            0.0
                        } else {
                            signed_degrees(a[2] - b[2])
                        }
                    })
                    .collect(),
            );
            let e = &measured.edge;
            let b = &base.edge;
            out.insert("edge".into(), vec![e[0], e[1], e[2] - b[2], e[3] - b[3]]);
        }
    }
    (out, chroma)
}

/// A metric's difference in CIELAB units: hue in degrees as ΔH*.
fn lab_difference(metric: &str, pinned: f64, now: f64, chroma: f64) -> f64 {
    if metric == "hue_h" || metric == "hue_dh" {
        signed_degrees(now - pinned).abs().to_radians() * chroma
    } else {
        (now - pinned).abs()
    }
}

fn round(v: f64) -> String {
    let s = format!("{v:.3}");
    if s == "-0.000" { "0.000".into() } else { s }
}

/// The file: one line per metric, so a diff names what moved.
fn write_file(
    path: &str,
    tolerance: &BTreeMap<String, f64>,
    adapter: &str,
    renders: &[(String, BTreeMap<String, Vec<f64>>)],
) {
    let mut s = String::from("{\n");
    let header = json!({
        "about": "Slider response on generated charts (crates/gpu/src/slider_response_tests.rs). Regenerate only for an intended change: UPDATE_SLIDER_RESPONSE=1 cargo test -p rawtonic-gpu -- --ignored slider_response; the PR explains it.",
        "versions": versions(),
        "recipe": "a new RAW edit's versions, Standard look and base curve (renders named camera-raw/…: the Camera Raw look every RAW file gets, sliders only), Sharpening and noise reduction 0, image key measured on each chart; Exposure in EV",
        "generated_on": adapter,
        "tolerance_basis": "max(0.2 CIELAB units, Metal-Vulkan spread); measured 2026-11-08 at most 0.041 (Apple M4 Pro Metal against RADV 680M Vulkan, notes: benchmark-results/srt-slider-regression-2026-11-08)",
    });
    for (key, value) in header.as_object().unwrap() {
        writeln!(s, "  {}: {},", json!(key), value).unwrap();
    }
    writeln!(
        s,
        "  \"tolerance\": {},",
        serde_json::to_string(tolerance).unwrap()
    )
    .unwrap();
    writeln!(s, "  \"labels\": {},", labels()).unwrap();
    s.push_str("  \"renders\": {\n");
    for (i, (name, metrics)) in renders.iter().enumerate() {
        writeln!(s, "    {}: {{", json!(name)).unwrap();
        for (j, (metric, values)) in metrics.iter().enumerate() {
            let values: Vec<String> = values.iter().map(|v| round(*v)).collect();
            let comma = if j + 1 < metrics.len() { "," } else { "" };
            writeln!(s, "      {}: [{}]{comma}", json!(metric), values.join(", ")).unwrap();
        }
        let comma = if i + 1 < renders.len() { "," } else { "" };
        writeln!(s, "    }}{comma}").unwrap();
    }
    s.push_str("  }\n}\n");
    std::fs::write(path, s).unwrap();
}

fn adapter_name() -> String {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
        .map(|adapter| {
            let info = adapter.get_info();
            format!("{:?}, {}", info.backend, info.name)
        })
        .unwrap_or_default()
}

#[test]
#[ignore = "requires a native GPU; every global slider's response on generated charts against tests/data/slider_response.json"]
fn slider_response_matches_the_pinned_measurement() {
    let started = Instant::now();
    let context = context();
    let slot = GpuProcessorSlot::new(None);
    let charts = Charts::new();
    let recipes = renders();
    // Each look is compared with its own unchanged render.
    let looks: Vec<(bool, Measured)> = [false, true]
        .into_iter()
        .map(|camera_raw| {
            let name = if camera_raw {
                format!("{CAMERA_RAW_PREFIX}baseline")
            } else {
                "baseline".to_string()
            };
            let recipe = &recipes.iter().find(|(n, _)| *n == name).unwrap().1;
            (
                camera_raw,
                measure(&context, &slot, &charts, recipe, camera_raw),
            )
        })
        .collect();
    let mut measured = Vec::new();
    for (name, recipe) in &recipes {
        let camera_raw = name.starts_with(CAMERA_RAW_PREFIX);
        let baseline = &looks.iter().find(|(c, _)| *c == camera_raw).unwrap().1;
        let bands = ramp_bands(baseline);
        let m = if is_baseline(name) {
            None
        } else {
            Some(measure(&context, &slot, &charts, recipe, camera_raw))
        };
        let (values, chroma) = metrics(
            m.as_ref().unwrap_or(baseline),
            m.as_ref().map(|_| baseline),
            &bands,
        );
        measured.push((name.clone(), values, chroma));
    }
    println!(
        "{} renders of 4 charts in {:.1} s",
        recipes.len(),
        started.elapsed().as_secs_f64()
    );

    let pinned: Option<Value> = std::fs::read_to_string(DATA_FILE)
        .ok()
        .map(|text| serde_json::from_str(&text).expect("slider_response.json parses"));
    let tolerance: BTreeMap<String, f64> = pinned
        .as_ref()
        .and_then(|p| serde_json::from_value(p["tolerance"].clone()).ok())
        .unwrap_or_else(|| {
            metric_shape(true)
                .into_iter()
                .chain(metric_shape(false))
                .map(|(m, _)| (m.to_string(), DEFAULT_TOLERANCE))
                .collect()
        });
    let table: Vec<_> = measured
        .iter()
        .map(|(name, values, _)| (name.clone(), values.clone()))
        .collect();
    let adapter = adapter_name();
    if let Ok(out) = std::env::var("SLIDER_RESPONSE_OUT") {
        write_file(&out, &tolerance, &adapter, &table);
        println!("wrote {out}");
    }
    if std::env::var("UPDATE_SLIDER_RESPONSE").is_ok_and(|v| v == "1") {
        write_file(DATA_FILE, &tolerance, &adapter, &table);
        println!("wrote {DATA_FILE}");
        return;
    }
    let pinned = pinned.unwrap_or_else(|| {
        panic!("{DATA_FILE} is missing: generate it with UPDATE_SLIDER_RESPONSE=1")
    });
    assert_eq!(
        pinned["versions"],
        versions(),
        "the file is pinned at other recipe versions: regenerate it (UPDATE_SLIDER_RESPONSE=1) and explain the change"
    );

    let mut moved: Vec<(f64, String)> = Vec::new();
    for (name, values, chroma) in &measured {
        for (metric, now) in values {
            let tol = tolerance.get(metric).copied().unwrap_or(DEFAULT_TOLERANCE);
            let Some(old) = pinned["renders"][name][metric].as_array() else {
                moved.push((f64::INFINITY, format!("{name} {metric}: not pinned")));
                continue;
            };
            let labels = labels();
            let label_set = match metric.as_str() {
                "ramp_l" | "ramp_dl" => "ramp",
                "tex_l" | "tex_dl" => "tex",
                "hue_l" | "hue_dl" | "hue_c" | "hue_dc" | "hue_h" | "hue_dh" => "hue",
                other => other,
            };
            for (i, (old, now)) in old.iter().zip(now).enumerate() {
                let old = old.as_f64().unwrap();
                let c = chroma.get(i).copied().unwrap_or(0.0);
                let diff = lab_difference(metric, old, *now, c);
                if diff > tol {
                    let label = labels[label_set][i].as_str().unwrap_or("?");
                    moved.push((
                        diff / tol,
                        format!(
                            "{name} {metric}[{label}]: pinned {old:.3}, now {now:.3} ({:+.3}; {diff:.3} > tolerance {tol})",
                            now - old
                        ),
                    ));
                }
            }
        }
    }
    moved.sort_by(|a, b| b.0.total_cmp(&a.0));
    let shown: Vec<&str> = moved.iter().take(80).map(|m| m.1.as_str()).collect();
    assert!(
        moved.is_empty(),
        "{} slider response values moved (largest first; intended changes: UPDATE_SLIDER_RESPONSE=1 and explain in the PR):\n{}",
        moved.len(),
        shown.join("\n")
    );
}

/// Runs without a GPU: the file pins every slider and value at the current
/// recipe versions, each metric with its full length.
#[test]
fn slider_response_file_covers_every_slider_at_the_current_versions() {
    let text = std::fs::read_to_string(DATA_FILE).expect("tests/data/slider_response.json");
    let pinned: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        pinned["versions"],
        versions(),
        "slider_response.json is pinned at other recipe versions: regenerate it on a GPU (UPDATE_SLIDER_RESPONSE=1) and explain the change"
    );
    for (name, _) in renders() {
        let render = &pinned["renders"][&name];
        for (metric, len) in metric_shape(is_baseline(&name)) {
            let values = render[metric].as_array();
            assert_eq!(
                values.map(Vec::len),
                Some(len),
                "{name} {metric} is not pinned with {len} values"
            );
        }
        assert!(
            pinned["tolerance"]
                .as_object()
                .is_some_and(|t| metric_shape(is_baseline(&name))
                    .iter()
                    .all(|(m, _)| t.contains_key(*m))),
            "every metric needs a tolerance"
        );
    }
}
