//! Compact storage of linear RGB for preview proxies (the app's Standard
//! and 1:1 previews): lossy JPEG XL (XYB, encoded by jxl-encoder, decoded by
//! jxl-oxide) of the linear float pixels after a shadow-weighted transfer
//! (`Transfer::HalfLinear`), which decoding inverts. Values above white and the
//! negative channels of colours beyond sRGB are kept (the transfer is odd
//! and unbounded, XYB has no range limit).
//!
//! Why the transfer: JPEG XL spends its bits as eyes see the image it is
//! given, so the deep shadows of the unedited linear data get few, and an
//! edit that lifts them (+1.5 EV, Shadows +100) shows the loss: smoothed
//! grain at 300 %. Encoding `x sqrt((1 + k) / (x + k))` keeps slope 1 (as
//! linear) in the shadows and turns into a square root above `k`, so the
//! encoder sees the shadows relatively brighter and gives them more of its
//! precision, the highlights a little less (never below half their
//! contrast, which a full log or a rational lift fell below: highlight
//! recovery got worse). Since 2026-10-10 the curve turns linear above
//! 0.18 (with its slope there), so highlight-heavy photographs keep
//! linear's relative precision where they lost some with the square root
//! (A7C default p99 0.90 -> 0.82, iPhone 1.53 -> 1.33; linear 0.80 and
//! 1.28), the shadows unchanged, 2 % more bytes on average.
//!
//! Chosen by measurement (crates/export/tests/proxy_codec_eval.rs, export
//! pipeline, 16-bit sRGB renders against the lossless half floats of
//! format 1; eight cameras, 12-53 MP, ISO 64-32000; a default recipe and a
//! pushed one: +1.5 EV, Shadows +100, Highlights -100, Contrast 40, Clarity
//! 50, Saturation 30). Against the linear JPEG XL at the same distance
//! (format 2), for 1:1 previews: pushed CIEDE2000 p99 2.38 -> 1.97 on
//! average (R5 II ISO 640 3.90 -> 2.64, X-T5 4.13 -> 3.23), grain energy
//! kept in the shadows 97.6 -> 99.0 % (R5 II 93.5 -> 97.7 %), mean
//! CIEDE2000 in the shadows 0.54 -> 0.45, default recipe p99 1.25 -> 1.19,
//! for 11 % more bytes (40-46 MP 7.9-14.6 MB instead of 6.8-11.1; half
//! floats 142-182 MB). Standard previews (3072 px) the same way: pushed p99
//! 2.34 -> 2.01, 5 % more bytes. Highlight-heavy photographs lose a little
//! (A7C default p99 0.80 -> 0.90, iPhone 1.28 -> 1.53). At equal size the
//! transfer beats a finer linear distance; linear gains, a shadow-only toe
//! gain, powers with a toe, logs and rational lifts (x (1 + k) / (x + k))
//! were measured and lost on highlights or size.
//!
//! Second run (2026-10-10, after #50 changed the shadow rendering), 1:1,
//! eight cameras: format 2 13.4 MB, default p99 1.25, pushed p99 2.27,
//! shadow grain 97.6 %; this encoding 15.3 MB, 1.15, 1.82, 99.0 %.
//! Standard (3072 px): 2.36 -> 2.58 MB, default p99 1.29 -> 1.20, pushed
//! 2.23 -> 1.86.
//!
//! Size: 1:1 previews of 40-46 MP take 8.1-14.6 MB (the R5 II at ISO 640,
//! 44 % of it shadows, 14.6). Getting it to ~12 MB means a coarser
//! distance, which gives back most of the gains (k 0.03, knee 0.15, d 0.33:
//! 12.4 MB, default p99 1.23 -> 1.49, pushed 2.62 -> 3.08); efforts 5 and
//! 7 make larger files, not better ones per byte. Numbers in the eval
//! test's header.

use serde::{Deserialize, Serialize};

/// How values are transformed before encoding; stored with each proxy so
/// decoding inverts the transfer it was made with.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Transfer {
    /// The linear values themselves (format 2 proxies).
    Linear,
    /// `x sqrt((1 + k) / (x + k))` per channel, odd: slope 1 in the
    /// shadows, a square root in the highlights; 1 stays 1 (#52 format 3
    /// proxies, still read).
    Half { k: f32 },
    /// `Half { k }` up to `knee`, linear above with its slope there: the
    /// shadows weighted as `Half`, the highlights at linear's relative
    /// precision.
    #[serde(rename_all = "camelCase")]
    HalfLinear { k: f32, knee: f32 },
}

/// `Half { k }` at `knee` and its slope there.
fn half_knee(k: f32, knee: f32) -> (f32, f32) {
    let root = ((1.0 + k) / (knee + k)).sqrt();
    (knee * root, root * (1.0 - knee / (2.0 * (knee + k))))
}

impl Transfer {
    pub fn forward(self, v: f32) -> f32 {
        match self {
            Transfer::Linear => v,
            Transfer::Half { k } => {
                let x = v.abs();
                (x * ((1.0 + k) / (x + k)).sqrt()).copysign(v)
            }
            Transfer::HalfLinear { k, knee } => {
                let x = v.abs();
                if x <= knee {
                    Transfer::Half { k }.forward(v)
                } else {
                    let (at_knee, slope) = half_knee(k, knee);
                    (at_knee + slope * (x - knee)).copysign(v)
                }
            }
        }
    }

    /// The exact inverse of `forward` (up to float rounding).
    pub fn inverse(self, v: f32) -> f32 {
        match self {
            Transfer::Linear => v,
            Transfer::Half { k } => {
                // (1 + k) x² - y² x - y² k = 0, its positive root.
                let y = v.abs();
                (y * (y + (y * y + 4.0 * (1.0 + k) * k).sqrt()) / (2.0 * (1.0 + k))).copysign(v)
            }
            Transfer::HalfLinear { k, knee } => {
                let (at_knee, slope) = half_knee(k, knee);
                let y = v.abs();
                if y <= at_knee {
                    Transfer::Half { k }.inverse(v)
                } else {
                    (knee + (y - at_knee) / slope).copysign(v)
                }
            }
        }
    }
}

/// The transfer new proxies are encoded with.
pub const TRANSFER: Transfer = Transfer::HalfLinear { k: 0.1, knee: 0.18 };

/// Butteraugli distance of the lossy encoding (of the transferred values;
/// 1.0 is libjxl's "visually lossless" for display images, these are
/// edited). The same for Standard and 1:1 previews: 0.3 for Standard ones
/// saved 7 % but lost on the default recipe.
pub const DISTANCE: f32 = 0.25;
/// Encoder effort: only the entropy coding changes with it (effort 3 is
/// about 20 % smaller than 2 at the same pixels).
const EFFORT: u8 = 3;

/// Encodes `pixels` (linear sRGB, RGB, rows of `width`) with `transfer` at
/// `distance`.
pub fn encode(
    pixels: &[f32],
    width: usize,
    distance: f32,
    transfer: Transfer,
) -> Result<Vec<u8>, String> {
    if width == 0 || pixels.is_empty() || !pixels.len().is_multiple_of(width * 3) {
        return Err("Pixel buffer does not match the width".into());
    }
    let height = pixels.len() / (width * 3);
    let bytes: Vec<u8> = pixels
        .iter()
        .flat_map(|v| transfer.forward(*v).to_ne_bytes())
        .collect();
    jxl_encoder::LossyConfig::new(distance)
        .with_effort(EFFORT)
        .encode(
            &bytes,
            width as u32,
            height as u32,
            jxl_encoder::PixelLayout::RgbLinearF32,
        )
        .map_err(|e| format!("JPEG XL encoding failed: {e}"))
}

/// Decodes a JPEG XL made by `encode` with `transfer` that must hold
/// `width` × `rows` pixels, to linear sRGB.
pub fn decode(
    bytes: &[u8],
    width: usize,
    rows: usize,
    transfer: Transfer,
) -> Result<Vec<f32>, String> {
    let mut image = jxl_oxide::JxlImage::builder()
        .read(std::io::Cursor::new(bytes))
        .map_err(|e| format!("JPEG XL: {e}"))?;
    if (image.width() as usize, image.height() as usize) != (width, rows) {
        return Err("Unexpected JPEG XL size".into());
    }
    image.request_color_encoding(jxl_oxide::EnumColourEncoding::srgb_linear(
        jxl_oxide::RenderingIntent::Relative,
    ));
    let render = image.render_frame(0).map_err(|e| format!("JPEG XL: {e}"))?;
    let pixels = render.image_all_channels();
    if pixels.channels() != 3 || pixels.buf().len() != width * rows * 3 {
        return Err("Unexpected JPEG XL channels".into());
    }
    let mut out = pixels.buf().to_vec();
    if transfer != Transfer::Linear {
        for v in &mut out {
            *v = transfer.inverse(*v);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smooth ramps from deep shadow to above white, a saturated colour
    /// beyond sRGB (negative red) and a warm colour, in bands of 16 rows.
    fn ramps(width: usize, height: usize) -> Vec<f32> {
        (0..width * height)
            .flat_map(|i| {
                let (x, y) = ((i % width) as f32, (i / width) as f32);
                let v = (x / width as f32 * 12.0 - 10.0).exp2();
                match (y as usize / 16) % 3 {
                    0 => [v, v, v],
                    1 => [-0.02 * v, 0.6 * v, 0.1 * v],
                    _ => [v * 1.3, v, v * 0.7],
                }
            })
            .collect()
    }

    /// The largest error relative to the pixel's largest channel, at least
    /// `floor` of white.
    fn worst_relative(a: &[f32], b: &[f32], floor: f32) -> f32 {
        a.as_chunks::<3>()
            .0
            .iter()
            .zip(b.as_chunks::<3>().0)
            .map(|(p, q)| {
                let largest = p.iter().fold(floor, |m, v| m.max(v.abs()));
                (0..3)
                    .map(|c| (p[c] - q[c]).abs() / largest)
                    .fold(0.0, f32::max)
            })
            .fold(0.0, f32::max)
    }

    #[test]
    fn the_transfers_invert_exactly_and_keep_signs() {
        let half = Transfer::Half { k: 0.1 };
        for transfer in [Transfer::Linear, half, TRANSFER] {
            for v in [
                0.0f32, 1e-7, 1e-4, 0.003, 0.1, 0.17, 0.18, 0.19, 0.5, 1.0, 2.5, 16.0, -1e-5,
                -0.02, -0.7,
            ] {
                let y = transfer.forward(v);
                assert_eq!(y.signum(), v.signum(), "{v}");
                let back = transfer.inverse(y);
                assert!(
                    (back - v).abs() <= v.abs() * 2e-6 + 1e-12,
                    "{transfer:?} {v} -> {y} -> {back}"
                );
            }
        }
        assert!((half.forward(1.0) - 1.0).abs() < 1e-6);
        // Below the knee as `Half`, continuous and with its slope at it.
        assert_eq!(TRANSFER.forward(0.05), half.forward(0.05));
        let (below, above) = (TRANSFER.forward(0.18 - 1e-4), TRANSFER.forward(0.18 + 1e-4));
        let slope = (half.forward(0.18) - half.forward(0.18 - 1e-3)) / 1e-3;
        assert!(((above - below) / 2e-4 - slope).abs() < 0.01 * slope);
        // Monotone, steeper than linear in the shadows.
        let mut last = TRANSFER.forward(-4.0);
        for i in -399..=400 {
            let y = TRANSFER.forward(i as f32 / 100.0);
            assert!(y > last);
            last = y;
        }
        assert!(TRANSFER.forward(0.01) > 0.03);
    }

    #[test]
    fn images_round_trip_close_to_their_values() {
        let (width, height) = (97, 48);
        let pixels = ramps(width, height);
        for transfer in [Transfer::Linear, TRANSFER] {
            let bytes = encode(&pixels, width, DISTANCE, transfer).unwrap();
            let back = decode(&bytes, width, height, transfer).unwrap();
            assert_eq!(back.len(), pixels.len());
            // Relative to the pixel's largest channel, at least 1 % of
            // white.
            let worst = worst_relative(&pixels, &back, 0.01);
            assert!(worst < 0.05, "{transfer:?}: worst relative error {worst}");
            // The negative channel and the values above white survive.
            assert!(back.as_chunks::<3>().0.iter().any(|p| p[0] < -0.01));
            assert!(back.iter().any(|v| *v > 1.5));
            // Far smaller than half floats (6 bytes per pixel).
            assert!(
                bytes.len() < width * height * 6 / 4,
                "{} bytes",
                bytes.len()
            );
        }
    }

    #[test]
    fn the_transfer_keeps_deep_shadows_closer() {
        // Fine noise on a deep shadow (0.2-0.6 % of white), as a RAW's
        // shadows hold it: the transfer keeps more of it.
        let (width, height) = (128, 64);
        let mut state = 12345u32;
        let pixels: Vec<f32> = (0..width * height * 3)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                0.004 + ((state >> 8) as f32 / (1u32 << 24) as f32 - 0.5) * 0.004
            })
            .collect();
        let error = |transfer: Transfer| {
            let bytes = encode(&pixels, width, DISTANCE, transfer).unwrap();
            let back = decode(&bytes, width, height, transfer).unwrap();
            pixels
                .iter()
                .zip(&back)
                .map(|(a, b)| (a - b).powi(2))
                .sum::<f32>()
        };
        let (linear, shaped) = (error(Transfer::Linear), error(TRANSFER));
        assert!(shaped < linear * 0.8, "{shaped} vs {linear}");
    }

    #[test]
    fn mismatched_or_damaged_data_is_refused() {
        let pixels = vec![0.25f32; 16 * 8 * 3];
        let bytes = encode(&pixels, 16, DISTANCE, TRANSFER).unwrap();
        assert!(decode(&bytes, 16, 9, TRANSFER).is_err());
        assert!(decode(&bytes, 15, 8, TRANSFER).is_err());
        assert!(decode(&bytes[..bytes.len() / 2], 16, 8, TRANSFER).is_err());
        assert!(decode(b"not an image", 16, 8, TRANSFER).is_err());
        assert!(encode(&pixels, 7, DISTANCE, TRANSFER).is_err());
    }
}
