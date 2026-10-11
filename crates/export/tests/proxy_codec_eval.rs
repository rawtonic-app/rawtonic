//! Measures the preview proxy encoding (rawtonic_engine::proxy_codec)
//! against the earlier lossless half floats (RTPX v1) on real RAWs: size,
//! encode/decode time (bands in parallel, as the app writes them) and the
//! rendered difference (export pipeline, 16-bit sRGB, default recipe and a
//! pushed one): CIEDE2000 mean/p99/p99.9/max, largest 8-bit step, 8x8
//! block-mean CIEDE2000 (p99.9/max: colour shifts, banding, blocking) and
//! the grain energy kept (8x8 block variance of L*, 1.0 = unchanged), the
//! last two also in the shadows alone (blocks below L* 30 in the default
//! rendering, where a pushed recipe lifts them).
//! RAWTONIC_PROXY_EVAL_SIZE=3072 measures at a Standard preview's size.
//! Format 2 (linear JPEG XL at distance 0.25) and other JPEG XL distances,
//! efforts and transfers are listed for comparison.
//!
//! RAWTONIC_LENSFUN_DB=src-tauri/lensfun_db RAWTONIC_PROXY_EVAL="a.RAF;b.NEF" \
//!   cargo test --release -p rawtonic-export --test proxy_codec_eval -- --ignored --nocapture
//! Optional: RAWTONIC_PROXY_EVAL_OUT=dir (crops around the largest error,
//! or at RAWTONIC_PROXY_EVAL_AT="x,y"),
//! RAWTONIC_PROXY_EVAL_CODECS=jpeg12 (filter by name),
//! RAWTONIC_PROXY_EVAL_SPECS="half0.1:0.28,lift0.3:0.25,lin:0.15" (the list
//! after the shipped encoding and format 2: `transfer:distance[:effort]`,
//! transfers lin, liftK, logK, powPtT, toeAxX0, halfK; see `Transfer`).
//!
//! Measured 2026-10-10 (eight cameras: X-T5, Z7, R5 II, A7C, R10 ISO
//! 12800, S5, CFV 50C, iPhone 12 Pro; 1:1, mean over the eight; pushed =
//! +1.5 EV, Shadows +100, Highlights -100, Contrast 40, Clarity 50,
//! Saturation 30):
//!
//! | encoding | MB | default mean/p99 | pushed mean/p99 | shadow grain | shadow dE |
//! |---|---|---|---|---|---|
//! | format 2: linear d0.25 | 13.4 | 0.187/1.25 | 0.354/2.38 | 0.976 | 0.54 |
//! | half k 0.1 d0.25 (#52) | 14.9 | 0.191/1.19 | 0.338/1.97 | 0.990 | 0.45 |
//! | half k 0.1 d0.28 | 14.0 | 0.210/1.30 | 0.371/2.14 | 0.987 | 0.49 |
//!
//! Second run after the shadow rendering changed (#50; pushed values lower
//! for every encoding): format 2 13.4 MB, 0.187/1.25, 0.338/2.27, 0.976,
//! 0.53; half (#52) 14.9 MB, 0.191/1.19, 0.324/1.86, 0.990, 0.44;
//! half-linear k 0.1 knee 0.18 d0.25 (shipped since) 15.3 MB, 0.180/1.15,
//! 0.307/1.82, 0.990, 0.43 (A7C default p99 0.90 -> 0.82, iPhone 1.53 ->
//! 1.33; format 2 0.80, 1.28). Smaller k at a coarser distance keeps the
//! shadows at less size but costs the default recipe (k 0.05 knee 0.15
//! d0.30: 13.8 MB, default p99 1.37; k 0.03 knee 0.15 d0.33: 13.1 MB,
//! 1.52, pushed 2.31, R5 II 12.4 MB); efforts 5 and 7 at d0.25 give 12.4
//! and 12.1 MB at five cameras (shipped 10.1) for slightly better quality
//! and no better quality per byte (d0.3 e5: 11.1 MB, default p99 1.52).
//!
//! At four cameras (R5 II, X-T5, A7C, R10; format 2 there 14.0 MB, pushed
//! p99 2.50): linear d0.15 19.3 MB (1.63), d0.18 17.4 MB (1.89); rational
//! lift k 0.3 d0.25 16.9 MB (1.86, but A7C default p99 0.80 -> 0.98); toe
//! gains x (1 + a x0 / (x + x0)) a 3 x0 0.03 14.7 MB (2.27), a 8 x0 0.01
//! 14.0 MB (2.60). Standard (3072 px, eight cameras): format 2 2.36 MB,
//! pushed p99 2.34; half 2.48 MB, 2.01; half k 0.1 d0.3 2.20 MB, 2.28
//! but default p99 1.29 -> 1.44; second run: format 2 2.36 MB, default
//! p99 1.29, pushed 2.23; half-linear 2.58 MB, 1.20, 1.86.
//!
//! Against Lightroom's full-size preview level (2026-10-10: 3.9-13.1 MB at
//! 24-61 MP, about 0.16-0.21 MB/MP; ours 0.20-0.38 MB/MP, the 61 MP
//! A7R V 21.2 MB), four cameras (R5 II, A7R V, X-T5, A7C), 1:1, shipped
//! 13.0 MB, 0.315 MB/MP, default p99 1.47, pushed p99 2.71 (mean over
//! the four). `RAWTONIC_PROXY_EVAL_SPECS` knobs (see `Spec`):
//! - Encoder choices at d0.25: custom coefficient orders (effort 4's
//!   only change) 14.6 -> 14.5 MB on the R5 II, same pixels; AC strategy
//!   search with 16/32/64-pixel transforms, 4x8/AFV or CfL two-pass 14.6
//!   -> 15.2 MB and p99 1.25 -> 1.35 (larger and worse).
//! - Chroma: luminance alone takes 84 % of the bytes (10.95 of 13.0 MB).
//!   Chroma at half resolution (2x2 means, bilinear or luminance-guided
//!   upsampling) saves 2.3 % for default p99 1.47 -> 1.54. Chroma scaled
//!   by 1.5/2 before encoding (finer XYB chroma) costs 9/22 % more bytes
//!   for p99 -3/-5 %; at equal size (x2 d0.3) p99 is worse (1.60).
//! - Where the worst 1 % lie (`RAWTONIC_PROXY_EVAL_BANDS`): mostly L*
//!   25-75 of the rendering, high chroma on the A7R V and A7C (60-79 %
//!   above C* 30), low chroma on the X-T5: no small region to spend bits
//!   on instead.
//! - Distance: d0.35 10.4 MB (0.253 MB/MP), default/pushed p99 1.85/3.46;
//!   d0.5 8.2 MB (0.200 MB/MP, Lightroom's level), 2.27/4.33 (+55/+60 %).
//! - Nine cameras (the eight above and the A7R V): shipped 12.0 MB, p99
//!   1.34/2.22; luminance-guided half-resolution chroma 11.6 MB,
//!   1.45/2.37; d0.35 9.8 MB, 1.68/2.81; d0.5 7.9 MB, 2.08/3.48. The R10
//!   at ISO 32000 is the outlier: 28.5 MB (1.19 MB/MP) at p99 0.40/0.68,
//!   because the rendering's noise reduction hides the error in the
//!   noise; d0.5 21.4 MB at 0.78/1.31. A distance that follows the noise
//!   level would shrink high-ISO previews without reaching base-ISO p99
//!   (not done: base-ISO photos are the ones above Lightroom's size).
//!
//! Lightroom's 1:1 is a rendered 8-bit JPEG; ours keeps the linear data
//! for full-resolution editing offline, and every way to its size found
//! so far costs p99 roughly in proportion (no encoding of the same pixels
//! is materially smaller).

use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use image::{DynamicImage, Rgb32FImage};
use rayon::prelude::*;

use rawtonic_engine::output_color_space::OutputColorSpace;
use rawtonic_export::export_processing::{
    ExportSource, RenderEnv, process_image_for_export_pipeline,
};
use rawtonic_gpu::gpu_processing::{GpuContext, RenderOutputPrecision};

// ---------- f16 baseline (copy of RTPX v1 band coding) ----------
fn f16_bits(v: f32) -> u16 {
    half::f16::from_f32(v).to_bits()
}
fn enc_f16(px: &[f32], width: usize) -> Vec<u8> {
    let rows = px.len() / (width * 3);
    let mut low = Vec::with_capacity(px.len());
    let mut high = Vec::with_capacity(px.len());
    for row in 0..rows {
        let start = row * width * 3;
        let mut prev = [0u16; 3];
        for x in 0..width {
            for c in 0..3 {
                let b = f16_bits(px[start + x * 3 + c]);
                let d = b.wrapping_sub(prev[c]);
                prev[c] = b;
                low.push(d as u8);
                high.push((d >> 8) as u8);
            }
        }
    }
    low.extend_from_slice(&high);
    let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
    e.write_all(&low).unwrap();
    e.finish().unwrap()
}
fn dec_f16(bytes: &[u8], width: usize, rows: usize) -> Vec<f32> {
    let count = width * rows * 3;
    let mut planes = Vec::new();
    flate2::read::DeflateDecoder::new(bytes)
        .read_to_end(&mut planes)
        .unwrap();
    let (low, high) = planes.split_at(count);
    let mut out = Vec::with_capacity(count);
    for row in 0..rows {
        let mut prev = [0u16; 3];
        for x in 0..width {
            let i = (row * width + x) * 3;
            for c in 0..3 {
                let d = low[i + c] as u16 | (high[i + c] as u16) << 8;
                prev[c] = prev[c].wrapping_add(d);
                out.push(half::f16::from_bits(prev[c]).to_f32());
            }
        }
    }
    out
}

const BAND: usize = 256;

trait Codec: Sync {
    fn name(&self) -> String;
    fn enc(&self, band: &[f32], w: usize) -> Vec<u8>;
    fn dec(&self, bytes: &[u8], w: usize, rows: usize) -> Vec<f32>;
}

struct F16;
impl Codec for F16 {
    fn name(&self) -> String {
        "f16-deflate (RTPX v1)".into()
    }
    fn enc(&self, band: &[f32], w: usize) -> Vec<u8> {
        // RTPX v1 bands are 64 rows
        band.chunks(w * 3 * 64)
            .flat_map(|b| {
                let e = enc_f16(b, w);
                let mut o = (e.len() as u32).to_le_bytes().to_vec();
                o.extend(e);
                o
            })
            .collect()
    }
    fn dec(&self, bytes: &[u8], w: usize, rows: usize) -> Vec<f32> {
        let mut out = Vec::new();
        let mut at = 0;
        let mut left = rows;
        while left > 0 {
            let r = left.min(64);
            let n = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
            at += 4;
            out.extend(dec_f16(&bytes[at..at + n], w, r));
            at += n;
            left -= r;
        }
        out
    }
}

/// The shipped encoding (rawtonic_engine::proxy_codec).
struct Shipped;
impl Codec for Shipped {
    fn name(&self) -> String {
        format!(
            "jxl {:?} d{} (shipped)",
            rawtonic_engine::proxy_codec::TRANSFER,
            rawtonic_engine::proxy_codec::DISTANCE
        )
    }
    fn enc(&self, band: &[f32], w: usize) -> Vec<u8> {
        rawtonic_engine::proxy_codec::encode(
            band,
            w,
            rawtonic_engine::proxy_codec::DISTANCE,
            rawtonic_engine::proxy_codec::TRANSFER,
        )
        .unwrap()
    }
    fn dec(&self, bytes: &[u8], w: usize, rows: usize) -> Vec<f32> {
        rawtonic_engine::proxy_codec::decode(bytes, w, rows, rawtonic_engine::proxy_codec::TRANSFER)
            .unwrap()
    }
}

struct Jxl {
    distance: f32,
    effort: u8,
    /// Linear gain before encoding (undone after decoding): more bits for
    /// the shadows, which edits lift.
    gain: f32,
}
impl Codec for Jxl {
    fn name(&self) -> String {
        format!(
            "jxl linear d{} e{} gain {}",
            self.distance, self.effort, self.gain
        )
    }
    fn enc(&self, band: &[f32], w: usize) -> Vec<u8> {
        let bytes: Vec<u8> = band
            .iter()
            .flat_map(|v| (v * self.gain).to_ne_bytes())
            .collect();
        jxl_encoder::LossyConfig::new(self.distance)
            .with_effort(self.effort)
            .encode(
                &bytes,
                w as u32,
                (band.len() / 3 / w) as u32,
                jxl_encoder::PixelLayout::RgbLinearF32,
            )
            .unwrap()
    }
    fn dec(&self, bytes: &[u8], _w: usize, _rows: usize) -> Vec<f32> {
        let mut image = jxl_oxide::JxlImage::builder()
            .read(std::io::Cursor::new(bytes))
            .unwrap();
        image.request_color_encoding(jxl_oxide::EnumColourEncoding::srgb_linear(
            jxl_oxide::RenderingIntent::Relative,
        ));
        let render = image.render_frame(0).unwrap();
        render
            .image_all_channels()
            .buf()
            .iter()
            .map(|v| v / self.gain)
            .collect()
    }
}

/// A transfer applied per channel before encoding and undone after
/// decoding (odd, so negative channels keep their sign), then JPEG XL of
/// the transferred values as if they were linear: steeper curves give the
/// shadows more of the encoder's precision.
#[derive(Clone, Copy)]
enum Transfer {
    Linear,
    /// x (1 + k) / (x + k) up to 1, then on with its slope at 1.
    Lift(f32),
    /// ln(1 + x / k) / ln(1 + 1 / k).
    Log(f32),
    /// x^p above a linear toe at `t` (continuous value).
    Power(f32, f32),
    /// x (1 + a x0 / (x + x0)): a gain of 1 + a in the deep shadows, none
    /// in the highlights.
    Toe(f32, f32),
    /// x sqrt((1 + k) / (x + k)): slope 1 in the shadows, square root in
    /// the highlights (log slope from 1 down to 1/2, never below).
    Half(f32),
    /// `Half(k)` up to `t`, linear above with its slope there: the
    /// highlights keep linear's relative precision.
    HalfLin(f32, f32),
}
impl Transfer {
    fn forward(self, v: f32) -> f32 {
        let (s, x) = (v.signum(), v.abs());
        s * match self {
            Transfer::Linear => x,
            Transfer::Lift(k) if x <= 1.0 => x * (1.0 + k) / (x + k),
            Transfer::Lift(k) => 1.0 + (x - 1.0) * k / (1.0 + k),
            Transfer::Log(k) => (x / k).ln_1p() / (1.0 / k).ln_1p(),
            Transfer::Power(p, t) if x < t => x * t.powf(p - 1.0),
            Transfer::Power(p, _) => x.powf(p),
            Transfer::Toe(a, x0) => x * (1.0 + a * x0 / (x + x0)),
            Transfer::Half(k) => x * ((1.0 + k) / (x + k)).sqrt(),
            Transfer::HalfLin(k, t) if x <= t => Transfer::Half(k).forward(x),
            Transfer::HalfLin(k, t) => {
                let (ft, slope) = half_knee(k, t);
                ft + slope * (x - t)
            }
        }
    }
    fn inverse(self, v: f32) -> f32 {
        let (s, y) = (v.signum(), v.abs());
        s * match self {
            Transfer::Linear => y,
            Transfer::Lift(k) if y <= 1.0 => y * k / (1.0 + k - y),
            Transfer::Lift(k) => 1.0 + (y - 1.0) * (1.0 + k) / k,
            Transfer::Log(k) => k * (y * (1.0 / k).ln_1p()).exp_m1(),
            Transfer::Power(p, t) if y < t.powf(p) => y / t.powf(p - 1.0),
            Transfer::Power(p, _) => y.powf(1.0 / p),
            Transfer::Toe(a, x0) => {
                // x² + (x0 (1 + a) - y) x - y x0 = 0, the positive root.
                let b = x0 * (1.0 + a) - y;
                let d = (b * b + 4.0 * y * x0).sqrt();
                if b > 0.0 {
                    2.0 * y * x0 / (b + d)
                } else {
                    (d - b) / 2.0
                }
            }
            Transfer::Half(k) => y * (y + (y * y + 4.0 * (1.0 + k) * k).sqrt()) / (2.0 * (1.0 + k)),
            Transfer::HalfLin(k, t) => {
                let (ft, slope) = half_knee(k, t);
                if y <= ft {
                    Transfer::Half(k).inverse(y)
                } else {
                    t + (y - ft) / slope
                }
            }
        }
    }
    fn name(self) -> String {
        match self {
            Transfer::Linear => "lin".into(),
            Transfer::Lift(k) => format!("lift{k}"),
            Transfer::Log(k) => format!("log{k}"),
            Transfer::Power(p, t) => format!("pow{p}t{t}"),
            Transfer::Toe(a, x0) => format!("toe{a}x{x0}"),
            Transfer::Half(k) => format!("half{k}"),
            Transfer::HalfLin(k, t) => format!("hl{k}t{t}"),
        }
    }
    fn parse(s: &str) -> Transfer {
        if s == "lin" {
            Transfer::Linear
        } else if let Some(k) = s.strip_prefix("lift") {
            Transfer::Lift(k.parse().unwrap())
        } else if let Some(k) = s.strip_prefix("log") {
            Transfer::Log(k.parse().unwrap())
        } else if let Some(rest) = s.strip_prefix("pow") {
            let (p, t) = rest.split_once('t').unwrap();
            Transfer::Power(p.parse().unwrap(), t.parse().unwrap())
        } else if let Some(rest) = s.strip_prefix("hl") {
            let (k, t) = rest.split_once('t').unwrap();
            Transfer::HalfLin(k.parse().unwrap(), t.parse().unwrap())
        } else if let Some(k) = s.strip_prefix("half") {
            Transfer::Half(k.parse().unwrap())
        } else if let Some(rest) = s.strip_prefix("toe") {
            let (a, x0) = rest.split_once('x').unwrap();
            Transfer::Toe(a.parse().unwrap(), x0.parse().unwrap())
        } else {
            panic!("transfer {s}")
        }
    }
}

/// `Half(k)` at `t` and its slope there.
fn half_knee(k: f32, t: f32) -> (f32, f32) {
    let root = ((1.0 + k) / (t + k)).sqrt();
    (t * root, root * (1.0 - t / (2.0 * (t + k))))
}

/// `transfer:distance[:effort[:knobs]]`, e.g. `log0.05:0.4` or
/// `hl0.1t0.18:0.25:3:o+a`. Knobs (`+`-separated) set the VarDCT encoder
/// directly on top of the effort's profile: `o` custom coefficient orders,
/// `a` AC strategy search with 16/32-pixel transforms, `A` also 64, `f`
/// 4x8/AFV transforms, `q` adaptive quantization, `g` gaborish, `p`
/// pixel-domain loss, `n` noise synthesis, `c` CfL two-pass and chromacity
/// adjustment, `tX` Y zeroing thresholds times X, `iX` intensity target X
/// nits, `kX` k_ac_quant X.
struct Spec {
    transfer: Transfer,
    distance: f32,
    effort: u8,
    knobs: Vec<String>,
}
impl Spec {
    fn parse(s: &str) -> Spec {
        let mut parts = s.split(':');
        Spec {
            transfer: Transfer::parse(parts.next().unwrap()),
            distance: parts.next().unwrap().parse().unwrap(),
            effort: parts.next().map_or(3, |e| e.parse().unwrap()),
            knobs: parts
                .next()
                .map(|k| k.split('+').map(str::to_string).collect())
                .unwrap_or_default(),
        }
    }

    /// The VarDCT encoder with the knobs applied.
    fn encoder(&self) -> jxl_encoder::vardct::VarDctEncoder {
        let mut profile =
            jxl_encoder::EffortProfile::lossy(self.effort, jxl_encoder::EncoderMode::Reference);
        let mut enc = jxl_encoder::vardct::VarDctEncoder::new(self.distance);
        let (mut noise, mut gaborish) = (false, false);
        for knob in &self.knobs {
            let (key, value) = knob.split_at(1);
            let num = || value.parse::<f32>().unwrap();
            match key {
                "o" => profile.custom_orders = true,
                "a" | "A" => {
                    profile.ac_strategy_enabled = true;
                    profile.try_dct16 = true;
                    profile.try_dct32 = true;
                    profile.try_dct64 = key == "A";
                }
                "f" => profile.try_dct4x8_afv = true,
                "q" => {
                    profile.use_adaptive_quant = true;
                    profile.adjust_quant_ac = true;
                    profile.initial_q_numerator = 0.39;
                }
                "g" => gaborish = true,
                "p" => profile.pixel_domain_loss = true,
                "n" => noise = true,
                "c" => {
                    profile.cfl_two_pass = true;
                    profile.chromacity_adjustment = true;
                }
                "t" => {
                    let m = num();
                    profile.fixed_thresholds_y = profile.fixed_thresholds_y.map(|t| t * m);
                }
                "i" => enc.intensity_target = num(),
                "s" | "S" | "Y" | "x" => {}
                "k" => profile.k_ac_quant = num(),
                _ => panic!("knob {knob}"),
            }
        }
        enc.effort = self.effort;
        enc.use_ans = profile.use_ans;
        enc.optimize_codes = profile.optimize_codes;
        enc.custom_orders = profile.custom_orders;
        enc.ac_strategy_enabled = profile.ac_strategy_enabled;
        enc.enable_gaborish = gaborish && self.distance > 0.5;
        enc.pixel_domain_loss = profile.pixel_domain_loss;
        enc.enable_noise = noise;
        enc.enable_patches = profile.patches;
        enc.enable_lz77 = profile.lz77;
        enc.butteraugli_iters = profile.butteraugli_iters;
        enc.profile = profile;
        enc
    }
}
impl Codec for Spec {
    fn name(&self) -> String {
        format!(
            "jxl {} d{} e{} {}",
            self.transfer.name(),
            self.distance,
            self.effort,
            self.knobs.join("+")
        )
    }
    fn enc(&self, band: &[f32], w: usize) -> Vec<u8> {
        if !self.knobs.is_empty() {
            let mut pixels: Vec<f32> = band.iter().map(|v| self.transfer.forward(*v)).collect();
            if let Some(gain) = self.chroma_gain() {
                scale_chroma(&mut pixels, gain);
            }
            if self.knobs.iter().any(|k| k == "s") {
                pixels = chroma_half(&pixels, w, false);
            } else if self.knobs.iter().any(|k| k == "S") {
                pixels = chroma_half(&pixels, w, true);
            } else if self.knobs.iter().any(|k| k == "Y") {
                // Luminance only: what the chroma costs.
                for p in pixels.as_chunks_mut::<3>().0 {
                    let y = 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2];
                    p.fill(y);
                }
            }
            if self
                .knobs
                .iter()
                .all(|k| k == "s" || k == "S" || k == "Y" || k.starts_with('x'))
            {
                let bytes: Vec<u8> = pixels.iter().flat_map(|v| v.to_ne_bytes()).collect();
                return jxl_encoder::LossyConfig::new(self.distance)
                    .with_effort(self.effort)
                    .encode(
                        &bytes,
                        w as u32,
                        (band.len() / 3 / w) as u32,
                        jxl_encoder::PixelLayout::RgbLinearF32,
                    )
                    .unwrap();
            }
            return self
                .encoder()
                .encode(w, band.len() / 3 / w, &pixels, None)
                .unwrap()
                .data;
        }
        let bytes: Vec<u8> = band
            .iter()
            .flat_map(|v| self.transfer.forward(*v).to_ne_bytes())
            .collect();
        jxl_encoder::LossyConfig::new(self.distance)
            .with_effort(self.effort)
            .encode(
                &bytes,
                w as u32,
                (band.len() / 3 / w) as u32,
                jxl_encoder::PixelLayout::RgbLinearF32,
            )
            .unwrap()
    }
    fn dec(&self, bytes: &[u8], _w: usize, _rows: usize) -> Vec<f32> {
        let mut image = jxl_oxide::JxlImage::builder()
            .read(std::io::Cursor::new(bytes))
            .unwrap();
        image.request_color_encoding(jxl_oxide::EnumColourEncoding::srgb_linear(
            jxl_oxide::RenderingIntent::Relative,
        ));
        let render = image.render_frame(0).unwrap();
        let mut px = render.image_all_channels().buf().to_vec();
        if let Some(gain) = self.chroma_gain() {
            scale_chroma(&mut px, 1.0 / gain);
        }
        px.iter().map(|v| self.transfer.inverse(*v)).collect()
    }
}

impl Spec {
    /// Knob `x<gain>`: chroma scaled by `gain` before encoding.
    fn chroma_gain(&self) -> Option<f32> {
        self.knobs
            .iter()
            .find_map(|k| k.strip_prefix('x').map(|g| g.parse().unwrap()))
    }
}

/// Scales each pixel's chroma (channels minus luminance) by `gain`; the
/// luminance stays.
fn scale_chroma(px: &mut [f32], gain: f32) {
    for p in px.as_chunks_mut::<3>().0 {
        let y = 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2];
        for v in p.iter_mut() {
            *v = y + gain * (*v - y);
        }
    }
}

/// Keeps the luminance of `px` (RGB rows of `w`, transferred values) and
/// replaces its chroma (each channel minus the luminance) by the chroma at
/// half resolution (2x2 means), upsampled bilinearly or, `guided`, with
/// weights that favour half-resolution samples of similar luminance (joint
/// bilateral), so colour edges follow luminance edges.
fn chroma_half(px: &[f32], w: usize, guided: bool) -> Vec<f32> {
    let h = px.len() / 3 / w;
    let (hw, hh) = (w.div_ceil(2), h.div_ceil(2));
    let lum = |p: &[f32; 3]| 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2];
    let y: Vec<f32> = px.as_chunks::<3>().0.iter().map(lum).collect();
    // Half-resolution chroma and luminance.
    let mut hc = vec![[0f32; 3]; hw * hh];
    let mut hy = vec![0f32; hw * hh];
    for j in 0..hh {
        for i in 0..hw {
            let (mut c, mut l, mut n) = ([0f32; 3], 0f32, 0f32);
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let (x, yy) = (2 * i + dx, 2 * j + dy);
                if x < w && yy < h {
                    let k = yy * w + x;
                    for ch in 0..3 {
                        c[ch] += px[k * 3 + ch] - y[k];
                    }
                    l += y[k];
                    n += 1.0;
                }
            }
            hc[j * hw + i] = c.map(|v| v / n);
            hy[j * hw + i] = l / n;
        }
    }
    let mut out = px.to_vec();
    out.par_chunks_mut(w * 3).enumerate().for_each(|(yy, row)| {
        // Sample centres at 2i + 0.5.
        let fy = ((yy as f32 - 0.5) / 2.0).clamp(0.0, (hh - 1) as f32);
        let j0 = fy.floor() as usize;
        let j1 = (j0 + 1).min(hh - 1);
        let ty = fy - j0 as f32;
        for x in 0..w {
            let fx = ((x as f32 - 0.5) / 2.0).clamp(0.0, (hw - 1) as f32);
            let i0 = fx.floor() as usize;
            let i1 = (i0 + 1).min(hw - 1);
            let tx = fx - i0 as f32;
            let yv = y[yy * w + x];
            let sigma = 0.1 * (yv.abs() + 0.02);
            let mut acc = [0f32; 3];
            let mut wsum = 0f32;
            for (jj, wy) in [(j0, 1.0 - ty), (j1, ty)] {
                for (ii, wx) in [(i0, 1.0 - tx), (i1, tx)] {
                    let k = jj * hw + ii;
                    let mut wt = wx * wy + 1e-6;
                    if guided {
                        let d = (hy[k] - yv) / sigma;
                        wt *= (-d * d).exp() + 1e-4;
                    }
                    for ch in 0..3 {
                        acc[ch] += wt * hc[k][ch];
                    }
                    wsum += wt;
                }
            }
            for ch in 0..3 {
                row[x * 3 + ch] = yv + acc[ch] / wsum;
            }
        }
    });
    out
}

fn run_codec(codec: &dyn Codec, img: &Rgb32FImage) -> (usize, f64, f64, Rgb32FImage) {
    let (w, h) = img.dimensions();
    let w = w as usize;
    let t = Instant::now();
    let bands: Vec<Vec<u8>> = img
        .as_raw()
        .par_chunks(w * 3 * BAND)
        .map(|b| codec.enc(b, w))
        .collect();
    let enc_s = t.elapsed().as_secs_f64();
    let size = bands.iter().map(Vec::len).sum();
    let t = Instant::now();
    let rows: Vec<usize> = (0..bands.len())
        .map(|i| BAND.min(h as usize - i * BAND))
        .collect();
    let decoded: Vec<Vec<f32>> = bands
        .par_iter()
        .zip(rows.par_iter())
        .map(|(b, &r)| codec.dec(b, w, r))
        .collect();
    let dec_s = t.elapsed().as_secs_f64();
    let out = Rgb32FImage::from_raw(w as u32, h, decoded.concat()).unwrap();
    (size, enc_s, dec_s, out)
}

// ---------- colour difference ----------
fn srgb_to_lab(rgb: [f32; 3]) -> [f32; 3] {
    let lin = rgb.map(|c| {
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    });
    let x = 0.4124 * lin[0] + 0.3576 * lin[1] + 0.1805 * lin[2];
    let y = 0.2126 * lin[0] + 0.7152 * lin[1] + 0.0722 * lin[2];
    let z = 0.0193 * lin[0] + 0.1192 * lin[1] + 0.9505 * lin[2];
    let f = |t: f32| {
        if t > 216.0 / 24389.0 {
            t.cbrt()
        } else {
            (24389.0 / 27.0 * t + 16.0) / 116.0
        }
    };
    let (fx, fy, fz) = (f(x / 0.95047), f(y), f(z / 1.08883));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}
fn de2000(a: [f32; 3], b: [f32; 3]) -> f32 {
    let (l1, a1, b1) = (a[0] as f64, a[1] as f64, a[2] as f64);
    let (l2, a2, b2) = (b[0] as f64, b[1] as f64, b[2] as f64);
    let c1 = (a1 * a1 + b1 * b1).sqrt();
    let c2 = (a2 * a2 + b2 * b2).sqrt();
    let cb = (c1 + c2) / 2.0;
    let g = 0.5 * (1.0 - (cb.powi(7) / (cb.powi(7) + 25f64.powi(7))).sqrt());
    let a1p = a1 * (1.0 + g);
    let a2p = a2 * (1.0 + g);
    let c1p = (a1p * a1p + b1 * b1).sqrt();
    let c2p = (a2p * a2p + b2 * b2).sqrt();
    let h = |b: f64, a: f64| {
        if a == 0.0 && b == 0.0 {
            0.0
        } else {
            let v = b.atan2(a).to_degrees();
            if v < 0.0 { v + 360.0 } else { v }
        }
    };
    let h1p = h(b1, a1p);
    let h2p = h(b2, a2p);
    let dlp = l2 - l1;
    let dcp = c2p - c1p;
    let mut dhp = h2p - h1p;
    if c1p * c2p != 0.0 {
        if dhp > 180.0 {
            dhp -= 360.0
        } else if dhp < -180.0 {
            dhp += 360.0
        }
    } else {
        dhp = 0.0
    }
    let dhh = 2.0 * (c1p * c2p).sqrt() * (dhp.to_radians() / 2.0).sin();
    let lbp = (l1 + l2) / 2.0;
    let cbp = (c1p + c2p) / 2.0;
    let hbp = if c1p * c2p == 0.0 {
        h1p + h2p
    } else if (h1p - h2p).abs() <= 180.0 {
        (h1p + h2p) / 2.0
    } else if h1p + h2p < 360.0 {
        (h1p + h2p + 360.0) / 2.0
    } else {
        (h1p + h2p - 360.0) / 2.0
    };
    let t = 1.0 - 0.17 * (hbp - 30.0).to_radians().cos()
        + 0.24 * (2.0 * hbp).to_radians().cos()
        + 0.32 * (3.0 * hbp + 6.0).to_radians().cos()
        - 0.20 * (4.0 * hbp - 63.0).to_radians().cos();
    let dtheta = 30.0 * (-((hbp - 275.0) / 25.0).powi(2)).exp();
    let rc = 2.0 * (cbp.powi(7) / (cbp.powi(7) + 25f64.powi(7))).sqrt();
    let sl = 1.0 + 0.015 * (lbp - 50.0).powi(2) / (20.0 + (lbp - 50.0).powi(2)).sqrt();
    let sc = 1.0 + 0.045 * cbp;
    let sh = 1.0 + 0.015 * cbp * t;
    let rt = -(2.0 * dtheta).to_radians().sin() * rc;
    ((dlp / sl).powi(2) + (dcp / sc).powi(2) + (dhh / sh).powi(2) + rt * (dcp / sc) * (dhh / sh))
        .sqrt() as f32
}

struct Stats {
    mean: f32,
    p99: f32,
    p999: f32,
    max: f32,
    max8: u16,
    argmax: usize,
    block_p999: f32,
    block_max: f32,
    grain: f32,
    /// Grain energy kept and mean CIEDE2000 in the shadow blocks.
    shadow_grain: f32,
    shadow_de: f32,
}

/// The 8x8 blocks of `img` whose mean L* is below `limit` (the shadows of
/// the default rendering).
fn dark_blocks(img: &DynamicImage, limit: f32) -> Vec<bool> {
    let img = img.to_rgb16();
    let (w, h) = img.dimensions();
    let (bw, bh) = (w as usize / 8, h as usize / 8);
    (0..bw * bh)
        .into_par_iter()
        .map(|i| {
            let (bx, by) = (i % bw, i / bw);
            let mut l = 0.0;
            for y in 0..8 {
                for x in 0..8 {
                    let p = img.get_pixel((bx * 8 + x) as u32, (by * 8 + y) as u32);
                    l += srgb_to_lab(p.0.map(|v| v as f32 / 65535.0))[0] / 64.0;
                }
            }
            l < limit
        })
        .collect()
}

fn compare(a: &DynamicImage, b: &DynamicImage, dark: &[bool]) -> Stats {
    let (a, b) = (a.to_rgb16(), b.to_rgb16());
    let mut des: Vec<f32> = a
        .as_raw()
        .par_chunks(3)
        .zip(b.as_raw().par_chunks(3))
        .map(|(p, q)| {
            let f = |s: &[u16]| [s[0], s[1], s[2]].map(|v| v as f32 / 65535.0);
            de2000(srgb_to_lab(f(p)), srgb_to_lab(f(q)))
        })
        .collect();
    let max8 = a
        .as_raw()
        .par_iter()
        .zip(b.as_raw().par_iter())
        .map(|(p, q)| ((*p as i32 >> 8) - (*q as i32 >> 8)).unsigned_abs() as u16)
        .max()
        .unwrap();
    let mean = des.iter().sum::<f32>() / des.len() as f32;
    let argmax = des
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .unwrap()
        .0;
    // 8x8 block means (colour shifts, banding, blocking) and grain energy.
    let (w, h) = a.dimensions();
    let (bw, bh) = (w as usize / 8, h as usize / 8);
    let lab_of = |img: &image::ImageBuffer<image::Rgb<u16>, Vec<u16>>| -> Vec<[f32; 3]> {
        img.as_raw()
            .par_chunks(3)
            .map(|s| srgb_to_lab([s[0], s[1], s[2]].map(|v| v as f32 / 65535.0)))
            .collect()
    };
    let (la, lb) = (lab_of(&a), lab_of(&b));
    // RAWTONIC_PROXY_EVAL_BANDS: where the worst 1 % lie, by the
    // reference's L* and chroma, and how textured their 8x8 block is.
    if std::env::var("RAWTONIC_PROXY_EVAL_BANDS").is_ok() {
        let mut sorted = des.clone();
        sorted.par_sort_unstable_by(f32::total_cmp);
        let p99 = sorted[((sorted.len() - 1) as f64 * 0.99) as usize];
        let edges = [0.0, 10.0, 25.0, 50.0, 75.0, 90.0, 101.0];
        let mut worst = [0usize; 6];
        let mut all = [0usize; 6];
        let mut chroma = [0usize; 3];
        for (i, lab) in la.iter().enumerate() {
            let band = edges.windows(2).position(|e| lab[0] < e[1]).unwrap_or(5);
            all[band] += 1;
            if des[i] > p99 {
                worst[band] += 1;
                let c = (lab[1] * lab[1] + lab[2] * lab[2]).sqrt();
                chroma[if c < 10.0 {
                    0
                } else if c < 30.0 {
                    1
                } else {
                    2
                }] += 1;
            }
        }
        let n = worst.iter().sum::<usize>().max(1) as f64;
        let t = la.len() as f64;
        println!(
            "    worst 1% (dE > {p99:.2}) by L* 0-10/10-25/25-50/50-75/75-90/90+: {} of pixels {}; chroma <10/10-30/30+: {}",
            worst
                .iter()
                .map(|v| format!("{:.0}%", 100.0 * *v as f64 / n))
                .collect::<Vec<_>>()
                .join(" "),
            all.iter()
                .map(|v| format!("{:.0}%", 100.0 * *v as f64 / t))
                .collect::<Vec<_>>()
                .join(" "),
            chroma
                .iter()
                .map(|v| format!("{:.0}%", 100.0 * *v as f64 / n))
                .collect::<Vec<_>>()
                .join(" "),
        );
    }
    let blocks: Vec<(f32, f64, f64, f64)> = (0..bw * bh)
        .into_par_iter()
        .map(|i| {
            let (bx, by) = (i % bw, i / bw);
            let mut ma = [0f32; 3];
            let mut mb = [0f32; 3];
            for y in 0..8 {
                for x in 0..8 {
                    let j = (by * 8 + y) * w as usize + bx * 8 + x;
                    for c in 0..3 {
                        ma[c] += la[j][c] / 64.0;
                        mb[c] += lb[j][c] / 64.0;
                    }
                }
            }
            let (mut va, mut vb) = (0f64, 0f64);
            for y in 0..8 {
                for x in 0..8 {
                    let j = (by * 8 + y) * w as usize + bx * 8 + x;
                    va += ((la[j][0] - ma[0]) as f64).powi(2);
                    vb += ((lb[j][0] - mb[0]) as f64).powi(2);
                }
            }
            let mut de = 0f64;
            for y in 0..8 {
                for x in 0..8 {
                    let j = (by * 8 + y) * w as usize + bx * 8 + x;
                    de += de2000(la[j], lb[j]) as f64 / 64.0;
                }
            }
            (de2000(ma, mb), va, vb, de)
        })
        .collect();
    let in_dark = || {
        blocks
            .iter()
            .zip(dark.iter().chain(std::iter::repeat(&false)))
            .filter(|(_, d)| **d)
            .map(|(b, _)| b)
    };
    let dark_count = in_dark().count().max(1) as f64;
    let shadow_grain = (in_dark().map(|b| b.2).sum::<f64>()
        / in_dark().map(|b| b.1).sum::<f64>().max(1e-9)) as f32;
    let shadow_de = (in_dark().map(|b| b.3).sum::<f64>() / dark_count) as f32;
    let grain =
        (blocks.iter().map(|b| b.2).sum::<f64>() / blocks.iter().map(|b| b.1).sum::<f64>()) as f32;
    let mut bde: Vec<f32> = blocks.iter().map(|b| b.0).collect();
    bde.par_sort_unstable_by(f32::total_cmp);
    let block_p999 = bde[((bde.len() - 1) as f64 * 0.999) as usize];
    let block_max = *bde.last().unwrap();
    des.par_sort_unstable_by(f32::total_cmp);
    let at = |q: f64| des[((des.len() - 1) as f64 * q) as usize];
    Stats {
        mean,
        p99: at(0.99),
        p999: at(0.999),
        max: *des.last().unwrap(),
        max8,
        argmax,
        block_p999,
        block_max,
        grain,
        shadow_grain,
        shadow_de,
    }
}

fn render(
    ctx: &GpuContext,
    proc_: &Mutex<Option<rawtonic_gpu::gpu_processing::GpuProcessorState>>,
    cache: &Mutex<Option<rawtonic_gpu::gpu_processing::GpuImageCache>>,
    path: &str,
    img: &Rgb32FImage,
    recipe: &serde_json::Value,
) -> DynamicImage {
    *cache.lock().unwrap() = None;
    let tm = |_r: bool| None;
    let lut = |_p: &str| None;
    let env = RenderEnv {
        context: ctx,
        processor: proc_,
        image_cache: cache,
        tonemapper_override: &tm,
        load_lut: &lut,
    };
    let src = ExportSource {
        pristine: Arc::new(DynamicImage::ImageRgb32F(img.clone())),
        patched: None,
    };
    process_image_for_export_pipeline(
        path,
        src,
        recipe,
        &env,
        true,
        "eval",
        RenderOutputPrecision::SixteenBit,
        OutputColorSpace::Srgb,
    )
    .unwrap()
}

#[test]
#[ignore]
fn proxy_codec_eval() {
    let files = std::env::var("RAWTONIC_PROXY_EVAL").expect("files ; separated");
    let out_dir = std::env::var("RAWTONIC_PROXY_EVAL_OUT").ok();
    let lensfun = std::env::var("RAWTONIC_LENSFUN_DB").unwrap();
    rawtonic_engine::lens_correction::set_global_lens_db(Arc::new(
        rawtonic_engine::lens_correction::load_lensfun_dir(Path::new(&lensfun)),
    ));
    let ctx = GpuContext::headless().unwrap();
    let proc_ = Mutex::new(None);
    let cache = Mutex::new(None);
    let recipes = [
        ("default", serde_json::json!({})),
        (
            "pushed",
            serde_json::json!({"exposure": 1.5, "shadows": 100, "contrast": 40, "clarity": 50, "saturation": 30, "highlights": -100}),
        ),
    ];
    let codecs: Vec<Box<dyn Codec>> = {
        let only = std::env::var("RAWTONIC_PROXY_EVAL_CODECS").unwrap_or_default();
        // The shipped encoding, then JPEG XL at other distances and efforts
        // for comparison.
        let jxl = |distance: f32, effort: u8, gain: f32| -> Box<dyn Codec> {
            Box::new(Jxl {
                distance,
                effort,
                gain,
            })
        };
        // The shipped encoding, and format 2 (linear JPEG XL at distance
        // 0.25, before the shadow transfer).
        let shipped = || -> Box<dyn Codec> { Box::new(Shipped) };
        let format_2 = || -> Box<dyn Codec> {
            Box::new(Spec {
                transfer: Transfer::Linear,
                distance: 0.25,
                effort: 3,
                knobs: Vec::new(),
            })
        };
        // RAWTONIC_PROXY_EVAL_SPECS="log0.05:0.4,lift0.3:0.25" replaces
        // the list after the shipped encoding (`transfer:distance[:effort]`).
        let all: Vec<Box<dyn Codec>> = match std::env::var("RAWTONIC_PROXY_EVAL_SPECS") {
            Ok(specs) => [shipped(), format_2()]
                .into_iter()
                .chain(
                    specs
                        .split(',')
                        .map(|s| Box::new(Spec::parse(s)) as Box<dyn Codec>),
                )
                .collect(),
            Err(_) => vec![
                shipped(),
                format_2(),
                jxl(0.25, 2, 1.0),
                jxl(0.25, 3, 4.0),
                jxl(0.5, 3, 4.0),
                jxl(0.5, 3, 16.0),
                jxl(1.0, 3, 16.0),
            ],
        };
        all.into_iter()
            .filter(|c| only.is_empty() || only.split(',').any(|o| c.name().contains(o)))
            .collect()
    };
    for file in files.split(';') {
        let bytes = std::fs::read(file).unwrap();
        let t = Instant::now();
        let base = rawtonic_engine::image_loader::load_base_image(
            &bytes,
            file,
            false,
            &rawtonic_engine::image_loader::BaseImageOptions::default(),
            None,
        )
        .unwrap()
        .to_rgb32f();
        // RAWTONIC_PROXY_EVAL_SIZE: a Standard preview's long edge instead.
        let base = match std::env::var("RAWTONIC_PROXY_EVAL_SIZE")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
        {
            Some(size) => rawtonic_engine::image_processing::downscale_f32_image(
                &DynamicImage::ImageRgb32F(base),
                size,
                size,
            )
            .to_rgb32f(),
            None => base,
        };
        let raw = base.as_raw();
        let (mn, mx) = raw
            .iter()
            .fold((f32::MAX, f32::MIN), |(a, b), &v| (a.min(v), b.max(v)));
        let neg = raw.iter().filter(|v| **v < 0.0).count() as f64 / raw.len() as f64;
        let over = raw.iter().filter(|v| **v > 1.0).count() as f64 / raw.len() as f64;
        println!(
            "\n== {} {}x{} decode {:.2}s min {mn:.4} max {mx:.3} neg {:.4}% >1 {:.4}%",
            Path::new(file).file_name().unwrap().to_string_lossy(),
            base.width(),
            base.height(),
            t.elapsed().as_secs_f64(),
            neg * 100.0,
            over * 100.0
        );
        let (fsize, fenc, fdec, f16img) = run_codec(&F16, &base);
        println!(
            "{:34} {:8.1} MB enc {:.2}s dec {:.2}s",
            F16.name(),
            fsize as f64 / 1e6,
            fenc,
            fdec
        );
        let refs: Vec<DynamicImage> = recipes
            .iter()
            .map(|(_, r)| render(&ctx, &proc_, &cache, file, &f16img, r))
            .collect();
        // Shadows: blocks below L* 30 in the default rendering.
        let dark = dark_blocks(&refs[0], 30.0);
        println!(
            "shadow blocks (L* < 30 at the default recipe): {:.1}%",
            100.0 * dark.iter().filter(|d| **d).count() as f64 / dark.len().max(1) as f64
        );
        {
            let mut line = format!("{:34}", "f32 (full decode) vs f16");
            for ((name, recipe), reference) in recipes.iter().zip(&refs) {
                let out = render(&ctx, &proc_, &cache, file, &base, recipe);
                let s = compare(reference, &out, &dark);
                line += &format!(
                    " | {name}: dE {:.3}/{:.2}/{:.2}/{:.1} max8 {} blk {:.2}/{:.2} grain {:.3} shadows grain {:.3} dE {:.2}",
                    s.mean,
                    s.p99,
                    s.p999,
                    s.max,
                    s.max8,
                    s.block_p999,
                    s.block_max,
                    s.grain,
                    s.shadow_grain,
                    s.shadow_de,
                );
            }
            println!("{line}");
        }
        for codec in &codecs {
            let (size, enc, dec, img) = run_codec(codec.as_ref(), &base);
            let mut line = format!(
                "{:34} {:8.1} MB ({:4.1}%) enc {:.2}s dec {:.2}s",
                codec.name(),
                size as f64 / 1e6,
                100.0 * size as f64 / fsize as f64,
                enc,
                dec
            );
            for ((name, recipe), reference) in recipes.iter().zip(&refs) {
                let out = render(&ctx, &proc_, &cache, file, &img, recipe);
                let s = compare(reference, &out, &dark);
                line += &format!(
                    " | {name}: dE {:.3}/{:.2}/{:.2}/{:.1} max8 {} blk {:.2}/{:.2} grain {:.3} shadows grain {:.3} dE {:.2}",
                    s.mean,
                    s.p99,
                    s.p999,
                    s.max,
                    s.max8,
                    s.block_p999,
                    s.block_max,
                    s.grain,
                    s.shadow_grain,
                    s.shadow_de,
                );
                if let Some(dir) = &out_dir {
                    let stem = Path::new(file).file_stem().unwrap().to_string_lossy();
                    let tag = codec.name().replace(' ', "_");
                    let (w, h) = (out.width(), out.height());
                    // RAWTONIC_PROXY_EVAL_AT="x,y": the same place for every
                    // encoding instead of its largest error.
                    let (cx, cy) = std::env::var("RAWTONIC_PROXY_EVAL_AT")
                        .ok()
                        .and_then(|at| {
                            let (x, y) = at.split_once(',')?;
                            Some((x.parse().ok()?, y.parse().ok()?))
                        })
                        .unwrap_or((
                            (s.argmax % w as usize) as u32,
                            (s.argmax / w as usize) as u32,
                        ));
                    let x0 = cx.saturating_sub(96).min(w - 192);
                    let y0 = cy.saturating_sub(96).min(h - 192);
                    let (r8, o8) = (reference.to_rgb8(), out.to_rgb8());
                    let pair = image::RgbImage::from_fn(192 * 2 + 4, 192, |x, y| {
                        if x < 192 {
                            *r8.get_pixel(x0 + x, y0 + y)
                        } else if x < 196 {
                            image::Rgb([255, 0, 255])
                        } else {
                            *o8.get_pixel(x0 + x - 196, y0 + y)
                        }
                    });
                    image::imageops::resize(
                        &pair,
                        pair.width() * 3,
                        pair.height() * 3,
                        image::imageops::FilterType::Nearest,
                    )
                    .save(format!("{dir}/{stem}-{name}-{tag}-max.png"))
                    .unwrap();
                }
            }
            println!("{line}");
        }
    }
}
