//! Embeds an XMP packet in exported JPEG and PNG files, where Lightroom and
//! Bridge read keywords, title and caption, and wraps JPEG XL codestreams in
//! the container that carries EXIF and XMP. TIFF (tag 700) and WebP carry XMP
//! through their own container code.

const JPEG_XMP_HEADER: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";
const PNG_XMP_KEYWORD: &[u8] = b"XML:com.adobe.xmp";

/// Inserts the packet as an APP1 segment after the leading APP0/APP1
/// segments (JFIF, EXIF), replacing an XMP segment that is already there.
pub fn jpeg_insert_xmp(jpeg: &mut Vec<u8>, packet: &[u8]) -> Result<(), String> {
    if jpeg.get(0..2) != Some(&[0xff, 0xd8]) {
        return Err("not a JPEG file".to_string());
    }
    let length = u16::try_from(2 + JPEG_XMP_HEADER.len() + packet.len())
        .map_err(|_| "XMP packet exceeds one JPEG segment".to_string())?;

    let mut at = 2;
    let mut insert_at = 2;
    let mut replace: Option<std::ops::Range<usize>> = None;
    while let Some(&[0xff, marker]) = jpeg.get(at..at + 2) {
        if !(0xe0..=0xef).contains(&marker) {
            break;
        }
        let size = jpeg
            .get(at + 2..at + 4)
            .map(|b| u16::from_be_bytes([b[0], b[1]]) as usize)
            .ok_or("JPEG segment is truncated")?;
        let end = at + 2 + size;
        if end > jpeg.len() || size < 2 {
            return Err("JPEG segment is truncated".to_string());
        }
        if marker == 0xe1 && jpeg[at + 4..end].starts_with(JPEG_XMP_HEADER) {
            replace = Some(at..end);
        }
        if (marker == 0xe0 || marker == 0xe1) && insert_at == at {
            insert_at = end;
        }
        at = end;
    }

    let mut segment = Vec::with_capacity(length as usize + 2);
    segment.extend_from_slice(&[0xff, 0xe1]);
    segment.extend_from_slice(&length.to_be_bytes());
    segment.extend_from_slice(JPEG_XMP_HEADER);
    segment.extend_from_slice(packet);
    match replace {
        Some(range) => {
            jpeg.splice(range, segment);
        }
        None => {
            jpeg.splice(insert_at..insert_at, segment);
        }
    }
    Ok(())
}

/// Inserts the packet as an uncompressed `iTXt` chunk before the image data,
/// replacing an XMP chunk that is already there.
pub fn png_insert_xmp(png: &mut Vec<u8>, packet: &[u8]) -> Result<(), String> {
    let mut data = Vec::with_capacity(PNG_XMP_KEYWORD.len() + 5 + packet.len());
    data.extend_from_slice(PNG_XMP_KEYWORD);
    // NUL, compression flag, compression method, empty language and
    // translated keyword.
    data.extend_from_slice(&[0, 0, 0, 0, 0]);
    data.extend_from_slice(packet);
    png_insert_chunk(png, b"iTXt", &data)
}

/// Writes TIFF-structured EXIF in the standard PNG eXIf chunk.
pub fn png_insert_exif(png: &mut Vec<u8>, exif: &[u8]) -> Result<(), String> {
    png_insert_chunk(png, b"eXIf", exif)
}

fn png_insert_chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) -> Result<(), String> {
    if png.get(0..8) != Some(b"\x89PNG\r\n\x1a\n") {
        return Err("not a PNG file".to_string());
    }
    let length = u32::try_from(data.len()).map_err(|_| "PNG metadata too large".to_string())?;
    let mut chunk = Vec::with_capacity(data.len() + 12);
    chunk.extend_from_slice(&length.to_be_bytes());
    chunk.extend_from_slice(kind);
    chunk.extend_from_slice(data);
    let mut crc = crc32fast::Hasher::new();
    crc.update(kind);
    crc.update(data);
    chunk.extend_from_slice(&crc.finalize().to_be_bytes());

    let mut at = 8;
    loop {
        let header = png.get(at..at + 8).ok_or("PNG has no image data")?;
        let size = u32::from_be_bytes(header[0..4].try_into().unwrap()) as usize;
        let end = at + 12 + size;
        if end > png.len() {
            return Err("PNG chunk is truncated".to_string());
        }
        match &header[4..8] {
            b"IDAT" => {
                png.splice(at..at, chunk);
                return Ok(());
            }
            existing
                if existing == kind
                    && (kind == b"eXIf" || png[at + 8..end - 4].starts_with(PNG_XMP_KEYWORD)) =>
            {
                png.splice(at..end, chunk);
                return Ok(());
            }
            _ => at = end,
        }
    }
}

/// Wraps a bare JPEG XL codestream in the ISO/IEC 18181-2 container with
/// `Exif` (TIFF-structured EXIF) and `xml ` (XMP) boxes. The metadata boxes go
/// before the codestream: the standard allows either order, and macOS ImageIO
/// only reads XMP placed there.
pub fn jxl_container(codestream: &[u8], exif: Option<&[u8]>, xmp: Option<&[u8]>) -> Vec<u8> {
    fn push_box(out: &mut Vec<u8>, fourcc: &[u8; 4], parts: &[&[u8]]) {
        let payload: usize = parts.iter().map(|p| p.len()).sum();
        match u32::try_from(8 + payload) {
            Ok(size) => {
                out.extend_from_slice(&size.to_be_bytes());
                out.extend_from_slice(fourcc);
            }
            Err(_) => {
                out.extend_from_slice(&1u32.to_be_bytes());
                out.extend_from_slice(fourcc);
                out.extend_from_slice(&(16 + payload as u64).to_be_bytes());
            }
        }
        for part in parts {
            out.extend_from_slice(part);
        }
    }
    let mut out = Vec::with_capacity(codestream.len() + 4096);
    push_box(&mut out, b"JXL ", &[&[0x0d, 0x0a, 0x87, 0x0a]]);
    push_box(&mut out, b"ftyp", &[b"jxl ", &[0; 4], b"jxl "]);
    if let Some(exif) = exif {
        // The payload starts with the offset of the TIFF header.
        push_box(&mut out, b"Exif", &[&[0; 4], exif]);
    }
    if let Some(xmp) = xmp {
        push_box(&mut out, b"xml ", &[xmp]);
    }
    push_box(&mut out, b"jxlc", &[codestream]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, ImageFormat, RgbImage};
    use std::io::Cursor;

    const PACKET: &[u8] = b"<?xpacket begin=\"\xef\xbb\xbf\"?><x:xmpmeta xmlns:x=\"adobe:ns:meta/\"/><?xpacket end=\"w\"?>";

    fn encoded(format: ImageFormat) -> (Vec<u8>, DynamicImage) {
        let image = DynamicImage::ImageRgb8(RgbImage::from_fn(9, 5, |x, y| {
            image::Rgb([x as u8 * 20, y as u8 * 40, 7])
        }));
        let mut bytes = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut bytes), format)
            .unwrap();
        (bytes, image)
    }

    fn jpeg_markers(jpeg: &[u8]) -> Vec<(u8, usize)> {
        let mut out = Vec::new();
        let mut at = 2;
        while jpeg[at] == 0xff && jpeg[at + 1] != 0xda {
            let size = u16::from_be_bytes([jpeg[at + 2], jpeg[at + 3]]) as usize;
            out.push((jpeg[at + 1], at));
            at += 2 + size;
        }
        out
    }

    #[test]
    fn jpeg_gets_one_xmp_segment_after_jfif_and_exif() {
        let (mut jpeg, _) = encoded(ImageFormat::Jpeg);
        let before = image::load_from_memory(&jpeg).unwrap();
        jpeg_insert_xmp(&mut jpeg, PACKET).unwrap();
        jpeg_insert_xmp(&mut jpeg, PACKET).unwrap();
        let markers = jpeg_markers(&jpeg);
        assert_eq!(markers[0].0, 0xe0);
        assert_eq!(markers[1].0, 0xe1);
        let xmp_segments = markers
            .iter()
            .filter(|(m, at)| *m == 0xe1 && jpeg[at + 4..].starts_with(JPEG_XMP_HEADER))
            .count();
        assert_eq!(xmp_segments, 1);
        let at = markers[1].1;
        assert_eq!(
            &jpeg[at + 4 + JPEG_XMP_HEADER.len()..][..PACKET.len()],
            PACKET
        );
        assert_eq!(
            image::load_from_memory(&jpeg).unwrap().as_bytes(),
            before.as_bytes()
        );
    }

    #[test]
    fn jpeg_refuses_packets_beyond_one_segment() {
        let (mut jpeg, _) = encoded(ImageFormat::Jpeg);
        let original = jpeg.clone();
        assert!(jpeg_insert_xmp(&mut jpeg, &vec![b' '; 70_000]).is_err());
        assert_eq!(jpeg, original);
    }

    #[test]
    fn jxl_container_puts_metadata_before_the_codestream() {
        let image = DynamicImage::ImageRgb8(RgbImage::from_fn(12, 7, |x, y| {
            image::Rgb([x as u8 * 20, y as u8 * 30, 99])
        }));
        let rgb = image.to_rgb8();
        let codestream = jxl_encoder::LosslessConfig::new()
            .encode(rgb.as_raw(), 12, 7, jxl_encoder::PixelLayout::Rgb8)
            .unwrap();
        let wrapped = jxl_container(&codestream, Some(b"II*\0exif"), Some(PACKET));

        let mut boxes = Vec::new();
        let mut at = 0;
        while at < wrapped.len() {
            let size = u32::from_be_bytes(wrapped[at..at + 4].try_into().unwrap()) as usize;
            boxes.push((wrapped[at + 4..at + 8].to_vec(), at));
            at += size;
        }
        assert_eq!(at, wrapped.len());
        let names: Vec<&[u8]> = boxes.iter().map(|(n, _)| n.as_slice()).collect();
        assert_eq!(names, [b"JXL ", b"ftyp", b"Exif", b"xml ", b"jxlc"]);
        let exif_at = boxes[2].1;
        assert_eq!(&wrapped[exif_at + 8..exif_at + 12], &[0; 4]);
        assert_eq!(&wrapped[boxes[4].1 + 8..], codestream.as_slice());

        let decode = |bytes: &[u8]| {
            let decoder =
                jxl_oxide::integration::JxlDecoder::new(Cursor::new(bytes.to_vec())).unwrap();
            DynamicImage::from_decoder(decoder).unwrap().to_rgb8()
        };
        assert_eq!(decode(&wrapped), rgb);
        assert_eq!(decode(&codestream), rgb);
    }

    #[test]
    fn png_gets_one_valid_itxt_chunk_before_image_data() {
        let (mut png, image) = encoded(ImageFormat::Png);
        png_insert_xmp(&mut png, PACKET).unwrap();
        png_insert_xmp(&mut png, PACKET).unwrap();
        // The png decoder checks chunk CRCs.
        let decoder = png::Decoder::new(Cursor::new(&png));
        let reader = decoder.read_info().unwrap();
        let itxt = &reader.info().utf8_text;
        assert_eq!(itxt.len(), 1);
        assert_eq!(itxt[0].keyword, "XML:com.adobe.xmp");
        assert_eq!(itxt[0].get_text().unwrap().as_bytes(), PACKET);
        assert_eq!(
            image::load_from_memory(&png).unwrap().as_bytes(),
            image.as_bytes()
        );
    }
}
