//! Edge fringes: colour along strong luminance edges that neither side of
//! the edge explains — demosaic false colour on thin lines near the
//! sensor's resolution limit, the rest of lateral chromatic aberration and
//! colour bleeding at dark-on-bright edges (branches against a bright sky).
//!
//! Since the multi-scale colour noise filter (`colorNoiseVersion` 2) the base
//! image is developed without the decode-time chroma blur, and the filter
//! protects colour at luminance edges, so these fringes stayed visible
//! where Capture One and Camera Raw show clean edges (notes:
//! reference/edge-fringes.md, G05). This pass runs on the lens-corrected
//! image (after the warp, so after any lateral CA correction) and before
//! the renderer's noise reduction and sharpening.
//!
//! Method: in square-root encoded RGB the colour differences `u = √R − √G`
//! and `v = √B − √G` are modelled in every 5 × 5 window as linear in the
//! luminance `Y = √((R + 2G + B) / 4)` (a guided filter with luminance as
//! the guide). A two-colour edge — a red object against grey, a dark branch
//! against the sky — is linear in luminance and kept; colour that changes
//! sides across a dark line (red on one side, cyan on the other) or
//! alternates along it is not, and is replaced by the window's model. Only
//! pixels near strong luminance edges change, flat areas (colour noise,
//! colour texture) are left to the noise reduction; a pixel's chroma is
//! never raised, so no colour is added.
//!
//! Version 3 adds a second stage for edges between two saturated colours
//! (a yellow coat against grey, cyan against magenta), where the demosaic
//! interpolates red and blue across the edge a pixel or two away from
//! green: the transition pixels get a colour or a brightness neither side
//! has (a light seam between cyan and magenta of equal luminance, which
//! the luminance model cannot see). Blur mixes the two sides in linear
//! light, so in a window holding two colours every pixel lies on the line
//! between them; the stage moves a pixel onto its window's principal colour
//! line (in linear RGB) where the window holds two colours and a strong
//! edge, and leaves windows of three or more colours alone, and pixels of a
//! colour no mix of the two makes (a small patch of a third colour on the
//! edge).

use std::borrow::Cow;

use image::DynamicImage;
use rayon::prelude::*;

mod axial;
pub mod gpu;
#[cfg(test)]
mod reference;

/// The recipe key naming the edge-fringe treatment a recipe was made with.
pub const VERSION_KEY: &str = "fringeVersion";
/// The newest treatment: 1 (2026-10-16) removes edge fringes as described
/// above in 5 × 5 windows; 2 (2026-10-17) in 7 × 7 windows; 3 (2026-10-20)
/// adds the colour-line stage; 4 (2026-10-20) first corrects chromatic
/// aberration across strong edges, noisy photos included (`axial`); 5
/// (2026-10-25) is version 4 computed on the GPU (`gpu`; the same stages,
/// equal within float rounding). Recipes without the key keep rendering
/// without it, recipes with 1–4 as before.
pub const VERSION: u64 = 5;

/// Window radius at full resolution: version 1's 5 × 5 spans a fringe on
/// both sides of a thin line. A lateral-CA rest of 1–2 px beside an edge
/// (bodies whose CA data under-state the CA, GF5) or a fringe on a 2–3 px
/// line keeps colour in it, which the colour noise reduction then breaks
/// into blotches along the edge; version 2's 7 × 7 removes those (bright-
/// edge excess lower on 50 of 55 files, GF5 3.33 → 2.40, GH3 1.63 → 0.90,
/// TZ96 1.89 → 0.97; neutral-edge chroma lower on 39, higher on 1; notes:
/// reference/edge-fringes.md "Version 2"). On a noisy photo (luminance
/// noise from half the edge test's start, about ISO 3200 and up) noise
/// alone passes the edge test, the 7 × 7 model evens out the fine chroma
/// noise unevenly and the colour noise reduction leaves blotches (EOS R10
/// ISO 32000: 14 → 1560 isolated specks per Mpx), so version 2 keeps 5 × 5
/// there and renders as version 1. `noise` is the full-resolution
/// luminance noise (`luminance_noise`).
fn radius_for(version: u64, noise: f32) -> f32 {
    if low_noise(version, noise) { 3.0 } else { 2.0 }
}

/// Whether version 2's wider windows and version 3's colour lines apply.
fn low_noise(version: u64, noise: f32) -> bool {
    version >= 2 && noise < 0.5 * EDGE_LOW
}

/// What the pass does to an image: the window radius (in the image's
/// pixels) and whether the colour-line stage runs. Version 3 runs it on
/// low-noise photos only: on noisy ones it found colour lines in the noise
/// and raised isolated colour specks after the colour noise reduction (EOS
/// R10 ISO 32000 651 → 1213 per Mpx, K-7 310 → 502; notes:
/// reference/edge-fringes.md "Version 3"), so they render as version 1.
/// Version 4 adds the chromatic-aberration stage on every photo
/// (`axial`), which scales its distances and its tolerance to noise;
/// version 5 runs the same plan on the GPU (`gpu`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Plan {
    pub radius: usize,
    pub colour_lines: bool,
    pub axial: Option<AxialPlan>,
    pub gpu: bool,
}

/// The chromatic-aberration stage's inputs: the image's own luminance
/// noise and its scale of the full resolution (as `f32` bits, so plans
/// compare and hash exactly).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AxialPlan {
    noise: u32,
    scale: u32,
}

impl AxialPlan {
    fn new(noise: f32, scale: f32) -> Self {
        Self {
            noise: noise.to_bits(),
            scale: scale.to_bits(),
        }
    }

    fn noise(self) -> f32 {
        f32::from_bits(self.noise)
    }

    fn scale(self) -> f32 {
        f32::from_bits(self.scale)
    }
}

/// The plan for `version` on an image at `scale` of the full resolution
/// whose full-resolution luminance noise is `noise`.
fn plan_for(version: u64, noise: f32, scale: f32) -> Plan {
    let scale = scale.min(1.0);
    Plan {
        radius: ((radius_for(version, noise) * scale).round() as usize).max(1),
        colour_lines: version >= 3 && low_noise(version, noise),
        // A reduced image's own noise is about its scale of the full one.
        axial: (version >= 4).then(|| AxialPlan::new(noise * scale, scale)),
        gpu: version >= 5,
    }
}

/// The plan of `version` at `scale` for a photo whose full-resolution
/// luminance noise is `noise` (the GPU crate's equivalence tests).
#[cfg(feature = "test-support")]
pub fn plan_of(version: u64, noise: f32, scale: f32) -> Plan {
    plan_for(version.min(VERSION), noise, scale)
}

/// [`apply`] of a GPU plan in tiles of `tile × tile` pixels.
#[cfg(feature = "test-support")]
pub fn apply_on_gpu_in_tiles(
    data: &mut [f32],
    w: usize,
    h: usize,
    stride: usize,
    plan: Plan,
    tile: usize,
) -> Result<(), String> {
    gpu::apply_in_tiles(data, w, h, stride, plan, tile)
}

/// [`apply_on_gpu_in_tiles`] on `device` instead of the registered one.
#[cfg(feature = "test-support")]
pub fn apply_on_device_in_tiles(
    device: &std::sync::Arc<wgpu::Device>,
    queue: &wgpu::Queue,
    data: &mut [f32],
    (w, h, stride): (usize, usize, usize),
    plan: Plan,
    tile: usize,
) -> Result<(), String> {
    gpu::apply_on_device(device, queue, data, (w, h, stride), plan, tile)
}

/// Runs the pass as `plan` says on `w × h` pixels of `stride` floats (RGB
/// first): the chromatic-aberration stage, the luminance model, then the
/// colour lines, each on the result of the one before. A GPU plan fails
/// when the GPU cannot run it (there is no CPU fallback).
pub fn apply(
    data: &mut [f32],
    w: usize,
    h: usize,
    stride: usize,
    plan: Plan,
) -> Result<(), String> {
    if plan.gpu {
        return gpu::apply(data, w, h, stride, plan);
    }
    if let Some(axial) = plan.axial {
        axial::apply(data, w, h, stride, axial.noise(), axial.scale());
    }
    suppress(data, w, h, stride, plan.radius);
    if plan.colour_lines {
        colour_lines(data, w, h, stride, plan.radius);
    }
    Ok(())
}

/// Regularisation of the colour-on-luminance slope (square-root units²):
/// windows with little luminance variation keep their mean colour.
const EPS: f32 = 4e-4;
/// Luminance standard deviation (square-root units) of a window from which
/// a pixel starts to count as near an edge, and from which fully.
const EDGE_LOW: f32 = 0.03;
const EDGE_HIGH: f32 = 0.08;
/// Below this resolution (reduced previews, thumbnails) a full-resolution
/// fringe of one or two pixels is averaged away; the pass is skipped.
const MIN_SCALE: f32 = 0.35;
/// Rows and columns per parallel tile: each tile reads 2r rows and columns
/// around it, and its buffers stay in the core's cache.
const BAND: usize = 24;
const TILE: usize = 256;
/// Rows and columns per parallel tile of the colour-line stage (as above;
/// 18 moments are summed around each tile, so its bands are taller).
const LINE_BAND: usize = 40;
const LINE_TILE: usize = 256;
/// Regularisation of the colour line (linear units²): a window of little
/// colour variation keeps its mean less a small part of the variation.
const LINE_EPS: f32 = 1e-6;
/// Share of a window's colour variance off its principal line (in square-
/// root RGB) up to which the window counts as holding two colours fully,
/// and from which not at all: a misregistered transition adds a few
/// percent, a third colour of a few pixels much more.
const TWO_COLOUR_FULL: f32 = 0.05;
/// How far beyond the channel ranges of its window's two colours (relative
/// to the distance between them) a pixel still moves fully, and from which
/// not at all (see `within_the_mixes`).
const MIX_FULL: f32 = 0.1;
const MIX_NONE: f32 = 0.25;
const TWO_COLOUR_NONE: f32 = 0.15;

/// A transient key (never saved) of the recipe a preview base is built
/// from while a control is dragged: version 4's chromatic-aberration stage
/// is left out there (it costs about 20–110 ms at preview sizes, on top of
/// the rest of the pass) and runs on the frame after the drag.
pub const DRAFT_KEY: &str = "fringeDraft";

/// Whether a draft (`DRAFT_KEY`) renders differently from the recipe: it
/// has the chromatic-aberration stage.
pub fn has_draft(adjustments: &serde_json::Value) -> bool {
    version_of(adjustments) >= 4
}

/// Whether `adjustments` is such a draft.
pub fn is_draft(adjustments: &serde_json::Value) -> bool {
    adjustments
        .get(DRAFT_KEY)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

/// The recipe's edge-fringe version: `fringeVersion`, 0 (none) when
/// missing, never newer than this renderer knows.
pub fn version_of(adjustments: &serde_json::Value) -> u64 {
    adjustments
        .get(VERSION_KEY)
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0)
        .min(VERSION)
}

/// Sets the current version on the recipe of a never-edited RAW (as the
/// editor does for new edits); recipes that were saved are left as they
/// are.
pub fn apply_default(adjustments: &mut serde_json::Value, never_edited: bool, is_raw: bool) {
    if !never_edited || !is_raw || adjustments.get(VERSION_KEY).is_some() {
        return;
    }
    if adjustments.is_null() {
        *adjustments = serde_json::json!({});
    }
    if let Some(map) = adjustments.as_object_mut() {
        map.insert(VERSION_KEY.to_string(), serde_json::json!(VERSION));
    }
}

/// The pass for a recipe on an image at `scale` of the full resolution
/// (1 for exports and full-resolution previews). Fails only where version
/// 5's GPU pass cannot run.
pub fn apply_for_recipe<'a>(
    image: Cow<'a, DynamicImage>,
    adjustments: &serde_json::Value,
    scale: f32,
) -> Result<Cow<'a, DynamicImage>, String> {
    let version = version_of(adjustments);
    if version == 0 || scale.is_nan() || scale < MIN_SCALE {
        return Ok(image);
    }
    if !matches!(
        image.as_ref(),
        DynamicImage::ImageRgb32F(_) | DynamicImage::ImageRgba32F(_)
    ) {
        return Ok(image);
    }
    let mut image = image;
    let (w, h) = (image.width() as usize, image.height() as usize);
    match image.to_mut() {
        DynamicImage::ImageRgb32F(img) => {
            let plan = recipe_plan(adjustments, img.as_raw(), w, h, 3, scale);
            apply(img.as_mut(), w, h, 3, plan)?
        }
        DynamicImage::ImageRgba32F(img) => {
            let plan = recipe_plan(adjustments, img.as_raw(), w, h, 4, scale);
            apply(img.as_mut(), w, h, 4, plan)?
        }
        _ => {}
    }
    Ok(image)
}

/// The plan of a recipe's pass (version > 0) on `data`, at `scale`.
fn recipe_plan(
    adjustments: &serde_json::Value,
    data: &[f32],
    w: usize,
    h: usize,
    stride: usize,
    scale: f32,
) -> Plan {
    recipe_plan_with_noise(adjustments, scale, || luminance_noise(data, w, h, stride))
}

/// [`recipe_plan`] with the image's luminance noise from `noise`.
fn recipe_plan_with_noise(
    adjustments: &serde_json::Value,
    scale: f32,
    noise: impl FnOnce() -> f32,
) -> Plan {
    let version = version_of(adjustments);
    // A reduced preview averages the noise down by about its scale.
    let noise = if version >= 2 {
        noise() / scale.min(1.0)
    } else {
        0.0
    };
    let plan = plan_for(version, noise, scale);
    Plan {
        axial: plan.axial.filter(|_| !is_draft(adjustments)),
        ..plan
    }
}

/// Whether [`apply_for_recipe_into_texture`] runs the recipe's pass on
/// `image` at full resolution: version 5's GPU pass on RGB floats.
pub fn runs_into_texture(image: &DynamicImage, adjustments: &serde_json::Value) -> bool {
    plan_for(version_of(adjustments), 0.0, 1.0).gpu
        && image.width() > 0
        && image.height() > 0
        && matches!(image, DynamicImage::ImageRgb32F(_))
}

/// [`apply_for_recipe`] at full resolution with the result written into
/// `texture` instead of the image (an `Rgba16Float` texture of `window`'s
/// size, x, y, width and height in the image, with `COPY_DST`), on `device`:
/// the texels equal those of uploading the pass's result cropped to
/// `window` (each channel `f16::from_f32`, alpha 1), but nothing is read
/// back or converted on the CPU. Only where [`runs_into_texture`].
pub fn apply_for_recipe_into_texture(
    device: &std::sync::Arc<wgpu::Device>,
    queue: &wgpu::Queue,
    image: &DynamicImage,
    adjustments: &serde_json::Value,
    window: [u32; 4],
    texture: &wgpu::Texture,
) -> Result<(), String> {
    let (w, h) = (image.width() as usize, image.height() as usize);
    let [x, y, width, height] = window.map(|v| v as usize);
    let Some(img) = image
        .as_rgb32f()
        .filter(|_| runs_into_texture(image, adjustments))
    else {
        return Err("The edge-fringe pass of this recipe does not run into a texture.".to_string());
    };
    if width == 0 || height == 0 || x + width > w || y + height > h {
        return Err("The texture window lies outside the image.".to_string());
    }
    let plan = recipe_plan(adjustments, img.as_raw(), w, h, 3, 1.0);
    gpu::apply_into_texture(
        device,
        queue,
        gpu::Pixels::Cpu(img.as_raw()),
        (w, h),
        plan,
        [x, y, width, height],
        texture,
    )
}

/// [`apply_for_recipe_into_texture`] of a warped frame in a GPU buffer
/// (`warp_gpu::warp_into_buffer`, `w × h` RGB f32): the plan's noise is
/// measured on every fourth row read back (`warp_gpu::read_every_fourth_row`),
/// the rows the CPU image's plan measures, and the bands are copied on the
/// GPU. The texels equal those of [`apply_for_recipe_into_texture`] of the
/// frame read back. For recipes whose pass runs on the GPU (`fringeVersion`
/// 5).
pub fn apply_for_recipe_into_texture_from_buffer(
    device: &std::sync::Arc<wgpu::Device>,
    queue: &wgpu::Queue,
    warped: &wgpu::Buffer,
    (w, h): (u32, u32),
    adjustments: &serde_json::Value,
    window: [u32; 4],
    texture: &wgpu::Texture,
) -> Result<(), String> {
    let [x, y, width, height] = window.map(|v| v as usize);
    let (w, h) = (w as usize, h as usize);
    if !plan_for(version_of(adjustments), 0.0, 1.0).gpu || w == 0 || h == 0 {
        return Err("The edge-fringe pass of this recipe does not run into a texture.".to_string());
    }
    if width == 0 || height == 0 || x + width > w || y + height > h {
        return Err("The texture window lies outside the image.".to_string());
    }
    let rows = crate::warp_gpu::read_every_fourth_row(device, queue, warped, (w as u32, h as u32))?;
    let plan = recipe_plan_with_noise(adjustments, 1.0, || {
        noise_of_rows(rows.par_chunks(w * 3), w, 3)
    });
    gpu::apply_into_texture(
        device,
        queue,
        gpu::Pixels::Gpu(warped),
        (w, h),
        plan,
        [x, y, width, height],
        texture,
    )
}

#[inline]
fn enc(v: f32) -> f32 {
    v.max(0.0).sqrt()
}

/// `out[x]` = sum of `row` over `x - r ..= x + r` (edges repeated), using
/// `pad` (at least `row.len() + 2r` long) as scratch.
fn box_row(row: &[f32], r: usize, pad: &mut [f32], out: &mut [f32]) {
    let w = row.len();
    let pad = &mut pad[..w + 2 * r];
    pad[..r].fill(row[0]);
    pad[r..r + w].copy_from_slice(row);
    pad[r + w..].fill(row[w - 1]);
    match r {
        1 => sum_window::<3>(pad, out),
        2 => sum_window::<5>(pad, out),
        3 => sum_window::<7>(pad, out),
        _ => {
            out.copy_from_slice(&pad[..w]);
            for dx in 1..=2 * r {
                for (o, v) in out.iter_mut().zip(&pad[dx..dx + w]) {
                    *o += v;
                }
            }
        }
    }
}

/// Columns per block of the vectorised loops. Each column's arithmetic is
/// the same, in the same order, as for a column alone, so results do not
/// depend on it.
const LANES: usize = 8;

/// `out[x]` = `values[x] + values[x + 1] + … + values[x + N - 1]`, added
/// in that order, a block of [`LANES`] columns at a time with the partial
/// sums in registers (the window sizes the pass uses are known at compile
/// time, so the reads need no bounds checks).
fn sum_window<const N: usize>(values: &[f32], out: &mut [f32]) {
    let (blocks, rest) = out.as_chunks_mut::<LANES>();
    let done = blocks.len() * LANES;
    for (x, o) in (0..done).step_by(LANES).zip(blocks) {
        let window = &values[x..x + LANES + N - 1];
        let mut acc: [f32; LANES] = window[..LANES].try_into().unwrap();
        for k in 1..N {
            for i in 0..LANES {
                acc[i] += window[k + i];
            }
        }
        *o = acc;
    }
    for (x, o) in (done..).zip(rest) {
        let window = &values[x..x + N];
        *o = window[0];
        for v in &window[1..] {
            *o += v;
        }
    }
}

/// `out` = sum of the `2r + 1` rows of `rows` (each `w` long) starting at
/// row `first`.
fn box_column(rows: &[f32], w: usize, first: usize, r: usize, out: &mut [f32]) {
    match r {
        1 => sum_rows::<3>(&rows[first * w..(first + 3) * w], out),
        2 => sum_rows::<5>(&rows[first * w..(first + 5) * w], out),
        3 => sum_rows::<7>(&rows[first * w..(first + 7) * w], out),
        _ => {
            out.copy_from_slice(&rows[first * w..(first + 1) * w]);
            for k in first + 1..=first + 2 * r {
                for (o, v) in out.iter_mut().zip(&rows[k * w..(k + 1) * w]) {
                    *o += v;
                }
            }
        }
    }
}

/// `out` = the sum of the `N` rows of `rows` (each `out.len()` long),
/// added in order, as [`sum_window`].
fn sum_rows<const N: usize>(rows: &[f32], out: &mut [f32]) {
    let w = out.len();
    let rows: [&[f32]; N] = std::array::from_fn(|k| &rows[k * w..(k + 1) * w]);
    let (blocks, rest) = out.as_chunks_mut::<LANES>();
    let done = blocks.len() * LANES;
    for (x, o) in (0..done).step_by(LANES).zip(blocks) {
        let mut acc: [f32; LANES] = rows[0][x..x + LANES].try_into().unwrap();
        for row in &rows[1..] {
            let v: &[f32; LANES] = row[x..x + LANES].try_into().unwrap();
            for i in 0..LANES {
                acc[i] += v[i];
            }
        }
        *o = acc;
    }
    for (x, o) in (done..).zip(rest) {
        *o = rows[0][x];
        for row in &rows[1..] {
            *o += row[x];
        }
    }
}

/// The photo's luminance noise (standard deviation, square-root units):
/// from the median absolute difference of horizontal neighbours on every
/// fourth row and second column, which edges and texture barely move.
fn luminance_noise(data: &[f32], w: usize, h: usize, stride: usize) -> f32 {
    noise_of_rows(
        (0..h)
            .into_par_iter()
            .step_by(4)
            .map(|y| &data[y * w * stride..(y + 1) * w * stride]),
        w,
        stride,
    )
}

/// [`luminance_noise`] of its rows (every fourth of the image).
fn noise_of_rows<'a>(
    rows: impl IndexedParallelIterator<Item = &'a [f32]>,
    w: usize,
    stride: usize,
) -> f32 {
    if w < 2 {
        return 0.0;
    }
    median_noise(
        rows.flat_map_iter(|row| row_differences(row, w, stride))
            .collect(),
    )
}

/// The sampled neighbour differences of one row of `w` pixels.
fn row_differences(row: &[f32], w: usize, stride: usize) -> impl Iterator<Item = f32> + '_ {
    let lum = move |x: usize| {
        let px = &row[x * stride..x * stride + 3];
        enc(0.25 * (px[0] + 2.0 * px[1] + px[2]))
    };
    (0..w.saturating_sub(1))
        .step_by(2)
        .map(move |x| (lum(x + 1) - lum(x)).abs())
}

fn median_noise(mut diffs: Vec<f32>) -> f32 {
    if diffs.is_empty() {
        return 0.0;
    }
    let mid = diffs.len() / 2;
    let (_, median, _) = diffs.select_nth_unstable_by(mid, f32::total_cmp);
    // Gaussian noise: median |difference| = 0.6745 · √2 · σ.
    *median / (0.6745 * std::f32::consts::SQRT_2)
}

/// The plan [`apply_for_recipe`] uses at full resolution for
/// `adjustments` on a `w × h` RGB image whose row `y` `row` gives (only
/// every fourth row is asked for): what a region cut from that image needs
/// to suppress fringes as the whole image does. None without the pass.
pub fn full_resolution_plan(
    adjustments: &serde_json::Value,
    w: usize,
    h: usize,
    row: impl Fn(usize) -> Vec<f32> + Sync,
) -> Option<Plan> {
    let version = version_of(adjustments);
    if version == 0 {
        return None;
    }
    let noise = if full_resolution_plan_reads_rows(adjustments, w) {
        let rows: Vec<usize> = (0..h).step_by(4).collect();
        let diffs = rows
            .into_par_iter()
            .flat_map_iter(|y| row_differences(&row(y), w, 3).collect::<Vec<_>>())
            .collect();
        median_noise(diffs)
    } else {
        0.0
    };
    Some(plan_for(version, noise, 1.0))
}

/// Whether [`full_resolution_plan`] of `adjustments` on an image `w`
/// wide asks for rows (to measure the noise).
pub fn full_resolution_plan_reads_rows(adjustments: &serde_json::Value, w: usize) -> bool {
    version_of(adjustments) >= 2 && w >= 2
}

/// How far [`apply`] reads around a pixel (rows and columns): the
/// luminance and colour-line stages twice their radius each, the
/// chromatic-aberration stage `axial::reach`.
pub fn reach(plan: Plan) -> usize {
    let stages = if plan.colour_lines {
        4 * plan.radius
    } else {
        2 * plan.radius
    };
    stages + plan.axial.map_or(0, |axial| axial::reach(axial.scale()))
}

/// Removes edge fringes in place. `data` holds `w × h` pixels of `stride`
/// floats (RGB first). Each band of rows reads the unmodified image around
/// it and returns its changes, which are written once all bands are done.
pub fn suppress(data: &mut [f32], w: usize, h: usize, stride: usize, r: usize) {
    if w == 0 || h == 0 {
        return;
    }
    let source: &[f32] = data;
    let across = w.div_ceil(TILE);
    let changes: Vec<Vec<(usize, f32, f32)>> = (0..h.div_ceil(BAND) * across)
        .into_par_iter()
        .map(|tile| {
            let (band, column) = (tile / across, tile % across);
            let rows = band * BAND..((band + 1) * BAND).min(h);
            let columns = column * TILE..((column + 1) * TILE).min(w);
            suppress_tile(source, w, h, stride, r, rows, columns)
        })
        .collect();
    data.par_chunks_mut(BAND * w * stride)
        .zip(changes.par_chunks(across))
        .enumerate()
        .for_each(|(band, (rows, tiles))| {
            let first = band * BAND * w;
            for &(i, red, blue) in tiles.iter().flatten() {
                rows[(i - first) * stride] = red;
                rows[(i - first) * stride + 2] = blue;
            }
        });
}

/// The changes to the pixels of `rows` × `columns` of the `width × h`
/// image `data`; a tile reads 2r rows and columns around it (see
/// [`colour_line_tile`]).
fn suppress_tile(
    data: &[f32],
    width: usize,
    h: usize,
    stride: usize,
    r: usize,
    rows: std::ops::Range<usize>,
    columns: std::ops::Range<usize>,
) -> Vec<(usize, f32, f32)> {
    let (y0, y1) = (rows.start, rows.end);
    let x_first = columns.start.saturating_sub(2 * r);
    let w = (columns.end + 2 * r).min(width) - x_first;
    let inner = columns.start - x_first..columns.end - x_first;
    let row_of = |y: isize| y.clamp(0, h as isize - 1) as usize;
    let inv_area = 1.0 / ((2 * r + 1) * (2 * r + 1)) as f32;
    let mut pad = vec![0.0f32; w + 2 * r];
    // Rows y0 - 2r .. y1 + 2r: luminance and colour differences, and the
    // horizontal sums of their moments Y, u, v, YY, Yu, Yv.
    let ext0 = y0 as isize - 2 * r as isize;
    let ext_rows = (y1 - y0) + 4 * r;
    let (mut lum, mut du, mut dv) = (
        vec![0.0f32; ext_rows * w],
        vec![0.0f32; ext_rows * w],
        vec![0.0f32; ext_rows * w],
    );
    for e in 0..ext_rows {
        let y = row_of(ext0 + e as isize);
        let src = &data[(y * width + x_first) * stride..(y * width + x_first + w) * stride];
        let span = e * w..(e + 1) * w;
        for (((px, l), u), v) in src
            .chunks_exact(stride)
            .zip(&mut lum[span.clone()])
            .zip(&mut du[span.clone()])
            .zip(&mut dv[span])
        {
            let g = enc(px[1]);
            *l = enc(0.25 * (px[0] + 2.0 * px[1] + px[2]));
            *u = enc(px[0]) - g;
            *v = enc(px[2]) - g;
        }
    }
    let mut hsum: [Vec<f32>; 6] = std::array::from_fn(|_| vec![0.0f32; ext_rows * w]);
    let mut moment = vec![0.0f32; w];
    for e in 0..ext_rows {
        let span = e * w..(e + 1) * w;
        let (l, u, v) = (&lum[span.clone()], &du[span.clone()], &dv[span.clone()]);
        for (k, sum) in hsum.iter_mut().enumerate() {
            match k {
                0 => moment.copy_from_slice(l),
                1 => moment.copy_from_slice(u),
                2 => moment.copy_from_slice(v),
                _ => {
                    let other = [l, u, v][k - 3];
                    for ((m, a), b) in moment.iter_mut().zip(l).zip(other) {
                        *m = a * b;
                    }
                }
            }
            box_row(&moment, r, &mut pad, &mut sum[span.clone()]);
        }
    }
    // Rows y0 - r .. y1 + r: the window's model (slopes and offsets of u and
    // v on Y) and its edge weight, then summed along rows. Mid row m is ext
    // row m + r; its window spans ext rows m ..= m + 2r.
    let mid_rows = (y1 - y0) + 2 * r;
    let mut csum: [Vec<f32>; 5] = std::array::from_fn(|_| vec![0.0f32; mid_rows * w]);
    let mut vs: [Vec<f32>; 6] = std::array::from_fn(|_| vec![0.0f32; w]);
    let mut coef: [Vec<f32>; 5] = std::array::from_fn(|_| vec![0.0f32; w]);
    for m in 0..mid_rows {
        for (k, out) in vs.iter_mut().enumerate() {
            box_column(&hsum[k], w, m, r, out);
        }
        for x in 0..w {
            let my = vs[0][x] * inv_area;
            let mu = vs[1][x] * inv_area;
            let mv = vs[2][x] * inv_area;
            let var = (vs[3][x] * inv_area - my * my).max(0.0);
            let au = (vs[4][x] * inv_area - my * mu) / (var + EPS);
            let av = (vs[5][x] * inv_area - my * mv) / (var + EPS);
            coef[0][x] = au;
            coef[1][x] = mu - au * my;
            coef[2][x] = av;
            coef[3][x] = mv - av * my;
            coef[4][x] = ((var.sqrt() - EDGE_LOW) / (EDGE_HIGH - EDGE_LOW)).clamp(0.0, 1.0);
        }
        let span = m * w..(m + 1) * w;
        for (k, sum) in csum.iter_mut().enumerate() {
            box_row(&coef[k], r, &mut pad, &mut sum[span.clone()]);
        }
    }
    let mut changes = Vec::new();
    let mut model: [Vec<f32>; 5] = std::array::from_fn(|_| vec![0.0f32; w]);
    for y in y0..y1 {
        // Output row y is mid row y - y0 + r (its window: mid rows y - y0 ..=
        // y - y0 + 2r) and ext row y - y0 + 2r.
        let m0 = y - y0;
        for (k, out) in model.iter_mut().enumerate() {
            box_column(&csum[k], w, m0, r, out);
        }
        let e = y - y0 + 2 * r;
        for x in inner.clone() {
            // Spread over the window, so a fringe a pixel or two beside the
            // edge's centre is covered fully.
            let weight = (2.0 * model[4][x] * inv_area).min(1.0);
            if weight <= 0.0 {
                continue;
            }
            let i = y * width + x_first + x;
            let px = &data[i * stride..][..3];
            if px.iter().any(|&c| c < 0.0 || !c.is_finite()) {
                continue;
            }
            let (l, u0, v0) = (lum[e * w + x], du[e * w + x], dv[e * w + x]);
            let mut u1 = (model[0][x] * l + model[1][x]) * inv_area;
            let mut v1 = (model[2][x] * l + model[3][x]) * inv_area;
            let (c0, c1) = (u0.hypot(v0), u1.hypot(v1));
            if c1 > c0 {
                // Never more colour than the pixel had.
                let k = if c1 > 0.0 { c0 / c1 } else { 0.0 };
                u1 *= k;
                v1 *= k;
            }
            let g = enc(px[1]);
            let red = (g + u0 + weight * (u1 - u0)).max(0.0);
            let blue = (g + v0 + weight * (v1 - v0)).max(0.0);
            changes.push((i, red * red, blue * blue));
        }
    }
    changes
}

/// The colour-line stage of version 3 in place (see the module notes):
/// every pixel near a strong edge in a window of two colours moves onto
/// the window's principal colour line in linear RGB, averaged over the
/// windows that hold it (as a guided filter averages its models). Bands of
/// rows read the unmodified image around them, as [`suppress`].
fn colour_lines(data: &mut [f32], w: usize, h: usize, stride: usize, r: usize) {
    if w == 0 || h == 0 {
        return;
    }
    let source: &[f32] = data;
    let across = w.div_ceil(LINE_TILE);
    let changes: Vec<Vec<(usize, [f32; 3])>> = (0..h.div_ceil(LINE_BAND) * across)
        .into_par_iter()
        .map_init(LineScratch::default, |scratch, tile| {
            let (band, column) = (tile / across, tile % across);
            let rows = band * LINE_BAND..((band + 1) * LINE_BAND).min(h);
            let columns = column * LINE_TILE..((column + 1) * LINE_TILE).min(w);
            colour_line_tile(source, w, h, stride, r, rows, columns, scratch)
        })
        .collect();
    data.par_chunks_mut(LINE_BAND * w * stride)
        .zip(changes.par_chunks(across))
        .enumerate()
        .for_each(|(band, (rows, tiles))| {
            let first = band * LINE_BAND * w;
            for (i, px) in tiles.iter().flatten() {
                rows[(i - first) * stride..(i - first) * stride + 3].copy_from_slice(px);
            }
        });
}

/// Index pairs of the six second moments of three channels.
const PAIRS: [(usize, usize); 6] = [(0, 0), (0, 1), (0, 2), (1, 1), (1, 2), (2, 2)];

/// Means and covariances (the six [`PAIRS`] entries) of `L` windows from
/// their box sums (three channels, then the six [`PAIRS`] products).
#[inline(always)]
fn moments<const L: usize>(sums: &[[f32; L]; 9], inv_area: f32) -> ([[f32; L]; 3], [[f32; L]; 6]) {
    let mut mean = [[0.0f32; L]; 3];
    for (m, s) in mean.iter_mut().zip(sums) {
        for i in 0..L {
            m[i] = s[i] * inv_area;
        }
    }
    let mut cov = [[0.0f32; L]; 6];
    for (k, &(a, b)) in PAIRS.iter().enumerate() {
        for i in 0..L {
            let v = sums[3 + k][i] * inv_area - mean[a][i] * mean[b][i];
            cov[k][i] = if a == b { v.max(0.0) } else { v };
        }
    }
    (mean, cov)
}

/// The principal axes and variances of `L` windows (see [`principal_of`]).
#[inline(always)]
fn principal<const L: usize>(cov: &[[f32; L]; 6]) -> ([[f32; L]; 3], [f32; L]) {
    let mut axis = [[0.0f32; L]; 3];
    let mut var = [0.0f32; L];
    for i in 0..L {
        let (e, v) = principal_of([
            cov[0][i], cov[1][i], cov[2][i], cov[3][i], cov[4][i], cov[5][i],
        ]);
        axis[0][i] = e[0];
        axis[1][i] = e[1];
        axis[2][i] = e[2];
        var[i] = v;
    }
    (axis, var)
}

/// The colour-line stage's weight for `L` windows from the covariances of
/// their square-root RGB: near a strong edge (as the luminance pass's
/// test, a neutral edge's principal variance being three times its
/// luminance variance) and holding two colours (little variance off its
/// principal line).
#[inline(always)]
fn line_weights<const L: usize>(cov: &[[f32; L]; 6]) -> [f32; L] {
    // The principal variance is at most the total and at most the
    // covariance's Frobenius norm: windows failing either test with those
    // bounds need no principal axis.
    let [c00, c01, c02, c11, c12, c22] = cov;
    let mut pass = [false; L];
    let mut total = [0.0f32; L];
    for i in 0..L {
        total[i] = c00[i] + c11[i] + c22[i];
        let off = c01[i] * c01[i] + c02[i] * c02[i] + c12[i] * c12[i];
        let frobenius = (c00[i] * c00[i] + c11[i] * c11[i] + c22[i] * c22[i] + 2.0 * off).sqrt();
        let bound = total[i].min(frobenius);
        pass[i] = bound > 3.0 * EDGE_LOW * EDGE_LOW && total[i] - bound < TWO_COLOUR_NONE * bound;
    }
    let mut out = [0.0f32; L];
    if !pass.contains(&true) {
        return out;
    }
    let (_, var) = principal(cov);
    for i in 0..L {
        let var = var[i];
        let off_line = ((total[i] - var) / var.max(1e-20)).max(0.0);
        let edge = (((var / 3.0).sqrt() - EDGE_LOW) / (EDGE_HIGH - EDGE_LOW)).clamp(0.0, 1.0);
        let two =
            ((TWO_COLOUR_NONE - off_line) / (TWO_COLOUR_NONE - TWO_COLOUR_FULL)).clamp(0.0, 1.0);
        out[i] = if pass[i] && var > 0.0 {
            edge * two
        } else {
            0.0
        };
    }
    out
}

/// The values of the `N` rows `rows[k]` at the `L` columns from `x0`.
#[inline(always)]
fn lanes_of<const N: usize, const L: usize>(rows: &[Vec<f32>], x0: usize) -> [[f32; L]; N] {
    let mut out = [[0.0f32; L]; N];
    for (o, row) in out.iter_mut().zip(rows) {
        o.copy_from_slice(&row[x0..x0 + L]);
    }
    out
}

/// The colour-line weights of the `L` windows of a row from column `x0`
/// into `out` (that row of weights), from the window sums of the square-
/// root moments.
fn weights_of_block<const L: usize>(root_sums: &[Vec<f32>], r: usize, x0: usize, out: &mut [f32]) {
    let inv_area = 1.0 / ((2 * r + 1) * (2 * r + 1)) as f32;
    let (_, cov) = moments(&lanes_of::<9, L>(root_sums, x0), inv_area);
    out[x0..x0 + L].copy_from_slice(&line_weights(&cov));
}

/// The projections of the `L` windows of mid row `m` from column `x0` onto
/// their colour lines, from the window sums of the linear moments,
/// into `coef` (that row of the ten map entries, see [`colour_line_tile`];
/// 0 where `needed` is false) and `centred` (the means and axes, row `m`).
#[allow(clippy::too_many_arguments)]
fn maps_of_block<const L: usize>(
    linear_sums: &[Vec<f32>],
    w: usize,
    m: usize,
    r: usize,
    x0: usize,
    needed: &[bool],
    weights: &[f32],
    coef: &mut [Vec<f32>],
    centred: &mut [Vec<f32>; 6],
) {
    let needed = &needed[x0..x0 + L];
    if !needed.contains(&true) {
        for c in coef.iter_mut() {
            c[x0..x0 + L].fill(0.0);
        }
        return;
    }
    let inv_area = 1.0 / ((2 * r + 1) * (2 * r + 1)) as f32;
    let (mean, cov) = moments(&lanes_of::<9, L>(linear_sums, x0), inv_area);
    let (axis, var) = principal(&cov);
    let mut maps = [[0.0f32; L]; 10];
    for i in 0..L {
        let e = [axis[0][i], axis[1][i], axis[2][i]];
        let mean = [mean[0][i], mean[1][i], mean[2][i]];
        let var = var[i];
        // A window of one colour (a clipped patch) maps to its mean.
        let shrink = var / (var + LINE_EPS);
        let t = shrink * (e[0] * mean[0] + e[1] * mean[1] + e[2] * mean[2]);
        let keep = needed[i];
        for c in 0..3 {
            maps[c][i] = if keep { mean[c] - t * e[c] } else { 0.0 };
        }
        for (k, &(a, b)) in PAIRS.iter().enumerate() {
            maps[3 + k][i] = if keep { shrink * e[a] * e[b] } else { 0.0 };
        }
        maps[9][i] = if keep && var > 0.0 {
            weights[x0 + i]
        } else {
            0.0
        };
    }
    for (c, map) in coef.iter_mut().zip(&maps) {
        c[x0..x0 + L].copy_from_slice(map);
    }
    for c in 0..3 {
        centred[c][m * w + x0..m * w + x0 + L].copy_from_slice(&mean[c]);
        centred[3 + c][m * w + x0..m * w + x0 + L].copy_from_slice(&axis[c]);
    }
}

/// The principal axis (unit vector) and variance of a window from its
/// covariance (the six [`PAIRS`] entries) by power iteration from the
/// covariance's largest column (three steps: the error falls with
/// (λ2/λ1)³, under 0.4 % where a window passes the two-colour test); axis
/// and variance 0 for a window of one colour.
#[inline(always)]
fn principal_of([a, b, c, d, e, f]: [f32; 6]) -> ([f32; 3], f32) {
    let n0 = a * a + b * b + c * c;
    let n1 = b * b + d * d + e * e;
    let n2 = c * c + e * e + f * f;
    let mut v = if n0 >= n1 && n0 >= n2 {
        [a, b, c]
    } else if n1 >= n2 {
        [b, d, e]
    } else {
        [c, e, f]
    };
    let mul = |v: [f32; 3]| {
        [
            a * v[0] + b * v[1] + c * v[2],
            b * v[0] + d * v[1] + e * v[2],
            c * v[0] + e * v[1] + f * v[2],
        ]
    };
    for _ in 0..3 {
        let inv = 1.0 / (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        v = mul(v.map(|t| t * inv));
    }
    let n = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    let flat = !(n > 1e-12 && n.is_finite());
    let v = if flat { [0.0; 3] } else { v.map(|t| t / n) };
    let cv = mul(v);
    let var = if flat {
        0.0
    } else {
        (v[0] * cv[0] + v[1] * cv[1] + v[2] * cv[2]).max(0.0)
    };
    (v, var)
}

/// Whether each of `w × rows` cells lies within `d` cells (rows and columns)
/// of a cell of `weights` above 0.
fn near_some(weights: &[f32], w: usize, rows: usize, d: usize) -> Vec<bool> {
    let mut along = vec![false; w * rows];
    for (row, out) in weights.chunks_exact(w).zip(along.chunks_exact_mut(w)) {
        let mut last = None;
        for x in 0..w {
            if row[x] > 0.0 {
                last = Some(x);
            }
            out[x] = last.is_some_and(|l| x - l <= d);
        }
        let mut next = None;
        for x in (0..w).rev() {
            if row[x] > 0.0 {
                next = Some(x);
            }
            out[x] |= next.is_some_and(|n| n - x <= d);
        }
    }
    let mut near = vec![false; w * rows];
    for y in 0..rows {
        let (first, last) = (y.saturating_sub(d), (y + d).min(rows - 1));
        let out = &mut near[y * w..(y + 1) * w];
        for k in first..=last {
            for (o, a) in out.iter_mut().zip(&along[k * w..(k + 1) * w]) {
                *o |= *a;
            }
        }
    }
    near
}

/// The colour-line stage's per-tile buffers (about 2 MB), kept for the
/// next tile a thread works on: fresh ones cost a fifth of the stage in
/// page faults (EOS D7200 benchmark photo 125 → 101 ms). Every value a
/// tile reads it has written first.
#[derive(Default)]
struct LineScratch {
    linear: [Vec<f32>; 3],
    root: [Vec<f32>; 3],
    hsum: [Vec<f32>; 18],
    weights: Vec<f32>,
    csum: [Vec<f32>; 10],
    centred: [Vec<f32>; 6],
}

/// `buffer` resized to `len` values (old values stay where they fit).
fn sized(buffer: &mut Vec<f32>, len: usize) -> &mut [f32] {
    buffer.resize(len, 0.0);
    buffer
}

/// The colour-line stage's changes to the pixels of `rows` × `columns` of
/// the `width × h` image `data`. Like rows (a band reads 2r rows around
/// it), the tile reads only 2r columns on either side; the box sums near
/// those columns' outer edges see the edge repeated (as at the image's
/// sides) and are wrong, but no changed pixel of the tile reads them.
#[allow(clippy::too_many_arguments)]
fn colour_line_tile(
    data: &[f32],
    width: usize,
    h: usize,
    stride: usize,
    r: usize,
    rows: std::ops::Range<usize>,
    columns: std::ops::Range<usize>,
    scratch: &mut LineScratch,
) -> Vec<(usize, [f32; 3])> {
    let (y0, y1) = (rows.start, rows.end);
    // The tile reads image columns x_first .. x_first + w and changes the
    // `inner` ones of them.
    let x_first = columns.start.saturating_sub(2 * r);
    let w = (columns.end + 2 * r).min(width) - x_first;
    let inner = columns.start - x_first..columns.end - x_first;
    let row_of = |y: isize| y.clamp(0, h as isize - 1) as usize;
    let inv_area = 1.0 / ((2 * r + 1) * (2 * r + 1)) as f32;
    let mut pad = vec![0.0f32; w + 2 * r];
    // Rows y0 - 2r .. y1 + 2r: linear RGB (the colour lines) and square-
    // root RGB (the edge and two-colour tests, on the scale of the
    // luminance pass), and the horizontal sums of their means and second
    // moments.
    let ext0 = y0 as isize - 2 * r as isize;
    let ext_rows = (y1 - y0) + 4 * r;
    let LineScratch {
        linear,
        root,
        hsum,
        weights,
        csum,
        centred,
    } = scratch;
    for buffer in linear
        .iter_mut()
        .chain(root.iter_mut())
        .chain(hsum.iter_mut())
    {
        sized(buffer, ext_rows * w);
    }
    for e in 0..ext_rows {
        let y = row_of(ext0 + e as isize);
        let src = &data[(y * width + x_first) * stride..(y * width + x_first + w) * stride];
        let span = e * w..(e + 1) * w;
        for c in 0..3 {
            let (lin, rt) = (&mut linear[c][span.clone()], &mut root[c][span.clone()]);
            for ((px, l), q) in src.chunks_exact(stride).zip(lin).zip(rt) {
                *l = px[c].max(0.0);
                *q = enc(px[c]);
            }
        }
    }
    let mut moment = vec![0.0f32; w];
    let (linear_sums, root_sums) = hsum.split_at_mut(9);
    for e in 0..ext_rows {
        moment_sums(
            root,
            e * w..(e + 1) * w,
            r,
            &mut pad,
            &mut moment,
            root_sums,
        );
    }
    // Rows y0 - r .. y1 + r: each window's weight; then, for windows within
    // 2r of a window of some weight (the only ones a changed pixel reads;
    // box sums are exact window sums, so the others may stay 0), its
    // projection onto its line as an affine map per output channel (offset
    // and three slopes); both summed along rows. Mid row m is ext row
    // m + r; its window spans ext rows m ..= m + 2r.
    let mid_rows = (y1 - y0) + 2 * r;
    let weights = sized(weights, mid_rows * w);
    let blocks = w - w % LANES;
    // A row of windows' sums of the nine moments.
    let mut sums: Vec<Vec<f32>> = (0..9).map(|_| vec![0.0f32; w]).collect();
    for (m, out) in weights.chunks_exact_mut(w).enumerate() {
        for (sum, rows) in sums.iter_mut().zip(root_sums.iter()) {
            box_column(rows, w, m, r, sum);
        }
        for x0 in (0..blocks).step_by(LANES) {
            weights_of_block::<LANES>(&sums, r, x0, out);
        }
        for x in blocks..w {
            weights_of_block::<1>(&sums, r, x, out);
        }
    }
    let weighted: Vec<bool> = weights
        .chunks_exact(w)
        .map(|row| row.iter().any(|&v| v > 0.0))
        .collect();
    if !weighted.contains(&true) {
        return Vec::new();
    }
    // The mid rows within 2r of a weighted one, and the linear moments of
    // the ext rows their windows span.
    let lined: Vec<bool> = (0..mid_rows)
        .map(|m| weighted[m.saturating_sub(2 * r)..(m + 2 * r + 1).min(mid_rows)].contains(&true))
        .collect();
    for e in 0..ext_rows {
        if lined[e.saturating_sub(2 * r)..(e + 1).min(mid_rows)].contains(&true) {
            moment_sums(
                linear,
                e * w..(e + 1) * w,
                r,
                &mut pad,
                &mut moment,
                linear_sums,
            );
        }
    }
    let needed = near_some(weights, w, mid_rows, 2 * r);
    // Per window: the offsets of the three channels, the six entries of the
    // projection's symmetric slope matrix (shrink · axis · axisᵀ, the
    // [`PAIRS`] entries) and the weight.
    for buffer in csum.iter_mut() {
        sized(buffer, mid_rows * w);
    }
    let mut coef: Vec<Vec<f32>> = (0..10).map(|_| vec![0.0f32; w]).collect();
    // The windows' means and axes, for the range test of the pixel each is
    // centred on.
    for buffer in centred.iter_mut() {
        sized(buffer, mid_rows * w);
    }
    for m in (0..mid_rows).filter(|&m| lined[m]) {
        let (weights, needed) = (&weights[m * w..(m + 1) * w], &needed[m * w..(m + 1) * w]);
        for (sum, rows) in sums.iter_mut().zip(linear_sums.iter()) {
            box_column(rows, w, m, r, sum);
        }
        for x0 in (0..blocks).step_by(LANES) {
            maps_of_block::<LANES>(&sums, w, m, r, x0, needed, weights, &mut coef, centred);
        }
        for x in blocks..w {
            maps_of_block::<1>(&sums, w, m, r, x, needed, weights, &mut coef, centred);
        }
        let span = m * w..(m + 1) * w;
        for (k, sum) in csum.iter_mut().enumerate() {
            box_row(&coef[k], r, &mut pad, &mut sum[span.clone()]);
        }
    }
    let mut changes = Vec::with_capacity(inner.len() * (y1 - y0));
    let mut model: Vec<Vec<f32>> = (0..10).map(|_| vec![0.0f32; w]).collect();
    let mut weight = vec![0.0f32; w];
    for y in y0..y1 {
        // Output row y is mid row y - y0 + r (its windows: mid rows y - y0
        // ..= y - y0 + 2r) and ext row y - y0 + 2r.
        let m0 = y - y0;
        if !weighted[m0..=m0 + 2 * r].contains(&true) {
            continue;
        }
        for (k, out) in model.iter_mut().enumerate() {
            box_column(&csum[k], w, m0, r, out);
        }
        // Spread over the window, as in the luminance pass.
        for (out, &sum) in weight.iter_mut().zip(&model[9]) {
            *out = (2.0 * sum * inv_area).min(1.0);
        }
        let e = y - y0 + 2 * r;
        let (mut mixes, mut mixes_at) = ([0.0f32; LANES], usize::MAX);
        for x in inner.clone() {
            if weight[x] <= 0.0 {
                continue;
            }
            let i = y * width + x_first + x;
            let px = &data[i * stride..][..3];
            if px.iter().any(|&c| c < 0.0 || !c.is_finite()) {
                continue;
            }
            let p = [
                linear[0][e * w + x],
                linear[1][e * w + x],
                linear[2][e * w + x],
            ];
            // Whole blocks of columns away from the image's sides at once.
            let x0 = x - x % LANES;
            let mix = if x0 >= r && x0 + LANES + r <= w {
                if mixes_at != x0 {
                    mixes = mixes_of_block(linear, centred, w, r, e, m0 + r, x0);
                    mixes_at = x0;
                }
                mixes[x - x0]
            } else {
                within_the_mixes(linear, centred, w, r, e, m0 + r, x, p)
            };
            let weight = weight[x] * mix;
            if weight <= 0.0 {
                continue;
            }
            // The [`PAIRS`] index of each slope matrix entry.
            const ENTRY: [[usize; 3]; 3] = [[0, 1, 2], [1, 3, 4], [2, 4, 5]];
            let s = |a: usize, b: usize| model[3 + ENTRY[a][b]][x];
            let out = std::array::from_fn(|c| {
                let projected =
                    (model[c][x] + s(c, 0) * p[0] + s(c, 1) * p[1] + s(c, 2) * p[2]) * inv_area;
                (p[c] + weight * (projected - p[c])).max(0.0)
            });
            changes.push((i, out));
        }
    }
    changes
}

/// The horizontal box sums of the three `channels` and their six [`PAIRS`]
/// products over the values at `span` into `sums` (the same span), with
/// `pad` and `moment` as scratch.
fn moment_sums(
    channels: &[Vec<f32>; 3],
    span: std::ops::Range<usize>,
    r: usize,
    pad: &mut [f32],
    moment: &mut [f32],
    sums: &mut [Vec<f32>],
) {
    for (k, sum) in sums.iter_mut().enumerate() {
        let row = if k < 3 {
            &channels[k][span.clone()]
        } else {
            let (a, b) = PAIRS[k - 3];
            for ((m, p), q) in moment
                .iter_mut()
                .zip(&channels[a][span.clone()])
                .zip(&channels[b][span.clone()])
            {
                *m = p * q;
            }
            &*moment
        };
        box_row(row, r, pad, &mut sum[span.clone()]);
    }
}

/// Misregistration mixes each channel of the two colours by its own share,
/// so it leaves every channel between the two colours' values: how far the
/// pixel `p` (linear RGB, at ext row `e` and column `x`) may move onto its
/// windows' lines, from 1 (inside the channel ranges of the two ends of the
/// line of the window centred on it, mid row `m`, a little beyond) to 0
/// (well outside: a small patch of a third colour on the edge, which can
/// pass the window's two-colour test).
#[allow(clippy::too_many_arguments)]
fn within_the_mixes(
    linear: &[Vec<f32>; 3],
    centred: &[Vec<f32>; 6],
    w: usize,
    r: usize,
    e: usize,
    m: usize,
    x: usize,
    p: [f32; 3],
) -> f32 {
    let mean: [f32; 3] = std::array::from_fn(|c| centred[c][m * w + x]);
    let axis: [f32; 3] = std::array::from_fn(|c| centred[3 + c][m * w + x]);
    // The line's ends: the window's extreme pixels along it.
    let (mut low, mut high) = (f32::MAX, f32::MIN);
    for row in e - r..=e + r {
        for dx in 0..=2 * r {
            let xx = (x + dx).saturating_sub(r).min(w - 1);
            let t = along(axis, mean, std::array::from_fn(|c| linear[c][row * w + xx]));
            low = low.min(t);
            high = high.max(t);
        }
    }
    mix_from_ends(axis, mean, low, high, p)
}

/// [`within_the_mixes`] for the [`LANES`] columns from `x0`, whose windows
/// lie inside the image (`r <= x0`, `x0 + LANES + r <= w`), one window
/// pixel at a time for all of them.
fn mixes_of_block(
    linear: &[Vec<f32>; 3],
    centred: &[Vec<f32>; 6],
    w: usize,
    r: usize,
    e: usize,
    m: usize,
    x0: usize,
) -> [f32; LANES] {
    let lanes = |v: &[f32], i: usize| -> [f32; LANES] { v[i..i + LANES].try_into().unwrap() };
    let mean: [[f32; LANES]; 3] = std::array::from_fn(|c| lanes(&centred[c], m * w + x0));
    let axis: [[f32; LANES]; 3] = std::array::from_fn(|c| lanes(&centred[3 + c], m * w + x0));
    let (mut low, mut high) = ([f32::MAX; LANES], [f32::MIN; LANES]);
    for row in e - r..=e + r {
        for dx in 0..=2 * r {
            let at = row * w + x0 + dx - r;
            let px: [[f32; LANES]; 3] = std::array::from_fn(|c| lanes(&linear[c], at));
            for i in 0..LANES {
                let t = axis[0][i] * (px[0][i] - mean[0][i])
                    + axis[1][i] * (px[1][i] - mean[1][i])
                    + axis[2][i] * (px[2][i] - mean[2][i]);
                low[i] = low[i].min(t);
                high[i] = high[i].max(t);
            }
        }
    }
    let p: [[f32; LANES]; 3] = std::array::from_fn(|c| lanes(&linear[c], e * w + x0));
    std::array::from_fn(|i| {
        mix_from_ends(
            axis.map(|a| a[i]),
            mean.map(|a| a[i]),
            low[i],
            high[i],
            p.map(|a| a[i]),
        )
    })
}

/// The position of `q` along the line through `mean` in direction `axis`.
#[inline(always)]
fn along(axis: [f32; 3], mean: [f32; 3], q: [f32; 3]) -> f32 {
    axis[0] * (q[0] - mean[0]) + axis[1] * (q[1] - mean[1]) + axis[2] * (q[2] - mean[2])
}

/// [`within_the_mixes`] from the line's ends (positions `low` and `high`
/// along it).
#[inline(always)]
fn mix_from_ends(axis: [f32; 3], mean: [f32; 3], low: f32, high: f32, p: [f32; 3]) -> f32 {
    let length = high - low;
    if axis == [0.0; 3] || length.is_nan() || length <= 0.0 {
        return 1.0;
    }
    let beyond = (0..3)
        .map(|c| {
            let (a, b) = (mean[c] + low * axis[c], mean[c] + high * axis[c]);
            (a.min(b) - p[c]).max(p[c] - a.max(b)).max(0.0)
        })
        .fold(0.0f32, f32::max)
        / length;
    ((MIX_NONE - beyond) / (MIX_NONE - MIX_FULL)).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb32FImage, RgbImage};

    fn chroma(p: [f32; 3]) -> f32 {
        let (r, g, b) = (enc(p[0]), enc(p[1]), enc(p[2]));
        (r - g).hypot(b - g)
    }

    /// The newest version the CPU computes (version 5, its GPU twin, is
    /// tested against it in the GPU crate's equivalence tests).
    const CPU_NEWEST: u64 = 4;

    fn run(img: &Rgb32FImage, version: u64) -> Rgb32FImage {
        let recipe = serde_json::json!({ VERSION_KEY: version });
        apply_for_recipe(
            Cow::Owned(DynamicImage::ImageRgb32F(img.clone())),
            &recipe,
            1.0,
        )
        .expect("the CPU pass")
        .into_owned()
        .into_rgb32f()
    }

    /// A dark neutral line on a bright sky, with lateral-CA-like fringes: red
    /// on one side, cyan on the other.
    fn branch(width: u32) -> Rgb32FImage {
        Rgb32FImage::from_fn(40, 40, |x, _| {
            let x = x as i32;
            let (l, r) = (18, 18 + width as i32);
            if x >= l && x < r {
                image::Rgb([0.05, 0.05, 0.05])
            } else if x == l - 1 {
                image::Rgb([0.55, 0.3, 0.3])
            } else if x == r {
                image::Rgb([0.3, 0.55, 0.55])
            } else {
                image::Rgb([0.8, 0.8, 0.8])
            }
        })
    }

    #[test]
    fn fringes_beside_a_dark_line_lose_most_of_their_colour() {
        for (version, width) in (1..=CPU_NEWEST).flat_map(|v| [1, 2, 3].map(|w| (v, w))) {
            let before = branch(width);
            let after = run(&before, version);
            for x in [17u32, 18 + width] {
                let (c0, c1) = (
                    chroma(before.get_pixel(x, 20).0),
                    chroma(after.get_pixel(x, 20).0),
                );
                assert!(
                    c1 < 0.4 * c0,
                    "v{version} width {width} x {x}: chroma {c0} -> {c1}"
                );
            }
        }
    }

    #[test]
    fn version_2_also_clears_two_pixel_wide_fringes() {
        // A lateral-CA rest of two pixels on each side of a dark line.
        let before = Rgb32FImage::from_fn(40, 40, |x, _| match x {
            18..=19 => image::Rgb([0.05, 0.05, 0.05]),
            16..=17 => image::Rgb([0.55, 0.3, 0.3]),
            20..=21 => image::Rgb([0.3, 0.55, 0.55]),
            _ => image::Rgb([0.8, 0.8, 0.8]),
        });
        let left = |version| {
            let after = run(&before, version);
            [16u32, 17, 20, 21]
                .iter()
                .map(|&x| chroma(after.get_pixel(x, 20).0))
                .sum::<f32>()
        };
        let (v1, v2) = (left(1), left(2));
        assert!(v2 < 0.7 * v1, "fringe chroma v1 {v1} v2 {v2}");
    }

    #[test]
    fn real_two_colour_edges_and_thin_coloured_lines_keep_their_colour() {
        // A saturated red line (1-3 px) on grey and a red object's edge.
        for width in [1i32, 2, 3, 12] {
            let before = Rgb32FImage::from_fn(40, 40, |x, _| {
                if (18..18 + width).contains(&(x as i32)) {
                    image::Rgb([0.5, 0.04, 0.03])
                } else {
                    image::Rgb([0.7, 0.7, 0.7])
                }
            });
            for version in 1..=CPU_NEWEST {
                let after = run(&before, version);
                for x in 18..(18 + width as u32).min(40) {
                    let (c0, c1) = (
                        chroma(before.get_pixel(x, 20).0),
                        chroma(after.get_pixel(x, 20).0),
                    );
                    assert!(
                        c1 > 0.95 * c0,
                        "v{version} width {width} x {x}: chroma {c0} -> {c1}"
                    );
                }
            }
        }
    }

    #[test]
    fn flat_colour_texture_and_recipes_without_the_key_are_untouched() {
        // Colour noise on a flat area: no edge, nothing changes.
        let noisy = Rgb32FImage::from_fn(32, 32, |x, y| {
            let n = ((x * 7 + y * 13) % 5) as f32 * 0.01;
            image::Rgb([0.4 + n, 0.4, 0.4 - n])
        });
        assert_eq!(run(&noisy, CPU_NEWEST), noisy);
        let fringed = branch(2);
        assert_eq!(run(&fringed, 0), fringed);
    }

    /// Flat grey with Gaussian-like noise of standard deviation `sigma` in
    /// the square-root encoding (sum of uniforms from a fixed generator).
    fn noisy_grey(sigma: f32) -> Rgb32FImage {
        let mut state = 0x2545_f491_u32;
        let mut uniform = move || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as f32 / u32::MAX as f32 - 0.5
        };
        Rgb32FImage::from_fn(96, 96, |_, _| {
            let mut channel = || {
                let n = (0..12).map(|_| uniform()).sum::<f32>();
                (0.6 + sigma * n).max(0.0).powi(2)
            };
            image::Rgb([channel(), channel(), channel()])
        })
    }

    #[test]
    fn the_noise_estimate_follows_the_noise() {
        for sigma in [0.005f32, 0.02, 0.05] {
            let img = noisy_grey(sigma);
            let noise = luminance_noise(img.as_raw(), 96, 96, 3);
            // Luminance averages four channel samples: about half the noise.
            let expected = sigma * (6.0f32).sqrt() / 4.0;
            assert!(
                (noise / expected - 1.0).abs() < 0.25,
                "sigma {sigma}: noise {noise} vs {expected}"
            );
        }
    }

    #[test]
    fn versions_2_and_3_render_noisy_photos_as_version_1() {
        let noisy = noisy_grey(0.05);
        assert_eq!(run(&noisy, 2), run(&noisy, 1));
        assert_eq!(run(&noisy, 3), run(&noisy, 1));
        let clean = noisy_grey(0.004);
        assert!(luminance_noise(clean.as_raw(), 96, 96, 3) < 0.5 * EDGE_LOW);
        assert_eq!(radius_for(2, 0.004), 3.0);
        assert_eq!(radius_for(1, 0.004), 2.0);
    }

    fn linear_mix(a: [f32; 3], b: [f32; 3], t: f32) -> image::Rgb<f32> {
        image::Rgb(std::array::from_fn(|c| t * a[c] + (1.0 - t) * b[c]))
    }

    /// Linear-light cyan and magenta of about equal luminance.
    const CYAN: [f32; 3] = [0.0, 0.37, 0.55];
    const MAGENTA: [f32; 3] = [0.64, 0.10, 0.35];

    /// The share of `a`'s colour (0 at `b`) along the line from `b` to `a`
    /// and the distance from that line, in linear RGB.
    fn off_line(p: [f32; 3], a: [f32; 3], b: [f32; 3]) -> f32 {
        let d: [f32; 3] = std::array::from_fn(|c| a[c] - b[c]);
        let q: [f32; 3] = std::array::from_fn(|c| p[c] - b[c]);
        let dd = d.iter().map(|v| v * v).sum::<f32>();
        let t = q.iter().zip(&d).map(|(x, y)| x * y).sum::<f32>() / dd;
        q.iter()
            .zip(&d)
            .map(|(x, y)| (x - t * y).powi(2))
            .sum::<f32>()
            .sqrt()
    }

    #[test]
    fn version_3_removes_a_misregistered_seam_between_two_colours() {
        // Cyan meets magenta over a 2 px blur, but red rises a pixel before
        // green falls (the demosaic's red interpolated across the edge): a
        // light seam neither colour has, at about equal luminance, which the
        // luminance model of versions 1 and 2 cannot see.
        let edge = |x: f32| ((x - 19.5) / 2.0 + 0.5).clamp(0.0, 1.0);
        let before = Rgb32FImage::from_fn(40, 40, |x, _| {
            let (tg, tr) = (edge(x as f32), edge(x as f32 + 1.0));
            image::Rgb([
                tr * MAGENTA[0] + (1.0 - tr) * CYAN[0],
                tg * MAGENTA[1] + (1.0 - tg) * CYAN[1],
                tg * MAGENTA[2] + (1.0 - tg) * CYAN[2],
            ])
        });
        let seam = |img: &Rgb32FImage| {
            (12..28)
                .map(|x| off_line(img.get_pixel(x, 20).0, MAGENTA, CYAN))
                .fold(0.0f32, f32::max)
        };
        let s0 = seam(&before);
        assert!(s0 > 0.1, "seam {s0}");
        let (v2, v3) = (seam(&run(&before, 2)), seam(&run(&before, 3)));
        assert!(v2 > 0.95 * s0, "version 2 leaves the seam: {s0} -> {v2}");
        assert!(v3 < 0.65 * s0, "version 3: {s0} -> {v3}");
    }

    #[test]
    fn version_3_keeps_blurred_two_colour_edges_and_a_third_colour() {
        // A clean edge between two colours (blurred in linear light) is on
        // its colour line already: version 3 moves it no more than the
        // luminance model alone (which bends a saturated one a little).
        for (a, b) in [
            (CYAN, MAGENTA),
            ([0.95, 0.42, 0.0], [0.28, 0.22, 0.10]),
            ([0.8, 0.8, 0.85], [0.02, 0.02, 0.02]),
        ] {
            let before = Rgb32FImage::from_fn(40, 40, |x, _| {
                linear_mix(a, b, ((x as f32 - 19.5) / 3.0 + 0.5).clamp(0.0, 1.0))
            });
            let moved = |after: Rgb32FImage| {
                after
                    .pixels()
                    .zip(before.pixels())
                    .flat_map(|(p, q)| (0..3).map(move |c| (p[c] - q[c]).abs()))
                    .fold(0.0f32, f32::max)
            };
            let (v2, v3) = (moved(run(&before, 2)), moved(run(&before, 3)));
            assert!(
                v3 <= v2 + 1e-4,
                "{a:?}/{b:?}: moved {v2} by version 2, {v3} by 3"
            );
        }
        // A green stripe of three pixels between magenta and grey: three
        // colours in a window, the stripe keeps its colour.
        let before = Rgb32FImage::from_fn(40, 40, |x, _| match x {
            0..=17 => image::Rgb(MAGENTA),
            18..=20 => image::Rgb([0.05, 0.45, 0.08]),
            _ => image::Rgb([0.3, 0.3, 0.3]),
        });
        let after = run(&before, 3);
        for x in 18..=20 {
            let (c0, c1) = (
                chroma(before.get_pixel(x, 20).0),
                chroma(after.get_pixel(x, 20).0),
            );
            assert!(c1 > 0.9 * c0, "stripe x {x}: chroma {c0} -> {c1}");
        }
    }

    #[test]
    fn version_3_keeps_a_small_third_colour_on_a_two_colour_edge() {
        // A 2 × 2 green patch on the edge between cyan and magenta, all of
        // about the same luminance (version 2 leaves it alone): no mix of
        // the two sides makes its colour, so it is no misregistration.
        let green = [0.15, 0.48, 0.13];
        let before = Rgb32FImage::from_fn(25, 25, |x, y| {
            if (12..=13).contains(&x) && (12..=13).contains(&y) {
                image::Rgb(green)
            } else if x < 13 {
                image::Rgb([0.0, 0.35, 0.54])
            } else {
                image::Rgb([0.64, 0.10, 0.40])
            }
        });
        assert_eq!(run(&before, 2), before);
        let after = run(&before, 3);
        for (x, y) in [(12, 12), (13, 12), (12, 13), (13, 13)] {
            let p = after.get_pixel(x, y).0;
            for c in 0..3 {
                assert!(
                    (p[c] - green[c]).abs() < 0.03,
                    "({x}, {y}): {green:?} -> {p:?}"
                );
            }
        }
    }

    /// Patches of saturated colours with curved edges whose red and blue
    /// lie a pixel off green, slight noise, clipped (zero), negative and
    /// very bright pixels, and with `seed` 2 also infinite, NaN and huge
    /// (1e20) ones: windows of every kind the stages tell apart.
    fn misregistered_patches(w: u32, h: u32, seed: u32) -> Rgb32FImage {
        const COLOURS: [[f32; 3]; 5] = [
            [0.0, 0.37, 0.55],
            [0.64, 0.10, 0.35],
            [0.95, 0.42, 0.0],
            [0.28, 0.22, 0.10],
            [0.8, 0.8, 0.85],
        ];
        let patch = |x: f32, y: f32| {
            let v = ((x * 0.21 + (y * 0.13).sin() * 3.0) / 6.0).floor() as i32
                + ((y * 0.17 + (x * 0.11).cos() * 2.0) / 7.0).floor() as i32 * 3;
            COLOURS[(v + seed as i32).rem_euclid(5) as usize]
        };
        let mut state = 0x9e37_79b9_u32 ^ seed;
        Rgb32FImage::from_fn(w, h, |x, y| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let noise = (state as f32 / u32::MAX as f32 - 0.5) * 0.004;
            let (x, y) = (x as f32, y as f32);
            let mut px = [
                patch(x + 1.0, y)[0],
                patch(x, y)[1],
                patch(x - 0.6, y + 0.4)[2],
            ]
            .map(|c| c + noise);
            match state % 97 {
                0 => px = [0.0; 3],
                1 => px[1] = -0.01,
                2 => px = px.map(|c| 6.0 * c),
                3 if seed == 2 => px[0] = f32::INFINITY,
                4 if seed == 2 => px[2] = f32::NAN,
                5 if seed == 2 => px[1] = 1e20,
                _ => {}
            }
            image::Rgb(px)
        })
    }

    #[test]
    fn the_stages_match_the_reference_bit_for_bit() {
        // The tiles of `w`-wide rows `rows` of `tile` columns each, their
        // changes in pixel order.
        fn tiled<T>(
            w: usize,
            tile: usize,
            mut one: impl FnMut(std::ops::Range<usize>) -> Vec<(usize, T)>,
        ) -> Vec<(usize, T)> {
            let mut all: Vec<_> = (0..w.div_ceil(tile))
                .flat_map(|t| one(t * tile..((t + 1) * tile).min(w)))
                .collect();
            all.sort_by_key(|&(i, _)| i);
            all
        }
        let lines = |changes: Vec<(usize, [f32; 3])>| {
            changes
                .into_iter()
                .map(|(i, px)| (i, px.map(f32::to_bits)))
                .collect::<Vec<_>>()
        };
        let luminance = |changes: Vec<(usize, f32, f32)>| {
            changes
                .into_iter()
                .map(|(i, red, blue)| (i, [red.to_bits(), blue.to_bits()]))
                .collect::<Vec<_>>()
        };
        // One scratch for all, as a thread keeps it from tile to tile.
        let mut scratch = LineScratch::default();
        let mut moved = [0, 0];
        let sizes = [
            (1, 9),
            (5, 30),
            (8, 8),
            (13, 41),
            (37, 50),
            (67, 2 * LINE_BAND as u32 + 5),
        ];
        for (w, h) in sizes {
            for seed in 0..3 {
                let img = misregistered_patches(w, h, seed);
                let (raw, w, h) = (img.as_raw(), w as usize, h as usize);
                for r in [1, 2, 3, 4] {
                    for (y0, y1) in [(0, h), (h / 3, h / 3 + h.min(LINE_BAND) / 2 + 1)] {
                        let old = luminance(reference::suppress_band(raw, w, h, 3, r, y0, y1));
                        let old_lines = lines(reference::colour_line_band(raw, w, h, 3, r, y0, y1));
                        // Whole rows, and tiles of 1, 5 and 16 columns.
                        for tile in [w, 1, 5, 16] {
                            let at = format!(
                                "{w} x {h}, seed {seed}, r {r}, rows {y0}..{y1}, tile {tile}"
                            );
                            let new = tiled(w, tile, |columns| {
                                luminance(suppress_tile(raw, w, h, 3, r, y0..y1, columns))
                            });
                            assert_eq!(new, old, "luminance stage, {at}");
                            let new = tiled(w, tile, |columns| {
                                let rows = y0..y1;
                                lines(colour_line_tile(
                                    raw,
                                    w,
                                    h,
                                    3,
                                    r,
                                    rows,
                                    columns,
                                    &mut scratch,
                                ))
                            });
                            assert_eq!(new, old_lines, "colour lines, {at}");
                        }
                        moved[0] += old.len();
                        moved[1] += old_lines.len();
                    }
                }
            }
        }
        assert!(
            moved[0] > 2000 && moved[1] > 2000,
            "only {moved:?} pixels moved"
        );
    }

    #[test]
    fn the_result_does_not_depend_on_the_band_layout() {
        // Taller than several bands, with edges across band borders.
        let img = Rgb32FImage::from_fn(37, 3 * BAND as u32 + 7, |x, y| {
            let v = ((x * 31 + y * 17) % 11) as f32 / 11.0;
            if (x + y) % 9 < 2 {
                image::Rgb([0.05 + 0.3 * v, 0.05, 0.1])
            } else {
                image::Rgb([0.8, 0.75 + 0.1 * v, 0.7])
            }
        });
        // Version 3's stages (the chromatic-aberration stage of version 4
        // has its own test of tiles).
        let banded = run(&img, 3);
        // Reference: the same per pixel computation with the whole image as
        // one band.
        let mut whole = img.clone();
        let (w, h) = (img.width() as usize, img.height() as usize);
        let plan = plan_for(3, luminance_noise(img.as_raw(), w, h, 3), 1.0);
        assert!(plan.colour_lines);
        let changes = suppress_tile(img.as_raw(), w, h, 3, plan.radius, 0..h, 0..w);
        for (i, red, blue) in changes {
            whole.as_mut()[i * 3] = red;
            whole.as_mut()[i * 3 + 2] = blue;
        }
        let first = whole.clone();
        let mut scratch = LineScratch::default();
        let all = colour_line_tile(
            first.as_raw(),
            w,
            h,
            3,
            plan.radius,
            0..h,
            0..w,
            &mut scratch,
        );
        for (i, px) in all {
            whole.as_mut()[i * 3..i * 3 + 3].copy_from_slice(&px);
        }
        assert_eq!(banded, whole);
    }

    #[test]
    fn a_draft_leaves_out_the_chromatic_aberration_stage_only() {
        // A blue fringe on the dark side of a bright edge.
        let fringed = Rgb32FImage::from_fn(40, 40, |x, _| match x {
            0..=15 => image::Rgb([0.02, 0.02, 0.02]),
            16..=19 => image::Rgb([0.03, 0.03, 0.03 + 0.06 * (x - 15) as f32]),
            _ => image::Rgb([0.8, 0.8, 0.8]),
        });
        let draft = serde_json::json!({ VERSION_KEY: 4, DRAFT_KEY: true });
        let drafted = apply_for_recipe(
            Cow::Owned(DynamicImage::ImageRgb32F(fringed.clone())),
            &draft,
            1.0,
        )
        .expect("the CPU pass")
        .into_owned()
        .into_rgb32f();
        // Version 3's stages with the same plan for them.
        assert_eq!(drafted, run(&fringed, 3));
        assert_ne!(run(&fringed, 4), drafted);
        assert!(has_draft(&serde_json::json!({ VERSION_KEY: 4 })));
        assert!(!has_draft(&serde_json::json!({ VERSION_KEY: 3 })));
    }

    #[test]
    fn reduced_previews_skip_and_other_formats_pass_through() {
        let recipe = serde_json::json!({ VERSION_KEY: VERSION });
        let fringed = DynamicImage::ImageRgb32F(branch(2));
        let small = apply_for_recipe(Cow::Borrowed(&fringed), &recipe, 0.25);
        assert!(matches!(small, Ok(Cow::Borrowed(_))));
        let eight = DynamicImage::ImageRgb8(RgbImage::new(8, 8));
        assert!(matches!(
            apply_for_recipe(Cow::Borrowed(&eight), &recipe, 1.0),
            Ok(Cow::Borrowed(_))
        ));
    }

    #[test]
    fn version_5_without_a_gpu_fails_instead_of_falling_back() {
        // No device is registered in these tests.
        let recipe = serde_json::json!({ VERSION_KEY: 5 });
        let fringed = DynamicImage::ImageRgb32F(branch(2));
        let error =
            apply_for_recipe(Cow::Borrowed(&fringed), &recipe, 1.0).expect_err("no CPU fallback");
        assert!(error.contains("graphics card"), "{error}");
        let plan = plan_for(5, 0.0, 1.0);
        assert!(plan.gpu && plan.colour_lines && plan.axial.is_some());
        assert_eq!(
            Plan { gpu: false, ..plan },
            plan_for(4, 0.0, 1.0),
            "version 5 plans version 4's stages"
        );
    }

    #[test]
    fn the_default_is_only_for_never_edited_raws() {
        let mut never = serde_json::Value::Null;
        apply_default(&mut never, true, true);
        assert_eq!(version_of(&never), VERSION);
        let mut saved = serde_json::json!({"exposure": 0.3});
        apply_default(&mut saved, false, true);
        assert!(saved.get(VERSION_KEY).is_none());
        let mut jpeg = serde_json::json!({});
        apply_default(&mut jpeg, true, false);
        assert!(jpeg.get(VERSION_KEY).is_none());
        assert_eq!(version_of(&serde_json::json!({VERSION_KEY: 7})), VERSION);
    }
}
