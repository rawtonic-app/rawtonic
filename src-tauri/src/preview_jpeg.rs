//! Browser preview JPEGs (Linux drafts and frames, multi-view cells),
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
