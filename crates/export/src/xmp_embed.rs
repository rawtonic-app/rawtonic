//! Embeds an XMP packet in exported JPEG and PNG files, where Lightroom and
//! Bridge read keywords, title and caption, and wraps JPEG XL codestreams in
//! the container that carries EXIF and XMP. TIFF (tag 700) and WebP carry XMP
//! through their own container code.

const JPEG_XMP_HEADER: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";
const PNG_XMP_KEYWORD: &[u8] = b"XML:com.adobe.xmp";

/// Largest standard XMP packet in the main APP1 segment (XMP specification,
/// part 3, "Storage in JPEG"); more goes to extension segments.
const JPEG_XMP_MAIN_MAX: usize = 65_502;
const JPEG_XMP_EXT_HEADER: &[u8] = b"http://ns.adobe.com/xmp/extension/\0";
/// Extension segment: header, 32-byte GUID, full length, offset, then data.
const JPEG_XMP_EXT_PREFIX: usize = JPEG_XMP_EXT_HEADER.len() + 32 + 4 + 4;
const JPEG_XMP_EXT_CHUNK: usize = 65_533 - JPEG_XMP_EXT_PREFIX;
const XMP_NOTE: &str = "http://ns.adobe.com/xmp/note/";

fn app1(payload_parts: &[&[u8]]) -> Vec<u8> {
    let payload: usize = payload_parts.iter().map(|p| p.len()).sum();
    let mut out = Vec::with_capacity(payload + 4);
    out.extend_from_slice(&[0xff, 0xe1]);
    out.extend_from_slice(&((payload + 2) as u16).to_be_bytes());
    for part in payload_parts {
        out.extend_from_slice(part);
    }
    out
}

/// Inserts the packet as an APP1 segment after the leading APP0/APP1
/// segments (JFIF, EXIF), replacing XMP segments that are already there. A
/// packet over one segment is split as the XMP specification (part 3)
/// describes: the standard XMP stays in the main packet, which gets
/// `xmpNote:HasExtendedXMP`, and the remaining properties go to
/// `http://ns.adobe.com/xmp/extension/` segments behind it. Which properties
/// stay in the main packet: see [`split_xmp`].
pub fn jpeg_insert_xmp(jpeg: &mut Vec<u8>, packet: &[u8]) -> Result<(), String> {
    if jpeg.get(0..2) != Some(&[0xff, 0xd8]) {
        return Err("not a JPEG file".to_string());
    }
    let (main, extended) = if packet.len() <= JPEG_XMP_MAIN_MAX {
        (packet.to_vec(), None)
    } else {
        let (main, guid, extended) = split_xmp(packet)?;
        (main, Some((guid, extended)))
    };

    let mut at = 2;
    let mut insert_at = 2;
    let mut replace: Vec<std::ops::Range<usize>> = Vec::new();
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
        if marker == 0xe1
            && (jpeg[at + 4..end].starts_with(JPEG_XMP_HEADER)
                || jpeg[at + 4..end].starts_with(JPEG_XMP_EXT_HEADER))
        {
            replace.push(at..end);
        } else if (marker == 0xe0 || marker == 0xe1) && insert_at == at {
            insert_at = end;
        }
        at = end;
    }

    let mut segments = app1(&[JPEG_XMP_HEADER, &main]);
    if let Some((guid, extended)) = &extended {
        let total = (extended.len() as u32).to_be_bytes();
        for (index, chunk) in extended.chunks(JPEG_XMP_EXT_CHUNK).enumerate() {
            let offset = ((index * JPEG_XMP_EXT_CHUNK) as u32).to_be_bytes();
            segments.extend(app1(&[
                JPEG_XMP_EXT_HEADER,
                guid.as_bytes(),
                &total,
                &offset,
                chunk,
            ]));
        }
    }
    // Existing XMP segments sit in the leading run; the new ones go where the
    // first old one was, else after JFIF/EXIF.
    let place = replace.first().map_or(insert_at, |r| r.start);
    for range in replace.into_iter().rev() {
        jpeg.drain(range);
    }
    jpeg.splice(place..place, segments);
    Ok(())
}

/// Splits a packet over one segment into the main packet (with
/// `xmpNote:HasExtendedXMP`), the GUID (upper-case MD5 of the extended part)
/// and the extended part (a full `x:xmpmeta` without `xpacket` wrapper).
///
/// What stays in the main packet, as a photo professional meets it first:
/// every attribute of the description (creating tool, Digital Source Type
/// marking of AI patches, rating, label) always; then the properties in this
/// order while they fit: rights, creator, other (IPTC) fields, title,
/// keywords, hierarchical keywords, caption, the rest. A property that does
/// not fit goes to the extension and later, smaller ones may still stay.
fn split_xmp(packet: &[u8]) -> Result<(Vec<u8>, String, Vec<u8>), String> {
    const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
    const DC: &str = "http://purl.org/dc/elements/1.1/";
    const LR: &str = "http://ns.adobe.com/lightroom/1.0/";
    let too_big = || "XMP packet exceeds one JPEG segment and cannot be split".to_string();
    let text = std::str::from_utf8(packet).map_err(|_| too_big())?;
    let doc = roxmltree::Document::parse(text).map_err(|_| too_big())?;
    let mut descriptions = doc
        .descendants()
        .filter(|n| n.has_tag_name((RDF, "Description")));
    let description = descriptions.next().ok_or_else(too_big)?;
    if descriptions.next().is_some() {
        return Err(too_big());
    }
    let children: Vec<_> = description.children().filter(|n| n.is_element()).collect();
    let rank = |n: &roxmltree::Node| match (n.tag_name().namespace(), n.tag_name().name()) {
        (Some(DC), "rights") => 0,
        (Some(DC), "creator") => 1,
        (Some(DC), "title") => 3,
        (Some(DC), "subject") => 4,
        (Some(LR), "hierarchicalSubject") => 5,
        (Some(DC), "description") => 6,
        (Some(DC | LR), _) => 7,
        _ => 2,
    };

    // The start tag ends at the first '>' outside quotes.
    let start = description.range().start;
    let mut quote = None;
    let mut tag_end = None;
    for (i, c) in text[start..].char_indices() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            (None, '>') => {
                tag_end = Some(start + i);
                break;
            }
            _ => {}
        }
    }
    let tag_end = tag_end.ok_or_else(too_big)?;
    let note_prefix = description
        .namespaces()
        .find(|ns| ns.uri() == XMP_NOTE)
        .and_then(|ns| ns.name().map(str::to_string));
    let (prefix, declaration) = match note_prefix {
        Some(prefix) => (prefix, String::new()),
        None => {
            if description.lookup_namespace_uri(Some("xmpNote")).is_some() {
                return Err(too_big());
            }
            (
                "xmpNote".to_string(),
                format!(" xmlns:xmpNote=\"{XMP_NOTE}\""),
            )
        }
    };
    let attribute = format!(
        "{declaration} {prefix}:HasExtendedXMP=\"{}\"",
        "0".repeat(32)
    );

    let removed: usize = children.iter().map(|c| c.range().len()).sum();
    let mut size = text.len() - removed + attribute.len();
    let mut order: Vec<usize> = (0..children.len()).collect();
    order.sort_by_key(|&i| rank(&children[i]));
    let mut moved = vec![true; children.len()];
    for i in order {
        let len = children[i].range().len();
        if size + len <= JPEG_XMP_MAIN_MAX {
            size += len;
            moved[i] = false;
        }
    }
    if moved.iter().all(|m| !m) || size > JPEG_XMP_MAIN_MAX {
        return Err(too_big());
    }

    let mut extended_children = String::new();
    let mut cuts: Vec<std::ops::Range<usize>> = Vec::new();
    for (child, moved) in children.iter().zip(&moved) {
        if *moved {
            extended_children.push_str(&text[child.range()]);
            cuts.push(child.range());
        }
    }
    let mut namespaces = String::new();
    for ns in description.namespaces() {
        let uri = ns.uri().replace('&', "&amp;").replace('"', "&quot;");
        match ns.name() {
            Some(name) => namespaces.push_str(&format!(" xmlns:{name}=\"{uri}\"")),
            None => namespaces.push_str(&format!(" xmlns=\"{uri}\"")),
        }
    }
    let extended = format!(
        "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"{RDF}\"><rdf:Description rdf:about=\"\"{namespaces}>{extended_children}</rdf:Description></rdf:RDF></x:xmpmeta>"
    )
    .into_bytes();
    let guid = {
        use md5::{Digest, Md5};
        Md5::digest(&extended)
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<String>()
    };

    let attribute = attribute.replace(&"0".repeat(32), &guid);
    let mut main = text.to_string();
    for range in cuts.into_iter().rev() {
        main.replace_range(range, "");
    }
    main.insert_str(tag_end, &attribute);
    Ok((main.into_bytes(), guid, extended))
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

    /// Spec-level reader (XMP part 3): main packet + reassembled extension.
    fn read_jpeg_xmp(jpeg: &[u8]) -> (String, Option<String>) {
        let mut main = None;
        let mut guid = String::new();
        let mut ext: Vec<u8> = Vec::new();
        let mut seen = 0usize;
        for (marker, at) in jpeg_markers(jpeg) {
            let size = u16::from_be_bytes([jpeg[at + 2], jpeg[at + 3]]) as usize;
            let body = &jpeg[at + 4..at + 2 + size];
            if marker != 0xe1 {
                continue;
            }
            if let Some(packet) = body.strip_prefix(JPEG_XMP_HEADER) {
                assert!(packet.len() <= JPEG_XMP_MAIN_MAX);
                main = Some(String::from_utf8(packet.to_vec()).unwrap());
            } else if let Some(rest) = body.strip_prefix(JPEG_XMP_EXT_HEADER) {
                guid = String::from_utf8(rest[..32].to_vec()).unwrap();
                let total = u32::from_be_bytes(rest[32..36].try_into().unwrap()) as usize;
                let offset = u32::from_be_bytes(rest[36..40].try_into().unwrap()) as usize;
                if ext.is_empty() {
                    ext = vec![0; total];
                }
                assert_eq!(offset, seen, "chunks in order");
                ext[offset..offset + rest.len() - 40].copy_from_slice(&rest[40..]);
                seen += rest.len() - 40;
            }
        }
        let main = main.unwrap();
        if ext.is_empty() {
            assert!(!main.contains("HasExtendedXMP"));
            return (main, None);
        }
        assert_eq!(seen, ext.len(), "extension complete");
        use md5::{Digest, Md5};
        let hash: String = Md5::digest(&ext)
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect();
        assert_eq!(hash, guid);
        assert!(main.contains(&format!("xmpNote:HasExtendedXMP=\"{guid}\"")));
        (main, Some(String::from_utf8(ext).unwrap()))
    }

    fn big_packet(generated: bool) -> (String, usize) {
        let metadata = rawtonic_metadata::Metadata {
            rating: 4,
            label: "Red".into(),
            title: "A title".into(),
            description: "A caption".into(),
            creator: "Jane Doe".into(),
            copyright: "(c) Jane Doe".into(),
            keywords: (0..6000)
                .map(|i| format!("keyword number {i:05} with a long tail"))
                .collect(),
            ..Default::default()
        };
        let source = generated.then_some(rawtonic_metadata::DIGITAL_SOURCE_COMPOSITE_AI);
        let packet =
            rawtonic_metadata::export_xmp_with_source(&metadata, "Rawtonic test", source).unwrap();
        assert!(packet.len() > 2 * JPEG_XMP_MAIN_MAX);
        (packet, metadata.keywords.len())
    }

    #[test]
    fn jpeg_keeps_xmp_larger_than_one_segment_complete() {
        for generated in [false, true] {
            let (packet, keywords) = big_packet(generated);
            let (mut jpeg, _) = encoded(ImageFormat::Jpeg);
            let before = image::load_from_memory(&jpeg).unwrap();
            jpeg_insert_xmp(&mut jpeg, packet.as_bytes()).unwrap();
            jpeg_insert_xmp(&mut jpeg, packet.as_bytes()).unwrap(); // replaces, never duplicates
            let (main, ext) = read_jpeg_xmp(&jpeg);
            let ext = ext.expect("extension segments");
            let main_doc = roxmltree::Document::parse(&main).unwrap();
            let ext_doc = roxmltree::Document::parse(&ext).unwrap();
            // Always in the main packet: tool, rating, label, rights, creator, marking.
            for needle in [
                "xmp:Rating=\"4\"",
                "xmp:Label=\"Red\"",
                "Rawtonic test",
                "(c) Jane Doe",
                "Jane Doe",
            ] {
                assert!(main.contains(needle), "{needle}");
            }
            assert_eq!(main.contains("DigitalSourceType"), generated);
            // Every keyword is in exactly one of the two parts.
            let subjects = |doc: &roxmltree::Document| {
                doc.descendants()
                    .filter(|n| {
                        n.has_tag_name("li") && n.parent().is_some_and(|p| p.has_tag_name("Bag"))
                    })
                    .count()
            };
            assert_eq!(subjects(&main_doc) + subjects(&ext_doc), keywords);
            assert!(ext.contains("keyword number 05999"));
            // Still a valid JPEG with unchanged pixels.
            assert_eq!(
                image::load_from_memory(&jpeg).unwrap().as_bytes(),
                before.as_bytes()
            );
            // exiftool, when installed, reads the merged result.
            {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join(format!("xmp-ext-{generated}.jpg"));
                std::fs::write(&path, &jpeg).unwrap();
                if let Ok(out) = std::process::Command::new("exiftool")
                    .args([
                        "-j",
                        "-m",
                        "-XMP-dc:Subject",
                        "-XMP-dc:Creator",
                        "-XMP-xmp:Rating",
                    ])
                    .arg(&path)
                    .output()
                {
                    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
                    assert_eq!(json[0]["Subject"].as_array().map_or(0, Vec::len), keywords);
                    assert_eq!(json[0]["Creator"], "Jane Doe");
                    assert_eq!(json[0]["Rating"], 4);
                }
            }
        }
    }

    #[test]
    fn jpeg_with_small_xmp_stays_a_single_main_segment() {
        let (mut jpeg, _) = encoded(ImageFormat::Jpeg);
        let plain = jpeg.clone();
        jpeg_insert_xmp(&mut jpeg, PACKET).unwrap();
        assert_eq!(
            jpeg.len(),
            plain.len() + 4 + JPEG_XMP_HEADER.len() + PACKET.len()
        );
        assert!(read_jpeg_xmp(&jpeg).1.is_none());
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
