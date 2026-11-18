//! Browser preview JPEGs (Linux drafts and frames, multi-view cells) and the
//! lossless PNG of a settled reduced frame ([`settled_lossless`]),
//! encoded in horizontal strips on several threads. One 4:4:4 encode of a
//! 1.5 MP drag draft took ~6.5 ms of the ~14 ms native job on a Ryzen 5
//! 3600X (notes: benchmark-results/linux-slider-drag-2026-10-17/).
//!
//! Each strip is a whole number of MCU rows encoded as its own baseline JPEG
//! with the same quantisation and (standard, unoptimised) Huffman tables.
//! The strips' entropy-coded data are joined with restart markers, and a
//! restart interval of one strip is declared. Blocks are transformed and
//! quantised exactly as in one encode and DC differences are lossless, so
//! the result decodes to the same pixels as a single encode.

use rayon::prelude::*;

/// A settled browser frame shown reduced (fit, a larger fit).
pub(crate) const SETTLED_QUALITY: u8 = 94;
/// A settled browser frame at the photograph's own resolution (100 % and
/// closer). Measured against the export on a 24 MP NEF at ISO 1600: quality
/// 94 left 1.09–1.42 codes mean difference per channel in sharpness crops
/// (2.1–3.6 on the grainy before without noise reduction) and lost a third
/// of the grain in a flat region; 98 left 0.68–0.87 (before 0.93–1.69); 99
/// leaves 0.43–0.68 (before 0.55–1.00), at about 1.5× the bytes of 98
/// (notes: parity-gaps/G24-compare-before-after.md, LNX1).
pub(crate) const ONE_TO_ONE_QUALITY: u8 = 99;

/// The quality of a settled frame with `scale` frame px per full-resolution
/// px of the photograph.
pub(crate) fn settled_quality(scale: f32) -> u8 {
    if scale >= 0.999 {
        ONE_TO_ONE_QUALITY
    } else {
        SETTLED_QUALITY
    }
}

/// Whether a settled browser frame with `scale` frame px per
/// full-resolution px goes to the webview lossless (PNG) instead of as a
/// JPEG. Against the export, every JPEG quality moved a reduced frame's
/// fine chroma out of the ±15 % bar (q94 removed it, q98–q100 added 8-bit
/// quantisation noise the noise-reduced export lacks); the lossless frame
/// stayed within it, as the native displays do. A PNG costs several times a
/// JPEG's encode, bytes and decode, growing with the frame; drags keep
/// their JPEG. At 100 % the frame is the whole photograph (a 24 MP PNG is
/// ~60 MB), so it stays a JPEG (docs/benchmark-results/
/// lnxj1-linux-preview-transport-2026-11-17/REPORT.md).
pub(crate) fn settled_lossless(scale: f32) -> bool {
    scale < 0.999
}

/// A settled browser frame encoded as [`settled_lossless`] and
/// [`settled_quality`] choose for its `scale`. Sent on Linux only (and
/// tested everywhere).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn encode_settled(image: &image::RgbaImage, scale: f32) -> Result<Vec<u8>, String> {
    encode_settled_like(image, scale, None)
}

/// [`encode_settled`], or as the frame it is compared with when
/// `lossless` says how that one came (a split's before beside the editor's
/// frame, which is a JPEG only at 100 %).
pub(crate) fn encode_settled_like(
    image: &image::RgbaImage,
    scale: f32,
    lossless: Option<bool>,
) -> Result<Vec<u8>, String> {
    match lossless.unwrap_or_else(|| settled_lossless(scale)) {
        true => encode_png_rgb(image),
        false if lossless.is_some() => encode_rgbx_444(image, ONE_TO_ONE_QUALITY),
        false => encode_rgbx_444(image, settled_quality(scale)),
    }
}

/// `image` as an 8-bit RGB PNG (alpha ignored), with fast compression and
/// the Up filter (the fastest of the filters at about the adaptive one's
/// size on photographs).
fn encode_png_rgb(image: &image::RgbaImage) -> Result<Vec<u8>, String> {
    use image::ImageEncoder;
    use image::codecs::png::{CompressionType, FilterType, PngEncoder};
    let rgb: Vec<u8> = image
        .as_raw()
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|&[r, g, b, _]| [r, g, b])
        .collect();
    let mut out = Vec::with_capacity(rgb.len() / 2);
    PngEncoder::new_with_quality(&mut out, CompressionType::Fast, FilterType::Up)
        .write_image(
            &rgb,
            image.width(),
            image.height(),
            image::ExtendedColorType::Rgb8,
        )
        .map_err(|e| e.to_string())?;
    Ok(out)
}

/// 4:4:4 MCUs are 8×8 pixels.
const MCU: usize = 8;
/// Below this many MCU rows per strip the threads cost more than they save.
const MIN_STRIP_MCU_ROWS: usize = 16;
const MAX_STRIPS: usize = 6;

pub(crate) fn encode_rgbx_444(image: &image::RgbaImage, quality: u8) -> Result<Vec<u8>, String> {
    let (width, height) = (image.width() as usize, image.height() as usize);
    let mcu_rows = height.div_ceil(MCU);
    let mcus_per_row = width.div_ceil(MCU).max(1);
    let strips = rayon::current_num_threads()
        .min(MAX_STRIPS)
        .min(mcu_rows / MIN_STRIP_MCU_ROWS);
    // DRI holds the restart interval in 16 bits.
    let strip_rows = mcu_rows
        .div_ceil(strips.max(1))
        .min(usize::from(u16::MAX) / mcus_per_row);
    let pixels = image.as_raw().as_slice();
    if strips < 2 || strip_rows == 0 {
        return encode(pixels, width, height, quality);
    }
    let row_bytes = width * 4;
    let parts = (0..mcu_rows)
        .step_by(strip_rows)
        .collect::<Vec<_>>()
        .into_par_iter()
        .map(|first| {
            let top = first * MCU;
            let bottom = height.min((first + strip_rows) * MCU);
            encode(
                &pixels[top * row_bytes..bottom * row_bytes],
                width,
                bottom - top,
                quality,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    join_strips(&parts, height, strip_rows * mcus_per_row)
}

fn encode(pixels: &[u8], width: usize, height: usize, quality: u8) -> Result<Vec<u8>, String> {
    let mut compressor = turbojpeg::Compressor::new().map_err(|e| e.to_string())?;
    compressor
        .set_quality(i32::from(quality.clamp(1, 100)))
        .map_err(|e| e.to_string())?;
    // Keep colour detail in the final frame and in small interactive patches.
    compressor
        .set_subsamp(turbojpeg::Subsamp::None)
        .map_err(|e| e.to_string())?;
    compressor
        .compress_to_vec(turbojpeg::Image {
            pixels,
            width,
            pitch: width * 4,
            height,
            format: turbojpeg::PixelFormat::RGBX,
        })
        .map_err(|e| e.to_string())
}

/// Byte ranges of a strip: everything before its scan header, the scan
/// header (SOS segment), and the entropy-coded data up to EOI.
struct Layout {
    sof_height_at: usize,
    sos: std::ops::Range<usize>,
    data_end: usize,
}

fn layout(jpeg: &[u8]) -> Result<Layout, String> {
    let bad = || "unexpected preview JPEG layout".to_string();
    if !jpeg.starts_with(&[0xFF, 0xD8]) || !jpeg.ends_with(&[0xFF, 0xD9]) {
        return Err(bad());
    }
    let mut at = 2;
    let mut sof_height_at = None;
    while at + 4 <= jpeg.len() {
        if jpeg[at] != 0xFF {
            return Err(bad());
        }
        let marker = jpeg[at + 1];
        let length = usize::from(u16::from_be_bytes([jpeg[at + 2], jpeg[at + 3]]));
        match marker {
            0xC0 => sof_height_at = Some(at + 5),
            // Progressive, arithmetic or an existing restart interval
            // would not join this way.
            0xC1..=0xCF if marker != 0xC4 && marker != 0xC8 && marker != 0xCC => return Err(bad()),
            0xDD => return Err(bad()),
            0xDA => {
                return Ok(Layout {
                    sof_height_at: sof_height_at.ok_or_else(bad)?,
                    sos: at..at + 2 + length,
                    data_end: jpeg.len() - 2,
                });
            }
            _ => {}
        }
        at += 2 + length;
    }
    Err(bad())
}

fn join_strips(parts: &[Vec<u8>], height: usize, interval: usize) -> Result<Vec<u8>, String> {
    let first = layout(&parts[0])?;
    let header = &parts[0][..first.sos.start];
    let mut out = Vec::with_capacity(parts.iter().map(Vec::len).sum::<usize>() + 64);
    out.extend_from_slice(header);
    let height = u16::try_from(height).map_err(|_| "preview too tall for JPEG".to_string())?;
    out[first.sof_height_at..first.sof_height_at + 2].copy_from_slice(&height.to_be_bytes());
    let interval = u16::try_from(interval).map_err(|_| "restart interval too long".to_string())?;
    out.extend_from_slice(&[0xFF, 0xDD, 0x00, 0x04]);
    out.extend_from_slice(&interval.to_be_bytes());
    out.extend_from_slice(&parts[0][first.sos.clone()]);
    for (index, part) in parts.iter().enumerate() {
        let strip = if index == 0 {
            first.sos.end..first.data_end
        } else {
            let other = layout(part)?;
            // Same tables and scan header, or the strips do not belong together.
            if part[..other.sof_height_at] != header[..first.sof_height_at]
                || part[other.sof_height_at + 2..other.sos.end]
                    != parts[0][first.sof_height_at + 2..first.sos.end]
            {
                return Err("preview JPEG strips differ in their tables".to_string());
            }
            out.extend_from_slice(&[0xFF, 0xD0 + ((index - 1) % 8) as u8]);
            other.sos.end..other.data_end
        };
        out.extend_from_slice(&part[strip]);
    }
    out.extend_from_slice(&[0xFF, 0xD9]);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn photo_like(width: u32, height: u32) -> image::RgbaImage {
        // Gradients, edges and pseudo-random grain: every block has AC energy.
        let mut seed = 0x2545_f491_u32;
        image::RgbaImage::from_fn(width, height, |x, y| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let grain = (seed % 23) as u8;
            let edge = if (x / 37 + y / 29) % 2 == 0 { 60 } else { 0 };
            image::Rgba([
                ((x * 255 / width) as u8).saturating_add(grain),
                ((y * 255 / height) as u8).saturating_add(edge),
                (((x + y) % 256) as u8).wrapping_add(grain),
                0,
            ])
        })
    }

    fn decode(jpeg: &[u8]) -> Vec<u8> {
        turbojpeg::decompress(jpeg, turbojpeg::PixelFormat::RGB)
            .unwrap()
            .pixels
    }

    #[test]
    fn strips_decode_to_the_pixels_of_one_encode() {
        // Odd sizes: a partial last MCU row and column.
        for (width, height) in [
            (1536, 1029),
            (1536, 1024),
            (1001, 667),
            (333, 1500),
            (17, 9),
        ] {
            let image = photo_like(width, height);
            for quality in [75, 94] {
                let single =
                    encode(image.as_raw(), width as usize, height as usize, quality).unwrap();
                let joined = encode_rgbx_444(&image, quality).unwrap();
                let header = turbojpeg::read_header(&joined).unwrap();
                assert_eq!(
                    (header.width, header.height),
                    (width as usize, height as usize)
                );
                assert_eq!(header.subsamp, turbojpeg::Subsamp::None);
                assert!(
                    decode(&single) == decode(&joined),
                    "{width}×{height} q{quality} differs"
                );
                // Other decoders read it too (the webview uses libjpeg).
                let other = image::load_from_memory(&joined).unwrap().to_rgb8();
                assert_eq!(other.dimensions(), (width, height));
            }
        }
    }

    #[test]
    fn settled_png_is_lossless_rgb() {
        for (width, height) in [(1536, 1029), (17, 9)] {
            let image = photo_like(width, height);
            let png = encode_png_rgb(&image).unwrap();
            assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
            let decoded = image::load_from_memory(&png).unwrap().to_rgb8();
            assert_eq!(decoded.dimensions(), (width, height));
            let rgb: Vec<u8> = image
                .as_raw()
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|&[r, g, b, _]| [r, g, b])
                .collect();
            assert!(decoded.as_raw() == &rgb, "{width}×{height} changed");
        }
        assert!(settled_lossless(0.25) && settled_lossless(0.998));
        assert!(!settled_lossless(1.0) && !settled_lossless(2.0));
        let small = photo_like(64, 48);
        assert!(encode_settled(&small, 0.5).unwrap().starts_with(b"\x89PNG"));
        // A split's before follows the after across the 100 % boundary.
        let png = encode_settled_like(&small, 1.0, Some(true)).unwrap();
        assert!(png.starts_with(b"\x89PNG"));
        let jpeg = encode_settled_like(&small, 0.8, Some(false)).unwrap();
        assert_eq!(
            (jpeg[..2] == [0xFF, 0xD8], jpeg),
            (true, encode_rgbx_444(&small, ONE_TO_ONE_QUALITY).unwrap())
        );
        assert!(
            encode_settled(&small, 1.0)
                .unwrap()
                .starts_with(&[0xFF, 0xD8])
        );
    }

    #[test]
    fn large_drafts_are_split() {
        let image = photo_like(1536, 1024);
        let joined = encode_rgbx_444(&image, 85).unwrap();
        if rayon::current_num_threads() >= 2 {
            assert!(
                joined.windows(2).any(|w| w == [0xFF, 0xDD]),
                "no restart interval"
            );
        }
    }

    /// `cargo test -p rawtonic-app --release preview_jpeg -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn strip_encode_timing() {
        let image = photo_like(1536, 1024);
        for (name, strips) in [("single", false), ("strips", true)] {
            let mut times = Vec::new();
            for _ in 0..60 {
                let start = std::time::Instant::now();
                let _ = if strips {
                    encode_rgbx_444(&image, 85)
                } else {
                    encode(image.as_raw(), 1536, 1024, 85)
                };
                times.push(start.elapsed().as_secs_f64() * 1000.0);
            }
            times.sort_by(f64::total_cmp);
            println!("{name}: p50 {:.2} ms, p95 {:.2} ms", times[30], times[57]);
        }
    }
}
