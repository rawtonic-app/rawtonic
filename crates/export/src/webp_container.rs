//! ICC, EXIF and XMP chunks for exported WebP files.
//!
//! libwebp writes lossy exports in the simple format (a single `VP8 ` chunk),
//! which cannot carry metadata; little_exif cannot convert that format and
//! appended a stray EXIF chunk instead. This rebuilds the file in the extended
//! format: `VP8X` with the matching flags and canvas size, `ICCP`, the image
//! chunks in their original order, then `EXIF` and `XMP `, each padded to an
//! even length as the container specification requires.

const FLAG_ANIMATION: u8 = 0x02;
const FLAG_XMP: u8 = 0x04;
const FLAG_EXIF: u8 = 0x08;
const FLAG_ALPHA: u8 = 0x10;
const FLAG_ICC: u8 = 0x20;

struct Chunk<'a> {
    fourcc: [u8; 4],
    payload: &'a [u8],
}

fn chunks(webp: &[u8]) -> Result<Vec<Chunk<'_>>, String> {
    if webp.len() < 12 || &webp[0..4] != b"RIFF" || &webp[8..12] != b"WEBP" {
        return Err("not a WebP file".to_string());
    }
    let riff_end =
        (u32::from_le_bytes(webp[4..8].try_into().unwrap()) as usize + 8).min(webp.len());
    let mut chunks = Vec::new();
    let mut at = 12;
    while at + 8 <= riff_end {
        let fourcc: [u8; 4] = webp[at..at + 4].try_into().unwrap();
        let size = u32::from_le_bytes(webp[at + 4..at + 8].try_into().unwrap()) as usize;
        let payload = webp.get(at + 8..at + 8 + size).ok_or_else(|| {
            format!(
                "WebP chunk {} is truncated",
                String::from_utf8_lossy(&fourcc)
            )
        })?;
        chunks.push(Chunk { fourcc, payload });
        at += 8 + size + (size & 1);
    }
    Ok(chunks)
}

/// Canvas size and alpha of a simple-format image chunk.
fn bitstream_info(chunk: &Chunk) -> Result<(u32, u32, bool), String> {
    let p = chunk.payload;
    match &chunk.fourcc {
        b"VP8 " if p.len() >= 10 && p[3..6] == [0x9d, 0x01, 0x2a] => {
            let w = u16::from_le_bytes([p[6], p[7]]) & 0x3fff;
            let h = u16::from_le_bytes([p[8], p[9]]) & 0x3fff;
            Ok((w as u32, h as u32, false))
        }
        b"VP8L" if p.len() >= 5 && p[0] == 0x2f => {
            let bits = u32::from_le_bytes(p[1..5].try_into().unwrap());
            Ok((
                (bits & 0x3fff) + 1,
                ((bits >> 14) & 0x3fff) + 1,
                (bits >> 28) & 1 == 1,
            ))
        }
        _ => Err("WebP image data is not VP8 or VP8L".to_string()),
    }
}

fn push_chunk(out: &mut Vec<u8>, fourcc: &[u8; 4], payload: &[u8]) -> Result<(), String> {
    let size = u32::try_from(payload.len()).map_err(|_| "WebP chunk too large".to_string())?;
    out.extend_from_slice(fourcc);
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(payload);
    if payload.len() % 2 == 1 {
        out.push(0);
    }
    Ok(())
}

/// Rewrites `webp` in the extended format. `None` keeps a chunk the file
/// already has; `Some` replaces it. `exif` is TIFF-structured EXIF data and
/// `xmp` a complete XMP packet.
pub fn set_chunks(
    webp: &[u8],
    icc: Option<&[u8]>,
    exif: Option<&[u8]>,
    xmp: Option<&[u8]>,
) -> Result<Vec<u8>, String> {
    let chunks = chunks(webp)?;
    let existing = |fourcc: &[u8; 4]| {
        chunks
            .iter()
            .find(|c| &c.fourcc == fourcc)
            .map(|c| c.payload)
    };
    let icc = icc.or_else(|| existing(b"ICCP"));
    let exif = exif.or_else(|| existing(b"EXIF"));
    let xmp = xmp.or_else(|| existing(b"XMP "));
    let image: Vec<&Chunk> = chunks
        .iter()
        .filter(|c| !matches!(&c.fourcc, b"VP8X" | b"ICCP" | b"EXIF" | b"XMP "))
        .collect();

    let (mut flags, canvas) = match chunks.iter().find(|c| &c.fourcc == b"VP8X") {
        Some(vp8x) if vp8x.payload.len() >= 10 => (
            vp8x.payload[0] & (FLAG_ALPHA | FLAG_ANIMATION),
            <[u8; 6]>::try_from(&vp8x.payload[4..10]).unwrap(),
        ),
        Some(_) => return Err("WebP VP8X chunk is truncated".to_string()),
        None => {
            let first = image.first().ok_or("WebP file has no image data")?;
            let (w, h, alpha) = bitstream_info(first)?;
            if w == 0 || h == 0 {
                return Err("WebP image has no size".to_string());
            }
            let (w, h) = ((w - 1).to_le_bytes(), (h - 1).to_le_bytes());
            (
                if alpha { FLAG_ALPHA } else { 0 },
                [w[0], w[1], w[2], h[0], h[1], h[2]],
            )
        }
    };
    if image.iter().any(|c| &c.fourcc == b"ALPH") {
        flags |= FLAG_ALPHA;
    }
    for (present, flag) in [
        (icc.is_some(), FLAG_ICC),
        (exif.is_some(), FLAG_EXIF),
        (xmp.is_some(), FLAG_XMP),
    ] {
        if present {
            flags |= flag;
        }
    }

    let mut body = Vec::with_capacity(webp.len() + 64);
    body.extend_from_slice(b"WEBP");
    let mut vp8x = [0u8; 10];
    vp8x[0] = flags;
    vp8x[4..10].copy_from_slice(&canvas);
    push_chunk(&mut body, b"VP8X", &vp8x)?;
    if let Some(icc) = icc {
        push_chunk(&mut body, b"ICCP", icc)?;
    }
    for chunk in &image {
        push_chunk(&mut body, &chunk.fourcc, chunk.payload)?;
    }
    if let Some(exif) = exif {
        push_chunk(&mut body, b"EXIF", exif)?;
    }
    if let Some(xmp) = xmp {
        push_chunk(&mut body, b"XMP ", xmp)?;
    }
    let size = u32::try_from(body.len()).map_err(|_| "WebP file too large".to_string())?;
    let mut out = Vec::with_capacity(body.len() + 8);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, RgbImage, RgbaImage};

    fn fourccs(webp: &[u8]) -> Vec<String> {
        chunks(webp)
            .unwrap()
            .iter()
            .map(|c| String::from_utf8_lossy(&c.fourcc).into_owned())
            .collect()
    }

    fn lossy(image: &DynamicImage) -> Vec<u8> {
        webp::Encoder::from_image(image)
            .unwrap()
            .encode(80.0)
            .to_vec()
    }

    #[test]
    fn simple_lossy_file_becomes_extended_with_all_chunks() {
        let image = DynamicImage::ImageRgb8(RgbImage::from_fn(37, 21, |x, y| {
            image::Rgb([(x * 6) as u8, (y * 11) as u8, 90])
        }));
        let original = lossy(&image);
        assert_eq!(fourccs(&original), ["VP8 "]);

        let out = set_chunks(&original, Some(b"icc!"), Some(b"II*\0exif"), Some(b"<x/>")).unwrap();
        assert_eq!(fourccs(&out), ["VP8X", "ICCP", "VP8 ", "EXIF", "XMP "]);
        let vp8x = &chunks(&out).unwrap()[0];
        assert_eq!(vp8x.payload[0], FLAG_ICC | FLAG_EXIF | FLAG_XMP);
        assert_eq!(&vp8x.payload[4..10], &[36, 0, 0, 20, 0, 0]);
        assert_eq!(
            u32::from_le_bytes(out[4..8].try_into().unwrap()) as usize + 8,
            out.len()
        );

        let before = image::load_from_memory(&original).unwrap();
        let after = image::load_from_memory(&out).unwrap();
        assert_eq!(before.as_bytes(), after.as_bytes());
    }

    #[test]
    fn keeps_alpha_and_existing_chunks() {
        let image = DynamicImage::ImageRgba8(RgbaImage::from_fn(16, 9, |x, y| {
            image::Rgba([x as u8 * 9, y as u8 * 20, 4, (x * 15) as u8])
        }));
        let with_icc = set_chunks(&lossy(&image), Some(b"profile"), None, None).unwrap();
        assert!(fourccs(&with_icc).contains(&"ALPH".to_string()));
        let out = set_chunks(&with_icc, None, Some(b"odd"), None).unwrap();
        assert_eq!(fourccs(&out), ["VP8X", "ICCP", "ALPH", "VP8 ", "EXIF"]);
        let vp8x = &chunks(&out).unwrap()[0];
        assert_eq!(vp8x.payload[0], FLAG_ICC | FLAG_EXIF | FLAG_ALPHA);
        assert_eq!(chunks(&out).unwrap()[1].payload, b"profile");
        assert_eq!(chunks(&out).unwrap()[4].payload, b"odd");
        assert_eq!(
            image::load_from_memory(&out).unwrap().as_bytes(),
            image::load_from_memory(&with_icc).unwrap().as_bytes()
        );
    }

    #[test]
    fn repairs_a_stray_exif_chunk_after_simple_image_data() {
        let image = DynamicImage::ImageRgb8(RgbImage::new(8, 8));
        let mut broken = lossy(&image);
        broken.extend_from_slice(b"EXIF\x04\0\0\0II*\0");
        let size = (broken.len() - 8) as u32;
        broken[4..8].copy_from_slice(&size.to_le_bytes());
        let out = set_chunks(&broken, Some(b"icc"), None, None).unwrap();
        assert_eq!(fourccs(&out), ["VP8X", "ICCP", "VP8 ", "EXIF"]);
    }

    #[test]
    fn lossless_dimensions_come_from_the_vp8l_header() {
        let image = DynamicImage::ImageRgba8(RgbaImage::from_fn(300, 7, |x, _| {
            image::Rgba([x as u8, 0, 0, 128])
        }));
        let mut lossless = Vec::new();
        image
            .write_to(
                &mut std::io::Cursor::new(&mut lossless),
                image::ImageFormat::WebP,
            )
            .unwrap();
        assert_eq!(fourccs(&lossless), ["VP8L"]);
        let out = set_chunks(&lossless, Some(b"icc"), None, None).unwrap();
        let vp8x = &chunks(&out).unwrap()[0];
        assert_eq!(vp8x.payload[0], FLAG_ICC | FLAG_ALPHA);
        assert_eq!(&vp8x.payload[4..10], &[43, 1, 0, 6, 0, 0]);
    }

    #[test]
    fn rejects_other_files() {
        assert!(set_chunks(b"\x89PNG\r\n\x1a\n", None, None, None).is_err());
    }
}
